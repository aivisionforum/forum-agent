use crate::meeting::{project_transcript, MeetingHost, MeetingOptions, MeetingRepository};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use forum_contracts::Uuid;
use forum_runtime::resource_budget::{LivePermit, ResourceBudget};
use moxin_dora_bridge::{
    controller::DataflowController, dispatcher::DynamicNodeDispatcher, DoraStatus, SharedDoraState,
    TranslationDirection,
};
use parking_lot::Mutex;
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
        options: MeetingOptions,
        recovery: Option<Uuid>,
        stop_generation: u64,
    },
    Stop,
    Direction(TranslationDirection),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    Started(String),
    Draining(String),
    Degraded(String),
    Stopped {
        session_id: String,
        incomplete: bool,
        translation_pending: u64,
    },
    ShutdownFailed(String),
    Error(String),
}
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
type LifecycleHandler = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;
type LifecycleSlot = Arc<Mutex<Option<LifecycleHandler>>>;

/// Lifecycle side effects must not use the lossy UI status mailbox. Clone the
/// callback under its lock, then invoke it without holding that lock.
fn emit_runtime_event(
    tx: &Sender<RuntimeEvent>,
    pending_rx: &Receiver<RuntimeEvent>,
    lifecycle: &LifecycleSlot,
    event: RuntimeEvent,
) {
    if matches!(
        &event,
        RuntimeEvent::Started(_) | RuntimeEvent::Stopped { .. }
    ) {
        let handler = lifecycle.lock().clone();
        if let Some(handler) = handler {
            handler(event.clone());
        }
    }
    publish_event(tx, pending_rx, event);
}

struct ActiveMeeting {
    _resource_permit: LivePermit,
    dispatcher: DynamicNodeDispatcher,
    host: MeetingHost,
    started_at: Instant,
    announced: bool,
    stopping_at: Option<Instant>,
    draining_announced: bool,
    degraded: bool,
    force_cleanup: bool,
    last_projection: Instant,
    last_health: Instant,
    last_cleanup: Option<Instant>,
}
impl ActiveMeeting {
    fn request_stop(&mut self) -> Result<(), String> {
        // Request device release before attempting disk work.
        self.dispatcher.request_capture_stop();
        self.stopping_at.get_or_insert_with(Instant::now);
        self.host.mark_stopping()
    }
    fn cleanup(&mut self, shared: &SharedDoraState) -> Result<RuntimeEvent, String> {
        self.dispatcher.stop().map_err(|e| e.to_string())?;
        if let Ok(update) = project_transcript(&self.host.repository.core, self.host.id()) {
            shared.translation.set(Some(update));
        }
        let status = self.host.status()?;
        let translation_pending = self.host.translation_pending()?;
        self.host.stop_server()?;
        shared.translation_overlay_active.set(false);
        shared.translation_overlay_status.set("idle".into());
        shared.translation_stream.set(None);
        shared.status.set(DoraStatus::default());
        *shared.capture_context.write() = None;
        Ok(RuntimeEvent::Stopped {
            session_id: self.host.id().to_string(),
            incomplete: status.incomplete || !status.transcript_sealed,
            translation_pending,
        })
    }
}
pub struct TranslationRuntime {
    lifecycle_handler: LifecycleSlot,
    contained: Arc<AtomicBool>,
    resource_budget: ResourceBudget,
    shared_state: Arc<SharedDoraState>,
    current_session: Arc<Mutex<Option<Uuid>>>,
    repository: Result<MeetingRepository, String>,
    stop_generation: Arc<Mutex<u64>>,
    command_tx: Sender<RuntimeCommand>,
    event_rx: Receiver<RuntimeEvent>,
    stop_tx: Option<Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl TranslationRuntime {
    pub fn new(data_root: PathBuf) -> Self {
        let lifecycle_handler: LifecycleSlot = Arc::new(Mutex::new(None));
        let worker_lifecycle = lifecycle_handler.clone();
        // A finished or panicked thread is not evidence of owned resource exit.
        let contained = Arc::new(AtomicBool::new(false));
        let worker_contained = contained.clone();
        let resource_budget = ResourceBudget::default();
        let worker_budget = resource_budget.clone();
        let repository = MeetingRepository::open(data_root);
        let worker_repository = repository.clone();
        let current_session = Arc::new(Mutex::new(None));
        let worker_session = current_session.clone();
        let stop_generation = Arc::new(Mutex::new(0u64));
        let worker_generation = stop_generation.clone();
        let shared_state = SharedDoraState::new();
        let worker_state = shared_state.clone();
        let (command_tx, command_rx) = bounded(16);
        let (event_tx, event_rx) = bounded(1);
        let pending_events = event_rx.clone();
        let (stop_tx, stop_rx) = bounded(1);
        let worker = thread::spawn(move || {
            let emit =
                |event| emit_runtime_event(&event_tx, &pending_events, &worker_lifecycle, event);
            let mut active: Option<ActiveMeeting> = None;
            let mut exiting = false;
            let mut exit_started = None;
            let mut exit_deadline_reported = false;
            loop {
                if stop_rx.try_recv().is_ok() {
                    exiting = true;
                    exit_started.get_or_insert_with(Instant::now);
                    if let Some(current) = active.as_mut() {
                        let _ = current.request_stop();
                    }
                }
                while let Ok(command) = command_rx.try_recv() {
                    match command {
                        RuntimeCommand::Start {
                            dataflow_path,
                            mut env_vars,
                            options,
                            recovery,
                            stop_generation,
                        } => {
                            if exiting || active.is_some() {
                                emit(RuntimeEvent::ShutdownFailed(
                                    "上一场仍在处理，请等待停止完成后再开始".into(),
                                ));
                                continue;
                            }
                            {
                                let current_generation = worker_generation.lock();
                                if *current_generation != stop_generation {
                                    emit(RuntimeEvent::Stopped {
                                        session_id: String::new(),
                                        incomplete: false,
                                        translation_pending: 0,
                                    });
                                    continue;
                                }
                                worker_state
                                    .capture_stop_requested
                                    .store(false, Ordering::Release);
                            }
                            let repository = match &worker_repository {
                                Ok(r) => r.clone(),
                                Err(e) => {
                                    emit(RuntimeEvent::Error(e.clone()));
                                    continue;
                                }
                            };
                            let resource_permit =
                                match worker_budget.enter_live(Duration::from_secs(5)) {
                                    Ok(permit) => permit,
                                    Err(error) => {
                                        emit(RuntimeEvent::Error(error));
                                        continue;
                                    }
                                };
                            if worker_state.capture_stop_requested.load(Ordering::Acquire) {
                                emit(RuntimeEvent::Stopped {
                                    session_id: String::new(),
                                    incomplete: false,
                                    translation_pending: 0,
                                });
                                continue;
                            }
                            let mut host = match MeetingHost::create(repository, options, recovery)
                            {
                                Ok(h) => h,
                                Err(e) => {
                                    emit(RuntimeEvent::Error(e));
                                    continue;
                                }
                            };
                            *worker_session.lock() = Some(host.id());
                            let configuration = match serde_json::to_string(&host.config) {
                                Ok(c) => c,
                                Err(e) => {
                                    emit(RuntimeEvent::Error(e.to_string()));
                                    continue;
                                }
                            };
                            env_vars.insert("FORUM_RUNTIME_CONFIG".into(), configuration);
                            env_vars.insert(
                                "FORUM_RETRY_EXHAUSTED".into(),
                                if recovery.is_some() { "1" } else { "0" }.into(),
                            );
                            if recovery.is_some() {
                                // A single explicit recovery action owns a persisted retry budget.
                                env_vars.insert(
                                    "FORUM_TRANSLATOR_RECOVERY_ID".into(),
                                    Uuid::new_v4().to_string(),
                                );
                            } else {
                                env_vars.remove("FORUM_TRANSLATOR_RECOVERY_ID");
                            }
                            env_vars.insert(
                                "ASR_SOURCE_LANGUAGE".into(),
                                host.setup.options.source_language.clone(),
                            );
                            let direction = TranslationDirection::new(
                                host.setup.options.source_language.clone(),
                                host.setup.options.target_language.clone(),
                                1,
                            );
                            worker_state
                                .translation_direction_active
                                .set(direction.clone());
                            worker_state.translation_direction_request.set(direction);
                            *worker_state.capture_context.write() = Some(host.capture_context());
                            worker_state.capture_progress.set(Default::default());
                            let controller = match DataflowController::new(&dataflow_path) {
                                Ok(mut c) => {
                                    c.set_envs(env_vars);
                                    c
                                }
                                Err(e) => {
                                    let _ = host.interrupt("invalid dataflow configuration");
                                    *worker_state.capture_context.write() = None;
                                    emit(RuntimeEvent::Error(e.to_string()));
                                    continue;
                                }
                            };
                            let mut dispatcher = DynamicNodeDispatcher::with_shared_state(
                                controller,
                                worker_state.clone(),
                            );
                            let started = dispatcher.start();
                            let mut next = ActiveMeeting {
                                _resource_permit: resource_permit,
                                dispatcher,
                                host,
                                started_at: Instant::now(),
                                announced: false,
                                stopping_at: None,
                                draining_announced: false,
                                degraded: false,
                                force_cleanup: false,
                                last_projection: Instant::now(),
                                last_health: Instant::now(),
                                last_cleanup: None,
                            };
                            if let Err(error) = started {
                                let _ = next.host.interrupt("owned runtime failed during startup");
                                next.force_cleanup = true;
                                match next.cleanup(&worker_state) {
                                    Ok(_) => emit(RuntimeEvent::Error(format!(
                                        "本地模型启动失败：{error}"
                                    ))),
                                    Err(cleanup) => {
                                        emit(RuntimeEvent::ShutdownFailed(format!(
                                            "启动失败：{error}；资源仍待回收：{cleanup}"
                                        )));
                                        active = Some(next);
                                    }
                                }
                            } else {
                                worker_state.translation_overlay_active.set(true);
                                worker_state
                                    .translation_overlay_status
                                    .set("warming".into());
                                active = Some(next);
                            }
                        }
                        RuntimeCommand::Stop => {
                            if let Some(current) = active.as_mut() {
                                current.last_cleanup = None;
                                if let Err(error) = current.request_stop() {
                                    emit(RuntimeEvent::Degraded(format!(
                                        "已请求停止采音，会议状态写入待恢复：{error}"
                                    )));
                                }
                            } else {
                                // Stop may race the previous completion receipt. Always acknowledge it.
                                let id = *worker_session.lock();
                                let receipt = id.and_then(|id| {
                                    worker_repository.as_ref().ok().and_then(|repo| {
                                        repo.core
                                            .call(move |store| {
                                                let status = store.session_status(id)?;
                                                let pending = store
                                                    .coverage(id)?
                                                    .iter()
                                                    .filter(|c| {
                                                        matches!(
                                                            c.state.as_str(),
                                                            "pending" | "requested" | "failed"
                                                        )
                                                    })
                                                    .count()
                                                    as u64;
                                                Ok((
                                                    status.incomplete || !status.transcript_sealed,
                                                    pending,
                                                ))
                                            })
                                            .ok()
                                    })
                                });
                                let (incomplete, translation_pending) =
                                    receipt.unwrap_or((id.is_some(), 0));
                                emit(RuntimeEvent::Stopped {
                                    session_id: id.map(|id| id.to_string()).unwrap_or_default(),
                                    incomplete,
                                    translation_pending,
                                });
                            }
                        }
                        RuntimeCommand::Direction(direction) => {
                            if let Some(current) = active.as_mut() {
                                if current.stopping_at.is_none() && !current.host.replay_only {
                                    // Capture owns the sample boundary and durable direction ACK.
                                    worker_state.translation_direction_request.set(direction);
                                }
                            }
                        }
                    }
                }
                let mut remove_active = false;
                if let Some(current) = active.as_mut() {
                    if worker_state.capture_stop_requested.load(Ordering::Acquire)
                        && current.stopping_at.is_none()
                    {
                        if let Err(error) = current.request_stop() {
                            emit(RuntimeEvent::Degraded(format!(
                                "音频停止已收到，状态待写入：{error}"
                            )));
                        }
                    }
                    let progress = current.dispatcher.capture_progress();
                    if current.stopping_at.is_none() {
                        if let Err(error) = current.host.activate_if_ready() {
                            let _ = current
                                .host
                                .interrupt("readiness state could not be committed");
                            let _ = current.request_stop();
                            emit(RuntimeEvent::Degraded(error));
                        }
                        if (progress.started
                            || current.host.replay_only && progress.devices_released)
                            && !current.announced
                        {
                            current.announced = true;
                            if !current.host.replay_only {
                                worker_budget.live_ready();
                            }
                            if current.host.replay_only {
                                emit(RuntimeEvent::Draining(
                                    "正在从本机录音恢复会议字幕；未打开音频设备".into(),
                                ));
                            } else {
                                emit(RuntimeEvent::Started(current.host.id().to_string()));
                            }
                        }
                        if let Some(error) = &progress.error {
                            let _ = current.host.interrupt("audio capture or recovery failed");
                            let _ = current.request_stop();
                            emit(RuntimeEvent::Degraded(error.clone()));
                        }
                        if current.started_at.elapsed() > Duration::from_secs(125)
                            && !current.announced
                        {
                            let _ = current.host.interrupt("ASR readiness timed out");
                            let _ = current.request_stop();
                            current.force_cleanup = true;
                            emit(RuntimeEvent::Degraded(
                                "ASR 启动超时，已停止本场准备".into(),
                            ));
                        }
                        if current.host.replay_only && progress.capture_sealed {
                            current.stopping_at = Some(Instant::now());
                        }
                    }
                    if current.last_health.elapsed() >= Duration::from_secs(1) {
                        current.last_health = Instant::now();
                        let health = current.dispatcher.controller().read().owned_node_health();
                        match health {
                            Ok(nodes) => {
                                if nodes
                                    .iter()
                                    .any(|n| n.node_id == "translator" && !n.running)
                                    && !current.degraded
                                {
                                    current.degraded = true;
                                    emit(RuntimeEvent::Degraded(
                                        "翻译进程已退出；原文继续保存，译文可稍后恢复".into(),
                                    ));
                                }
                                if nodes.iter().any(|n| n.node_id == "asr" && !n.running)
                                    && current.stopping_at.is_none()
                                {
                                    current.force_cleanup = true;
                                }
                            }
                            Err(_) => current.force_cleanup = true,
                        }
                        let pressure = if current.stopping_at.is_some() || current.host.replay_only
                        {
                            Some("实时字幕正在收尾或恢复".to_string())
                        } else if current.degraded || current.force_cleanup {
                            Some("实时识别或翻译异常，优先保留原文".to_string())
                        } else if progress.outbox_pending > 0 {
                            Some("原文持久化正在等待确认".to_string())
                        } else {
                            let id = current.host.id();
                            let terminal = current
                                .host
                                .repository
                                .core
                                .call(move |s| Ok(s.terminal_segment_ids(id)?.len()));
                            if terminal.is_err() {
                                Some("会议存储暂不可用".to_string())
                            } else if progress
                                .segments_closed
                                .saturating_sub(terminal.unwrap_or(0))
                                > 4
                            {
                                Some("语音识别存在积压，优先处理原文".to_string())
                            } else {
                                match current.host.translation_backlog() {
                                    Ok(pending) if pending > 8 => {
                                        Some(format!("有 {pending} 项字幕等待处理，暂缓会议分析"))
                                    }
                                    Err(_) => Some("会议存储暂不可用".into()),
                                    _ => None,
                                }
                            }
                        };
                        if let Some(reason) = pressure.as_ref().filter(|reason| {
                            reason.starts_with("语音识别存在积压")
                                || reason.starts_with("有 ")
                                || reason.as_str() == "原文持久化正在等待确认"
                        }) {
                            worker_budget
                                .set_transient_pressure(reason.clone(), Duration::from_secs(8));
                        } else {
                            worker_budget.set_pressure(pressure);
                        }
                    }
                    if current.force_cleanup && current.stopping_at.is_none() {
                        let _ = current
                            .host
                            .interrupt("ASR or owned runtime exited unexpectedly");
                        let _ = current.request_stop();
                    }
                    if current.last_projection.elapsed() >= Duration::from_millis(250) {
                        current.last_projection = Instant::now();
                        match project_transcript(&current.host.repository.core, current.host.id()) {
                            Ok(update) => worker_state.translation.set(Some(update)),
                            Err(e) => {
                                let _ = current.request_stop();
                                emit(RuntimeEvent::Degraded(format!(
                                    "会议存储不可用，已停止新增采音：{e}"
                                )));
                            }
                        }
                    }
                    if let Some(stopping) = current.stopping_at {
                        if progress.devices_released && !current.draining_announced {
                            current.draining_announced = true;
                            emit(RuntimeEvent::Draining(
                                "音频设备已释放，正在保存最后的字幕".into(),
                            ));
                        }
                        let sealed = if progress.capture_sealed {
                            current.host.try_seal().unwrap_or(false)
                        } else {
                            false
                        };
                        let translation_done =
                            sealed && current.host.translation_pending().is_ok_and(|n| n == 0);
                        let canceled_preparation = progress.devices_released
                            && !progress.started
                            && !progress.capture_sealed;
                        let timed_out =
                            stopping.elapsed() >= Duration::from_secs(30) || canceled_preparation;
                        if (translation_done
                            || timed_out
                            || current.force_cleanup && progress.devices_released)
                            && current
                                .last_cleanup
                                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
                        {
                            current.last_cleanup = Some(Instant::now());
                            if !sealed {
                                let _ = current
                                    .host
                                    .interrupt("stopped with pending durable recovery work");
                            }
                            match current.cleanup(&worker_state) {
                                Ok(event) => {
                                    emit(event);
                                    remove_active = true;
                                }
                                Err(error) => {
                                    current.force_cleanup = true;
                                    emit(RuntimeEvent::ShutdownFailed(error));
                                }
                            }
                        }
                    }
                }
                if remove_active {
                    active = None;
                }
                if exiting && active.is_none() {
                    worker_contained.store(true, Ordering::Release);
                    break;
                }
                if exiting
                    && !exit_deadline_reported
                    && exit_started.is_some_and(|t| t.elapsed() > Duration::from_secs(40))
                {
                    exit_deadline_reported = true;
                    if let Some(current) = active.as_mut() {
                        let _ = current
                            .host
                            .interrupt("application exit exceeded drain deadline");
                        current.force_cleanup = true;
                        current.last_cleanup = None;
                    }
                    emit(RuntimeEvent::ShutdownFailed(
                        "退出收尾超过期限，资源退出尚未确认；保留所有权并继续重试回收".into(),
                    ));
                    // Keep ActiveMeeting and its LivePermit. The ordinary
                    // bounded cleanup path retries every five seconds; only
                    // successful cleanup may remove the active owner.
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Self {
            lifecycle_handler,
            contained,
            resource_budget,
            shared_state,
            current_session,
            repository,
            stop_generation,
            command_tx,
            event_rx,
            stop_tx: Some(stop_tx),
            worker: Some(worker),
        }
    }
    pub fn current_session_id(&self) -> Option<Uuid> {
        *self.current_session.lock()
    }
    pub fn resource_budget(&self) -> ResourceBudget {
        self.resource_budget.clone()
    }
    pub fn set_lifecycle_handler(&self, handler: LifecycleHandler) {
        *self.lifecycle_handler.lock() = Some(handler);
    }
    pub fn begin_shutdown(&self) {
        self.shared_state
            .capture_stop_requested
            .store(true, Ordering::Release);
        if let Some(tx) = &self.stop_tx {
            let _ = tx.try_send(());
        }
    }
    pub fn shutdown_complete(&self) -> bool {
        self.contained.load(Ordering::Acquire)
            && self
                .worker
                .as_ref()
                .is_none_or(|worker| worker.is_finished())
    }
    pub fn repository(&self) -> Result<&MeetingRepository, String> {
        self.repository.as_ref().map_err(Clone::clone)
    }
    pub fn start(
        &self,
        dataflow_path: PathBuf,
        env_vars: HashMap<String, String>,
        options: MeetingOptions,
        recovery: Option<Uuid>,
    ) -> Result<(), String> {
        let stop_generation = *self.stop_generation.lock();
        self.command_tx
            .try_send(RuntimeCommand::Start {
                dataflow_path,
                env_vars,
                options,
                recovery,
                stop_generation,
            })
            .map_err(|e| format!("启动队列不可用：{e}"))
    }
    pub fn stop(&self) -> Result<(), String> {
        {
            let mut generation = self.stop_generation.lock();
            *generation = generation.wrapping_add(1);
            self.shared_state
                .capture_stop_requested
                .store(true, Ordering::Release);
        }
        match self.command_tx.try_send(RuntimeCommand::Stop) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()), // worker also observes the non-droppable stop flag
            Err(TrySendError::Disconnected(_)) => Err("本地运行线程已关闭".into()),
        }
    }
    pub fn change_direction(&self, direction: TranslationDirection) -> Result<(), String> {
        self.command_tx
            .try_send(RuntimeCommand::Direction(direction))
            .map_err(|e| e.to_string())
    }
    pub fn shared_state(&self) -> &Arc<SharedDoraState> {
        &self.shared_state
    }
    pub fn poll_events(&self) -> Vec<RuntimeEvent> {
        self.event_rx.try_iter().collect()
    }
}
impl Drop for TranslationRuntime {
    fn drop(&mut self) {
        self.shared_state
            .capture_stop_requested
            .store(true, Ordering::Release);
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.try_send(());
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
    fn stop_without_active_session_acknowledges_and_allows_retry() {
        let root = PathBuf::from("/tmp").join(format!("forum-idle-stop-{}", Uuid::new_v4()));
        let runtime = TranslationRuntime::new(root.clone());
        for _ in 0..2 {
            runtime.stop().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(event) = runtime.poll_events().into_iter().next() {
                    assert!(matches!(
                        event,
                        RuntimeEvent::Stopped {
                            incomplete: false,
                            translation_pending: 0,
                            ..
                        }
                    ));
                    break;
                }
                assert!(Instant::now() < deadline, "idle Stop lost acknowledgement");
                thread::sleep(Duration::from_millis(10));
            }
        }
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
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
        let final_event = RuntimeEvent::Stopped {
            session_id: "test".into(),
            incomplete: true,
            translation_pending: 2,
        };
        publish_event(&send, &recv, final_event.clone());
        assert_eq!(recv.try_recv().unwrap(), final_event);
    }
    #[test]
    fn lifecycle_survives_overwritten_ui_mailbox_without_duplicate_callbacks() {
        let (send, recv) = bounded(1);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let output = observed.clone();
        let lifecycle: LifecycleSlot = Arc::new(Mutex::new(Some(Arc::new(move |event| {
            output.lock().push(event);
        }))));
        let started = RuntimeEvent::Started("session-a".into());
        let stopped = RuntimeEvent::Stopped {
            session_id: "session-a".into(),
            incomplete: false,
            translation_pending: 0,
        };
        emit_runtime_event(&send, &recv, &lifecycle, started.clone());
        for _ in 0..4 {
            emit_runtime_event(
                &send,
                &recv,
                &lifecycle,
                RuntimeEvent::Degraded("UI replaced".into()),
            );
        }
        emit_runtime_event(&send, &recv, &lifecycle, stopped.clone());
        emit_runtime_event(
            &send,
            &recv,
            &lifecycle,
            RuntimeEvent::Error("latest UI state".into()),
        );
        assert_eq!(*observed.lock(), vec![started, stopped]);
        assert_eq!(
            recv.try_recv().unwrap(),
            RuntimeEvent::Error("latest UI state".into())
        );
        assert!(recv.try_recv().is_err());
    }

    #[test]
    fn lifecycle_callback_can_replace_handler_without_locking_itself() {
        let (send, recv) = bounded(1);
        let lifecycle: LifecycleSlot = Arc::new(Mutex::new(None));
        let callback_slot = lifecycle.clone();
        *lifecycle.lock() = Some(Arc::new(move |_| {
            let mut unlocked = callback_slot
                .try_lock()
                .expect("callback ran under lifecycle lock");
            *unlocked = None;
        }));
        emit_runtime_event(&send, &recv, &lifecycle, RuntimeEvent::Started("a".into()));
        assert!(lifecycle.lock().is_none());
    }

    #[test]
    fn idle_shutdown_confirms_containment_before_reporting_complete() {
        let root = PathBuf::from("/tmp").join(format!("forum-idle-exit-{}", Uuid::new_v4()));
        let runtime = TranslationRuntime::new(root.clone());
        assert!(!runtime.shutdown_complete());
        runtime.begin_shutdown();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.shutdown_complete() {
            assert!(Instant::now() < deadline, "idle exit was not confirmed");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(runtime.contained.load(Ordering::Acquire));
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finished_or_panicked_worker_without_containment_is_not_shutdown_complete() {
        let root = PathBuf::from("/tmp").join(format!("forum-unconfirmed-exit-{}", Uuid::new_v4()));
        let mut runtime = TranslationRuntime::new(root.clone());
        runtime.begin_shutdown();
        runtime.worker.take().unwrap().join().unwrap();
        for panic_worker in [false, true] {
            // Model-free fault injection at the exact proof boundary: a joined
            // thread is insufficient if its owned cleanup was never confirmed.
            runtime.contained.store(false, Ordering::Release);
            runtime.worker = Some(thread::spawn(move || {
                assert!(!panic_worker, "injected worker panic before exit proof");
            }));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !runtime.worker.as_ref().unwrap().is_finished() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            assert!(!runtime.shutdown_complete());
            let _ = runtime.worker.take().unwrap().join();
        }
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }
}
