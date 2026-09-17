//! Desktop-owned meeting repository and per-session private ingestion endpoint.
//! Neither a model node nor the webview can open the SQLite writer directly.
use forum_contracts::*;
use forum_core::{CoreHandle, PageKey, SessionStatus, StoreError};
use forum_runtime::{DurableProducer, Endpoint, RpcError, RpcRequest, RuntimeConfig, UdsServer};
use moxin_dora_bridge::{data::SentenceUnit, CaptureContext, TranslationUpdate};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::result::Result;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingOptions {
    pub source_language: String,
    pub target_language: String,
    pub recording_enabled: bool,
    pub system_audio: bool,
    #[serde(default = "default_max_segment_ms")]
    pub max_segment_ms: u64,
}

fn default_max_segment_ms() -> u64 { 10_000 }

impl MeetingOptions {
    pub fn targets(&self) -> Vec<String> {
        match self.target_language.as_str() {
            "none" => vec![],
            "bilingual" => vec!["zh".into(), "en".into()],
            language => vec![language.into()],
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(1000..=30000).contains(&self.max_segment_ms) { return Err("音频段长度应为 1 到 30 秒".into()); }
        if !matches!(
            self.source_language.as_str(),
            "auto" | "zh" | "en" | "ja" | "fr"
        ) || !matches!(
            self.target_language.as_str(),
            "none" | "bilingual" | "zh" | "en" | "ja" | "fr"
        ) {
            return Err("不支持的语种设置".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSetup {
    pub session: SessionSpec,
    pub track: TrackSpec,
    pub options: MeetingOptions,
}

#[derive(Clone)]
pub struct MeetingRepository {
    pub core: CoreHandle,
    root: PathBuf,
    identity: [Uuid; 3],
}

pub fn core_error(error: StoreError) -> RpcError {
    RpcError::new(error.code(), error.retryable(), error)
}

fn private_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| e.to_string())?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("会议目录不是本机普通目录".into());
    }
    // This is a Forum-specific directory, not the shared Application Support root.
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

impl MeetingRepository {
    pub fn export_markdown(&self, session_id: Uuid) -> Result<String, String> {
        self.core
            .call(move |store| {
                let export = store.export_session(session_id)?;
                let mut output = format!(
                    "# {}\n\n会议：{}\n\n状态：{}；原文封存：{}；存在缺口：{}\n\n",
                    export.snapshot.session.title,
                    session_id,
                    export.status.state.as_str(),
                    export.status.transcript_sealed,
                    export.status.incomplete
                );
                output.push_str("## 原文\n\n");
                for item in &export.sources {
                    match &item.transcript {
                        Some(row) => {
                            let source = &row.payload;
                            output.push_str(&format!(
                                "### {} ms · {} · v{} · {:?}\n\n{}\n\n",
                                source.audio.start_ms,
                                source.segment_id,
                                source.revision.get(),
                                source.status,
                                if source.status == TranscriptStatus::Success {
                                    source.text.as_str()
                                } else {
                                    source.reason.as_deref().unwrap_or("识别未完成")
                                }
                            ));
                        }
                        None => output.push_str(&format!(
                            "待恢复音频段：{} · {} ms · 尚无原文结果\n\n",
                            item.segment_id, item.audio.start_ms
                        )),
                    }
                }
                output.push_str("## 译文\n\n");
                for translation in &export.translations {
                    output.push_str(&format!(
                        "### {} · {} · {}\n\n原文：{}\n\n{}\n\n",
                        translation.request.translation_id,
                        translation.request.target_language,
                        translation.state,
                        translation.request.input_text,
                        translation
                            .result
                            .as_ref()
                            .map(|r| r.text.as_str())
                            .unwrap_or("译文尚未完成，可恢复重试")
                    ));
                }
                for gap in &export.gaps {
                    output.push_str(&format!(
                        "音频缺口 {}–{} ms：{}\n\n",
                        gap.audio.start_ms, gap.audio.end_ms, gap.reason
                    ));
                }
                Ok(output)
            })
            .map_err(|e| e.to_string())
    }
    pub fn open(root: PathBuf) -> Result<Self, String> {
        private_dir(&root)?;
        private_dir(&root.join("store"))?;
        private_dir(&root.join("sessions"))?;
        let path = root.join("store/device.json");
        let identity: [Uuid; 3] = if path.exists() {
            if fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("会议身份文件不能是符号链接".into());
            }
            serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        } else {
            let identity = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
            write_new_json(&path, &identity)?;
            identity
        };
        if identity.iter().any(Uuid::is_nil) {
            return Err("会议身份文件无效".into());
        }
        let core =
            CoreHandle::open(root.join("store/forum.sqlite"), 128).map_err(|e| e.to_string())?;
        Ok(Self {
            core,
            root,
            identity,
        })
    }

    pub fn sessions(&self) -> Result<Value, String> {
        self.core
            .call(|store| Ok(serde_json::to_value(store.list_sessions(100)?)?))
            .map_err(|e| e.to_string())
    }

    pub fn page(
        &self,
        session_id: Uuid,
        cursor: Option<u64>,
        after: Option<PageKey>,
    ) -> Result<Value, String> {
        self.core
            .call(move |store| {
                let page = store.snapshot_page(session_id, cursor, after, 100)?;
                let ids: BTreeSet<_> = page.items.iter().map(|item| item.segment_id).collect();
                let translations = store.translation_records_for_segments_at(
                    session_id,
                    ids.into_iter().collect(),
                    page.cursor,
                )?;
                let mut value = serde_json::to_value(page)?;
                value["translations"] = serde_json::to_value(translations)?;
                Ok(value)
            })
            .map_err(|e| e.to_string())
    }

    pub fn setup(&self, session_id: Uuid) -> Result<SessionSetup, String> {
        let path = self.session_dir(session_id).join("session.json");
        let setup: SessionSetup =
            serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        setup.session.validate().map_err(|e| e.to_string())?;
        setup.track.validate().map_err(|e| e.to_string())?;
        setup.options.validate()?;
        if setup.session.session_id != session_id || setup.track.session_id != session_id {
            return Err("恢复资料不属于所选会议".into());
        }
        Ok(setup)
    }

    pub fn session_dir(&self, id: Uuid) -> PathBuf {
        self.root.join("sessions").join(id.to_string())
    }

    fn create_setup(&self, options: MeetingOptions) -> Result<SessionSetup, String> {
        options.validate()?;
        let id = Uuid::new_v4();
        let session = SessionSpec {
            session_id: id,
            event_id: self.identity[1],
            room_id: self.identity[2],
            owner_device_id: self.identity[0],
            title: format!("Forum {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")),
        };
        let track = TrackSpec {
            track_id: Uuid::new_v4(),
            session_id: id,
            kind: if options.system_audio {
                TrackKind::System
            } else {
                TrackKind::Mic
            },
            sample_rate: 16000,
        };
        let setup = SessionSetup {
            session,
            track,
            options,
        };
        private_dir(&self.session_dir(id))?;
        write_new_json(&self.session_dir(id).join("session.json"), &setup)?;
        let saved = setup.clone();
        self.core
            .call(move |store| {
                store.create_session(&saved.session)?;
                store.create_track(&saved.track)
            })
            .map_err(|e| e.to_string())?;
        Ok(setup)
    }
}

#[derive(Default)]
struct ModelReadiness {
    roles: BTreeSet<String>,
    prior_runs: Vec<forum_runtime::RunRecord>,
    asr_outbox_replayed: bool,
    replay_dispatch: Option<Vec<ReplayExpectedRevision>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayExpectedRevision {
    segment_id: Uuid,
    revision: Revision,
}

pub struct MeetingHost {
    pub repository: MeetingRepository,
    pub setup: SessionSetup,
    pub config: RuntimeConfig,
    pub replay_only: bool,
    readiness: Arc<Mutex<ModelReadiness>>,
    producer: DurableProducer,
    server: UdsServer,
    socket_directory: PathBuf,
    activated: bool,
}

impl MeetingHost {
    pub fn create(
        repository: MeetingRepository,
        options: MeetingOptions,
        recovery: Option<Uuid>,
    ) -> Result<Self, String> {
        let setup = match recovery {
            Some(id) => repository.setup(id)?,
            None => repository.create_setup(options)?,
        };
        let session_id = setup.session.session_id;
        let track_id = setup.track.track_id;
        let replay_only = recovery.is_some();
        let socket_directory =
            PathBuf::from("/tmp").join(format!("forum-{}", Uuid::new_v4().simple()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&socket_directory)
            .map_err(|e| e.to_string())?;
        let endpoint = Endpoint::new(socket_directory.join("core.sock"));
        let config = RuntimeConfig {
            endpoint: endpoint.clone(),
            session: setup.session.clone(),
            producer_dir: repository.session_dir(session_id).join("producers"),
            producer_name: "node".into(),
        };
        let readiness = Arc::new(Mutex::new(ModelReadiness::default()));
        let node_readiness = readiness.clone();
        let core = repository.core.clone();
        let scope = setup.session.clone();
        let server = UdsServer::bind(endpoint, move |request| {
            handle_rpc(
                &core,
                &scope,
                track_id,
                replay_only,
                &node_readiness,
                request,
            )
        })
        .map_err(|e| e.to_string())?;
        let producer =
            DurableProducer::open(config.for_producer("host").map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let mut host = Self {
            repository,
            setup,
            config,
            replay_only,
            readiness,
            producer,
            server,
            socket_directory,
            activated: false,
        };
        for (_, receipt) in host.producer.flush_pending() {
            receipt.map_err(|e| e.to_string())?;
        }
        if !replay_only {
            host.transition(SessionState::Preparing, "preparing owned ASR runtime")?;
        }
        Ok(host)
    }

    pub fn id(&self) -> Uuid {
        self.setup.session.session_id
    }
    pub fn status(&self) -> Result<SessionStatus, String> {
        let id = self.id();
        self.repository
            .core
            .call(move |store| store.session_status(id))
            .map_err(|e| e.to_string())
    }
    pub fn transition(&mut self, next: SessionState, reason: &str) -> Result<(), String> {
        let state = self.status()?.state;
        if state == next {
            return Ok(());
        }
        let pending = self
            .producer
            .append(
                EventType::SessionChanged,
                &SessionTransition {
                    expected_state: state,
                    next_state: next,
                    reason: reason.into(),
                },
            )
            .map_err(|e| e.to_string())?;
        self.producer
            .flush_one(pending.message_id)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn capture_context(&self) -> CaptureContext {
        CaptureContext {
            max_segment_ms: self.setup.options.max_segment_ms,            runtime: self.config.clone(),
            track: self.setup.track.clone(),
            recording_dir: self.repository.session_dir(self.id()).join("audio"),
            recording_enabled: self.setup.options.recording_enabled,
            replay_only: self.replay_only,
            configured_source_language: self.setup.options.source_language.clone(),
            target_languages: self.setup.options.targets(),
            direction_epoch: 1,
        }
    }

    pub fn activate_if_ready(&mut self) -> Result<bool, String> {
        if self.activated {
            return Ok(true);
        }
        if !self.readiness.lock().roles.contains("asr") {
            return Ok(false);
        }
        if !self.replay_only {
            self.transition(SessionState::Ready, "ASR model and private transport ready")?;
            self.transition(
                SessionState::Recording,
                "capture authorized after readiness barrier",
            )?;
        }
        self.activated = true;
        Ok(true)
    }

    pub fn mark_stopping(&mut self) -> Result<(), String> {
        match self.status()?.state {
            SessionState::Recording => {
                self.transition(SessionState::Stopping, "operator requested stop")
            }
            SessionState::Created | SessionState::Preparing | SessionState::Ready => {
                self.interrupt("stopped during preparation")
            }
            _ => Ok(()),
        }
    }
    pub fn interrupt(&mut self, reason: &str) -> Result<(), String> {
        if matches!(
            self.status()?.state,
            SessionState::Completed | SessionState::Interrupted
        ) {
            return Ok(());
        }
        self.transition(SessionState::Interrupted, reason)
    }

    pub fn try_seal(&mut self) -> Result<bool, String> {
        let status = self.status()?;
        if !status.capture_stopped {
            return Ok(false);
        }
        let id = self.id();
        if self.replay_only {
            let Some(expected) = self.readiness.lock().replay_dispatch.clone() else {
                return Ok(false);
            };
            let replay_committed = self
                .repository
                .core
                .call(move |store| {
                    for entry in expected {
                        match store.transcript_revision(id, entry.segment_id, entry.revision) {
                            Ok(_) => {}
                            Err(StoreError::NotFound(_) | StoreError::DependencyNotReady) => {
                                return Ok(false)
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Ok(true)
                })
                .map_err(|e| e.to_string())?;
            if !replay_committed {
                return Ok(false);
            }
        }
        let seal = self
            .repository
            .core
            .call(move |store| store.capture_seal(id))
            .map_err(|e| e.to_string())?;
        let Some(seal) = seal else { return Ok(false) };
        let pending_runs = self
            .repository
            .core
            .call(move |store| store.unsealed_producer_runs(id))
            .map_err(|e| e.to_string())?;
        let prior_runs = self.readiness.lock().prior_runs.clone();
        if self.readiness.lock().asr_outbox_replayed {
            for prior in prior_runs {
                if !pending_runs.contains(&prior.run_id) {
                    continue;
                }
                if prior.owner_pid == 0 || prior.owner_pid > i32::MAX as u32 {
                    continue;
                }
                // A PID from the producer's owned journal is only queried. An
                // existing/reused PID is never killed or presumed contained.
                let gone = unsafe { libc::kill(prior.owner_pid as i32, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if !gone {
                    continue;
                }
                let journal = self
                    .config
                    .producer_dir
                    .join("asr")
                    .join(format!("{}.run", prior.run_id));
                let recorded: forum_runtime::RunRecord =
                    serde_json::from_slice(&fs::read(journal).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                if recorded.run_id != prior.run_id
                    || recorded.owner_pid != prior.owner_pid
                    || recorded.session_id != id
                    || recorded.producer_name != "asr"
                {
                    return Err("旧 ASR 进程恢复证据不匹配".into());
                }
                let evidence=ProducerRecoveryEvidence {capture_manifest_sha256:seal.manifest_sha256.clone(),
                    owned_process_exited:true,outbox_replayed:true,reason:"ASR run journal verified; OS reports prior owner PID absent; durable outbox replay acknowledged".into()};
                self.repository
                    .core
                    .call(move |store| {
                        store.reconcile_abandoned_producer(id, prior.run_id, evidence)
                    })
                    .map_err(|e| e.to_string())?;
            }
        }
        if !self
            .repository
            .core
            .call(move |store| store.unsealed_producer_runs(id))
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Ok(false);
        }
        if status.transcript_sealed {
            return Ok(true);
        }
        // Once written, a timed-out request is retried with the SAME identity.
        let existing = self
            .producer
            .pending()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.event["type"] == json!("session.transcript_sealed"));
        let pending = match existing {
            Some(p) => p,
            None => self
                .producer
                .append(
                    EventType::TranscriptSealed,
                    &TranscriptSeal {
                        capture_manifest_sha256: seal.manifest_sha256,
                    },
                )
                .map_err(|e| e.to_string())?,
        };
        match self.producer.flush_one(pending.message_id) {
            Ok(_) => Ok(true),
            Err(e) if e.retryable => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn translation_pending(&self) -> Result<u64, String> {
        let id = self.id();
        self.repository
            .core
            .call(move |store| {
                Ok(store
                    .coverage(id)?
                    .iter()
                    .filter(|c| matches!(c.state.as_str(), "pending" | "requested" | "failed"))
                    .count() as u64)
            })
            .map_err(|e| e.to_string())
    }

    pub fn stop_server(&mut self) -> Result<(), String> {
        self.server.stop().map_err(|e| e.to_string())
    }
}

impl Drop for MeetingHost {
    fn drop(&mut self) {
        if self.server.stop().is_ok() {
            let _ = fs::remove_dir(&self.socket_directory);
        }
    }
}

fn invalid(message: &str) -> RpcError {
    RpcError::new("INVALID_REQUEST", false, message)
}
fn handle_rpc(
    core: &CoreHandle,
    scope: &SessionSpec,
    track_id: Uuid,
    replay_only: bool,
    readiness: &Arc<Mutex<ModelReadiness>>,
    request: RpcRequest,
) -> Result<Value, RpcError> {
    let params = request.params;
    let id = scope.session_id;
    if request.method == "ingest" {
        let event = params
            .get("event")
            .cloned()
            .ok_or_else(|| invalid("event required"))?;
        if event["session_id"] != json!(id)
            || event["event_id"] != json!(scope.event_id)
            || event["room_id"] != json!(scope.room_id)
        {
            return Err(RpcError::new(
                "SCOPE_MISMATCH",
                false,
                "ingestion endpoint belongs to another meeting",
            ));
        }
        return serde_json::to_value(core.ingest_json(event).map_err(core_error)?)
            .map_err(|e| invalid(&e.to_string()));
    }
    if params["session_id"] != json!(id) {
        return Err(RpcError::new("SCOPE_MISMATCH", false, "session required"));
    }
    match request.method.as_str() {
        "ready" => {
            let role = params["role"]
                .as_str()
                .ok_or_else(|| invalid("role required"))?;
            if !matches!(role, "asr" | "translator") {
                return Err(invalid("unknown model role"));
            }
            // Readiness is published only after validating the whole message.
            let prior_runs: Vec<forum_runtime::RunRecord> = if role == "asr" {
                let records = serde_json::from_value(
                    params
                        .get("prior_run_records")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                )
                .map_err(|e| invalid(&e.to_string()))?;
                let records: Vec<forum_runtime::RunRecord> = records;
                if records.len() > 1000
                    || records.iter().any(|r| {
                        r.run_id.is_nil() || r.session_id != id || r.producer_name != "asr"
                    })
                {
                    return Err(invalid("invalid prior ASR runs"));
                }
                records
            } else {
                vec![]
            };
            let mut ready = readiness.lock();
            if role == "asr" {
                ready.prior_runs = prior_runs;
                ready.asr_outbox_replayed = params["outbox_replayed"] == true;
            }
            ready.roles.insert(role.into());
            Ok(json!({"ready":true}))
        }
        "status" => {
            let (status, registered, terminal, asr_terminal, seal) = core
                .call(move |store| {
                    Ok((
                        store.session_status(id)?,
                        store.registered_segment_ids(id, track_id)?,
                        if replay_only {
                            store.terminal_segment_ids_for_replay(id)?
                        } else {
                            store.terminal_segment_ids(id)?
                        },
                        store.terminal_segment_ids(id)?,
                        store.capture_seal(id)?,
                    ))
                })
                .map_err(core_error)?;
            let roles = if replay_only
                || matches!(
                    status.state,
                    SessionState::Recording
                        | SessionState::Stopping
                        | SessionState::Draining
                        | SessionState::Completed
                ) {
                readiness.lock().roles.iter().cloned().collect::<Vec<_>>()
            } else {
                vec![]
            };
            let replay_dispatch = readiness.lock().replay_dispatch.clone();
            Ok(
                json!({"capture_dispatch_complete":!replay_only||replay_dispatch.is_some(),"replay_expected_revisions":replay_dispatch.unwrap_or_default(),
                "asr_terminal_segment_ids":asr_terminal,"ready_roles":roles,"capture_stopped":status.capture_stopped,"transcript_sealed":status.transcript_sealed,
                "capture_manifest_sha256":seal.map(|s|s.manifest_sha256),
                "registered_segment_ids":registered,"terminal_segment_ids":terminal,"state":status.state}),
            )
        }
        "recovery_revisions" => {
            let ids: Vec<Uuid> = serde_json::from_value(
                params
                    .get("segment_ids")
                    .cloned()
                    .ok_or_else(|| invalid("segment_ids required"))?,
            )
            .map_err(|e| invalid(&e.to_string()))?;
            if ids.is_empty() || ids.len() > 256 {
                return Err(invalid("request 1 to 256 segment IDs"));
            }
            core.call(move |store| Ok(serde_json::to_value(store.recovery_revisions(id, ids)?)?))
                .map_err(core_error)
        }
        "capture_dispatch_complete" => {
            if !replay_only {
                return Err(invalid("dispatch receipt is only valid during replay"));
            }
            let expected: Vec<ReplayExpectedRevision> = serde_json::from_value(
                params
                    .get("segments")
                    .cloned()
                    .ok_or_else(|| invalid("segments required"))?,
            )
            .map_err(|e| invalid(&e.to_string()))?;
            let unique: BTreeSet<_> = expected.iter().map(|entry| entry.segment_id).collect();
            if expected.len() > 10000
                || unique.len() != expected.len()
                || unique.iter().any(Uuid::is_nil)
            {
                return Err(invalid("invalid replay segment set"));
            }
            let (registered, sealed) = core
                .call(move |store| {
                    Ok((
                        store.registered_segment_ids(id, track_id)?,
                        store.session_status(id)?.capture_stopped,
                    ))
                })
                .map_err(core_error)?;
            if !sealed || unique.iter().any(|id| !registered.contains(id)) {
                return Err(invalid("replay dispatch must follow durable capture seal"));
            }
            let mut state = readiness.lock();
            if state
                .replay_dispatch
                .as_ref()
                .is_some_and(|previous| previous != &expected)
            {
                return Err(RpcError::new(
                    "CONFLICT",
                    false,
                    "replay dispatch receipt changed",
                ));
            }
            state.replay_dispatch = Some(expected);
            Ok(json!({"accepted":true}))
        }
        "poll_translation" | "translation_requests" => {
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(64)
                .clamp(1, 100) as u32;
            let requested = request.method == "translation_requests";
            let after: Option<forum_core::TranslationPageKey> =
                serde_json::from_value(params.get("after").cloned().unwrap_or(Value::Null))
                    .map_err(|e| invalid(&e.to_string()))?;
            core.call(move |store| {
                if requested {
                    Ok(serde_json::to_value(
                        store.unresolved_translation_page(id, after, limit)?,
                    )?)
                } else {
                    Ok(serde_json::to_value(
                        store.pending_translation_work(id, limit)?,
                    )?)
                }
            })
            .map_err(core_error)
        }
        _ => Err(RpcError::new(
            "METHOD_NOT_FOUND",
            false,
            "unknown local meeting method",
        )),
    }
}

/// A compact overlay projection. SQL data remains complete; captions show the
/// latest 200 source segments, including untranslated and failed segments.
pub fn project_transcript(core: &CoreHandle, id: Uuid) -> Result<TranslationUpdate, String> {
    core.call(move |store| {
        use moxin_dora_bridge::data::{
            DurableTranslationBatch, DurableTranslationDelivery, DurableTranslationIdentity,
        };
        let snapshot = store.session_snapshot(id)?;
        // Speech deliveries must not disappear when their source scrolls out
        // of the 200-row caption window. Bind every page to the source cursor.
        let mut translations = Vec::new();
        let mut after = None;
        loop {
            let page = store.translation_page(id, Some(snapshot.cursor), after, 1000)?;
            translations.extend(page.items);
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
        let source_positions: std::collections::HashMap<_, _> = snapshot
            .transcript
            .iter()
            .enumerate()
            .map(|(index, row)| ((row.payload.segment_id, row.payload.revision.get()), index))
            .collect();
        let mut ordered = Vec::new();
        for translation in &translations {
            if !matches!(translation.state.as_str(), "final" | "passthrough") {
                continue;
            }
            let Some(result) = &translation.result else {
                continue;
            };
            let sources = &translation.request.source_spans;
            if sources.is_empty()
                || !sources.iter().all(|source| {
                    source_positions
                        .contains_key(&(source.segment_id, source.segment_revision.get()))
                })
            {
                continue;
            }
            let first = &sources[0];
            let delivery = DurableTranslationDelivery {
                identity: DurableTranslationIdentity {
                    translation_id: result.translation_id,
                    revision: result.revision.get(),
                    attempt: result.attempt,
                },
                source_segment_ids: sources.iter().map(|source| source.segment_id).collect(),
                target_language: translation.request.target_language.clone(),
                text: result.text.clone(),
            };
            ordered.push((
                source_positions[&(first.segment_id, first.segment_revision.get())],
                first.start_utf8,
                delivery,
            ));
        }
        ordered.sort_by(|a, b| {
            (a.0, a.1, &a.2.target_language, a.2.identity.translation_id).cmp(&(
                b.0,
                b.1,
                &b.2.target_language,
                b.2.identity.translation_id,
            ))
        });
        let deliveries = ordered
            .into_iter()
            .map(|(_, _, delivery)| delivery)
            .collect::<Vec<_>>();
        let tail = store.snapshot_tail(id, 200)?;
        let mut history = Vec::new();
        for item in &tail.items {
            let Some(row) = &item.transcript else {
                history.push(SentenceUnit {
                    source_text: "[音频已保存，原文待处理]".into(),
                    translation: String::new(),
                    source_language: "auto".into(),
                    target_language: tail.status.target_languages.join(","),
                    direction_epoch: tail.status.direction_epoch,
                });
                continue;
            };
            let source = &row.payload;
            let mut outputs = Vec::new();
            for translation in &translations {
                if translation.state == "stale" {
                    continue;
                }
                if translation.request.source_spans.first().is_some_and(|s| {
                    s.segment_id == source.segment_id && s.segment_revision == source.revision
                }) {
                    if let Some(result) = &translation.result {
                        outputs.push(format!(
                            "[{}] {}",
                            translation.request.target_language, result.text
                        ));
                    }
                }
            }
            history.push(SentenceUnit {
                source_text: match source.status {
                    TranscriptStatus::Failed => "[该段识别失败，可从录音恢复]".into(),
                    TranscriptStatus::Empty => "[该段没有识别到文字]".into(),
                    TranscriptStatus::Success => source.text.clone(),
                },
                translation: outputs.join("\n"),
                source_language: source
                    .detected_language
                    .clone()
                    .unwrap_or_else(|| source.configured_source_language.clone()),
                target_language: source.target_languages.join(","),
                direction_epoch: source.direction_epoch,
            });
        }
        let completed_count = deliveries.len() as u64;
        Ok(TranslationUpdate {
            history,
            pending_source_text: String::new(),
            completed_count,
            durable_deliveries: Some(DurableTranslationBatch {
                session_id: id,
                items: deliveries,
            }),
        })
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "meeting_tests.rs"]
mod tests;
