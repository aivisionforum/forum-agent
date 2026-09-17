use super::*;
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};

type Operation = Box<dyn FnOnce(&mut Store) + Send + 'static>;
enum Command {
    Run(Operation),
    Shutdown(mpsc::Sender<()>),
}
struct Inner {
    sender: SyncSender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// A bounded actor handle. Never call blocking methods from an audio callback.
#[derive(Clone)]
pub struct CoreHandle {
    inner: Arc<Inner>,
}

pub struct CoreTicket<T> {
    receiver: Receiver<Result<T>>,
}
impl<T> CoreTicket<T> {
    pub fn wait(self) -> Result<T> {
        self.wait_timeout(Duration::from_secs(4))
    }
    pub fn wait_timeout(self, timeout: Duration) -> Result<T> {
        self.receiver.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => StoreError::AckUnknown,
            RecvTimeoutError::Disconnected => StoreError::ActorClosed,
        })?
    }
    pub fn try_take(&self) -> Result<Option<T>> {
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(StoreError::ActorClosed),
        }
    }
}

impl CoreHandle {
    pub fn open(path: impl AsRef<Path>, capacity: usize) -> Result<Self> {
        if capacity == 0 || capacity > 4096 {
            return Err(ValidationError::Invalid("actor_capacity").into());
        }
        let path = path.as_ref().to_owned();
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("forum-core-writer".into())
            .spawn(move || {
                let mut store = match Store::open(path) {
                    Ok(store) => {
                        let _ = ready_tx.send(Ok(()));
                        store
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let mut shutdown = None;
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Run(operation) => operation(&mut store),
                        Command::Shutdown(reply) => {
                            shutdown = Some(reply);
                            break;
                        }
                    }
                }
                // Confirmation means the SQLite connection and process lock are
                // actually closed, not merely that shutdown was queued.
                drop(store);
                if let Some(reply) = shutdown {
                    let _ = reply.send(());
                }
            })?;
        ready_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| StoreError::AckUnknown)??;
        Ok(Self {
            inner: Arc::new(Inner {
                sender,
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    pub fn try_call<T: Send + 'static, F: FnOnce(&mut Store) -> Result<T> + Send + 'static>(
        &self,
        operation: F,
    ) -> Result<CoreTicket<T>> {
        let (reply, receiver) = mpsc::channel();
        let command = Command::Run(Box::new(move |store| {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(store)))
                    .unwrap_or(Err(StoreError::ActorPanicked));
            let _ = reply.send(result);
        }));
        self.inner
            .sender
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => StoreError::QueueFull,
                TrySendError::Disconnected(_) => StoreError::ActorClosed,
            })?;
        Ok(CoreTicket { receiver })
    }

    pub fn call<T: Send + 'static, F: FnOnce(&mut Store) -> Result<T> + Send + 'static>(
        &self,
        operation: F,
    ) -> Result<T> {
        self.try_call(operation)?.wait()
    }

    pub fn ingest_json(&self, event: serde_json::Value) -> Result<Receipt> {
        check_event_size(&event)?;
        self.call(move |store| store.ingest_json(event))
    }
    pub fn try_ingest_json(&self, event: serde_json::Value) -> Result<CoreTicket<Receipt>> {
        check_event_size(&event)?;
        self.try_call(move |store| store.ingest_json(event))
    }
    pub fn shutdown(&self) -> Result<()> {
        {
            let mut thread = self
                .inner
                .thread
                .lock()
                .map_err(|_| StoreError::ActorPanicked)?;
            if thread.as_ref().is_none_or(|t| t.is_finished()) {
                if let Some(t) = thread.take() {
                    t.join().map_err(|_| StoreError::ActorPanicked)?;
                }
                return Ok(());
            }
        }
        let (reply, receiver) = mpsc::channel();
        self.inner
            .sender
            .try_send(Command::Shutdown(reply))
            .map_err(|e| match e {
                TrySendError::Full(_) => StoreError::QueueFull,
                TrySendError::Disconnected(_) => StoreError::AckUnknown,
            })?;
        receiver
            .recv_timeout(Duration::from_secs(4))
            .map_err(|_| StoreError::AckUnknown)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while self
            .inner
            .thread
            .lock()
            .map_err(|_| StoreError::ActorPanicked)?
            .as_ref()
            .is_some_and(|t| !t.is_finished())
        {
            if std::time::Instant::now() >= deadline {
                return Err(StoreError::AckUnknown);
            }
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(thread) = self
            .inner
            .thread
            .lock()
            .map_err(|_| StoreError::ActorPanicked)?
            .take()
        {
            thread.join().map_err(|_| StoreError::ActorPanicked)?;
        }
        Ok(())
    }
}

fn check_event_size(event: &serde_json::Value) -> Result<()> {
    if serde_json::to_vec(event)?.len() > 1024 * 1024 {
        return Err(ValidationError::Invalid("event_max_bytes").into());
    }
    Ok(())
}

impl StoreError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AlreadyOwned => "ALREADY_OWNED",
            Self::QueueFull => "QUEUE_FULL",
            Self::ActorClosed => "CORE_UNAVAILABLE",
            Self::ActorPanicked => "CORE_REQUEST_FAILED",
            Self::AckUnknown => "ACK_UNKNOWN",
            Self::DependencyNotReady => "DEPENDENCY_NOT_READY",
            Self::LateResult => "LATE_RESULT",
            Self::CoverageConflict => "COVERAGE_CONFLICT",
            Self::ScopeMismatch => "SCOPE_MISMATCH",
            Self::EventIdConflict => "EVENT_ID_CONFLICT",
            Self::EntityConflict => "ENTITY_CONFLICT",
            Self::InvalidState => "INVALID_STATE",
            Self::SealMismatch => "SEAL_MISMATCH",
            Self::InvalidCursor => "RESET_REQUIRED",
            Self::RevisionConflict { .. } => "REVISION_CONFLICT",
            Self::NotFound(_) => "NOT_FOUND",
            Self::UnsupportedDatabase(_) => "UNSUPPORTED_DATABASE",
            Self::Validation(_) | Self::Json(_) => "INVALID_EVENT",
            Self::Database(_) | Self::Io(_) => "STORE_UNAVAILABLE",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::AckUnknown
                | Self::QueueFull
                | Self::ActorClosed
                | Self::DependencyNotReady
                | Self::Database(_)
                | Self::Io(_)
        )
    }
}

impl Store {
    pub fn ingest_json(&mut self, event: serde_json::Value) -> Result<Receipt> {
        check_event_size(&event)?;
        let event_type: EventType =
            serde_json::from_value(event.get("type").cloned().unwrap_or_default())?;
        match event_type {
            EventType::AudioSegmentClosed => self.register_capture(&serde_json::from_value(event)?),
            EventType::TranscriptFinal => self.ingest_final(&serde_json::from_value(event)?),
            EventType::TranscriptRevised => self.revise_transcript(&serde_json::from_value(event)?),
            EventType::SessionChanged => self.transition_session(&serde_json::from_value(event)?),
            EventType::AudioGap => self.register_gap(&serde_json::from_value(event)?),
            EventType::CaptureStopped => self.capture_stopped(&serde_json::from_value(event)?),
            EventType::ProducerSealed => self.producer_sealed(&serde_json::from_value(event)?),
            EventType::ProducerReconciled => Err(StoreError::InvalidState),
            EventType::TranscriptSealed => self.seal_transcript(&serde_json::from_value(event)?),
            EventType::DirectionChanged => self.change_direction(&serde_json::from_value(event)?),
            EventType::TranslationRequested => {
                self.request_translation(&serde_json::from_value(event)?)
            }
            EventType::TranslationFinal => self.finish_translation(&serde_json::from_value(event)?),
            EventType::TranslationFailed => self.fail_translation(&serde_json::from_value(event)?),
        }
    }
}
