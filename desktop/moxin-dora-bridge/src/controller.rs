//! Dataflow lifecycle controller
//!
//! Manages the lifecycle of dora dataflows:
//! - Start dataflow with env configuration
//! - Stop dataflow and cleanup resources
//! - Monitor dataflow status

use crate::error::{BridgeError, BridgeResult};
use crate::owned_process::run_bounded;
use crate::owned_runtime::{
    DynamicNodeContext, ExecutableNode, OwnedRuntime, ShutdownOutcome, StopReport,
};
use crate::parser::{DataflowParser, ParsedDataflow};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};

#[derive(serde::Deserialize)]
struct ListedDataflow {
    uuid: String,
    status: String,
}

fn parse_dataflow_list(output: &[u8]) -> BridgeResult<Vec<ListedDataflow>> {
    let text = std::str::from_utf8(output)
        .map_err(|e| BridgeError::InvalidData(format!("Invalid Dora list UTF-8: {e}")))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            // Serde structs can also deserialize positional arrays. The CLI
            // contract is one JSON object per line; reject other shapes.
            if !line.trim_start().starts_with('{') {
                return Err(BridgeError::InvalidData(
                    "Dora list must contain one JSON object per line".into(),
                ));
            }
            let flow: ListedDataflow = serde_json::from_str(line)?;
            uuid::Uuid::parse_str(&flow.uuid)
                .map_err(|_| BridgeError::InvalidData("Invalid UUID in Dora list".into()))?;
            if !matches!(flow.status.as_str(), "Running" | "Finished" | "Failed") {
                return Err(BridgeError::InvalidData(format!(
                    "Unknown Dora flow status: {}",
                    flow.status
                )));
            }
            Ok(flow)
        })
        .collect()
}

fn instance_runtime_dir(instance_id: &str, configured_root: Option<&OsStr>) -> PathBuf {
    let root = configured_root
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("forum-agent").join("dora"));
    root.join(instance_id)
}

/// Copy the local graph into its owned directory without changing the caller's file.
/// F01 supports dynamic nodes and local executable paths; no implicit PATH lookup,
/// build/download hooks or remote deployment is accepted by this entry point.
struct PreparedDescriptor {
    path: PathBuf,
    executable_nodes: Vec<ExecutableNode>,
}

fn prepare_private_descriptor(source: &Path, directory: &Path) -> BridgeResult<PreparedDescriptor> {
    let mut descriptor: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(source)?)?;
    let root = descriptor
        .as_mapping_mut()
        .ok_or_else(|| BridgeError::ParseError("descriptor must be a mapping".into()))?;
    for field in root.keys() {
        if !matches!(field.as_str(), Some("nodes" | "communication")) {
            return Err(BridgeError::NotSupported(format!(
                "private F01 descriptor does not support top-level field {field:?}"
            )));
        }
    }
    let base = source
        .parent()
        .ok_or_else(|| BridgeError::ParseError("descriptor has no parent directory".into()))?;
    let nodes = root
        .get_mut(serde_yaml::Value::String("nodes".into()))
        .and_then(serde_yaml::Value::as_sequence_mut)
        .ok_or_else(|| BridgeError::ParseError("descriptor nodes must be a sequence".into()))?;
    let mut executable_nodes = Vec::new();
    for node in nodes {
        let node = node
            .as_mapping_mut()
            .ok_or_else(|| BridgeError::ParseError("node must be a mapping".into()))?;
        for field in node.keys() {
            if !matches!(
                field.as_str(),
                Some("id" | "name" | "description" | "path" | "inputs" | "outputs" | "env")
            ) {
                return Err(BridgeError::NotSupported(format!(
                    "private F01 runtime does not support node field {field:?}"
                )));
            }
        }
        let path = node
            .get(serde_yaml::Value::String("path".into()))
            .ok_or_else(|| {
                BridgeError::NotSupported(
                    "private F01 nodes require path: dynamic or a local executable".into(),
                )
            })?;
        let text = path
            .as_str()
            .ok_or_else(|| BridgeError::ParseError("node path must be text".into()))?;
        if text == "dynamic" {
            continue;
        }
        let absolute = if Path::new(text).is_absolute() {
            PathBuf::from(text)
        } else {
            base.join(text)
        };
        let absolute = absolute.canonicalize().map_err(|error| {
            BridgeError::StartFailed(format!(
                "local node path {} is unavailable: {error}",
                absolute.display()
            ))
        })?;
        if !absolute.is_file() {
            return Err(BridgeError::StartFailed(
                "node path must be a local file".into(),
            ));
        }
        let id = node
            .get(serde_yaml::Value::String("id".into()))
            .and_then(serde_yaml::Value::as_str)
            .ok_or_else(|| BridgeError::ParseError("executable node requires an ID".into()))?
            .to_owned();
        let mut env = HashMap::new();
        if let Some(values) = node.get(serde_yaml::Value::String("env".into())) {
            let values = values
                .as_mapping()
                .ok_or_else(|| BridgeError::ParseError("node env must be a mapping".into()))?;
            for (key, value) in values {
                let key = key
                    .as_str()
                    .ok_or_else(|| BridgeError::ParseError("node env key must be text".into()))?;
                let value = value.as_str().ok_or_else(|| {
                    BridgeError::NotSupported("F01 node env values must be explicit strings".into())
                })?;
                if key.starts_with("DORA_")
                    || key.starts_with("ZENOH_")
                    || key.starts_with("FORUM_AGENT_")
                {
                    return Err(BridgeError::NotSupported(format!(
                        "node env may not override runtime key {key}"
                    )));
                }
                env.insert(key.to_owned(), value.to_owned());
            }
        }
        executable_nodes.push(ExecutableNode {
            id,
            program: absolute,
            env,
        });
        node.insert("path".into(), "dynamic".into());
    }
    let mut communication = serde_yaml::Mapping::new();
    communication.insert("_unstable_local".into(), "Tcp".into());
    root.insert(
        "communication".into(),
        serde_yaml::Value::Mapping(communication),
    );
    let path = directory.join("private-dataflow.yml");
    std::fs::write(&path, serde_yaml::to_string(&descriptor)?)?;
    Ok(PreparedDescriptor {
        path,
        executable_nodes,
    })
}

/// Dataflow state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataflowState {
    /// Dataflow is stopped
    Stopped,
    /// Dataflow is starting
    Starting,
    /// Dataflow is running
    Running {
        started_at: Instant,
        dataflow_id: String,
    },
    /// Dataflow is stopping
    Stopping,
    /// Dataflow encountered an error
    Error { message: String },
}

impl Default for DataflowState {
    fn default() -> Self {
        DataflowState::Stopped
    }
}

impl DataflowState {
    pub fn is_running(&self) -> bool {
        matches!(self, DataflowState::Running { .. })
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, DataflowState::Stopped)
    }
}

/// Controller for managing dataflow lifecycle
pub struct DataflowController {
    /// Path to the dataflow YAML file
    dataflow_path: PathBuf,
    /// Parsed dataflow information
    parsed: Option<ParsedDataflow>,
    /// Current state
    state: Arc<RwLock<DataflowState>>,
    /// Environment variables to apply
    env_vars: HashMap<String, String>,
    /// Unique files and a directly owned coordinator/daemon pair.
    runtime_dir: PathBuf,
    flow_name: String,
    /// Only a UUID received from this controller's start command grants ownership.
    owned_dataflow_id: Option<String>,
    /// A start command without a verifiable UUID must never invent ownership.
    start_outcome_uncertain: bool,
    /// Explicit executable or a bundled sibling; never resolve a Conda/Python
    /// wrapper implicitly through PATH. Fake-CLI tests inject their own path.
    cli_program: Option<PathBuf>,
    runtime: Option<OwnedRuntime>,
    last_shutdown: Option<StopReport>,
    runtime_directory_identity: Option<(u64, u64)>,
    #[cfg(test)]
    allow_test_cli_script: bool,
}

impl DataflowController {
    fn validated_program(&self) -> BridgeResult<&Path> {
        if !self.runtime_dir.is_absolute() {
            return Err(BridgeError::StartFailed(
                "FORUM_AGENT_DORA_RUNTIME_DIR must be an absolute directory".into(),
            ));
        }
        let program = self.cli_program.as_deref().ok_or_else(|| BridgeError::StartFailed(
            "Dora executable is not configured; set FORUM_AGENT_DORA_BIN to the verified native binary".into()
        ))?;
        if !program.is_absolute() {
            return Err(BridgeError::StartFailed(
                "FORUM_AGENT_DORA_BIN must be an absolute path to a verified native binary".into(),
            ));
        }
        #[cfg(test)]
        if self.allow_test_cli_script {
            return Ok(program);
        }
        let mut header = [0u8; 32];
        std::fs::File::open(program)?
            .read_exact(&mut header)
            .map_err(|error| {
                BridgeError::StartFailed(format!(
                    "Dora executable has no native Mach-O header: {error}"
                ))
            })?;
        if u32::from_le_bytes(header[0..4].try_into().unwrap()) != 0xfeedfacf
            || u32::from_le_bytes(header[4..8].try_into().unwrap()) != 0x0100000c
            || u32::from_le_bytes(header[12..16].try_into().unwrap()) != 2
        {
            return Err(BridgeError::StartFailed(
                "Forum F01 requires a native arm64 Mach-O Dora executable, not a script or PATH wrapper".into(),
            ));
        }
        Ok(program)
    }

    /// Initially create a fresh 0700 leaf. Later operations may reuse only that
    /// same inode; a preexisting leaf, symlink or replacement is never adopted.
    fn prepare_runtime_directory(&mut self) -> BridgeResult<()> {
        if let Some((device, inode)) = self.runtime_directory_identity {
            let metadata = std::fs::symlink_metadata(&self.runtime_dir)?;
            if !metadata.is_dir()
                || metadata.dev() != device
                || metadata.ino() != inode
                || metadata.mode() & 0o777 != 0o700
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(BridgeError::StartFailed(
                    "Owned runtime directory identity or permissions changed".into(),
                ));
            }
            return Ok(());
        }
        let parent = self
            .runtime_dir
            .parent()
            .ok_or_else(|| BridgeError::StartFailed("Runtime directory has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&self.runtime_dir)
            .map_err(|error| {
                BridgeError::StartFailed(format!(
                    "Cannot exclusively create private runtime directory: {error}"
                ))
            })?;
        let metadata = std::fs::symlink_metadata(&self.runtime_dir)?;
        self.runtime_directory_identity = Some((metadata.dev(), metadata.ino()));
        self.prepare_runtime_directory()
    }

    pub fn runtime_directory(&self) -> &Path {
        &self.runtime_dir
    }

    fn command(&self) -> BridgeResult<Command> {
        let program = self.validated_program()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| BridgeError::NotConnected)?;
        Ok(runtime.command(program, &self.env_vars))
    }

    fn list_dataflows(&self) -> BridgeResult<Vec<ListedDataflow>> {
        let mut command = self.command()?;
        command.args(["list", "--format", "json"]);
        self.runtime
            .as_ref()
            .ok_or(BridgeError::NotConnected)?
            .append_control_args(&mut command);
        let output = run_bounded(command, &self.runtime_dir, Duration::from_secs(3))?;
        if !output.status.success() {
            return Err(BridgeError::ConnectionFailed(format!(
                "Cannot query owned Dora coordinator: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        parse_dataflow_list(&output.stdout)
    }

    /// Create a new controller for a dataflow
    pub fn new(dataflow_path: impl AsRef<Path>) -> BridgeResult<Self> {
        let original_path = dataflow_path.as_ref();
        // Canonicalize to avoid surprises when callers pass relative paths coming
        // from different working directories. If canonicalize fails (e.g. missing
        // file), fall back to the provided path so the parser can surface the error.
        let path = original_path
            .canonicalize()
            .unwrap_or_else(|_| original_path.to_path_buf());

        // Parse the dataflow
        let parsed = DataflowParser::parse(&path)?;
        let instance_id = uuid::Uuid::new_v4().to_string();
        let runtime_root = std::env::var_os("FORUM_AGENT_DORA_RUNTIME_DIR");
        let cli_program = std::env::var_os("FORUM_AGENT_DORA_BIN")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let executable = std::env::current_exe().ok()?;
                let filename = if cfg!(windows) { "dora.exe" } else { "dora" };
                let bundled = executable.parent()?.join(filename);
                bundled.is_file().then_some(bundled)
            });

        Ok(Self {
            dataflow_path: path,
            parsed: Some(parsed),
            state: Arc::new(RwLock::new(DataflowState::Stopped)),
            env_vars: HashMap::new(),
            runtime_dir: instance_runtime_dir(&instance_id, runtime_root.as_deref()),
            flow_name: format!("forum-agent-{instance_id}"),
            owned_dataflow_id: None,
            start_outcome_uncertain: false,
            cli_program,
            runtime: None,
            last_shutdown: None,
            runtime_directory_identity: None,
            #[cfg(test)]
            allow_test_cli_script: false,
        })
    }

    /// Get the parsed dataflow
    pub fn parsed(&self) -> Option<&ParsedDataflow> {
        self.parsed.as_ref()
    }

    /// Get current state
    pub fn state(&self) -> DataflowState {
        self.state.read().clone()
    }

    /// Set environment variable for the dataflow
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.env_vars.insert(key.into(), value.into());
    }

    /// Set multiple environment variables
    pub fn set_envs(&mut self, vars: HashMap<String, String>) {
        self.env_vars.extend(vars);
    }

    /// Check if all required env vars are set
    pub fn check_env_requirements(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if let Some(parsed) = &self.parsed {
            for req in &parsed.env_requirements {
                if req.required {
                    if !self.env_vars.contains_key(&req.key) && std::env::var(&req.key).is_err() {
                        missing.push(req.key.clone());
                    }
                }
            }
        }
        missing
    }

    /// Create an exclusively owned coordinator/daemon pair on private loopback ports.
    pub fn ensure_daemon(&mut self) -> BridgeResult<()> {
        if self.runtime.as_ref().is_some_and(OwnedRuntime::is_started) {
            return Ok(());
        }
        let program = self.validated_program()?.to_path_buf();
        self.prepare_runtime_directory()?;
        if self.runtime.is_none() {
            let mut version = Command::new(&program);
            version
                .arg("--version")
                .current_dir(&self.runtime_dir)
                .envs(&self.env_vars);
            let output = run_bounded(version, &self.runtime_dir, Duration::from_secs(4))?;
            if !output.status.success()
                || !String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.trim() == "dora-cli 0.4.1")
            {
                return Err(BridgeError::StartFailed(
                    "Forum F01 requires the verified native Dora CLI 0.4.1".into(),
                ));
            }
            self.runtime = Some(OwnedRuntime::new(&self.runtime_dir)?);
        }
        self.runtime
            .as_mut()
            .unwrap()
            .start(&program, &self.env_vars)?;
        debug!("Private Dora runtime is ready");
        Ok(())
    }

    pub fn dynamic_node_context(&self) -> BridgeResult<DynamicNodeContext> {
        if !self.state.read().is_running() {
            return Err(BridgeError::DataflowNotRunning);
        }
        let id = self
            .owned_dataflow_id
            .as_ref()
            .ok_or(BridgeError::DataflowNotRunning)?;
        let runtime = self.runtime.as_ref().ok_or(BridgeError::NotConnected)?;
        Ok(DynamicNodeContext {
            daemon_addr: runtime.daemon_addr(),
            dataflow_id: uuid::Uuid::parse_str(id)
                .map_err(|error| BridgeError::InvalidData(error.to_string()))?,
            daemon_pid: runtime.daemon_pid()?,
        })
    }

    /// Exact retained child identities, never values obtained by process scanning.
    pub fn owned_process_pids(&self) -> Vec<u32> {
        self.runtime
            .as_ref()
            .map(OwnedRuntime::owned_process_pids)
            .unwrap_or_default()
    }

    /// Only directly spawned node children; useful for bounded lifecycle diagnostics.
    pub fn owned_node_pids(&self) -> Vec<u32> {
        self.runtime
            .as_ref()
            .map(OwnedRuntime::node_pids)
            .unwrap_or_default()
    }

    pub fn last_shutdown_report(&self) -> Option<StopReport> {
        self.last_shutdown.clone()
    }

    /// Start the dataflow
    pub fn start(&mut self) -> BridgeResult<String> {
        if self.owned_dataflow_id.is_some() || self.state.read().is_running() {
            return Err(BridgeError::DataflowAlreadyRunning);
        }
        if self.start_outcome_uncertain {
            return Err(BridgeError::StartFailed(
                "Previous start outcome is unknown; stop the owned runtime before retrying".into(),
            ));
        }

        *self.state.write() = DataflowState::Starting;
        let result = self.start_inner();
        if let Err(error) = &result {
            *self.state.write() = DataflowState::Error {
                message: error.to_string(),
            };
        }
        result
    }

    fn start_inner(&mut self) -> BridgeResult<String> {
        let missing = self.check_env_requirements();
        if !missing.is_empty() {
            return Err(BridgeError::StartFailed(format!(
                "Missing required env vars: {}",
                missing.join(", ")
            )));
        }

        self.validated_program()?;
        self.prepare_runtime_directory()?;
        let descriptor = prepare_private_descriptor(&self.dataflow_path, &self.runtime_dir)?;
        self.ensure_daemon()?;
        if self
            .list_dataflows()?
            .iter()
            .any(|flow| flow.status == "Running")
        {
            return Err(BridgeError::StartFailed(
                "Owned runtime already has a running dataflow; stop this runtime before retrying"
                    .into(),
            ));
        }
        let mut command = self.command()?;
        command
            .arg("start")
            .arg(&descriptor.path)
            .arg("--name")
            .arg(&self.flow_name)
            .arg("--detach");
        self.runtime
            .as_ref()
            .unwrap()
            .append_control_args(&mut command);
        info!("Starting private dataflow: {:?}", descriptor.path);
        let output = match run_bounded(command, &self.runtime_dir, Duration::from_secs(15)) {
            Ok(output) => output,
            Err(error) => {
                self.start_outcome_uncertain = true;
                return Err(error);
            }
        };

        // Even a failing start may have allocated a real flow before a node
        // failed. Preserve that UUID so cleanup can target only our flow.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_text = format!("{stderr}\n{stdout}");
        self.owned_dataflow_id = Self::parse_dataflow_id(&output_text);
        self.start_outcome_uncertain = self.owned_dataflow_id.is_none();

        if !output.status.success() {
            return Err(BridgeError::StartFailed(format!(
                "Dora start failed: {}",
                stderr.trim()
            )));
        }

        let dataflow_id = self.owned_dataflow_id.clone().ok_or_else(|| {
            BridgeError::StartFailed(
                "Dora start returned no unambiguous valid flow UUID; outcome unknown. \
                 Forum will not invent an ID or stop an unidentified flow."
                    .into(),
            )
        })?;

        info!("Dataflow started with ID: {}", dataflow_id);

        self.runtime.as_mut().unwrap().spawn_nodes(
            uuid::Uuid::parse_str(&dataflow_id)
                .map_err(|error| BridgeError::InvalidData(error.to_string()))?,
            &descriptor.executable_nodes,
            &self.env_vars,
        )?;

        // Update state
        *self.state.write() = DataflowState::Running {
            started_at: Instant::now(),
            dataflow_id: dataflow_id.clone(),
        };

        Ok(dataflow_id)
    }

    /// Stop with a bounded CLI wait, then contain the exclusively owned runtime.
    /// `last_shutdown_report` distinguishes a CLI acknowledgement from fallback.
    pub fn stop(&mut self) -> BridgeResult<()> {
        self.stop_with_options(None)
    }

    pub fn stop_with_grace_duration(&mut self, duration: Duration) -> BridgeResult<()> {
        self.stop_with_options(Some(duration))
    }

    pub fn force_stop(&mut self) -> BridgeResult<()> {
        self.stop_with_options(Some(Duration::ZERO))
    }

    fn stop_with_options(&mut self, grace_duration: Option<Duration>) -> BridgeResult<()> {
        if self.runtime.is_none() && self.owned_dataflow_id.is_none() {
            self.start_outcome_uncertain = false;
            *self.state.write() = DataflowState::Stopped;
            return Ok(());
        }
        *self.state.write() = DataflowState::Stopping;
        let mut acknowledged = false;
        let mut detail = None;
        if let Some(id) = self.owned_dataflow_id.as_ref() {
            let result = (|| {
                let mut command = self.command()?;
                command.arg("stop").arg(id);
                if grace_duration == Some(Duration::ZERO) {
                    command.arg("--force");
                } else {
                    command.arg("--grace-duration").arg(format!(
                        "{}ms",
                        grace_duration.unwrap_or(Duration::from_secs(2)).as_millis()
                    ));
                }
                self.runtime
                    .as_ref()
                    .ok_or(BridgeError::NotConnected)?
                    .append_control_args(&mut command);
                run_bounded(command, &self.runtime_dir, Duration::from_secs(3))
            })();
            match result {
                Ok(output) if output.status.success() => acknowledged = true,
                Ok(output) => {
                    detail = Some(format!(
                        "Dora stop did not acknowledge completion: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
                Err(error) => detail = Some(error.to_string()),
            }
        } else {
            detail =
                Some("No verified flow receipt; containing only the directly owned runtime".into());
        }
        let containment = self
            .runtime
            .as_mut()
            .ok_or_else(|| {
                BridgeError::StopFailed(
                    "Flow ownership exists without an owned runtime; refusing shared cleanup"
                        .into(),
                )
            })
            .and_then(OwnedRuntime::contain);
        let contained = containment.is_ok();
        if let Err(error) = &containment {
            detail = Some(format!(
                "{}; containment failed: {error}",
                detail.unwrap_or_default()
            ));
        }
        self.last_shutdown = Some(StopReport {
            acknowledged,
            contained,
            outcome: if !contained {
                ShutdownOutcome::StopFailed
            } else if acknowledged {
                ShutdownOutcome::StoppedGracefully
            } else {
                ShutdownOutcome::StoppedByOwnedRuntimeFallback
            },
            detail: detail.clone(),
        });
        if contained {
            self.runtime = None;
            self.owned_dataflow_id = None;
            self.start_outcome_uncertain = false;
            *self.state.write() = DataflowState::Stopped;
            Ok(())
        } else {
            let message = detail
                .unwrap_or_else(|| "owned runtime containment failed; ownership retained".into());
            *self.state.write() = DataflowState::Error {
                message: message.clone(),
            };
            Err(BridgeError::StopFailed(message))
        }
    }

    /// Get dataflow status
    pub fn get_status(&self) -> BridgeResult<DataflowStatus> {
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.check_node_health()?;
        }
        let state = self.state.read().clone();

        match state {
            DataflowState::Running {
                ref dataflow_id,
                ref started_at,
            } => {
                // Exact UUID/status lookup, never substring matching a stale
                // table row. A failed query is unknown, not a stopped flow.
                let is_running = self
                    .list_dataflows()?
                    .iter()
                    .any(|flow| flow.uuid == *dataflow_id && flow.status == "Running");
                let uptime = started_at.elapsed();

                Ok(DataflowStatus {
                    state: if is_running {
                        DataflowState::Running {
                            dataflow_id: dataflow_id.clone(),
                            started_at: *started_at,
                        }
                    } else {
                        DataflowState::Stopped
                    },
                    uptime: Some(uptime),
                    node_count: self.parsed.as_ref().map(|p| p.nodes.len()).unwrap_or(0),
                    moxin_node_count: self
                        .parsed
                        .as_ref()
                        .map(|p| p.moxin_nodes.len())
                        .unwrap_or(0),
                })
            }
            other => Ok(DataflowStatus {
                state: other,
                uptime: None,
                node_count: self.parsed.as_ref().map(|p| p.nodes.len()).unwrap_or(0),
                moxin_node_count: self
                    .parsed
                    .as_ref()
                    .map(|p| p.moxin_nodes.len())
                    .unwrap_or(0),
            }),
        }
    }

    /// Parse dataflow ID from dora start output
    fn parse_dataflow_id(output: &str) -> Option<String> {
        // Dora 0.4 emits these exact receipts. Do not claim ownership of an
        // unrelated UUID in node logs, or invent an ID when output changes.
        let mut receipt: Option<String> = None;
        for line in output.lines() {
            let line = line.trim();
            let Some(candidate) = line
                .strip_prefix("dataflow start triggered: ")
                .or_else(|| line.strip_prefix("dataflow started: "))
            else {
                continue;
            };
            let id = uuid::Uuid::parse_str(candidate.trim()).ok()?.to_string();
            if receipt.as_ref().is_some_and(|previous| previous != &id) {
                return None;
            }
            receipt = Some(id);
        }
        receipt
    }
}

impl Drop for DataflowController {
    fn drop(&mut self) {
        // Try to stop the dataflow if running
        if self.owned_dataflow_id.is_some() || self.runtime.is_some() {
            if let Err(e) = self.stop() {
                error!("Failed to stop dataflow on drop: {}", e);
            }
        }
        // Never discover or stop services belonging to another controller.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNED_FLOW: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER_FLOW: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn ownership_requires_an_unambiguous_start_receipt() {
        assert_eq!(
            DataflowController::parse_dataflow_id(&format!(
                "dataflow start triggered: {OWNED_FLOW}\ndataflow started: {OWNED_FLOW}"
            )),
            Some(OWNED_FLOW.into())
        );
        for output in [
            format!("node log mentions {OTHER_FLOW}"),
            "dataflow started: not-a-uuid".into(),
            format!("dataflow started: {OWNED_FLOW}\ndataflow started: {OTHER_FLOW}"),
        ] {
            assert_eq!(DataflowController::parse_dataflow_id(&output), None);
        }
    }

    #[test]
    fn runtime_directories_are_instance_specific_even_with_an_explicit_root() {
        let root = std::env::temp_dir().join("forum-controller-root");
        let first = instance_runtime_dir(OWNED_FLOW, Some(root.as_os_str()));
        let second = instance_runtime_dir(OTHER_FLOW, Some(root.as_os_str()));
        assert_eq!(first.parent(), Some(root.as_path()));
        assert_ne!(first, second);
        assert_eq!(
            instance_runtime_dir(OWNED_FLOW, None),
            std::env::temp_dir()
                .join("forum-agent/dora")
                .join(OWNED_FLOW)
        );
    }

    #[test]
    fn malformed_list_is_not_treated_as_an_idle_network() {
        assert!(parse_dataflow_list(b"").unwrap().is_empty());
        for output in [
            b"not json".as_slice(),
            br#"{"uuid":"bad","status":"Running"}"#,
            br#"{"uuid":"11111111-1111-4111-8111-111111111111","status":"Unknown"}"#,
            br#"{"uuid":"11111111-1111-4111-8111-111111111111"}"#,
            br#"["11111111-1111-4111-8111-111111111111","Finished"]"#,
            br#"{"uuid":"11111111-1111-4111-8111-111111111111","status":"Running","status":"Finished"}"#,
        ] {
            assert!(parse_dataflow_list(output).is_err());
        }
    }

    // Fake CLI launches only private Python loopback listeners; no real Dora or audio.
    #[cfg(unix)]
    mod fake_cli {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        struct FakeDora {
            root: PathBuf,
            executable: PathBuf,
        }
        impl FakeDora {
            fn new() -> Self {
                let root = std::env::temp_dir()
                    .join(format!("forum-controller-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir(&root).unwrap();
                let root = root.canonicalize().unwrap();
                let executable = root.join("fake-dora");
                std::fs::write(&executable, r#"#!/usr/bin/python3
import json, os, pathlib, socket, sys, time
root=pathlib.Path(os.environ['FAKE_DORA_ROOT'])
args=sys.argv[1:]
with (root/'calls').open('a') as log:
 log.write(json.dumps({'args':args,'cwd':os.getcwd(),'zenoh':os.environ.get('ZENOH_CONFIG')})+'\n')
if args == ['--version']:
 print('dora-cli 0.4.1'); sys.exit(0)
verb=args[0]
if verb in ('coordinator','daemon'):
 if (root/(verb+'.exit')).exists(): sys.exit(int((root/(verb+'.exit')).read_text()))
 flags=['--port','--control-port'] if verb=='coordinator' else ['--local-listen-port']
 listeners=[]
 for flag in flags:
  listener=socket.socket(); listener.bind(('127.0.0.1',int(args[args.index(flag)+1]))); listener.listen(); listeners.append(listener)
 while True: time.sleep(1)
if verb in ('list','start','stop'):
 if (root/(verb+'.sleep')).exists(): time.sleep(30)
 sys.stdout.write((root/(verb+'.stdout')).read_text())
 sys.stderr.write((root/(verb+'.stderr')).read_text())
 sys.exit(int((root/(verb+'.exit')).read_text()))
sys.exit(97)
"#).unwrap();
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                for verb in ["list", "start", "stop"] {
                    for stream in ["stdout", "stderr"] {
                        std::fs::write(root.join(format!("{verb}.{stream}")), "").unwrap();
                    }
                    std::fs::write(root.join(format!("{verb}.exit")), "0").unwrap();
                }
                std::fs::write(
                    root.join("start.stderr"),
                    format!(
                        "dataflow start triggered: {OWNED_FLOW}\ndataflow started: {OWNED_FLOW}\n"
                    ),
                )
                .unwrap();
                std::fs::write(root.join("flow.yml"), "nodes: []\n").unwrap();
                Self { root, executable }
            }
            fn controller(&self) -> DataflowController {
                let mut controller = DataflowController::new(self.root.join("flow.yml")).unwrap();
                controller.cli_program = Some(self.executable.clone());
                controller.allow_test_cli_script = true;
                controller.runtime_dir = self.root.join(&controller.flow_name);
                controller.set_env("FAKE_DORA_ROOT", self.root.to_string_lossy());
                controller
            }
            fn write(&self, file: &str, text: &str) {
                std::fs::write(self.root.join(file), text).unwrap();
            }
            fn calls(&self) -> Vec<serde_json::Value> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            }
        }
        impl Drop for FakeDora {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn native_header_is_checked_before_a_configured_script_can_execute() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.allow_test_cli_script = false;
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("Mach-O"));
            assert!(fake.calls().is_empty());
        }

        #[test]
        fn directory_is_private_and_preexisting_or_replaced_leaves_are_rejected() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            std::fs::create_dir(&controller.runtime_dir).unwrap();
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("exclusively create"));
            std::fs::remove_dir(&controller.runtime_dir).unwrap();
            std::os::unix::fs::symlink(&fake.root, &controller.runtime_dir).unwrap();
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("exclusively create"));
            std::fs::remove_file(&controller.runtime_dir).unwrap();
            controller.prepare_runtime_directory().unwrap();
            assert_eq!(
                std::fs::metadata(&controller.runtime_dir)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let moved = controller.runtime_dir.with_extension("old");
            std::fs::rename(&controller.runtime_dir, &moved).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&controller.runtime_dir)
                .unwrap();
            assert!(controller.prepare_runtime_directory().is_err());
        }

        #[test]
        fn every_control_command_targets_the_owned_private_endpoint() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            assert_eq!(controller.start().unwrap(), OWNED_FLOW);
            let context = controller.dynamic_node_context().unwrap();
            assert!(context.daemon_addr.ip().is_loopback());
            assert_ne!(context.daemon_addr.port(), 53291);
            assert_eq!(context.dataflow_id.to_string(), OWNED_FLOW);
            controller.stop().unwrap();
            let report = controller.last_shutdown_report().unwrap();
            assert!(report.acknowledged && report.contained);
            assert_eq!(report.outcome, ShutdownOutcome::StoppedGracefully);
            let mut control_ports = std::collections::HashSet::new();
            for call in fake.calls() {
                let args = call["args"].as_array().unwrap();
                if ["list", "start", "stop"].contains(&args[0].as_str().unwrap()) {
                    let index = args
                        .iter()
                        .position(|arg| arg == "--coordinator-port")
                        .unwrap();
                    let port = args[index + 1].as_str().unwrap();
                    assert_ne!(port, "6012");
                    control_ports.insert(port.to_string());
                    assert!(call["zenoh"].as_str().unwrap().ends_with("zenoh.json5"));
                }
            }
            assert_eq!(control_ports.len(), 1);
        }

        #[test]
        fn unknown_start_uuid_can_contain_owned_services_without_stopping_an_unknown_flow() {
            let fake = FakeDora::new();
            fake.write("start.stderr", &format!("unrelated UUID {OTHER_FLOW}"));
            let mut controller = fake.controller();
            assert!(controller.start().is_err());
            assert!(controller.start_outcome_uncertain && controller.runtime.is_some());
            assert!(controller.start().is_err());
            controller.stop().unwrap();
            assert!(controller.runtime.is_none() && !controller.start_outcome_uncertain);
            let report = controller.last_shutdown_report().unwrap();
            assert!(!report.acknowledged && report.contained);
            assert!(!fake.calls().iter().any(|call| call["args"][0] == "stop"));
        }

        #[test]
        fn bounded_stop_fallback_contains_only_this_runtime_and_keeps_peer_alive() {
            let fake = FakeDora::new();
            let peer_fake = FakeDora::new();
            let mut controller = fake.controller();
            let mut peer = peer_fake.controller();
            controller.start().unwrap();
            peer.start().unwrap();
            fake.write("stop.sleep", "1");
            let started = Instant::now();
            controller.stop().unwrap();
            assert!(started.elapsed() < Duration::from_secs(7));
            let report = controller.last_shutdown_report().unwrap();
            assert!(!report.acknowledged && report.contained);
            assert_eq!(
                report.outcome,
                ShutdownOutcome::StoppedByOwnedRuntimeFallback
            );
            assert!(controller.owned_dataflow_id.is_none());
            peer_fake.write(
                "list.stdout",
                &format!(r#"{{"uuid":"{OWNED_FLOW}","status":"Running"}}"#),
            );
            assert!(peer.get_status().unwrap().state.is_running());
            peer.stop().unwrap();
        }

        #[test]
        fn daemon_start_failure_retains_then_cleans_the_partial_runtime_without_a_uuid() {
            let fake = FakeDora::new();
            fake.write("daemon.exit", "1");
            let mut controller = fake.controller();
            assert!(controller.start().is_err());
            assert!(controller.runtime.is_some() && controller.owned_dataflow_id.is_none());
            controller.stop().unwrap();
            assert!(controller.last_shutdown_report().unwrap().contained);
            assert!(controller.runtime.is_none());
        }

        #[test]
        fn failed_containment_keeps_the_flow_identity_for_retry() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.start().unwrap();
            let runtime = controller.runtime.take();
            assert!(controller.stop().is_err());
            assert_eq!(controller.owned_dataflow_id.as_deref(), Some(OWNED_FLOW));
            assert_eq!(
                controller.last_shutdown_report().unwrap().outcome,
                ShutdownOutcome::StopFailed
            );
            assert!(controller.start().is_err());
            controller.runtime = runtime;
            controller.stop().unwrap();
        }

        #[test]
        fn malformed_private_list_and_query_failure_are_not_silently_stopped() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            fake.write("list.stdout", "not JSON");
            assert!(controller.start().is_err());
            controller.stop().unwrap();
            fake.write("list.stdout", "");
            controller.start().unwrap();
            fake.write("list.exit", "1");
            assert!(controller.get_status().is_err());
            controller.stop().unwrap();
        }

        #[test]
        fn explicit_program_and_absolute_runtime_are_required_without_path_fallback() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.cli_program = None;
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("FORUM_AGENT_DORA_BIN"));
            controller.cli_program = Some(PathBuf::from("dora"));
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("absolute path"));
            controller.cli_program = Some(fake.executable.clone());
            controller.runtime_dir = PathBuf::from("relative-runtime");
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("absolute directory"));
            assert!(fake.calls().is_empty());
        }

        #[test]
        fn descriptor_rejects_unowned_runtime_hooks_before_starting_services() {
            let fake = FakeDora::new();
            let target = fake.root.join("runtime");
            std::fs::create_dir(&target).unwrap();
            for field in [
                "operators: []",
                "operator: {}",
                "custom: {}",
                "args: --something",
                "restart_policy: Always",
                "build: echo hello",
                "git: https://example.invalid/repo",
            ] {
                fake.write(
                    "flow.yml",
                    &format!("nodes:\n  - id: test\n    path: dynamic\n    {field}\n"),
                );
                assert!(
                    prepare_private_descriptor(&fake.root.join("flow.yml"), &target).is_err(),
                    "accepted {field}"
                );
            }
            fake.write(
                "flow.yml",
                "debug:\n  publish_all_messages_to_zenoh: true\nnodes: []\n",
            );
            assert!(prepare_private_descriptor(&fake.root.join("flow.yml"), &target).is_err());
            assert!(fake.calls().is_empty());
        }

        #[test]
        fn descriptor_copy_uses_tcp_and_absolute_node_paths() {
            let fake = FakeDora::new();
            fake.write("flow.yml", "communication:\n  _unstable_local: UnixDomain\nnodes:\n  - id: test\n    path: fake-dora\n  - id: mic\n    path: dynamic\n");
            let target = fake.root.join("runtime");
            std::fs::create_dir(&target).unwrap();
            let copy = prepare_private_descriptor(&fake.root.join("flow.yml"), &target).unwrap();
            let parsed: serde_yaml::Value =
                serde_yaml::from_str(&std::fs::read_to_string(&copy.path).unwrap()).unwrap();
            assert_eq!(
                parsed["communication"]["_unstable_local"].as_str(),
                Some("Tcp")
            );
            assert_eq!(parsed["nodes"][0]["path"].as_str(), Some("dynamic"));
            assert_eq!(copy.executable_nodes.len(), 1);
            assert_eq!(copy.executable_nodes[0].program, fake.executable);
            assert_eq!(parsed["nodes"][1]["path"].as_str(), Some("dynamic"));
            assert!(std::fs::read_to_string(fake.root.join("flow.yml"))
                .unwrap()
                .contains("UnixDomain"));
        }
    }
}

/// Dataflow status information
#[derive(Debug, Clone)]
pub struct DataflowStatus {
    pub state: DataflowState,
    pub uptime: Option<Duration>,
    pub node_count: usize,
    pub moxin_node_count: usize,
}
