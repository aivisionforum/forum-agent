//! # hen-local-init
//!
//! First-run model downloader for AI Vision Forum's owned model directory.
//! Replaces the conda/Python bootstrap: downloads ASR and translator models
//! directly via HTTP, with ModelScope as the default provider and Hugging Face
//! available as a fallback.
//!
//! ## Configuration (environment variables)
//!
//! All variables are optional and have sensible defaults:
//!
//! | Variable                          | Default                                              |
//! |-----------------------------------|------------------------------------------------------|
//! | `FORUM_AGENT_BOOTSTRAP_STATE_PATH` | `~/Library/Logs/AI Vision Forum/bootstrap_state.txt` |
//! | `FORUM_AGENT_MODEL_COMPONENT`    | `core` (the ASR and translation pair only)          |
//! | `QWEN3_ASR_MODEL_PATH`            | `~/Library/Application Support/AI Vision Forum/models/qwen3-asr-1.7b` |
//! | `QWEN3_ASR_REPO`                  | `mlx-community/Qwen3-ASR-1.7B-8bit`                 |
//! | `QWEN35_TRANSLATOR_MODEL_PATH`    | `~/Library/Application Support/AI Vision Forum/models/Hy-MT2-1.8B-4bit` |
//! | `QWEN35_TRANSLATOR_REPO`          | `mlx-community/Hy-MT2-1.8B-4bit`                 |
//! | `MOXIN_MODEL_PROVIDER`            | `auto` (`modelscope`/`huggingface` force one path)  |
//! | `MOXIN_MODELSCOPE_ENDPOINT`       | `https://modelscope.cn`                             |
//! | `HF_ENDPOINT`                     | `https://huggingface.co` (Hugging Face provider)    |
//!
//! Model-path overrides must resolve to those two owned directories. External
//! caches (including ~/.OminiX) may be read by inference, never repaired here.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

// ── State file ────────────────────────────────────────────────────────────────
//
// Format consumed by screen.rs poll_runtime_initialization:
//   "{current}/{total}|{title}|{detail}|{pct}\n"
// where pct is overall download progress as a float 0.0000–1.0000.

// Actual download sizes in bytes (measured 2026-04-17, `du -sk` × 1024)
const BYTES_TRANSLATOR: u64 = 1_017_400_000; // Hy-MT2-1.8B-4bit
const BYTES_ASR: u64 = 2_473_308_160; // Qwen3-ASR-1.7B-8bit
const TOTAL_BYTES: u64 = BYTES_TRANSLATOR + BYTES_ASR;
const MODEL_COMPLETION_MARKER: &str = ".moxin-model-complete.json";
const BOOTSTRAP_VERSION: u32 = 1;
const DEFAULT_HF_ENDPOINT: &str = "https://huggingface.co";
const DEFAULT_MODELSCOPE_ENDPOINT: &str = "https://modelscope.cn";
const HTTP_USER_AGENT: &str = "AIVisionForum/hen-local-init";
const PROVIDER_PROBE_REPO: &str = "mlx-community/Hy-MT2-1.8B-4bit";
const PROVIDER_PROBE_FILE: &str = "config.json";
const BOOTSTRAP_LOCK_FILE: &str = "bootstrap.lock";
const MAX_DOWNLOAD_FILE_ATTEMPTS: usize = 4;
const OWNED_MODELS_RELATIVE: &str = "Library/Application Support/AI Vision Forum/models";
const ASR_DIRECTORY: &str = "qwen3-asr-1.7b";
const TRANSLATOR_DIRECTORY: &str = "Hy-MT2-1.8B-4bit";

/// Validated write ownership. No filesystem mutations occur during validation.
/// The host's home directory is the trust anchor; below it, even symlinks that
/// currently point inside the owned tree are rejected to avoid ambiguous writes.
struct OwnedModelTargets {
    home: PathBuf,
    root: PathBuf,
    asr: PathBuf,
    translator: PathBuf,
}

fn absolute_without_parent(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!(
            "Model download path must be absolute and contain no '..': {}",
            path.display()
        );
    }
    Ok(path.components().collect())
}

fn reject_symlink_components(anchor: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(anchor)
        .context("Model path is outside the ownership anchor")?;
    let mut current = anchor.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("Invalid model path component");
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("Model download/repair refuses a symbolic link: {}. External caches are read-only inference sources.", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect model path {}", current.display()))
            }
        }
    }
    Ok(())
}

fn reject_tree_symlinks(path: &Path) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect model entry {}", current.display()))
            }
        };
        if metadata.file_type().is_symlink() {
            bail!("Model download/repair refuses a symbolic link: {}. External caches are read-only inference sources.", current.display());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&current)? {
                pending.push(entry?.path());
            }
        } else if !metadata.is_file() {
            bail!(
                "Model directory contains a non-regular entry: {}",
                current.display()
            );
        }
    }
    Ok(())
}

impl OwnedModelTargets {
    fn validate(home: &Path, asr: &Path, translator: &Path) -> Result<Self> {
        let home_lexical = absolute_without_parent(home)?;
        let home = home
            .canonicalize()
            .context("Resolve home directory for Forum model ownership")?;
        let normalize = |target: &Path| -> Result<PathBuf> {
            let target = absolute_without_parent(target)?;
            // macOS /var -> /private/var and an explicitly relocated home are
            // allowed only at the trusted home anchor, not beneath Library.
            Ok(if let Ok(relative) = target.strip_prefix(&home_lexical) {
                home.join(relative)
            } else {
                target
            })
        };
        let asr = normalize(asr)?;
        let translator = normalize(translator)?;
        if asr == translator || asr.starts_with(&translator) || translator.starts_with(&asr) {
            bail!("ASR and translator download directories must be distinct and cannot contain each other");
        }
        let root = home.join(OWNED_MODELS_RELATIVE);
        if asr != root.join(ASR_DIRECTORY) || translator != root.join(TRANSLATOR_DIRECTORY) {
            bail!("Downloads and repairs are restricted to {} and {}. External QWEN*_MODEL_PATH caches are read-only inference sources; this installer will not modify them.",
                root.join(ASR_DIRECTORY).display(), root.join(TRANSLATOR_DIRECTORY).display());
        }
        let targets = Self {
            home,
            root,
            asr,
            translator,
        };
        targets.check(&targets.asr)?;
        targets.check(&targets.translator)?;
        Ok(targets)
    }

    fn check(&self, target: &Path) -> Result<()> {
        if target != self.asr && target != self.translator {
            bail!("Unowned model write target: {}", target.display());
        }
        reject_symlink_components(&self.home, target)?;
        if target.exists() && !target.is_dir() {
            bail!(
                "Model write target is not a directory: {}",
                target.display()
            );
        }
        reject_tree_symlinks(target)
    }
}

fn validate_component(component: Option<&str>) -> Result<()> {
    if component.is_some_and(|value| value != "core") {
        bail!("FORUM_AGENT_MODEL_COMPONENT supports only 'core' (ASR + translation) in this build");
    }
    Ok(())
}

fn model_file_destination(target: &Path, filename: &str) -> Result<PathBuf> {
    let relative = Path::new(filename);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("Invalid model repository file path: {filename}");
    }
    let destination = target.join(relative);
    reject_symlink_components(target, &destination)?;
    Ok(destination)
}

const ASR_MODEL_FILES: &[&str] = &[
    ".gitattributes",
    "README.md",
    "chat_template.json",
    "config.json",
    "generation_config.json",
    "merges.txt",
    "model.safetensors",
    "model.safetensors.index.json",
    "preprocessor_config.json",
    "tokenizer_config.json",
    "vocab.json",
];

const QWEN35_TRANSLATOR_MODEL_FILES: &[&str] = &[
    ".gitattributes", "README.md", "LICENSE.txt", "chat_template.jinja", "config.json",
    "generation_config.json", "model.safetensors", "model.safetensors.index.json",
    "tokenizer.json", "tokenizer_config.json", "mlx_manifest.json",
];

fn write_state(
    state_file: Option<&Path>,
    current: usize,
    total: usize,
    title: &str,
    detail: &str,
    bytes_done: u64,
    total_bytes: u64,
) {
    let pct = if total_bytes > 0 {
        (bytes_done as f64 / total_bytes as f64).min(0.99)
    } else {
        0.0
    };
    eprintln!(
        "[hen-local-init] {}/{} {} — {} ({:.1}%)",
        current,
        total,
        title,
        detail,
        pct * 100.0
    );
    let Some(path) = state_file else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        path,
        format!("{}/{}|{}|{}|{:.4}\n", current, total, title, detail, pct),
    );
}

fn file_exists(path: &Path) -> bool {
    path.metadata().map(|m| m.is_file()).unwrap_or(false)
}

fn file_len(path: &Path) -> u64 {
    path.metadata().map(|m| m.len()).unwrap_or(0)
}

fn format_bytes_per_second(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB/s", bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024.0 * 1024.0 {
        format!("{:.1} MB/s", bytes_per_sec / (1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1024.0)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

#[derive(Debug)]
struct BootstrapLock {
    path: PathBuf,
    _file: File,
}

impl Drop for BootstrapLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn parse_bootstrap_lock_pid(contents: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        let pid = line.strip_prefix("pid=").unwrap_or(line);
        pid.parse::<u32>().ok().filter(|pid| *pid > 0)
    })
}

fn process_id_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn bootstrap_lock_is_active(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    parse_bootstrap_lock_pid(&contents)
        .map(process_id_is_running)
        .unwrap_or(false)
}

fn acquire_bootstrap_lock(path: &Path) -> Result<BootstrapLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }

    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                write!(file, "pid={}\n", std::process::id())
                    .with_context(|| format!("write bootstrap lock {}", path.display()))?;
                return Ok(BootstrapLock {
                    path: path.to_path_buf(),
                    _file: file,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if bootstrap_lock_is_active(path) {
                    bail!(
                        "another hen-local-init bootstrap is already running; lock: {}",
                        path.display()
                    );
                }
                fs::remove_file(path)
                    .with_context(|| format!("remove stale bootstrap lock {}", path.display()))?;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("create bootstrap lock {}", path.display()));
            }
        }
    }

    bail!("could not acquire bootstrap lock {}", path.display())
}

#[derive(Serialize, Deserialize)]
struct ModelCompletionMarker {
    repo_id: String,
    bootstrap_version: u32,
}

fn model_completion_marker_path(dir: &Path) -> PathBuf {
    dir.join(MODEL_COMPLETION_MARKER)
}

fn model_completion_marker_valid(dir: &Path, repo_id: &str) -> bool {
    let marker_path = model_completion_marker_path(dir);
    let Ok(contents) = fs::read_to_string(marker_path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<ModelCompletionMarker>(&contents) else {
        return false;
    };
    marker.repo_id == repo_id
}

fn write_model_completion_marker(dir: &Path, repo_id: &str) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("mkdir {:?}", dir))?;
    let marker = ModelCompletionMarker {
        repo_id: repo_id.to_string(),
        bootstrap_version: BOOTSTRAP_VERSION,
    };
    let marker_path = model_completion_marker_path(dir);
    let body = serde_json::to_string_pretty(&marker)?;
    fs::write(&marker_path, body)
        .with_context(|| format!("write model completion marker {:?}", marker_path))
}

fn ensure_model_dir_ready(
    dir: &Path,
    repo_id: &str,
    ready_check: impl Fn(&Path) -> bool,
) -> Result<bool> {
    if ready_check(dir) {
        if !model_completion_marker_valid(dir, repo_id) {
            eprintln!(
                "[hen-local-init] complete model found without a valid marker, writing {}",
                dir.display()
            );
            write_model_completion_marker(dir, repo_id)?;
        }
        return Ok(true);
    }

    if model_completion_marker_valid(dir, repo_id) {
        eprintln!(
            "[hen-local-init] marker present but model is incomplete, clearing {}",
            dir.display()
        );
        if dir.exists() {
            fs::remove_dir_all(dir)
                .with_context(|| format!("remove incomplete model dir {}", dir.display()))?;
        }
        return Ok(false);
    }

    if dir.exists() {
        eprintln!(
            "[hen-local-init] model directory without a valid completion marker, removing {}",
            dir.display()
        );
        fs::remove_dir_all(dir)
            .with_context(|| format!("remove incomplete model dir {}", dir.display()))?;
    }
    Ok(false)
}

// ── Model readiness checks ────────────────────────────────────────────────────

fn asr_model_ready(dir: &Path) -> bool {
    nonempty_file(&dir.join("config.json"))
        && nonempty_file(&dir.join("tokenizer_config.json"))
        && (nonempty_file(&dir.join("tokenizer.json"))
            || (nonempty_file(&dir.join("vocab.json")) && nonempty_file(&dir.join("merges.txt"))))
        && model_weights_ready(dir)
}

fn qwen35_translation_model_ready(dir: &Path) -> bool {
    nonempty_file(&dir.join("config.json"))
        && nonempty_file(&dir.join("tokenizer.json"))
        && nonempty_file(&dir.join("tokenizer_config.json"))
        && model_weights_ready(dir)
}

fn nonempty_file(path: &Path) -> bool {
    file_exists(path) && file_len(path) > 0
}

fn model_weights_ready(dir: &Path) -> bool {
    if nonempty_file(&dir.join("model.safetensors")) {
        return true;
    }
    let Ok(contents) = fs::read(dir.join("model.safetensors.index.json")) else {
        return false;
    };
    let Ok(index) = serde_json::from_slice::<serde_json::Value>(&contents) else {
        return false;
    };
    let Some(map) = index
        .get("weight_map")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    !map.is_empty()
        && map.values().all(|name| {
            name.as_str()
                .and_then(|name| model_file_destination(dir, name).ok())
                .is_some_and(|path| nonempty_file(&path))
        })
}

// ── Model download providers ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelProvider {
    Auto,
    HuggingFace,
    ModelScope,
}

impl ModelProvider {
    fn from_env_value(value: Option<&str>) -> Result<Self> {
        let normalized = value.unwrap_or("").trim().to_ascii_lowercase();
        match normalized.as_str() {
            "" | "auto" => Ok(Self::Auto),
            "modelscope" | "ms" => Ok(Self::ModelScope),
            "huggingface" | "hf" => Ok(Self::HuggingFace),
            other => bail!(
                "unsupported MOXIN_MODEL_PROVIDER={other:?}; expected auto, modelscope, or huggingface"
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct DownloadProvider {
    kind: ModelProvider,
    endpoint: String,
}

impl DownloadProvider {
    fn providers_from_env() -> Result<Vec<Self>> {
        let preference =
            ModelProvider::from_env_value(env::var("MOXIN_MODEL_PROVIDER").ok().as_deref())?;
        let modelscope = Self::modelscope(endpoint_from_env(
            "MOXIN_MODELSCOPE_ENDPOINT",
            DEFAULT_MODELSCOPE_ENDPOINT,
        ));
        let huggingface = Self::huggingface(endpoint_from_env("HF_ENDPOINT", DEFAULT_HF_ENDPOINT));

        match preference {
            ModelProvider::Auto => {
                let probe_client = build_http_client(Duration::from_secs(5))?;
                let modelscope_probe = probe_provider(&probe_client, &modelscope);
                let huggingface_probe = probe_provider(&probe_client, &huggingface);
                if let Err(err) = &modelscope_probe {
                    eprintln!("[hen-local-init] ModelScope probe failed: {err:#}");
                }
                if let Err(err) = &huggingface_probe {
                    eprintln!("[hen-local-init] Hugging Face probe failed: {err:#}");
                }
                let order =
                    auto_provider_order(modelscope_probe.is_ok(), huggingface_probe.is_ok())?;
                Ok(order
                    .into_iter()
                    .map(|kind| provider_for_kind(kind, &modelscope, &huggingface))
                    .collect())
            }
            ModelProvider::HuggingFace => Ok(vec![huggingface]),
            ModelProvider::ModelScope => Ok(vec![modelscope]),
        }
    }

    fn huggingface(endpoint: String) -> Self {
        Self {
            kind: ModelProvider::HuggingFace,
            endpoint: normalize_endpoint(&endpoint),
        }
    }

    fn modelscope(endpoint: String) -> Self {
        Self {
            kind: ModelProvider::ModelScope,
            endpoint: normalize_endpoint(&endpoint),
        }
    }

    fn name(&self) -> &'static str {
        match self.kind {
            ModelProvider::Auto => "auto",
            ModelProvider::HuggingFace => "huggingface",
            ModelProvider::ModelScope => "modelscope",
        }
    }

    fn repo_file_url(&self, repo_id: &str, filename: &str) -> String {
        match self.kind {
            ModelProvider::Auto => unreachable!("auto provider must be resolved before download"),
            ModelProvider::HuggingFace => {
                format!("{}/{}/resolve/main/{}", self.endpoint, repo_id, filename)
            }
            ModelProvider::ModelScope => {
                format!(
                    "{}/models/{}/resolve/master/{}",
                    self.endpoint, repo_id, filename
                )
            }
        }
    }

    fn huggingface_repo_info_url(&self, repo_id: &str) -> String {
        format!("{}/api/models/{}", self.endpoint, repo_id)
    }
}

fn provider_for_kind(
    kind: ModelProvider,
    modelscope: &DownloadProvider,
    huggingface: &DownloadProvider,
) -> DownloadProvider {
    match kind {
        ModelProvider::ModelScope => modelscope.clone(),
        ModelProvider::HuggingFace => huggingface.clone(),
        ModelProvider::Auto => unreachable!("auto provider must be resolved before download"),
    }
}

fn auto_provider_order(
    modelscope_reachable: bool,
    huggingface_reachable: bool,
) -> Result<Vec<ModelProvider>> {
    match (modelscope_reachable, huggingface_reachable) {
        (true, true) => Ok(vec![ModelProvider::ModelScope, ModelProvider::HuggingFace]),
        (true, false) => Ok(vec![ModelProvider::ModelScope]),
        (false, true) => Ok(vec![ModelProvider::HuggingFace]),
        (false, false) => bail!("could not reach ModelScope or Hugging Face model endpoints"),
    }
}

fn probe_provider(client: &reqwest::blocking::Client, provider: &DownloadProvider) -> Result<()> {
    let url = provider.repo_file_url(PROVIDER_PROBE_REPO, PROVIDER_PROBE_FILE);
    let resp = client
        .head(&url)
        .send()
        .with_context(|| format!("HEAD {}", url))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        bail!("HTTP {} probing {}", resp.status(), provider.name())
    }
}

fn run_with_provider_fallback(
    providers: &[DownloadProvider],
    operation_name: &str,
    mut run: impl FnMut(usize, &DownloadProvider) -> Result<()>,
) -> Result<()> {
    if providers.is_empty() {
        bail!("no model download providers configured for {operation_name}");
    }

    let mut failures = Vec::new();
    for (attempt, provider) in providers.iter().enumerate() {
        match run(attempt, provider) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!(
                    "[hen-local-init] {} failed via {}: {:#}",
                    operation_name,
                    provider.name(),
                    err
                );
                failures.push(format!("{}: {:#}", provider.name(), err));
            }
        }
    }

    bail!(
        "{} failed using all reachable providers: {}",
        operation_name,
        failures.join(" | ")
    )
}

fn endpoint_from_env(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn build_http_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(HTTP_USER_AGENT)
        .build()
        .context("Build HTTP client")
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}

fn modelscope_manifest_files(repo_id: &str) -> Result<&'static [&'static str]> {
    match repo_id {
        "mlx-community/Qwen3-ASR-1.7B-8bit" => Ok(ASR_MODEL_FILES),
        "mlx-community/Hy-MT2-1.8B-4bit" => Ok(QWEN35_TRANSLATOR_MODEL_FILES),
        _ => bail!("no built-in ModelScope manifest for {}", repo_id),
    }
}

#[derive(Deserialize)]
struct Sibling {
    rfilename: String,
}

#[derive(Deserialize)]
struct RepoInfo {
    siblings: Vec<Sibling>,
}

/// Fetch the list of files in a model repo.
fn list_repo_files(
    client: &reqwest::blocking::Client,
    provider: &DownloadProvider,
    repo_id: &str,
) -> Result<Vec<String>> {
    match provider.kind {
        ModelProvider::Auto => unreachable!("auto provider must be resolved before listing files"),
        ModelProvider::HuggingFace => {
            let url = provider.huggingface_repo_info_url(repo_id);
            let info: RepoInfo = client
                .get(&url)
                .send()
                .with_context(|| format!("GET {}", url))?
                .error_for_status()
                .with_context(|| format!("HTTP error listing {}", repo_id))?
                .json()
                .context("Parse repo info JSON")?;
            Ok(info.siblings.into_iter().map(|s| s.rfilename).collect())
        }
        ModelProvider::ModelScope => Ok(modelscope_manifest_files(repo_id)?
            .iter()
            .map(|filename| (*filename).to_string())
            .collect()),
    }
}

/// Download a single file from a model repo to `dest`.
///
/// Uses `Range` requests for resume: if `dest` already exists and is non-empty,
/// only the remaining bytes are fetched and appended.
/// Returns the number of bytes actually written in this call.
fn download_file(
    client: &reqwest::blocking::Client,
    provider: &DownloadProvider,
    repo_id: &str,
    filename: &str,
    dest: &Path,
    mut on_progress: impl FnMut(u64, f64),
) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {:?}", dest.parent()))?;
    }

    let existing_bytes = dest.metadata().map(|m| m.len()).unwrap_or(0);
    let url = provider.repo_file_url(repo_id, filename);

    let mut req = client.get(&url);
    if existing_bytes > 0 {
        req = req.header("Range", format!("bytes={}-", existing_bytes));
    }

    let resp = req.send().with_context(|| format!("GET {}", url))?;
    let status = resp.status();

    // 416 Range Not Satisfiable = file is already complete
    if status.as_u16() == 416 {
        return Ok(0);
    }

    if !status.is_success() {
        bail!("HTTP {} downloading {}/{}", status, repo_id, filename);
    }

    let is_partial = status.as_u16() == 206;
    let mut file = if is_partial {
        OpenOptions::new()
            .append(true)
            .open(dest)
            .with_context(|| format!("open for append {:?}", dest))?
    } else {
        File::create(dest).with_context(|| format!("create {:?}", dest))?
    };

    let mut resp = resp;
    let mut downloaded: u64 = 0;
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut buf = [0_u8; 256 * 1024];

    loop {
        let n = resp
            .read(&mut buf)
            .with_context(|| format!("read body of {}/{}", repo_id, filename))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .with_context(|| format!("write {:?}", dest))?;
        downloaded += n as u64;

        let should_report = last_report.elapsed() >= Duration::from_millis(250);
        if should_report {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let speed = downloaded as f64 / elapsed;
            on_progress(downloaded, speed);
            last_report = Instant::now();
        }
    }

    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    let speed = downloaded as f64 / elapsed;
    on_progress(downloaded, speed);
    Ok(downloaded)
}

fn should_retry_download_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}").to_lowercase();
    if msg.contains("http 4") {
        return false;
    }
    [
        "body",
        "connection",
        "end of file",
        "timed out",
        "timeout",
        "request",
    ]
    .iter()
    .any(|needle| msg.contains(needle))
}

fn download_file_with_retries(
    client: &reqwest::blocking::Client,
    provider: &DownloadProvider,
    repo_id: &str,
    filename: &str,
    dest: &Path,
    mut on_progress: impl FnMut(u64, f64),
) -> Result<u64> {
    let initial_len = file_len(dest);

    for attempt in 1..=MAX_DOWNLOAD_FILE_ATTEMPTS {
        let attempt_base = file_len(dest).saturating_sub(initial_len);
        match download_file(
            client,
            provider,
            repo_id,
            filename,
            dest,
            |written_so_far, speed_bps| {
                on_progress(attempt_base + written_so_far, speed_bps);
            },
        ) {
            Ok(_) => return Ok(file_len(dest).saturating_sub(initial_len)),
            Err(err)
                if attempt < MAX_DOWNLOAD_FILE_ATTEMPTS && should_retry_download_error(&err) =>
            {
                eprintln!(
                    "[hen-local-init] retrying {}/{} via {} after transient download error (attempt {}/{}): {:#}",
                    repo_id,
                    filename,
                    provider.name(),
                    attempt + 1,
                    MAX_DOWNLOAD_FILE_ATTEMPTS,
                    err
                );
                std::thread::sleep(Duration::from_secs(attempt as u64));
            }
            Err(err) => return Err(err),
        }
    }

    bail!("download retry loop exhausted for {}/{}", repo_id, filename)
}

/// Download all files in a model repo to `target_dir`.
///
/// Already-present files are skipped. The `state_file` is updated per-file
/// so the UI progress bar reflects real download activity.
/// `bytes_done` is updated after each file; `total_bytes` is used for pct.
fn download_repo(
    client: &reqwest::blocking::Client,
    provider: &DownloadProvider,
    repo_id: &str,
    target_dir: &Path,
    state_file: Option<&Path>,
    step: usize,
    total_steps: usize,
    bytes_done: &mut u64,
    total_bytes: u64,
) -> Result<()> {
    fs::create_dir_all(target_dir).with_context(|| format!("mkdir {:?}", target_dir))?;

    let short_name = repo_id.split('/').last().unwrap_or(repo_id);
    eprintln!(
        "[hen-local-init] listing files for {} via {}",
        repo_id,
        provider.name()
    );

    let files = list_repo_files(client, provider, repo_id)
        .with_context(|| format!("list files for {}", repo_id))?;

    eprintln!("[hen-local-init] {} file(s) in {}", files.len(), repo_id);

    for (i, filename) in files.iter().enumerate() {
        let dest = model_file_destination(target_dir, filename)?;
        if dest.exists() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
            eprintln!("[hen-local-init] skip (exists): {}", filename);
            continue;
        }
        eprintln!(
            "[hen-local-init] downloading [{}/{}]: {}",
            i + 1,
            files.len(),
            filename
        );
        write_state(
            state_file,
            step,
            total_steps,
            &format!("Downloading {}", short_name),
            &format!("[{}/{}] {}", i + 1, files.len(), filename),
            *bytes_done,
            total_bytes,
        );
        let written = download_file_with_retries(
            client,
            provider,
            repo_id,
            filename,
            &dest,
            |written_so_far, speed_bps| {
                write_state(
                    state_file,
                    step,
                    total_steps,
                    &format!("Downloading {}", short_name),
                    &format!(
                        "[{}/{}] {} • {}",
                        i + 1,
                        files.len(),
                        filename,
                        format_bytes_per_second(speed_bps),
                    ),
                    *bytes_done + written_so_far,
                    total_bytes,
                );
            },
        )
        .with_context(|| format!("download {}/{}", repo_id, filename))?;
        *bytes_done += written;
    }
    Ok(())
}

fn download_model_with_provider_fallback(
    client: &reqwest::blocking::Client,
    owned: &OwnedModelTargets,
    providers: &[DownloadProvider],
    repo_id: &str,
    target_dir: &Path,
    state_file: Option<&Path>,
    step: usize,
    total_steps: usize,
    bytes_done: &mut u64,
    total_bytes: u64,
    ready_check: impl Fn(&Path) -> bool + Copy,
    incomplete_message: &str,
) -> Result<()> {
    let short_name = repo_id.split('/').last().unwrap_or(repo_id);
    let bytes_before_model = *bytes_done;

    run_with_provider_fallback(providers, repo_id, |attempt, provider| {
        // Recheck before every repair/fallback; never feed a borrowed cache
        // or a newly replaced symlink to remove_dir_all or download_repo.
        owned.check(target_dir)?;
        if attempt > 0 {
            *bytes_done = bytes_before_model;
            if target_dir.exists() {
                fs::remove_dir_all(target_dir)
                    .with_context(|| format!("remove failed model dir {}", target_dir.display()))?;
            }
            write_state(
                state_file,
                step,
                total_steps,
                &format!("Retrying {}", short_name),
                &format!("Switching to {}", provider.name()),
                *bytes_done,
                total_bytes,
            );
        }

        download_repo(
            client,
            provider,
            repo_id,
            target_dir,
            state_file,
            step,
            total_steps,
            bytes_done,
            total_bytes,
        )?;

        if !ready_check(target_dir) {
            bail!("{}: {}", incomplete_message, target_dir.display());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hen-local-init-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn owned_targets_accept_only_expected_directories_without_creating_them() {
        let home = unique_temp_dir("owned-model-home");
        let root = home.join(OWNED_MODELS_RELATIVE);
        let asr = root.join(ASR_DIRECTORY);
        let translator = root.join(TRANSLATOR_DIRECTORY);
        let owned = OwnedModelTargets::validate(&home, &asr, &translator).unwrap();
        assert_eq!(
            owned.asr,
            home.canonicalize()
                .unwrap()
                .join(OWNED_MODELS_RELATIVE)
                .join(ASR_DIRECTORY)
        );
        assert!(!root.exists());
        fs::create_dir_all(&asr).unwrap();
        fs::write(asr.join("model.safetensors"), b"existing-owned-weights").unwrap();
        owned.check(&owned.asr).unwrap();
        assert_eq!(
            fs::read(asr.join("model.safetensors")).unwrap(),
            b"existing-owned-weights"
        );
        assert!(!asr.join(MODEL_COMPLETION_MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn owned_targets_reject_external_root_parent_same_and_nested_paths() {
        let home = unique_temp_dir("reject-model-paths");
        let root = home.join(OWNED_MODELS_RELATIVE);
        let asr = root.join(ASR_DIRECTORY);
        let translator = root.join(TRANSLATOR_DIRECTORY);
        let shared = home.join(".OminiX/models/shared");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("keep"), b"translator-cache").unwrap();
        let error = OwnedModelTargets::validate(&home, &shared, &translator)
            .err()
            .unwrap();
        assert!(error.to_string().contains("read-only inference"));
        for bad_asr in [
            root.clone(),
            home.clone(),
            PathBuf::from("/"),
            PathBuf::from("relative"),
            root.join("../escape"),
            root.join("other-model"),
        ] {
            assert!(OwnedModelTargets::validate(&home, &bad_asr, &translator).is_err());
        }
        assert!(OwnedModelTargets::validate(&home, &asr, &asr).is_err());
        assert!(OwnedModelTargets::validate(&home, &asr, &asr.join("nested")).is_err());
        assert!(OwnedModelTargets::validate(&home, &translator, &asr).is_err());
        assert_eq!(fs::read(shared.join("keep")).unwrap(), b"translator-cache");
        assert!(!root.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owned_targets_reject_model_symlink_to_shared_cache() {
        use std::os::unix::fs::symlink;
        let home = unique_temp_dir("model-symlink");
        let root = home.join(OWNED_MODELS_RELATIVE);
        let shared = home.join(".OminiX/models/shared");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("keep"), b"shared-survives").unwrap();
        symlink(&shared, root.join(ASR_DIRECTORY)).unwrap();
        assert!(OwnedModelTargets::validate(
            &home,
            &root.join(ASR_DIRECTORY),
            &root.join(TRANSLATOR_DIRECTORY)
        )
        .is_err());
        assert_eq!(fs::read(shared.join("keep")).unwrap(), b"shared-survives");
        assert!(!shared.join(MODEL_COMPLETION_MARKER).exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owned_targets_reject_symlinked_model_root_and_dangling_links() {
        use std::os::unix::fs::symlink;
        let home = unique_temp_dir("root-symlink");
        let root = home.join(OWNED_MODELS_RELATIVE);
        let external = home.join("external");
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("keep"), b"untouched").unwrap();
        symlink(&external, &root).unwrap();
        assert!(OwnedModelTargets::validate(
            &home,
            &root.join(ASR_DIRECTORY),
            &root.join(TRANSLATOR_DIRECTORY)
        )
        .is_err());
        assert_eq!(fs::read(external.join("keep")).unwrap(), b"untouched");
        fs::remove_file(&root).unwrap();
        fs::create_dir(&root).unwrap();
        symlink(home.join("missing-shared"), root.join(TRANSLATOR_DIRECTORY)).unwrap();
        assert!(OwnedModelTargets::validate(
            &home,
            &root.join(ASR_DIRECTORY),
            &root.join(TRANSLATOR_DIRECTORY)
        )
        .is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_rechecks_reject_nested_file_and_late_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let home = unique_temp_dir("nested-symlink");
        let root = home.join(OWNED_MODELS_RELATIVE);
        let owned = OwnedModelTargets::validate(
            &home,
            &root.join(ASR_DIRECTORY),
            &root.join(TRANSLATOR_DIRECTORY),
        )
        .unwrap();
        fs::create_dir_all(&owned.asr).unwrap();
        let outside_file = home.join("private-file");
        fs::write(&outside_file, b"must-not-change").unwrap();
        symlink(&outside_file, owned.asr.join(MODEL_COMPLETION_MARKER)).unwrap();
        assert!(owned.check(&owned.asr).is_err());
        assert!(model_file_destination(&owned.asr, MODEL_COMPLETION_MARKER).is_err());
        assert_eq!(fs::read(&outside_file).unwrap(), b"must-not-change");
        fs::remove_file(owned.asr.join(MODEL_COMPLETION_MARKER)).unwrap();
        let external_dir = home.join("shared");
        fs::create_dir(&external_dir).unwrap();
        symlink(&external_dir, owned.asr.join("nested")).unwrap();
        assert!(owned.check(&owned.asr).is_err());
        fs::remove_file(owned.asr.join("nested")).unwrap();
        fs::remove_dir(&owned.asr).unwrap();
        symlink(&external_dir, &owned.asr).unwrap();
        assert!(owned.check(&owned.asr).is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn repository_filenames_cannot_escape_model_directory() {
        let target = unique_temp_dir("repo-files");
        for filename in ["", "/tmp/escape", "../escape", "nested/../../escape", "."] {
            assert!(model_file_destination(&target, filename).is_err());
        }
        assert_eq!(
            model_file_destination(&target, "nested/config.json").unwrap(),
            target.join("nested/config.json")
        );
        assert!(fs::read_dir(&target).unwrap().next().is_none());
        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn component_is_explicitly_core_only() {
        assert!(validate_component(None).is_ok());
        assert!(validate_component(Some("core")).is_ok());
        for component in ["asr", "translator", "", "../core"] {
            assert!(validate_component(Some(component)).is_err());
        }
    }

    #[test]
    fn ready_check_requires_expected_translator_files() {
        let dir = unique_temp_dir("translator-ready");
        fs::write(dir.join("config.json"), b"{}").unwrap();
        fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        fs::write(dir.join("tokenizer_config.json"), b"{}").unwrap();
        File::create(dir.join("model.safetensors")).unwrap();

        assert!(!qwen35_translation_model_ready(&dir));
        fs::write(dir.join("model.safetensors"), b"weights").unwrap();
        assert!(qwen35_translation_model_ready(&dir));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn asr_readiness_requires_tokenizer_and_actual_weights_not_just_config() {
        let dir = unique_temp_dir("asr-ready");
        fs::write(dir.join("config.json"), b"{}").unwrap();
        assert!(!asr_model_ready(&dir));
        fs::write(dir.join("tokenizer_config.json"), b"{}").unwrap();
        fs::write(dir.join("vocab.json"), b"{}").unwrap();
        fs::write(dir.join("merges.txt"), b"merge").unwrap();
        assert!(!asr_model_ready(&dir));
        fs::write(dir.join("model.safetensors"), b"weights").unwrap();
        assert!(asr_model_ready(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn indexed_weights_require_every_nonempty_local_shard() {
        let dir = unique_temp_dir("indexed-weights");
        fs::write(
            dir.join("model.safetensors.index.json"),
            br#"{"weight_map":{"a":"one.safetensors","b":"two.safetensors"}}"#,
        )
        .unwrap();
        fs::write(dir.join("one.safetensors"), b"one").unwrap();
        assert!(!model_weights_ready(&dir));
        File::create(dir.join("two.safetensors")).unwrap();
        assert!(!model_weights_ready(&dir));
        fs::write(dir.join("two.safetensors"), b"two").unwrap();
        assert!(model_weights_ready(&dir));
        fs::write(
            dir.join("model.safetensors.index.json"),
            br#"{"weight_map":{"a":"../outside.safetensors"}}"#,
        )
        .unwrap();
        assert!(!model_weights_ready(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn formats_download_speed_human_readably() {
        assert_eq!(format_bytes_per_second(512.0), "512 B/s");
        assert_eq!(format_bytes_per_second(2048.0), "2.0 KB/s");
        assert_eq!(format_bytes_per_second(5.5 * 1024.0 * 1024.0), "5.5 MB/s");
    }

    #[test]
    fn model_provider_defaults_to_auto_and_accepts_aliases() {
        assert_eq!(
            ModelProvider::from_env_value(None).unwrap(),
            ModelProvider::Auto
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("  ")).unwrap(),
            ModelProvider::Auto
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("auto")).unwrap(),
            ModelProvider::Auto
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("modelscope")).unwrap(),
            ModelProvider::ModelScope
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("ms")).unwrap(),
            ModelProvider::ModelScope
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("huggingface")).unwrap(),
            ModelProvider::HuggingFace
        );
        assert_eq!(
            ModelProvider::from_env_value(Some("hf")).unwrap(),
            ModelProvider::HuggingFace
        );
    }

    #[test]
    fn model_provider_rejects_unknown_values() {
        let err = ModelProvider::from_env_value(Some("unknown")).unwrap_err();
        assert!(err.to_string().contains("MOXIN_MODEL_PROVIDER"));
    }

    #[test]
    fn auto_provider_order_prefers_modelscope_with_huggingface_fallback() {
        assert_eq!(
            auto_provider_order(true, true).unwrap(),
            vec![ModelProvider::ModelScope, ModelProvider::HuggingFace]
        );
        assert_eq!(
            auto_provider_order(true, false).unwrap(),
            vec![ModelProvider::ModelScope]
        );
        assert_eq!(
            auto_provider_order(false, true).unwrap(),
            vec![ModelProvider::HuggingFace]
        );
    }

    #[test]
    fn auto_provider_order_errors_when_no_provider_is_reachable() {
        let err = auto_provider_order(false, false).unwrap_err();
        assert!(err
            .to_string()
            .contains("could not reach ModelScope or Hugging Face"));
    }

    #[test]
    fn download_provider_builds_provider_specific_file_urls() {
        let hf = DownloadProvider::huggingface("https://hf.example/".to_string());
        let modelscope = DownloadProvider::modelscope("https://modelscope.example/".to_string());

        assert_eq!(
            hf.repo_file_url("owner/repo", "nested/file.txt"),
            "https://hf.example/owner/repo/resolve/main/nested/file.txt"
        );
        assert_eq!(
            modelscope.repo_file_url("owner/repo", "nested/file.txt"),
            "https://modelscope.example/models/owner/repo/resolve/master/nested/file.txt"
        );
    }

    #[test]
    fn modelscope_manifest_uses_fixed_file_lists() {
        let asr_files = modelscope_manifest_files("mlx-community/Qwen3-ASR-1.7B-8bit").unwrap();
        assert_eq!(
            asr_files,
            &[
                ".gitattributes",
                "README.md",
                "chat_template.json",
                "config.json",
                "generation_config.json",
                "merges.txt",
                "model.safetensors",
                "model.safetensors.index.json",
                "preprocessor_config.json",
                "tokenizer_config.json",
                "vocab.json",
            ]
        );

        let translator_files =
            modelscope_manifest_files("mlx-community/Hy-MT2-1.8B-4bit").unwrap();
        assert!(translator_files.contains(&"tokenizer.json"));
    }

    #[test]
    fn modelscope_manifest_rejects_unknown_repos() {
        let err = modelscope_manifest_files("custom/repo").unwrap_err();
        assert!(err
            .to_string()
            .contains("no built-in ModelScope manifest for custom/repo"));
    }

    #[test]
    fn http_client_sends_forum_user_agent() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                headers.push(line);
            }
            tx.send(headers).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        let client = build_http_client(Duration::from_secs(1)).unwrap();
        client
            .get(format!("http://{addr}/probe"))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap();

        let headers = rx.recv().unwrap().join("");
        let headers_lower = headers.to_ascii_lowercase();
        assert!(
            headers_lower.contains("user-agent: aivisionforum/hen-local-init"),
            "request headers did not contain the expected User-Agent:\n{headers}"
        );
    }

    #[test]
    fn download_file_retries_short_body_with_range_resume() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let mut first_reader = BufReader::new(first.try_clone().unwrap());
            loop {
                let mut line = String::new();
                first_reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
                .unwrap();

            let (mut second, _) = listener.accept().unwrap();
            let mut second_reader = BufReader::new(second.try_clone().unwrap());
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                second_reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                headers.push(line);
            }
            tx.send(headers.join("")).unwrap();
            second
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 7\r\nContent-Range: bytes 3-9/10\r\n\r\ndefghij",
                )
                .unwrap();
        });

        let dir = unique_temp_dir("range-resume");
        let dest = dir.join("file.bin");
        let provider = DownloadProvider::modelscope(format!("http://{addr}"));
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let written = download_file_with_retries(
            &client,
            &provider,
            "owner/repo",
            "file.bin",
            &dest,
            |_, _| {},
        )
        .unwrap();

        assert_eq!(written, 10);
        assert_eq!(fs::read(&dest).unwrap(), b"abcdefghij");
        let second_headers = rx.recv().unwrap();
        let second_headers_lower = second_headers.to_ascii_lowercase();
        assert!(
            second_headers_lower.contains("range: bytes=3-"),
            "second request did not resume from byte 3:\n{second_headers}"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bootstrap_lock_blocks_active_second_instance() {
        let dir = unique_temp_dir("active-lock");
        let lock_path = dir.join("bootstrap.lock");
        let lock = acquire_bootstrap_lock(&lock_path).unwrap();

        let err = acquire_bootstrap_lock(&lock_path).unwrap_err();
        assert!(err.to_string().contains("already running"));

        drop(lock);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bootstrap_lock_replaces_stale_pid() {
        let dir = unique_temp_dir("stale-lock");
        let lock_path = dir.join("bootstrap.lock");
        fs::write(&lock_path, "pid=999999\n").unwrap();

        let lock = acquire_bootstrap_lock(&lock_path).unwrap();
        assert!(lock_path.exists());
        drop(lock);
        assert!(!lock_path.exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn provider_fallback_retries_next_provider_after_error() {
        let providers = vec![
            DownloadProvider::modelscope("https://modelscope.example/".to_string()),
            DownloadProvider::huggingface("https://hf.example/".to_string()),
        ];
        let mut attempts = Vec::new();

        run_with_provider_fallback(&providers, "test operation", |_, provider| {
            attempts.push(provider.kind);
            if provider.kind == ModelProvider::ModelScope {
                bail!("modelscope failed");
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(
            attempts,
            vec![ModelProvider::ModelScope, ModelProvider::HuggingFace]
        );
    }

    #[test]
    fn ensure_model_dir_ready_migrates_complete_unmarked_model_dir() {
        let dir = unique_temp_dir("unmarked-migrate");
        fs::write(dir.join("config.json"), b"{}").unwrap();
        fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        fs::write(dir.join("tokenizer_config.json"), b"{}").unwrap();
        fs::write(dir.join("model.safetensors"), b"weights").unwrap();

        let ready = ensure_model_dir_ready(
            &dir,
            "mlx-community/Hy-MT2-1.8B-4bit",
            qwen35_translation_model_ready,
        )
        .unwrap();

        assert!(ready);
        assert!(model_completion_marker_valid(
            &dir,
            "mlx-community/Hy-MT2-1.8B-4bit"
        ));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ensure_model_dir_ready_removes_incomplete_model_dir() {
        let dir = unique_temp_dir("incomplete-remove");
        fs::write(dir.join("config.json"), b"{}").unwrap();
        File::create(dir.join("model.safetensors")).unwrap();

        let ready = ensure_model_dir_ready(
            &dir,
            "mlx-community/Hy-MT2-1.8B-4bit",
            qwen35_translation_model_ready,
        )
        .unwrap();

        assert!(!ready);
        assert!(!dir.exists());
    }
}

// ── Configuration ─────────────────────────────────────────────────────────────

struct Config {
    state_file: Option<PathBuf>,
    asr_dir: PathBuf,
    asr_repo: String,
    qwen35_translator_dir: PathBuf,
    qwen35_translator_repo: String,
}

fn resolve_config(home: &Path) -> Config {
    let owned_root = home.join(OWNED_MODELS_RELATIVE);
    Config {
        state_file: env::var("FORUM_AGENT_BOOTSTRAP_STATE_PATH")
            .ok()
            .map(PathBuf::from)
            .or_else(|| Some(home.join("Library/Logs/AI Vision Forum/bootstrap_state.txt"))),
        asr_dir: env::var("QWEN3_ASR_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| owned_root.join(ASR_DIRECTORY)),
        asr_repo: env::var("QWEN3_ASR_REPO")
            .unwrap_or_else(|_| "mlx-community/Qwen3-ASR-1.7B-8bit".to_string()),
        qwen35_translator_dir: env::var("QWEN35_TRANSLATOR_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| owned_root.join(TRANSLATOR_DIRECTORY)),
        qwen35_translator_repo: env::var("QWEN35_TRANSLATOR_REPO")
            .unwrap_or_else(|_| "mlx-community/Hy-MT2-1.8B-4bit".to_string()),
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // No lock, state write, directory repair or provider/network work precedes
    // this guard. Invalid external cache paths therefore have no side effects.
    validate_component(env::var("FORUM_AGENT_MODEL_COMPONENT").ok().as_deref())?;
    let home =
        dirs::home_dir().context("Cannot determine home directory for Forum model ownership")?;
    let mut cfg = resolve_config(&home);
    let owned = OwnedModelTargets::validate(&home, &cfg.asr_dir, &cfg.qwen35_translator_dir)?;
    cfg.asr_dir = owned.asr.clone();
    cfg.qwen35_translator_dir = owned.translator.clone();
    // The installation lock follows the model root, not a per-window log path.
    let lock_path = owned.root.join(BOOTSTRAP_LOCK_FILE);
    let _bootstrap_lock = acquire_bootstrap_lock(&lock_path)?;
    let state_file = cfg.state_file.as_deref();
    let providers = DownloadProvider::providers_from_env()?;
    let provider_names = providers
        .iter()
        .map(|provider| provider.name())
        .collect::<Vec<_>>()
        .join(" -> ");
    eprintln!("[hen-local-init] model provider order: {}", provider_names);

    // 2 potential downloads: Hy-MT2 translator and ASR.
    let total: usize = 2;
    owned.check(&cfg.qwen35_translator_dir)?;
    let translator_ready = ensure_model_dir_ready(
        &cfg.qwen35_translator_dir,
        &cfg.qwen35_translator_repo,
        qwen35_translation_model_ready,
    )?;
    owned.check(&cfg.asr_dir)?;
    let asr_ready = ensure_model_dir_ready(&cfg.asr_dir, &cfg.asr_repo, asr_model_ready)?;

    let mut bytes_done: u64 = 0;
    if translator_ready {
        bytes_done += BYTES_TRANSLATOR;
    }
    if asr_ready {
        bytes_done += BYTES_ASR;
    }

    write_state(
        state_file,
        0,
        total,
        "Check Models",
        "Verifying model files",
        bytes_done,
        TOTAL_BYTES,
    );

    let client = build_http_client(Duration::from_secs(3600))?;

    // ── Step 1: Hy-MT2 translator (required) ─────────────────────────────────
    if translator_ready {
        eprintln!("[hen-local-init] Hy-MT2 translator model already ready, skipping");
        write_state(
            state_file,
            1,
            total,
            "Hy-MT2 Translator",
            "Already present",
            bytes_done,
            TOTAL_BYTES,
        );
    } else {
        write_state(
            state_file,
            1,
            total,
            "Downloading Hy-MT2 Translator",
            "Starting...",
            bytes_done,
            TOTAL_BYTES,
        );
        download_model_with_provider_fallback(
            &client,
            &owned,
            &providers,
            &cfg.qwen35_translator_repo,
            &cfg.qwen35_translator_dir,
            state_file,
            1,
            total,
            &mut bytes_done,
            TOTAL_BYTES,
            qwen35_translation_model_ready,
            "Hy-MT2 translator model incomplete after download",
        )
        .with_context(|| "Hy-MT2 translator download failed")?;
        owned.check(&cfg.qwen35_translator_dir)?;
        write_model_completion_marker(&cfg.qwen35_translator_dir, &cfg.qwen35_translator_repo)?;
        eprintln!("[hen-local-init] Hy-MT2 translator download complete");
    }

    // ── Step 2: ASR (required) ─────────────────────────────────────────────────
    if asr_ready {
        eprintln!("[hen-local-init] ASR model already ready, skipping");
        write_state(
            state_file,
            2,
            total,
            "ASR Model",
            "Already present",
            bytes_done,
            TOTAL_BYTES,
        );
    } else {
        write_state(
            state_file,
            2,
            total,
            "Downloading ASR Model",
            "Starting...",
            bytes_done,
            TOTAL_BYTES,
        );
        download_model_with_provider_fallback(
            &client,
            &owned,
            &providers,
            &cfg.asr_repo,
            &cfg.asr_dir,
            state_file,
            2,
            total,
            &mut bytes_done,
            TOTAL_BYTES,
            asr_model_ready,
            "ASR model incomplete after download",
        )
        .with_context(|| "ASR model download failed")?;
        owned.check(&cfg.asr_dir)?;
        write_model_completion_marker(&cfg.asr_dir, &cfg.asr_repo)?;
        eprintln!("[hen-local-init] ASR download complete");
    }

    write_state(
        state_file,
        total,
        total,
        "Done",
        "All models ready",
        TOTAL_BYTES,
        TOTAL_BYTES,
    );
    println!("[hen-local-init] initialization complete");
    Ok(())
}
