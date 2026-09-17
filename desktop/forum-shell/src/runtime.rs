use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use moxin_dora_bridge::{
    controller::DataflowController, dispatcher::DynamicNodeDispatcher, DoraStatus, SharedDoraState,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

enum RuntimeCommand {
    Start {
        dataflow_path: PathBuf,
        env_vars: HashMap<String, String>,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    Started(String),
    Stopped,
    ShutdownFailed(String),
    Error(String),
}

/// A paused UI consumes the latest lifecycle state when it resumes. Never
/// block cleanup behind a full event pipe or drop the final shutdown state.
fn publish_event(
    tx: &Sender<RuntimeEvent>,
    pending_rx: &Receiver<RuntimeEvent>,
    mut event: RuntimeEvent,
) {
    loop {
        match tx.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(latest)) => {
                event = latest;
                let _ = pending_rx.try_recv();
            }
        }
    }
}

pub struct TranslationRuntime {
    shared_state: Arc<SharedDoraState>,
    command_tx: Sender<RuntimeCommand>,
    event_rx: Receiver<RuntimeEvent>,
    stop_tx: Option<Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TranslationRuntime {
    pub fn new() -> Self {
        let (command_tx, command_rx) = bounded(16);
        let (event_tx, event_rx) = bounded(1);
        let pending_events = event_rx.clone();
        let (stop_tx, stop_rx) = bounded(1);
        let running = Arc::new(AtomicBool::new(false));
        let shared_state = SharedDoraState::new();
        let worker_running = Arc::clone(&running);
        let worker_state = Arc::clone(&shared_state);

        let worker = thread::spawn(move || {
            let mut dispatcher: Option<DynamicNodeDispatcher> = None;
            let mut started_at: Option<Instant> = None;
            let mut last_status_check = Instant::now();

            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                while let Ok(command) = command_rx.try_recv() {
                    if stop_rx.try_recv().is_ok() {
                        if let Some(mut current) = dispatcher.take() {
                            let _ = current.stop();
                        }
                        return;
                    }
                    match command {
                        RuntimeCommand::Start {
                            dataflow_path,
                            env_vars,
                        } => {
                            if let Some(previous) = dispatcher.as_mut() {
                                if let Err(error) = previous.stop() {
                                    publish_event(&event_tx, &pending_events, RuntimeEvent::ShutdownFailed(format!(
                                        "Previous session cleanup is incomplete; retry Stop before starting: {error}"
                                    )));
                                    continue;
                                }
                            }
                            dispatcher = None;
                            // Paths and runtime endpoints stay scoped to owned subprocesses.
                            let result =
                                DataflowController::new(&dataflow_path).map(|mut controller| {
                                    controller.set_envs(env_vars);
                                    DynamicNodeDispatcher::with_shared_state(
                                        controller,
                                        Arc::clone(&worker_state),
                                    )
                                });
                            let result = result.and_then(|mut next| match next.start() {
                                Ok(id) => Ok((next, id)),
                                Err(error) => {
                                    if let Err(cleanup) = next.stop() {
                                        dispatcher = Some(next);
                                        publish_event(&event_tx, &pending_events, RuntimeEvent::ShutdownFailed(format!(
                                            "Start failed: {error}; cleanup still pending: {cleanup}"
                                        )));
                                    }
                                    Err(error)
                                }
                            });

                            match result {
                                Ok((next, id)) => {
                                    let active_bridges = next
                                        .bindings()
                                        .into_iter()
                                        .filter(|binding| {
                                            binding.state
                                                == moxin_dora_bridge::BridgeState::Connected
                                        })
                                        .map(|binding| binding.node_id.clone())
                                        .collect();
                                    worker_state.status.set(DoraStatus {
                                        active_bridges,
                                        last_error: None,
                                    });
                                    worker_running.store(true, Ordering::Release);
                                    worker_state.translation_overlay_active.set(true);
                                    worker_state
                                        .translation_overlay_status
                                        .set("warming".into());
                                    started_at = Some(Instant::now());
                                    dispatcher = Some(next);
                                    publish_event(
                                        &event_tx,
                                        &pending_events,
                                        RuntimeEvent::Started(id),
                                    );
                                }
                                Err(error) => {
                                    if dispatcher.is_some() {
                                        continue;
                                    }
                                    let message =
                                        format!("Failed to start translation dataflow: {error}");
                                    worker_running.store(false, Ordering::Release);
                                    worker_state.status.set(DoraStatus {
                                        active_bridges: Vec::new(),
                                        last_error: Some(message.clone()),
                                    });
                                    worker_state.translation_overlay_active.set(false);
                                    worker_state.translation_overlay_status.set("idle".into());
                                    publish_event(
                                        &event_tx,
                                        &pending_events,
                                        RuntimeEvent::Error(message),
                                    );
                                }
                            }
                        }
                        RuntimeCommand::Stop => {
                            worker_state
                                .translation_overlay_status
                                .set("stopping".into());
                            if let Some(current) = dispatcher.as_mut() {
                                if let Err(error) = current.stop() {
                                    worker_state.set_error(Some(error.to_string()));
                                    publish_event(
                                        &event_tx,
                                        &pending_events,
                                        RuntimeEvent::ShutdownFailed(format!(
                                        "Translation cleanup is incomplete; retry Stop: {error}"
                                    )),
                                    );
                                    continue;
                                }
                                log::info!(
                                    "Owned Dora shutdown: {:?}",
                                    current.controller().read().last_shutdown_report()
                                );
                            }
                            dispatcher = None;
                            worker_running.store(false, Ordering::Release);
                            worker_state.status.set(DoraStatus::default());
                            worker_state.translation_overlay_active.set(false);
                            worker_state.translation_overlay_status.set("idle".into());
                            started_at = None;
                            publish_event(&event_tx, &pending_events, RuntimeEvent::Stopped);
                        }
                    }
                }

                let in_startup_grace = started_at
                    .map(|started| started.elapsed() < Duration::from_secs(10))
                    .unwrap_or(false);
                if !in_startup_grace && last_status_check.elapsed() >= Duration::from_secs(2) {
                    last_status_check = Instant::now();
                    let ended = dispatcher.as_ref().map(|current| {
                        current
                            .controller()
                            .read()
                            .get_status()
                            .map(|status| !status.state.is_running())
                    });
                    if matches!(ended, Some(Ok(true)) | Some(Err(_)))
                        && worker_running.load(Ordering::Acquire)
                    {
                        if let Some(current) = dispatcher.as_mut() {
                            match current.stop() {
                                Ok(()) => {
                                    dispatcher = None;
                                    worker_running.store(false, Ordering::Release);
                                    worker_state.translation_overlay_active.set(false);
                                    worker_state.translation_overlay_status.set("idle".into());
                                    started_at = None;
                                    publish_event(
                                        &event_tx,
                                        &pending_events,
                                        RuntimeEvent::Stopped,
                                    );
                                }
                                Err(error) => {
                                    // Retain ownership and suppress repeated status-triggered cleanup.
                                    worker_running.store(false, Ordering::Release);
                                    publish_event(
                                        &event_tx,
                                        &pending_events,
                                        RuntimeEvent::ShutdownFailed(error.to_string()),
                                    );
                                }
                            }
                        }
                    }
                }

                thread::sleep(Duration::from_millis(12));
            }

            if let Some(mut current) = dispatcher {
                let _ = current.stop();
            }
        });

        Self {
            shared_state,
            command_tx,
            event_rx,
            stop_tx: Some(stop_tx),
            worker: Some(worker),
        }
    }

    pub fn start(
        &self,
        dataflow_path: PathBuf,
        env_vars: HashMap<String, String>,
    ) -> Result<(), String> {
        self.command_tx
            .try_send(RuntimeCommand::Start {
                dataflow_path,
                env_vars,
            })
            .map_err(|error| format!("Could not submit translation start command: {error}"))
    }

    pub fn stop(&self) -> Result<(), String> {
        self.command_tx
            .try_send(RuntimeCommand::Stop)
            .map_err(|error| format!("Could not submit translation stop command: {error}"))
    }

    pub fn shared_state(&self) -> &Arc<SharedDoraState> {
        &self.shared_state
    }

    pub fn poll_events(&self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

impl Drop for TranslationRuntime {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.try_send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paused_ui_receives_final_cleanup_state_after_many_transitions() {
        let (send, recv) = bounded(1);
        for n in 0..40 {
            publish_event(&send, &recv, RuntimeEvent::Started(n.to_string()));
        }
        publish_event(
            &send,
            &recv,
            RuntimeEvent::ShutdownFailed("worker still owned".into()),
        );
        assert_eq!(
            recv.try_recv().unwrap(),
            RuntimeEvent::ShutdownFailed("worker still owned".into())
        );
        publish_event(&send, &recv, RuntimeEvent::Stopped);
        assert_eq!(recv.try_recv().unwrap(), RuntimeEvent::Stopped);
    }
}
