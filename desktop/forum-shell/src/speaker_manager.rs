//! Optional ECAPA host. It reads already-durable private PCM; never opens a device.
//! The immutable revision and recording identity are checked again by core at commit.
use forum_contracts::*;
use forum_core::{CoreHandle, PageKey, TranscriptRecord};
use forum_runtime::{
    recording::RecordingManifest,
    resource_budget::ResourceBudget,
    worker::{attempt_directory, read_result, WorkerProcess},
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::result::Result;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const JOB_BUDGET: Duration = Duration::from_secs(30);
const MAX_PCM_SAMPLES: usize = 480_000;
#[derive(Clone, Debug, Serialize)]
pub struct SpeakerStatus {
    pub enabled: bool,
    pub session_id: Option<Uuid>,
    pub state: String,
    pub notice: Option<String>,
    pub active_segment: Option<Uuid>,
    pub completed: u64,
    pub skipped: u64,
}
impl Default for SpeakerStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            session_id: None,
            state: "disabled".into(),
            notice: Some("匿名说话人分析默认关闭；启用后仅处理所选场次已有录音。".into()),
            active_segment: None,
            completed: 0,
            skipped: 0,
        }
    }
}
struct Inner {
    core: CoreHandle,
    root: PathBuf,
    resources: Option<PathBuf>,
    budget: ResourceBudget,
    state: Mutex<SpeakerStatus>,
    model_path: Mutex<Option<PathBuf>>,
    changed: Condvar,
    generation: AtomicU64,
    stopped: AtomicBool,
    contained: AtomicBool,
    pending_commits: AtomicU64,
}
pub struct SpeakerManager {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}
#[derive(Clone)]
pub struct SpeakerClient(Arc<Inner>);
impl SpeakerManager {
    pub fn new(
        core: CoreHandle,
        data_root: PathBuf,
        resources: Option<PathBuf>,
        budget: ResourceBudget,
    ) -> Self {
        let inner = Arc::new(Inner {
            core,
            root: data_root,
            resources,
            budget,
            state: Mutex::new(SpeakerStatus::default()),
            model_path: Mutex::new(None),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            contained: AtomicBool::new(true),
            pending_commits: AtomicU64::new(0),
        });
        let task = inner.clone();
        let worker = thread::spawn(move || run_loop(task));
        Self {
            inner,
            worker: Some(worker),
        }
    }
    pub fn client(&self) -> SpeakerClient {
        SpeakerClient(self.inner.clone())
    }
    pub fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.inner.changed.notify_all();
    }
    pub fn shutdown_complete(&self) -> bool {
        self.inner.contained.load(Ordering::Acquire)
            && self.inner.pending_commits.load(Ordering::Acquire) == 0
            && self.worker.as_ref().is_none_or(|w| w.is_finished())
    }
}
impl Drop for SpeakerManager {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
impl SpeakerClient {
    pub fn enable(
        &self,
        session: Uuid,
        model_path: Option<PathBuf>,
    ) -> Result<SpeakerStatus, String> {
        if model_path
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || !p.is_dir())
        {
            return Err("模型目录必须是已准备好的绝对本地路径".into());
        }
        if self.0.stopped.load(Ordering::Acquire) || !self.0.contained.load(Ordering::Acquire) {
            return Err("说话人进程正在退出或尚未确认已释放".into());
        }
        self.0
            .core
            .call(move |s| s.session_status(session))
            .map_err(|e| e.to_string())?;
        let mut state = self.0.state.lock();
        *self.0.model_path.lock() = model_path;
        self.0.generation.fetch_add(1, Ordering::AcqRel);
        *state = SpeakerStatus {
            enabled: true,
            session_id: Some(session),
            state: "preparing".into(),
            notice: Some("正在检查独立说话人运行时与模型；不会下载模型。".into()),
            ..SpeakerStatus::default()
        };
        self.0.changed.notify_all();
        Ok(state.clone())
    }
    pub fn disable(&self) -> SpeakerStatus {
        let mut s = self.0.state.lock();
        self.0.generation.fetch_add(1, Ordering::AcqRel);
        s.enabled = false;
        s.state = if s.active_segment.is_some() {
            "stopping"
        } else {
            "disabled"
        }
        .into();
        s.notice = Some("已停止提交新的说话人标签；正在运行的分析会被取消。".into());
        self.0.changed.notify_all();
        s.clone()
    }
    pub fn status(&self) -> SpeakerStatus {
        self.0.state.lock().clone()
    }
    pub fn list(&self, session: Uuid) -> Result<Vec<SpeakerAssignment>, String> {
        self.0
            .core
            .call(move |s| s.list_speaker_assignments(session))
            .map_err(|e| e.to_string())
    }
    pub fn correct(&self, command: SpeakerAssignmentCommand) -> Result<SpeakerAssignment, String> {
        if !matches!(command.origin, SpeakerAssignmentOrigin::Human { .. }) {
            return Err("人工修订必须注明操作者与原因".into());
        }
        self.0
            .core
            .call(move |s| s.apply_speaker_assignment(&command))
            .map_err(|e| e.to_string())
    }
}
#[derive(Clone)]
struct ModelSetup {
    python: PathBuf,
    source: Option<PathBuf>,
    model: PathBuf,
    manifest: String,
}
impl ModelSetup {
    fn resolve(
        root: &Path,
        resources: Option<&Path>,
        model_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let python = std::env::var_os("FORUM_SPEAKER_PYTHON")
            .map(PathBuf::from)
            .or_else(|| resources.map(|r| r.join("speaker-worker/python/bin/python3.12")))
            .ok_or("尚未安装独立说话人 Python 运行时")?;
        if !python.is_absolute() || !python.is_file() {
            return Err("说话人 Python 运行时不完整".into());
        }
        let source = std::env::var_os("FORUM_SPEAKER_WORKER_SOURCE").map(PathBuf::from);
        if source.as_ref().is_some_and(|s| {
            !s.is_absolute() || !s.join("forum_speaker_worker/__main__.py").is_file()
        }) {
            return Err("说话人开发模块路径无效".into());
        }
        // The legacy repository's data/ecapa_model is never searched implicitly.
        let model = model_path
            .or_else(|| std::env::var_os("FORUM_SPEAKER_MODEL_PATH").map(PathBuf::from))
            .unwrap_or_else(|| root.join("models/ecapa"));
        if !model.is_absolute() || !model.is_dir() {
            return Err("尚未准备 ECAPA 模型；录音和字幕仍可使用。请安装模型或显式配置 FORUM_SPEAKER_MODEL_PATH。".into());
        }
        let path = model.join("embedding_model.ckpt");
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| format!("ECAPA模型不可用：{e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 * 1024 {
            return Err("ECAPA模型文件大小/类型无效".into());
        }
        let mut digest = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        Ok(Self {
            python,
            source,
            model,
            manifest: format!("sha256:{:x}", digest.finalize()),
        })
    }
    fn command(&self) -> Command {
        let mut c = Command::new(&self.python);
        c.args(["-I", "-B"]);
        if let Some(source) = &self.source {
            c.args(["-c","import sys,runpy;sys.path.insert(0,sys.argv.pop(1));runpy.run_module('forum_speaker_worker',run_name='__main__')"]).arg(source);
        } else {
            c.args(["-m", "forum_speaker_worker"]);
        }
        c.env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("HF_HUB_DISABLE_IMPLICIT_TOKEN", "1");
        c
    }
}
#[derive(Clone)]
struct Candidate {
    record: TranscriptRecord,
    expected_assignment: Option<Revision>,
}
fn interrupted(inner: &Inner, generation: u64) -> bool {
    inner.stopped.load(Ordering::Acquire) || inner.generation.load(Ordering::Acquire) != generation
}
fn update(inner: &Inner, generation: u64, change: impl FnOnce(&mut SpeakerStatus)) {
    let mut s = inner.state.lock();
    if inner.generation.load(Ordering::Acquire) == generation {
        change(&mut s);
    }
}
fn pause(inner: &Inner, duration: Duration) {
    let mut state = inner.state.lock();
    inner.changed.wait_for(&mut state, duration);
}
fn run_loop(inner: Arc<Inner>) {
    let mut seen_generation = u64::MAX;
    let mut setup: Option<ModelSetup> = None;
    let mut failed = HashSet::new();
    let mut after: Option<PageKey> = None;
    let mut cursor: Option<u64> = None;
    while !inner.stopped.load(Ordering::Acquire) {
        let generation = inner.generation.load(Ordering::Acquire);
        let status = inner.state.lock().clone();
        if !status.enabled {
            if status.active_segment.is_none() {
                inner.state.lock().state = "disabled".into();
            }
            pause(&inner, Duration::from_millis(250));
            continue;
        }
        let Some(session) = status.session_id else {
            pause(&inner, Duration::from_millis(250));
            continue;
        };
        if generation != seen_generation {
            seen_generation = generation;
            setup = None;
            failed.clear();
            after = None;
            cursor = None;
        }
        if setup.is_none() {
            let selected_model = inner.model_path.lock().clone();
            match ModelSetup::resolve(&inner.root, inner.resources.as_deref(), selected_model) {
                Ok(model) => {
                    setup = Some(model);
                    update(&inner, generation, |s| {
                        s.state = "waiting".into();
                        s.notice = None;
                    });
                }
                Err(e) => {
                    update(&inner, generation, |s| {
                        s.state = "unavailable".into();
                        s.notice = Some(e);
                    });
                    pause(&inner, Duration::from_secs(2));
                    continue;
                }
            }
        }
        let page_after = after.clone();
        let page_cursor = cursor;
        let page = inner.core.call(move |s| {
            let page = s.snapshot_page(session, page_cursor, page_after, 100)?;
            let assignments = s.list_speaker_assignments(session)?;
            Ok((page, assignments))
        });
        let (page, assignments) = match page {
            Ok(v) => v,
            Err(e) => {
                update(&inner, generation, |s| {
                    s.state = "error".into();
                    s.notice = Some(format!("读取稳定原文失败：{e}"));
                });
                pause(&inner, Duration::from_secs(1));
                continue;
            }
        };
        let assignment_map: HashMap<Uuid, SpeakerAssignment> =
            assignments.into_iter().map(|a| (a.segment_id, a)).collect();
        cursor = Some(page.cursor);
        after = page.next_after.clone();
        for record in page.items.into_iter().filter_map(|item| item.transcript) {
            if interrupted(&inner, generation) {
                break;
            }
            if record.payload.status != TranscriptStatus::Success
                || record.payload.text.trim().is_empty()
            {
                continue;
            }
            let key = (record.payload.segment_id, record.payload.revision.get());
            if failed.contains(&key) {
                continue;
            }
            let assignment = assignment_map.get(&record.payload.segment_id);
            if assignment.is_some_and(|a| {
                a.current || matches!(a.origin, SpeakerAssignmentOrigin::Human { .. })
            }) {
                continue;
            }
            let candidate = Candidate {
                record,
                expected_assignment: assignment.map(|a| a.revision),
            };
            let permit = match inner.budget.admit(true) {
                Ok(p) => p,
                Err(reason) => {
                    update(&inner, generation, |s| {
                        s.state = "waiting".into();
                        s.notice = Some(reason);
                    });
                    after = None;
                    cursor = None;
                    pause(&inner, Duration::from_millis(300));
                    break;
                }
            };
            update(&inner, generation, |s| {
                s.active_segment = Some(key.0);
                s.state = "running".into();
                s.notice = None;
            });
            let outcome =
                process_candidate(&inner, generation, setup.as_ref().unwrap(), &candidate);
            if !inner.contained.load(Ordering::Acquire) {
                std::mem::forget(permit);
                return;
            }
            drop(permit);
            let cancelled = interrupted(&inner, generation);
            let yielded = outcome
                .as_ref()
                .err()
                .is_some_and(|e| e.starts_with("资源等待："));
            if !cancelled {
                match outcome {
                    Ok(reason) => update(&inner, generation, |s| {
                        s.completed += 1;
                        s.state = "waiting".into();
                        s.notice = reason;
                    }),
                    Err(e) => {
                        if !yielded {
                            failed.insert(key);
                        }
                        update(&inner, generation, |s| {
                            if !yielded {
                                s.skipped += 1;
                            }
                            s.state = "waiting".into();
                            s.notice = Some(if yielded {
                                e
                            } else {
                                format!("此段保留未知说话人：{e}")
                            });
                        });
                    }
                }
            }
            {
                let mut state = inner.state.lock();
                state.active_segment = None;
                if !state.enabled {
                    state.state = "disabled".into();
                }
            }
            if cancelled {
                break;
            }
            if yielded {
                after = None;
                cursor = None;
                pause(&inner, Duration::from_millis(300));
                break;
            }
            if failed.len() >= 10000 {
                update(&inner, generation, |s| {
                    s.state = "unavailable".into();
                    s.enabled = false;
                    s.notice =
                        Some("本次说话人分析跳过段数达到上限，请检查录音后重新启用。".into());
                });
                break;
            }
        }
        if after.is_none() {
            cursor = None;
            pause(&inner, Duration::from_millis(800));
        }
    }
    let mut state = inner.state.lock();
    state.enabled = false;
    state.active_segment = None;
    state.state = "stopped".into();
}
fn read_pcm(root: &Path, c: &Candidate) -> Result<Vec<u8>, String> {
    let r = &c.record;
    let base = root
        .join("sessions")
        .join(r.session_id.to_string())
        .join("audio");
    let track = base.join("tracks").join(r.payload.track_id.to_string());
    let directory = if track.join("manifest.json").exists() {
        track
    } else {
        base
    };
    // Check every private directory component inside the session; a forged
    // manifest must not redirect this host to arbitrary files or named pipes.
    let session = root.join("sessions").join(r.session_id.to_string());
    let relative = directory
        .strip_prefix(&session)
        .map_err(|e| e.to_string())?;
    for path in [root.to_path_buf(), root.join("sessions")] {
        let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || m.uid() != unsafe { libc::geteuid() }
            || m.mode() & 0o777 != 0o700
        {
            return Err("录音根目录必须是本机所有者的0700真实目录".into());
        }
    }
    let mut check = session.clone();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            check.push(component.as_os_str());
        }
        let m = fs::symlink_metadata(&check).map_err(|_| "此场次没有可恢复的私有录音")?;
        if !m.is_dir()
            || m.file_type().is_symlink()
            || m.uid() != unsafe { libc::geteuid() }
            || m.mode() & 0o777 != 0o700
        {
            return Err("录音目录必须是本机所有者的0700真实目录".into());
        }
    }
    let bytes = read_result(&directory, "manifest.json", None)
        .map_err(|e| format!("录音清单不可用：{e}"))?;
    let manifest: RecordingManifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if manifest.schema_version != 1
        || manifest.track.session_id != r.session_id
        || manifest.track.track_id != r.payload.track_id
        || manifest.track.sample_rate != 16000
    {
        return Err("录音清单不属于当前场次/音轨".into());
    }
    let segment = manifest
        .segments
        .iter()
        .find(|s| s.segment_id == r.payload.segment_id)
        .ok_or("录音清单没有当前稳定段")?;
    if segment.audio != r.payload.audio
        || segment.metadata.audio != r.payload.audio
        || segment.metadata.session_id != r.session_id
        || segment.metadata.track_id != r.payload.track_id
        || segment.metadata.segment_id != r.payload.segment_id
    {
        return Err("录音段的身份或时钟范围与原文不匹配".into());
    }
    let expected = format!("segment-{}.f32le", r.payload.segment_id);
    if segment.file.as_deref() != Some(expected.as_str()) {
        return Err("本段没有保存PCM；不能计算声纹".into());
    }
    let count = r
        .payload
        .audio
        .end_sample
        .checked_sub(r.payload.audio.start_sample)
        .ok_or("录音范围无效")? as usize;
    if count == 0 || count > MAX_PCM_SAMPLES {
        return Err("说话人段必须在30秒以内".into());
    }
    let hash = segment.sha256.as_deref().ok_or("录音段缺少SHA")?;
    let raw = read_result(&directory, &expected, Some(hash)).map_err(|e| e.to_string())?;
    if raw.len() != count * 4 {
        return Err("录音段长度不匹配".into());
    }
    let mut pcm = Vec::with_capacity(count * 2);
    for bytes in raw.chunks_exact(4) {
        let value = f32::from_le_bytes(bytes.try_into().unwrap());
        if !value.is_finite() || value.abs() > 1.01 {
            return Err("录音含非有限或越界采样".into());
        }
        let quantized = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        pcm.extend(quantized.to_le_bytes());
    }
    Ok(pcm)
}
fn write_pcm(directory: &Path, pcm: &[u8]) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("segment.pcm"))
        .map_err(|e| e.to_string())?;
    f.write_all(pcm)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Quality {
    duration_seconds: f64,
    rms: f64,
    clipping_ratio: f64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerResult {
    schema_version: u32,
    job_id: Uuid,
    attempt: u32,
    session_id: Uuid,
    track_id: Uuid,
    segment_id: Uuid,
    segment_revision: Revision,
    model_manifest_id: String,
    pcm_sha256: String,
    status: String,
    reason: Option<String>,
    embedding: Option<Vec<f32>>,
    quality: Quality,
}
fn validate_result(
    value: Value,
    job: Uuid,
    c: &Candidate,
    model: &ModelSetup,
    hash: &str,
    samples: usize,
) -> Result<WorkerResult, String> {
    let v: WorkerResult = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if v.schema_version != 1
        || v.job_id != job
        || v.attempt != 1
        || v.session_id != c.record.session_id
        || v.track_id != c.record.payload.track_id
        || v.segment_id != c.record.payload.segment_id
        || v.segment_revision != c.record.payload.revision
        || v.model_manifest_id != model.manifest
        || v.pcm_sha256 != hash
    {
        return Err("说话人结果身份/模型/录音摘要不匹配".into());
    }
    if !v.quality.duration_seconds.is_finite()
        || (v.quality.duration_seconds - samples as f64 / 16000.0).abs() > 0.001
        || !v.quality.rms.is_finite()
        || !(0.0..=1.0).contains(&v.quality.rms)
        || !v.quality.clipping_ratio.is_finite()
        || !(0.0..=1.0).contains(&v.quality.clipping_ratio)
    {
        return Err("说话人结果音频质量范围无效".into());
    }
    match (v.status.as_str(), v.embedding.as_ref(), v.reason.as_deref()) {
        ("embedding", Some(e), None)
            if v.quality.duration_seconds >= 1.5
                && v.quality.rms >= 0.005
                && v.quality.clipping_ratio <= 0.01
                && e.len() == 192
                && e.iter().all(|v| v.is_finite())
                && (e.iter().map(|v| v * v).sum::<f32>().sqrt() - 1.0).abs() <= 0.02 => {}
        ("unknown", None, Some("too_short")) if v.quality.duration_seconds < 1.5 => {}
        ("unknown", None, Some("quiet_or_silent")) if v.quality.rms < 0.005 => {}
        ("unknown", None, Some("clipped_audio")) if v.quality.clipping_ratio > 0.01 => {}
        // This host never infers overlap from a track name or a voice vector.
        // Until a separate overlap detector is available, an unsolicited overlap
        // result is rejected rather than pretending the host established it.
        _ => return Err("说话人结果状态或向量无效".into()),
    }
    Ok(v)
}
fn await_response(
    worker: &WorkerProcess,
    id: u64,
    inner: &Inner,
    generation: u64,
    deadline: Instant,
) -> Result<Value, String> {
    loop {
        if interrupted(inner, generation) {
            return Err("已取消".into());
        }
        if let Some(reason) = inner.budget.yield_reason() {
            return Err(format!("资源等待：{reason}"));
        }
        if Instant::now() >= deadline {
            return Err("说话人分析超过30秒预算".into());
        }
        if let Some(v) = worker
            .poll(Duration::from_millis(100))
            .map_err(|e| e.to_string())?
        {
            if serde_json::to_vec(&v).map_err(|e| e.to_string())?.len() > 64 * 1024 {
                return Err("说话人协议帧超过64KiB".into());
            }
            if v["id"] != id {
                return Err("说话人响应ID不匹配".into());
            }
            if let Some(e) = v.get("error") {
                return Err(format!(
                    "说话人运行时错误：{}",
                    e.get("code").unwrap_or(&Value::Null)
                ));
            }
            return v
                .get("result")
                .cloned()
                .ok_or("说话人响应缺少result".into());
        }
    }
}
fn process_candidate(
    inner: &Arc<Inner>,
    generation: u64,
    model: &ModelSetup,
    c: &Candidate,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + JOB_BUDGET;
    let pcm = read_pcm(&inner.root, c)?;
    let hash = format!("{:x}", Sha256::digest(&pcm));
    if interrupted(inner, generation) {
        return Err("已取消".into());
    }
    let job = Uuid::new_v4();
    let job_root = inner.root.join("speaker-jobs");
    let dir = attempt_directory(&job_root, job, 1).map_err(|e| e.to_string())?;
    write_pcm(&dir, &pcm)?;
    struct TemporaryPcm(PathBuf);
    impl Drop for TemporaryPcm {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _temporary = TemporaryPcm(dir.join("segment.pcm"));
    let mut worker = WorkerProcess::spawn(model.command(), &dir.join("worker.stderr.log"))
        .map_err(|e| e.to_string())?;
    let outcome = (|| {
        worker.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"instance_id":Uuid::new_v4(),"job_root":job_root,"model_grant":{"profile":"ecapa-voxceleb-v1","model_path":model.model,"model_manifest_id":model.manifest}}}),Duration::from_millis(500)).map_err(|e|e.to_string())?;
        let initialized = await_response(&worker, 1, inner, generation, deadline)?;
        if initialized["protocol_version"] != 1 {
            return Err("说话人协议版本不兼容".into());
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        if remaining == 0 {
            return Err("说话人分析预算耗尽".into());
        }
        worker.send(&json!({"jsonrpc":"2.0","id":2,"method":"jobs.run","params":{"job_id":job,"attempt":1,"remaining_budget_ms":remaining,"session_id":c.record.session_id,"track_id":c.record.payload.track_id,"segment_id":c.record.payload.segment_id,"segment_revision":c.record.payload.revision,"pcm":{"relative_path":"segment.pcm","sha256":hash,"sample_rate":16000,"channels":1,"format":"s16le"},"overlap":false}}),Duration::from_millis(500)).map_err(|e|e.to_string())?;
        validate_result(
            await_response(&worker, 2, inner, generation, deadline)?,
            job,
            c,
            model,
            &hash,
            pcm.len() / 2,
        )
    })();
    if outcome.is_err() {
        let _=worker.send(&json!({"jsonrpc":"2.0","id":3,"method":"jobs.cancel","params":{"job_id":job,"attempt":1}}),Duration::from_millis(200));
    }
    if let Err(e) = worker.stop(Duration::from_millis(500)) {
        inner.contained.store(false, Ordering::Release);
        let mut state = inner.state.lock();
        state.state = "uncontained".into();
        state.enabled = false;
        state.notice = Some(format!("无法确认说话人子进程退出，已停止后续后台任务：{e}"));
        return Err(e.to_string());
    }
    // The input is a temporary private copy; retain bounded metadata/logs only.
    let _ = fs::remove_file(dir.join("segment.pcm"));
    let result = outcome?;
    if interrupted(inner, generation) {
        return Err("已取消；结果未写入".into());
    }
    let candidate = c.clone();
    let manifest = model.manifest.clone();
    let notice = if result.status == "unknown" {
        Some(format!(
            "此段保留未知说话人：{}",
            result.reason.as_deref().unwrap_or("unknown")
        ))
    } else {
        None
    };
    let command_id = Uuid::new_v4();
    let owner = inner.clone();
    inner.pending_commits.fetch_add(1, Ordering::AcqRel);
    let ticket = inner.core.try_call(move |store| {
        struct Pending(Arc<Inner>);
        impl Drop for Pending {
            fn drop(&mut self) {
                self.0.pending_commits.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _pending = Pending(owner.clone());
        // Lock in the core closure, not in the caller waiting on core. Thus even
        // an ACK timeout cannot let a queued result commit after disable returns.
        let state = owner.state.lock();
        if interrupted(&owner, generation) || !state.enabled {
            return Err(forum_core::StoreError::InvalidState);
        }
        if let Some(embedding) = result.embedding {
            store.apply_speaker_embedding(&SpeakerEmbeddingCommand {
                request_id: command_id,
                session_id: candidate.record.session_id,
                segment_id: candidate.record.payload.segment_id,
                source_revision: candidate.record.payload.revision,
                expected_revision: candidate.expected_assignment,
                model_manifest_id: manifest,
                model_version: "ecapa-voxceleb-v1".into(),
                pcm_sha256: hash,
                embedding,
            })
        } else {
            store.apply_speaker_assignment(&SpeakerAssignmentCommand {
                request_id: command_id,
                session_id: candidate.record.session_id,
                segment_id: candidate.record.payload.segment_id,
                source_revision: candidate.record.payload.revision,
                expected_revision: candidate.expected_assignment,
                label: SpeakerLabel::Unknown,
                origin: SpeakerAssignmentOrigin::Automatic {
                    model_manifest_id: manifest,
                    model_version: "ecapa-voxceleb-v1".into(),
                },
            })
        }
    });
    let ticket = match ticket {
        Ok(t) => t,
        Err(e) => {
            inner.pending_commits.fetch_sub(1, Ordering::AcqRel);
            return Err(e.to_string());
        }
    };
    ticket.wait().map_err(|e| e.to_string())?;
    Ok(notice)
}

#[cfg(test)]
#[path = "speaker_manager_tests.rs"]
mod tests;
