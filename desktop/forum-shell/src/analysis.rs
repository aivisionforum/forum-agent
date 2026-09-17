//! Desktop analysis coordinator. Models consume immutable snapshots; core alone commits.
use forum_contracts::*;
use forum_core::CoreHandle;
use forum_runtime::{
    resource_budget::ResourceBudget,
    worker::{attempt_directory, read_result, write_snapshot, WorkerProcess},
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::result::Result;
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn kind_name(kind: AnalysisKind) -> String {
    serde_json::to_value(kind)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRequest {
    pub request_id: Uuid,
    pub session_ids: Vec<Uuid>,
    pub kind: AnalysisKind,
    pub automatic: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobAction {
    pub job_id: Uuid,
    pub expected_attempt: u32,
}

#[derive(Clone)]
struct ModelSetup {
    python: PathBuf,
    source: Option<PathBuf>,
    prompts: PathBuf,
    model: PathBuf,
    manifest: String,
    profile: String,
}
impl ModelSetup {
    fn resolve(resources: Option<&Path>, batch: bool) -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("无法定位本机模型目录")?;
        let bundle = resources.map(|p| p.join("meeting-worker"));
        let python = std::env::var_os("FORUM_MEETING_PYTHON")
            .map(PathBuf::from)
            .or_else(|| bundle.as_ref().map(|p| p.join("python/bin/python3.12")))
            .ok_or("会议分析运行时未准备，请使用含独立 Python 的开发包")?;
        if !python.is_absolute() || !python.is_file() {
            return Err("会议分析 Python 运行时不完整".into());
        }
        let source = std::env::var_os("FORUM_MEETING_WORKER_SOURCE").map(PathBuf::from);
        let package = source
            .as_ref()
            .map(|p| p.join("forum_meeting_worker"))
            .unwrap_or_else(|| {
                python
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("lib/python3.12/site-packages/forum_meeting_worker")
            });
        if source
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || !p.is_dir())
            || !package.join("main.py").is_file()
        {
            return Err("会议分析程序不完整".into());
        }
        let batch_path = if batch {
            std::env::var_os("FORUM_ANALYSIS_BATCH_MODEL_PATH").map(PathBuf::from)
        } else {
            None
        };
        let is_large = batch_path.is_some();
        let owned = crate::preferences::preferences_dir().join("models/qwen3-8b");
        let cached=home.join(".cache/huggingface/hub/models--mlx-community--Qwen3-8B-4bit/snapshots/545dc4251c05440727734bcd94334791f6ab0192");
        let model = batch_path
            .or_else(|| std::env::var_os("FORUM_ANALYSIS_MODEL_PATH").map(PathBuf::from))
            .unwrap_or_else(|| {
                if owned.join("config.json").is_file() {
                    owned
                } else {
                    cached
                }
            });
        if !model.is_absolute() || !model.is_dir() {
            return Err("尚未准备本机会议分析模型；不会自动下载".into());
        }
        let manifest = forum_runtime::model_fingerprint(&model)
            .map_err(|e| format!("会议模型校验失败：{e}"))?;
        Ok(Self {
            python,
            source,
            prompts: package.join("prompts/v1"),
            model,
            manifest,
            profile: if is_large {
                "meeting-32b-v1"
            } else {
                "meeting-8b-v1"
            }
            .into(),
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.python);
        command.arg("-I");
        if let Some(source) = &self.source {
            command.args(["-c","import sys,runpy;sys.path.insert(0,sys.argv.pop(1));runpy.run_module('forum_meeting_worker',run_name='__main__')"]).arg(source);
        } else {
            command.args(["-m", "forum_meeting_worker"]);
        }
        command
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("HF_HUB_DISABLE_IMPLICIT_TOKEN", "1");
        command
    }
    fn config(&self, kind: AnalysisKind) -> Result<AnalysisConfig, String> {
        let name = kind_name(kind);
        let prompt = fs::read(self.prompts.join(format!("{name}.txt")))
            .map_err(|_| "会议分析提示词尚未完整打包")?;
        let profile: Value = serde_json::from_str(include_str!(
            "../../../profiles/ai-vision-forum/profile.json"
        ))
        .map_err(|e| e.to_string())?;
        let mut config = AnalysisConfig {
            model_profile: self.profile.clone(),
            model_manifest_id: self.manifest.clone(),
            prompt_version: format!("{name}-v1"),
            prompt_sha256: sha(&prompt),
            profile_id: "ai-vision-forum".into(),
            profile_version: "v1".into(),
            profile_sha256: sha(include_bytes!(
                "../../../profiles/ai-vision-forum/profile.json"
            )),
            effective_config_hash: String::new(),
            projection_policy_hash: sha(canonical_json(&profile["publication"]).as_bytes()),
            generation: json!({"temperature":0.0,"max_output_tokens":1024,"safety_tokens":128,"max_retries":1,"context_limit":8192}),
        };
        config.effective_config_hash = config.computed_hash();
        Ok(config)
    }
    fn grant(&self) -> Value {
        json!({"profile":self.profile,"model_path":self.model,"model_manifest_id":self.manifest,"context_limit":8192,"max_output_tokens":1024})
    }
}

struct Inner {
    core: CoreHandle,
    root: PathBuf,
    resources: Option<PathBuf>,
    budget: ResourceBudget,
    models: Mutex<HashMap<bool, ModelSetup>>,
    stopped: AtomicBool,
    contained: AtomicBool,
    lifecycle_persisted: AtomicBool,
    notice: Mutex<Option<String>>,
    automatic: Mutex<Automatic>,
}
#[derive(Default)]
struct Automatic {
    live: Option<(Uuid, Instant)>,
    precompute: VecDeque<Uuid>,
}
impl Automatic {
    fn next_job(&mut self, now: Instant) -> Option<(Uuid, AnalysisKind)> {
        while let Some(id) = self.precompute.pop_front() {
            if self.live.as_ref().is_some_and(|(live, _)| *live == id) {
                return Some((id, AnalysisKind::Minutes));
            }
        }
        if let Some((id, last)) = self.live.as_mut() {
            if now.duration_since(*last) >= Duration::from_secs(180) {
                *last = now;
                let id = *id;
                self.precompute.push_back(id);
                return Some((id, AnalysisKind::Insight));
            }
        }
        None
    }
}

pub struct AnalysisManager {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}
#[derive(Clone)]
pub struct AnalysisClient(Arc<Inner>);
impl AnalysisManager {
    pub fn new(
        core: CoreHandle,
        root: PathBuf,
        resources: Option<PathBuf>,
        budget: ResourceBudget,
    ) -> Self {
        let inner = Arc::new(Inner {
            core,
            root,
            resources,
            budget,
            models: Mutex::new(HashMap::new()),
            stopped: AtomicBool::new(false),
            contained: AtomicBool::new(true),
            lifecycle_persisted: AtomicBool::new(true),
            notice: Mutex::new(None),
            automatic: Mutex::new(Automatic::default()),
        });
        let worker_inner = inner.clone();
        let worker = thread::spawn(move || run_loop(worker_inner));
        Self {
            inner,
            worker: Some(worker),
        }
    }
    pub fn client(&self) -> AnalysisClient {
        AnalysisClient(self.inner.clone())
    }
    pub fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.budget.shutdown();
    }
    pub fn shutdown_complete(&self) -> bool {
        self.inner.contained.load(Ordering::Acquire)
            && self.inner.lifecycle_persisted.load(Ordering::Acquire)
            && self
                .worker
                .as_ref()
                .is_none_or(|worker| worker.is_finished())
    }
}
impl Drop for AnalysisManager {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl AnalysisClient {
    pub fn session_started(&self, id: Uuid) {
        self.0.automatic.lock().live = Some((id, Instant::now()));
    }
    pub fn session_stopped(&self, id: Uuid) {
        {
            let mut auto = self.0.automatic.lock();
            if auto
                .live
                .as_ref()
                .is_some_and(|(session, _)| *session == id)
            {
                auto.live = None;
            }
            auto.precompute.retain(|session| *session != id);
        }
        // Persist before the runtime declares shutdown complete, even after the
        // analysis thread stopped. A later launch can create the actual job.
        match self.0.core.call(move |s| s.record_analysis_stop_intent(id)) {
            Ok(()) => {
                self.0.lifecycle_persisted.store(true, Ordering::Release);
            }
            Err(error) => {
                self.0.lifecycle_persisted.store(false, Ordering::Release);
                *self.0.notice.lock() = Some(format!("会后纪要请求未能保存：{error}"));
            }
        }
    }
    pub fn notice(&self) -> Option<String> {
        self.0.notice.lock().clone()
    }
    fn setup(&self, batch: bool) -> Result<ModelSetup, String> {
        let mut models = self.0.models.lock();
        if let Some(model) = models.get(&batch) {
            return Ok(model.clone());
        }
        let model = ModelSetup::resolve(self.0.resources.as_deref(), batch)?;
        models.insert(batch, model.clone());
        Ok(model)
    }
    pub fn create(&self, request: JobRequest) -> Result<AnalysisJob, String> {
        let batch = !matches!(
            request.kind,
            AnalysisKind::Insight
                | AnalysisKind::SuggestedQuestions
                | AnalysisKind::RedactionReview
        );
        let precompute = request.automatic
            && request.kind == AnalysisKind::Minutes
            && self
                .0
                .automatic
                .lock()
                .live
                .as_ref()
                .is_some_and(|(id, _)| request.session_ids.as_slice() == [*id]);
        let config = self.setup(batch && !precompute)?.config(request.kind)?;
        let create = CreateAnalysisJob {
            request_id: request.request_id,
            session_ids: request.session_ids,
            kind: request.kind,
            config,
            budget_ms: if batch { 180_000 } else { 90_000 },
            max_attempts: 3,
            automatic: request.automatic,
        };
        self.0
            .core
            .call(move |store| store.create_analysis_job(&create))
            .map_err(|e| e.to_string())
    }
    pub fn cancel(&self, action: JobAction) -> Result<AnalysisJob, String> {
        self.0
            .core
            .call(move |s| s.cancel_analysis_job(action.job_id, action.expected_attempt))
            .map_err(|e| e.to_string())
    }
    pub fn retry(&self, action: JobAction) -> Result<AnalysisJob, String> {
        self.0
            .core
            .call(move |s| s.retry_analysis_job(action.job_id, action.expected_attempt))
            .map_err(|e| e.to_string())
    }
}

fn run_loop(inner: Arc<Inner>) {
    let client = AnalysisClient(inner.clone());
    let _ = inner
        .core
        .call(|s| s.interrupt_running_analysis_jobs("应用重新启动，分析进程需恢复".into()));
    let mut next_intent = Instant::now();
    while !inner.stopped.load(Ordering::Acquire) {
        if Instant::now() >= next_intent {
            next_intent = Instant::now() + Duration::from_secs(1);
            let intents = inner
                .core
                .call(|s| s.pending_analysis_stop_intents(8))
                .unwrap_or_default();
            for (id, marker) in intents {
                if inner.stopped.load(Ordering::Acquire) {
                    break;
                }
                match client.create(JobRequest {
                    request_id: Uuid::new_v4(),
                    session_ids: vec![id],
                    kind: AnalysisKind::Minutes,
                    automatic: true,
                }) {
                    Ok(_) => {
                        if let Err(error) = inner
                            .core
                            .call(move |s| s.ack_analysis_stop_intent(id, marker))
                        {
                            *inner.notice.lock() =
                                Some(format!("纪要已入队，但自动请求确认失败：{error}"));
                        } else {
                            *inner.notice.lock() = None;
                        }
                    }
                    Err(error) => {
                        *inner.notice.lock() =
                            Some(format!("自动纪要请求已保存，等待运行条件：{error}"));
                        next_intent = Instant::now() + Duration::from_secs(30);
                        break;
                    }
                }
            }
        }
        let automatic = inner.automatic.lock().next_job(Instant::now());
        if let Some((id, kind)) = automatic {
            if let Err(error) = client.create(JobRequest {
                request_id: Uuid::new_v4(),
                session_ids: vec![id],
                kind,
                automatic: true,
            }) {
                *inner.notice.lock() = Some(format!("自动会议分析尚未入队：{error}"));
                log::warn!("自动会议分析未入队：{error}");
            } else {
                *inner.notice.lock() = None;
            }
        }
        let jobs = inner
            .core
            .call(|s| s.next_analysis_jobs(100))
            .unwrap_or_default();
        let mut admitted = false;
        for job in jobs {
            if inner.stopped.load(Ordering::Acquire) {
                break;
            }
            let id = job.job_id;
            let attempt = job.attempt;
            if now_ms() >= job.deadline_at_ms {
                let _ = inner.core.call(move |s| {
                    s.fail_analysis_job(id, attempt, "DEADLINE_EXCEEDED: 排队已超过总预算".into())
                });
                continue;
            }
            let live_eligible = job.config.model_profile == "meeting-8b-v1"
                && (matches!(
                    job.kind,
                    AnalysisKind::Insight | AnalysisKind::SuggestedQuestions
                ) || job.kind == AnalysisKind::Minutes && job.automatic);
            let permit = match inner.budget.admit(live_eligible) {
                Ok(p) => p,
                Err(reason) => {
                    if job.state != AnalysisJobState::Waiting
                        || job.progress.wait_reason.as_ref() != Some(&reason)
                    {
                        let _ = inner
                            .core
                            .call(move |s| s.wait_analysis_job(id, attempt, reason));
                    }
                    continue;
                }
            };
            let claimed = match inner.core.call(move |s| s.claim_analysis_job(id, attempt)) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if claimed.state != AnalysisJobState::Running {
                continue;
            }
            let batch = job.config.model_profile == "meeting-32b-v1";
            let outcome = client
                .setup(batch)
                .and_then(|setup| execute(&inner, &claimed, &setup));
            match outcome {
                Ok(Execution::Completed(result)) => {
                    if let Err(error) = inner.core.call(move |s| s.finish_analysis_job(&result)) {
                        let message = format!("结果验证失败：{error}");
                        let _ = inner
                            .core
                            .call(move |s| s.fail_analysis_job(id, attempt, message));
                    }
                }
                Ok(Execution::Cancelled) => {
                    let _ = inner
                        .core
                        .call(move |s| s.complete_analysis_cancel(id, attempt));
                }
                Ok(Execution::Interrupted(reason)) => {
                    let _ = inner
                        .core
                        .call(move |s| s.interrupt_analysis_job(id, attempt, reason));
                }
                Ok(Execution::Uncontained(reason)) => {
                    let _ = inner
                        .core
                        .call(move |s| s.interrupt_analysis_job(id, attempt, reason));
                    // Never grant new models while owned process exit is uncertain.
                    inner.contained.store(false, Ordering::Release);
                    *inner.notice.lock() = Some("后台进程退出尚未确认，已暂停新的模型任务".into());
                    std::mem::forget(permit);
                    inner.stopped.store(true, Ordering::Release);
                    break;
                }
                Err(error) => {
                    let _ = inner
                        .core
                        .call(move |s| s.fail_analysis_job(id, attempt, error));
                }
            }
            drop(permit);
            admitted = true;
            break;
        }
        if !admitted {
            thread::sleep(Duration::from_millis(200));
        }
    }
}
enum Execution {
    Completed(AnalysisResult),
    Cancelled,
    Interrupted(String),
    Uncontained(String),
}

fn execute(inner: &Inner, job: &AnalysisJob, setup: &ModelSetup) -> Result<Execution, String> {
    if setup.manifest != job.config.model_manifest_id || setup.profile != job.config.model_profile {
        return Err("MODEL_UNAVAILABLE: 已排队任务的模型版本与本机不符".into());
    }
    let id = job.job_id;
    let attempt = job.attempt;
    let snapshot = inner
        .core
        .call(move |s| s.analysis_snapshot(id))
        .map_err(|e| e.to_string())?;
    let root = inner.root.join("jobs");
    let directory = attempt_directory(&root, id, attempt).map_err(|e| e.to_string())?;
    let hash =
        write_snapshot(&directory, snapshot.compact_json.as_bytes()).map_err(|e| e.to_string())?;
    if hash != job.snapshot_sha256 {
        return Err("INVALID_SNAPSHOT: 快照指纹不一致".into());
    }
    let checkpoints = inner
        .core
        .call(move |s| s.reusable_analysis_checkpoints(id))
        .map_err(|e| e.to_string())?;
    let checkpoint_bytes = serde_json::to_vec(&checkpoints).map_err(|e| e.to_string())?;
    // New attempt files are host owned; bytes come only from acknowledged core checkpoints.
    write_private(&directory.join("checkpoints.json"), &checkpoint_bytes)?;
    let mut worker = WorkerProcess::spawn(
        setup.command(),
        &directory.join(format!("worker-{}.log", Uuid::new_v4())),
    )
    .map_err(|e| e.to_string())?;
    let result = (|| -> Result<Execution, String> {
        let initialize = json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocol_version":1,"instance_id":Uuid::new_v4(),"job_root":root,"profile_id":job.config.profile_id,"profile_version":job.config.profile_version,"model_grants":[setup.grant()]}});
        worker
            .send(&initialize, Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        let handshake_deadline = Instant::now() + Duration::from_secs(5);
        let ready = loop {
            if inner.stopped.load(Ordering::Acquire) || inner.budget.yield_reason().is_some() {
                return Ok(Execution::Interrupted(
                    inner
                        .budget
                        .yield_reason()
                        .unwrap_or_else(|| "应用正在退出".into()),
                ));
            }
            if Instant::now() >= handshake_deadline {
                return Err("WORKER_EXITED: 分析握手超时".into());
            }
            if let Some(message) = worker
                .poll(Duration::from_millis(100))
                .map_err(|e| e.to_string())?
            {
                break message;
            }
        };
        if ready["id"] != "init"
            || ready.get("error").is_some()
            || ready["result"]["protocol_version"] != 1
        {
            return Err("UNSUPPORTED_PROTOCOL: 分析进程握手失败".into());
        }
        let remaining = job.deadline_at_ms.saturating_sub(now_ms());
        if remaining == 0 {
            return Err("DEADLINE_EXCEEDED".into());
        }
        let deadline = Instant::now() + Duration::from_millis(remaining);
        let run_id = format!("run-{id}-{attempt}");
        worker.send(&json!({"jsonrpc":"2.0","id":run_id,"method":"jobs.run","params":{"job_id":id,"attempt":attempt,"kind":job.kind,"session_ids":job.session_ids,"snapshot":{"id":job.snapshot_id,"relative_path":"input.json","sha256":job.snapshot_sha256,"input_cursor":snapshot.snapshot.input_cursor},"model_profile":job.config.model_profile,"prompt_version":job.config.prompt_version,"remaining_budget_ms":remaining,"config":job.config,"confirmed_checkpoints":{"relative_path":"checkpoints.json","sha256":sha(&checkpoint_bytes)}}}),Duration::from_secs(1)).map_err(|e|e.to_string())?;
        let mut last_state = Instant::now() - Duration::from_secs(1);
        loop {
            if Instant::now() >= deadline {
                return Err("DEADLINE_EXCEEDED: 分析任务超过含排队的总预算".into());
            }
            if inner.stopped.load(Ordering::Acquire) || inner.budget.yield_reason().is_some() {
                let reason = inner
                    .budget
                    .yield_reason()
                    .unwrap_or_else(|| "应用正在退出".into());
                let _=worker.send(&json!({"jsonrpc":"2.0","id":"cancel","method":"jobs.cancel","params":{"job_id":id,"attempt":attempt}}),Duration::from_millis(250));
                return Ok(Execution::Interrupted(reason));
            }
            if last_state.elapsed() >= Duration::from_millis(200) {
                last_state = Instant::now();
                let current = inner
                    .core
                    .call(move |s| s.analysis_job(id))
                    .map_err(|e| e.to_string())?;
                if current.attempt != attempt || current.state == AnalysisJobState::CancelRequested
                {
                    let _=worker.send(&json!({"jsonrpc":"2.0","id":"cancel","method":"jobs.cancel","params":{"job_id":id,"attempt":attempt}}),Duration::from_millis(250));
                    return Ok(Execution::Cancelled);
                }
            }
            let Some(message) = worker
                .poll(Duration::from_millis(100))
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            if message["method"] == "jobs.progress" {
                let p = &message["params"];
                if p["job_id"] != id.to_string() || p["attempt"] != attempt {
                    return Err("INVALID_MODEL_OUTPUT: 进度任务身份不一致".into());
                }
                let progress = AnalysisProgress {
                    phase: p["phase"]
                        .as_str()
                        .unwrap_or("generating")
                        .chars()
                        .take(128)
                        .collect(),
                    completed_units: p["completed_units"]
                        .as_u64()
                        .unwrap_or(0)
                        .min(u32::MAX as u64) as u32,
                    total_units: p["total_units"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
                    wait_reason: None,
                };
                inner
                    .core
                    .call(move |s| s.update_analysis_progress(id, attempt, progress))
                    .map_err(|e| e.to_string())?;
            } else if message["method"] == "jobs.checkpoint" {
                let checkpoint: AnalysisCheckpoint =
                    serde_json::from_value(message["params"].clone()).map_err(|e| e.to_string())?;
                if checkpoint.job_id != id || checkpoint.attempt != attempt {
                    return Err("INVALID_MODEL_OUTPUT: checkpoint 身份不一致".into());
                }
                let ack = json!({"jsonrpc":"2.0","method":"jobs.checkpoint_ack","params":{"job_id":id,"attempt":attempt,"step_index":checkpoint.step_index,"result_sha256":checkpoint.result_sha256}});
                inner
                    .core
                    .call(move |s| s.confirm_analysis_checkpoint(&checkpoint))
                    .map_err(|e| e.to_string())?;
                worker
                    .send(&ack, Duration::from_millis(250))
                    .map_err(|e| e.to_string())?;
            } else if message["id"] == run_id {
                if let Some(error) = message.get("error") {
                    return Err(format!(
                        "{}: {}",
                        error["data"]["code"]
                            .as_str()
                            .unwrap_or("INVALID_MODEL_OUTPUT"),
                        error["message"].as_str().unwrap_or("分析失败")
                    ));
                }
                let result = &message["result"];
                if result["job_id"] != id.to_string()
                    || result["attempt"] != attempt
                    || result["snapshot_id"] != job.snapshot_id.to_string()
                    || result["snapshot_sha256"] != job.snapshot_sha256
                {
                    return Err("INVALID_MODEL_OUTPUT: 结果回执身份不一致".into());
                }
                let path = result["result_ref"]
                    .as_str()
                    .ok_or("INVALID_MODEL_OUTPUT: 缺少结果文件")?;
                let digest = result["result_sha256"]
                    .as_str()
                    .ok_or("INVALID_MODEL_OUTPUT: 缺少结果指纹")?;
                let bytes =
                    read_result(&directory, path, Some(digest)).map_err(|e| e.to_string())?;
                let output: AnalysisResult =
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                return Ok(Execution::Completed(output));
            } else {
                return Err("INVALID_MODEL_OUTPUT: 未知分析协议消息".into());
            }
        }
    })();
    if let Err(error) = worker.stop(Duration::from_secs(2)) {
        return Ok(Execution::Uncontained(format!(
            "后台进程尚未确认退出：{error}；原任务错误：{}",
            result.as_ref().err().map(String::as_str).unwrap_or("none")
        )));
    }
    result
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temporary, path))
        .map_err(|e| e.to_string())?;
    fs::File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadence_queues_insight_then_current_session_minutes_only() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let now = Instant::now();
        let mut automatic = Automatic {
            live: Some((first, now)),
            ..Default::default()
        };
        assert!(automatic.next_job(now + Duration::from_secs(179)).is_none());
        assert_eq!(
            automatic.next_job(now + Duration::from_secs(180)),
            Some((first, AnalysisKind::Insight))
        );
        assert_eq!(
            automatic.next_job(now + Duration::from_secs(181)),
            Some((first, AnalysisKind::Minutes))
        );
        assert!(automatic.next_job(now + Duration::from_secs(182)).is_none());
        assert_eq!(
            automatic.next_job(now + Duration::from_secs(360)),
            Some((first, AnalysisKind::Insight))
        );
        automatic.live = Some((second, now + Duration::from_secs(360)));
        assert!(automatic.next_job(now + Duration::from_secs(361)).is_none());
    }
    #[test]
    fn stop_after_analysis_shutdown_is_durable_without_model_setup() {
        let root = std::env::temp_dir().join(format!("forum-analysis-stop-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("core.sqlite");
        let core = CoreHandle::open(&path, 32).unwrap();
        let id = Uuid::new_v4();
        core.call(move |s| {
            s.create_session(&SessionSpec {
                session_id: id,
                event_id: Uuid::new_v4(),
                room_id: Uuid::new_v4(),
                owner_device_id: Uuid::new_v4(),
                title: "合成退出持久化测试".into(),
            })
        })
        .unwrap();
        let manager =
            AnalysisManager::new(core.clone(), root.clone(), None, ResourceBudget::default());
        let client = manager.client();
        manager.shutdown();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !manager.shutdown_complete() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        client.session_started(id);
        client.session_stopped(id);
        assert!(manager.shutdown_complete());
        assert_eq!(
            core.call(|s| s.pending_analysis_stop_intents(8))
                .unwrap()
                .len(),
            1
        );
        drop(client);
        drop(manager);
        core.shutdown().unwrap();
        drop(core);
        let reopened = CoreHandle::open(&path, 32).unwrap();
        assert_eq!(
            reopened
                .call(|s| s.pending_analysis_stop_intents(8))
                .unwrap()[0]
                .0,
            id
        );
        reopened.shutdown().unwrap();
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
