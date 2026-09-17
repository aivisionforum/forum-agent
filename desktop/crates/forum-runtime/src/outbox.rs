use crate::{atomic_write, private_directory, Endpoint, RpcError, RpcResult, RuntimeClient};
use anyhow::{ensure, Context};
use forum_contracts::{Event, EventType, Producer, SessionSpec, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub endpoint: Endpoint,
    pub session: SessionSpec,
    pub producer_dir: PathBuf,
    pub producer_name: String,
}
impl RuntimeConfig {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(value) = std::env::var_os("FORUM_RUNTIME_CONFIG") else {
            return Ok(None);
        };
        let config: Self = serde_json::from_str(
            &value
                .into_string()
                .map_err(|_| anyhow::anyhow!("runtime config is not UTF-8"))?,
        )?;
        config.validate()?;
        Ok(Some(config))
    }
    pub fn for_producer(&self, name: &str) -> anyhow::Result<Self> {
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "invalid producer role"
        );
        let mut config = self.clone();
        config.producer_dir = config.producer_dir.join(name);
        config.producer_name = name.into();
        Ok(config)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        self.endpoint.validate()?;
        self.session.validate()?;
        ensure!(
            self.producer_dir.is_absolute() && !self.producer_name.trim().is_empty(),
            "invalid producer configuration"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableReceipt {
    pub message_id: Uuid,
    pub store_seq: u64,
    pub duplicate: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub run_id: Uuid,
    #[serde(default)]
    pub owner_pid: u32,
    pub session_id: Uuid,
    pub producer_name: String,
}

#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub message_id: Uuid,
    pub event: Value,
    pub path: PathBuf,
}

struct ProducerOwnership {
    file: File,
    owner_pid: u32,
}
impl Drop for ProducerOwnership {
    fn drop(&mut self) {
        // flock belongs to the open file description, which fork/dup can share.
        // Release when the actual producer closes, but never from a fork copy.
        if self.owner_pid == std::process::id() {
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

pub struct DurableProducer {
    config: RuntimeConfig,
    client: RuntimeClient,
    run_id: Uuid,
    sequence: u64,
    append_poisoned: bool,
    cleanup_warning: Option<String>,
    // Exclusive filesystem lock prevents two workers sharing one producer journal.
    _ownership: ProducerOwnership,
}
impl DurableProducer {
    pub fn open(config: RuntimeConfig) -> anyhow::Result<Self> {
        config.validate()?;
        private_directory(&config.producer_dir)?;
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(config.producer_dir.join("producer.lock"))?;
        ensure!(
            unsafe { libc::flock(ownership.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "producer journal already owned"
        );
        let ownership = ProducerOwnership {
            file: ownership,
            owner_pid: std::process::id(),
        };
        let client = RuntimeClient::new(config.endpoint.clone());
        let run_id = Uuid::new_v4();
        atomic_write(
            &config.producer_dir.join(format!("{run_id}.run")),
            &serde_json::to_vec(&RunRecord {
                run_id,
                owner_pid: std::process::id(),
                session_id: config.session.session_id,
                producer_name: config.producer_name.clone(),
            })?,
        )?;
        Ok(Self {
            config,
            client,
            run_id,
            sequence: 0,
            append_poisoned: false,
            cleanup_warning: None,
            _ownership: ownership,
        })
    }
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }
    pub fn cleanup_warning(&self) -> Option<&str> {
        self.cleanup_warning.as_deref()
    }
    pub fn prior_run_records(&self) -> anyhow::Result<Vec<RunRecord>> {
        let mut runs = Vec::new();
        for entry in fs::read_dir(&self.config.producer_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| extension == "run") {
                let metadata = fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() < 4096,
                    "invalid run journal"
                );
                let record: RunRecord = serde_json::from_slice(&fs::read(path)?)?;
                ensure!(
                    record.session_id == self.config.session.session_id
                        && record.producer_name == self.config.producer_name,
                    "producer run scope mismatch"
                );
                if record.run_id != self.run_id {
                    runs.push(record);
                }
            }
        }
        runs.sort_by_key(|record| record.run_id);
        Ok(runs)
    }
    pub fn prior_run_ids(&self) -> anyhow::Result<Vec<Uuid>> {
        Ok(self
            .prior_run_records()?
            .into_iter()
            .map(|record| record.run_id)
            .collect())
    }
    pub fn last_seq(&self) -> u64 {
        self.sequence
    }
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
    pub fn client(&self) -> &RuntimeClient {
        &self.client
    }
    pub fn append<T: Serialize>(
        &mut self,
        event_type: EventType,
        payload: &T,
    ) -> anyhow::Result<PendingEvent> {
        ensure!(
            !self.append_poisoned,
            "outbox append previously failed; reopen/replay before producing more events"
        );
        // Backpressure is explicit before accepting a new durable event. Never
        // silently overwrite an older final when core is unavailable.
        ensure!(
            self.pending()?.len() < 10_000,
            "producer outbox capacity exhausted; stop capture"
        );
        let seq = self
            .sequence
            .checked_add(1)
            .context("producer sequence exhausted")?;
        let event = Event {
            schema_version: SCHEMA_VERSION,
            message_id: Uuid::new_v4(),
            event_type,
            event_id: self.config.session.event_id,
            room_id: self.config.session.room_id,
            session_id: self.config.session.session_id,
            producer: Producer {
                name: self.config.producer_name.clone(),
                run_id: self.run_id,
                seq,
            },
            payload,
        };
        event.validate_envelope(event_type)?;
        let value = serde_json::to_value(&event)?;
        let bytes = serde_json::to_vec(&value)?;
        ensure!(
            bytes.len() < crate::ipc::MAX_RPC_BYTES - 4096,
            "producer event exceeds RPC capacity"
        );
        let path = self.config.producer_dir.join(format!(
            "{}-{seq:020}-{}.json",
            self.run_id, event.message_id
        ));
        ensure!(!path.exists(), "outbox identity collision");
        if let Err(error) = atomic_write(&path, &bytes) {
            // Rename may have succeeded before directory sync failed. Do not
            // reuse this sequence for a different body in the current process.
            self.append_poisoned = true;
            return Err(error);
        }
        self.sequence = seq;
        Ok(PendingEvent {
            message_id: event.message_id,
            event: value,
            path,
        })
    }
    pub fn pending(&self) -> anyhow::Result<Vec<PendingEvent>> {
        let mut paths = fs::read_dir(&self.config.producer_dir)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
        paths.sort();
        ensure!(
            paths.len() <= 10_000,
            "producer outbox exceeds bounded capacity"
        );
        let mut events = Vec::new();
        for path in paths {
            let meta = fs::symlink_metadata(&path)?;
            ensure!(
                meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.len() <= crate::ipc::MAX_RPC_BYTES as u64,
                "invalid outbox file"
            );
            let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
            let message_id: Uuid = serde_json::from_value(
                value
                    .get("message_id")
                    .context("outbox message ID missing")?
                    .clone(),
            )?;
            ensure!(
                !message_id.is_nil()
                    && value["session_id"] == json!(self.config.session.session_id)
                    && value["event_id"] == json!(self.config.session.event_id)
                    && value["room_id"] == json!(self.config.session.room_id),
                "outbox event scope mismatch"
            );
            ensure!(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.ends_with(&message_id.to_string())),
                "outbox filename identity mismatch"
            );
            events.push(PendingEvent {
                message_id,
                event: value,
                path,
            });
        }
        Ok(events)
    }
    pub fn flush_one(&mut self, message_id: Uuid) -> RpcResult<DurableReceipt> {
        let pending = self
            .pending()
            .map_err(|e| RpcError::new("OUTBOX_IO", false, e))?
            .into_iter()
            .find(|event| event.message_id == message_id)
            .ok_or_else(|| RpcError::new("OUTBOX_NOT_FOUND", false, "message is not pending"))?;
        let value = self.client.call("ingest", json!({"event":pending.event}))?;
        let receipt: DurableReceipt =
            serde_json::from_value(value).map_err(|e| RpcError::new("INVALID_RECEIPT", true, e))?;
        if receipt.message_id != message_id || receipt.store_seq == 0 {
            return Err(RpcError::new(
                "INVALID_RECEIPT",
                true,
                "ack does not match pending message",
            ));
        }
        // A durable ack for this exact body has arrived. A crash before unlink
        // produces a duplicate replay; a missing/failed ack leaves the file.
        let cleanup = fs::remove_file(&pending.path)
            .and_then(|_| File::open(&self.config.producer_dir).and_then(|f| f.sync_all()));
        if let Err(error) = cleanup {
            // Commit already happened and its exact receipt arrived. Cleanup
            // failure must never turn that fact back into a failed operation;
            // any surviving journal entry can safely replay as a duplicate.
            self.cleanup_warning = Some(error.to_string());
            eprintln!("Forum durable event committed; outbox cleanup needs retry: {error}");
        }
        Ok(receipt)
    }
    pub fn flush_pending(&mut self) -> Vec<(Uuid, RpcResult<DurableReceipt>)> {
        match self.pending() {
            Ok(events) => {
                let mut receipts = Vec::new();
                for event in events {
                    let result = self.flush_one(event.message_id);
                    let failed = result.is_err();
                    receipts.push((event.message_id, result));
                    // Preserve producer order and bound one failed flush to one
                    // RPC deadline. Do not multiply an unavailable core timeout
                    // by every queued event during Stop or startup recovery.
                    if failed {
                        break;
                    }
                }
                receipts
            }
            Err(error) => vec![(Uuid::nil(), Err(RpcError::new("OUTBOX_IO", false, error)))],
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::mem::ManuallyDrop;

    struct Fixture {
        root: PathBuf,
        config: RuntimeConfig,
    }
    impl Fixture {
        fn new() -> Self {
            let root = PathBuf::from("/tmp").join(format!("forum-producer-lock-{}", Uuid::new_v4()));
            private_directory(&root).unwrap();
            let config = RuntimeConfig {
                endpoint: Endpoint::new(root.join("core.sock")),
                session: SessionSpec {
                    session_id: Uuid::new_v4(),
                    event_id: Uuid::new_v4(),
                    room_id: Uuid::new_v4(),
                    owner_device_id: Uuid::new_v4(),
                    title: "synthetic lock test".into(),
                },
                producer_dir: root.join("producer"),
                producer_name: "lock-test".into(),
            };
            Self { root, config }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn duplicated_fd_cannot_extend_closed_producer_ownership() {
        let fixture = Fixture::new();
        let first = DurableProducer::open(fixture.config.clone()).unwrap();
        let duplicate = first._ownership.file.try_clone().unwrap();
        assert!(DurableProducer::open(fixture.config.clone()).is_err());
        drop(first);
        // No sleeps: the duplicated kernel open-file description remains alive.
        let next = DurableProducer::open(fixture.config.clone()).unwrap();
        assert!(DurableProducer::open(fixture.config.clone()).is_err());
        drop(duplicate);
        assert!(DurableProducer::open(fixture.config.clone()).is_err());
        drop(next);
        DurableProducer::open(fixture.config.clone()).unwrap();
    }

    #[test]
    fn dropping_fork_copy_cannot_unlock_active_producer() {
        let fixture = Fixture::new();
        let first = DurableProducer::open(fixture.config.clone()).unwrap();
        let duplicate = ProducerOwnership {
            file: first._ownership.file.try_clone().unwrap(),
            owner_pid: std::process::id(),
        };
        let mut duplicate = ManuallyDrop::new(duplicate);
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // Only PID/FD syscalls after fork: never allocate or touch the
            // producer's other Rust state in this multithreaded test process.
            unsafe {
                ManuallyDrop::drop(&mut duplicate);
                libc::_exit(0);
            }
        }
        // The parent's duplicate must close without releasing the real owner.
        duplicate.owner_pid = 0;
        unsafe {
            ManuallyDrop::drop(&mut duplicate);
        }
        assert!(pid > 0, "fork failed");
        let mut status = 0;
        loop {
            let result = unsafe { libc::waitpid(pid, &mut status, 0) };
            if result == pid {
                break;
            }
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::Interrupted
            );
        }
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        assert!(DurableProducer::open(fixture.config.clone()).is_err());
        drop(first);
        DurableProducer::open(fixture.config.clone()).unwrap();
    }
}
