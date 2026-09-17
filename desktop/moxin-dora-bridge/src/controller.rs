//! Dataflow lifecycle controller
//!
//! Manages the lifecycle of dora dataflows:
//! - Start dataflow with env configuration
//! - Stop dataflow and cleanup resources
//! - Monitor dataflow status

use crate::error::{BridgeError, BridgeResult};
use crate::parser::{DataflowParser, ParsedDataflow};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::ffi::OsStr;
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
    /// This directory separates Forum files, not Dora ports or dynamic nodes.
    runtime_dir: PathBuf,
    flow_name: String,
    /// Only a UUID received from this controller's start command grants ownership.
    owned_dataflow_id: Option<String>,
    /// A start command without a verifiable UUID must never invent ownership.
    start_outcome_uncertain: bool,
    /// Explicit executable or a bundled sibling; never resolve a Conda/Python
    /// wrapper implicitly through PATH. Fake-CLI tests inject their own path.
    cli_program: Option<PathBuf>,
}

impl DataflowController {
    fn command(&self) -> BridgeResult<Command> {
        if !self.runtime_dir.is_absolute() {
            return Err(BridgeError::StartFailed(
                "FORUM_AGENT_DORA_RUNTIME_DIR must be an absolute directory".into(),
            ));
        }
        let program = self.cli_program.as_ref().ok_or_else(|| {
            BridgeError::StartFailed(
                "Dora executable is not configured; set FORUM_AGENT_DORA_BIN to the verified \
                 native binary or bundle it beside the application executable"
                    .into(),
            )
        })?;
        if !program.is_absolute() {
            return Err(BridgeError::StartFailed(
                "FORUM_AGENT_DORA_BIN must be an absolute path to a verified native binary".into(),
            ));
        }
        std::fs::create_dir_all(&self.runtime_dir)?;
        let mut command = Command::new(program);
        command.current_dir(&self.runtime_dir);
        command.envs(&self.env_vars);
        Ok(command)
    }

    fn list_dataflows(&self) -> BridgeResult<Vec<ListedDataflow>> {
        let output = self
            .command()?
            .args(["list", "--format", "json"])
            .output()?;
        if !output.status.success() {
            return Err(BridgeError::ConnectionFailed(format!(
                "Cannot query the shared Dora network: {}. Forum F01 attaches only; \
                 it does not start or own a shared coordinator/daemon.",
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

    /// Check the existing shared network without bootstrapping or owning it.
    ///
    /// Kept under the original API name for callers. A successful coordinator
    /// query is not a full daemon/dynamic-node readiness check; start can fail.
    /// `dora up` spawns detached children, so its launcher Child is not proof
    /// that this controller owns a coordinator or daemon.
    pub fn ensure_daemon(&mut self) -> BridgeResult<()> {
        self.list_dataflows()?;
        debug!("Existing shared Dora coordinator is reachable; not owned by Forum");
        Ok(())
    }

    /// Start the dataflow
    pub fn start(&mut self) -> BridgeResult<String> {
        if self.owned_dataflow_id.is_some() || self.state.read().is_running() {
            return Err(BridgeError::DataflowAlreadyRunning);
        }
        if self.start_outcome_uncertain {
            return Err(BridgeError::StartFailed(
                "Previous start outcome is unknown; inspect the shared Dora network before retrying"
                    .into(),
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

        // dora-node-api 0.4.x dynamic initialization uses a fixed local port and
        // identifies nodes by node_id. Different flow names or working dirs do
        // not make simultaneous dynamic nodes safe. Refuse existing workloads;
        // never stop them. This preflight cannot prevent an external CLI racing
        // its own start; dedicated coordinator/daemon isolation remains F01 work.
        let running: Vec<_> = self
            .list_dataflows()?
            .into_iter()
            .filter(|flow| flow.status == "Running")
            .map(|flow| flow.uuid)
            .collect();
        if !running.is_empty() {
            return Err(BridgeError::StartFailed(format!(
                "Shared Dora network has running flow(s): {}. Forum F01 cannot safely \
                 attach duplicate dynamic nodes; stop the other application yourself \
                 before starting Forum. No existing flow was stopped.",
                running.join(", ")
            )));
        }

        let mut cmd = self.command()?;
        cmd.arg("start")
            // Use the absolute path so dora always resolves node paths relative to
            // the actual dataflow file location.
            .arg(&self.dataflow_path)
            .arg("--name")
            .arg(&self.flow_name)
            .arg("--detach");

        // Execute
        info!("Starting dataflow: {:?}", self.dataflow_path);
        let output = cmd.output().map_err(|e| {
            BridgeError::StartFailed(format!("Failed to execute dora start: {}", e))
        })?;

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

        // Update state
        *self.state.write() = DataflowState::Running {
            started_at: Instant::now(),
            dataflow_id: dataflow_id.clone(),
        };

        Ok(dataflow_id)
    }

    /// Stop the dataflow gracefully (default 15s grace period)
    pub fn stop(&mut self) -> BridgeResult<()> {
        self.stop_with_options(None)
    }

    /// Stop the dataflow with a custom grace duration
    ///
    /// After the grace duration, nodes that haven't stopped will be killed (SIGKILL).
    ///
    /// # Arguments
    /// * `grace_duration` - How long to wait before killing. None uses dora's default (15s).
    pub fn stop_with_grace_duration(&mut self, grace_duration: Duration) -> BridgeResult<()> {
        self.stop_with_options(Some(grace_duration))
    }

    /// Force stop the dataflow immediately (0s grace period)
    ///
    /// This will immediately kill all nodes without waiting for graceful shutdown.
    pub fn force_stop(&mut self) -> BridgeResult<()> {
        self.stop_with_options(Some(Duration::from_secs(0)))
    }

    /// Stop the dataflow with options
    fn stop_with_options(&mut self, grace_duration: Option<Duration>) -> BridgeResult<()> {
        let dataflow_id = match self.owned_dataflow_id.clone() {
            Some(id) => id,
            None if self.state.read().is_stopped() => return Ok(()),
            None => return Err(BridgeError::DataflowNotRunning),
        };

        *self.state.write() = DataflowState::Stopping;

        let grace_str = grace_duration
            .map(|d| format!("{}s", d.as_secs()))
            .unwrap_or_else(|| "default".to_string());
        info!("Stopping dataflow: {} (grace: {})", dataflow_id, grace_str);
        // Build dora stop command
        let mut cmd = match self.command() {
            Ok(command) => command,
            Err(error) => {
                *self.state.write() = DataflowState::Error {
                    message: error.to_string(),
                };
                return Err(error);
            }
        };
        cmd.arg("stop").arg(&dataflow_id);

        // Add grace duration if specified
        if let Some(duration) = grace_duration {
            cmd.arg("--grace-duration")
                .arg(format!("{}s", duration.as_secs()));
        }

        // Execute dora stop
        let output = match cmd.output() {
            Ok(output) => output,
            Err(error) => {
                let message = format!("Failed to execute dora stop: {error}");
                *self.state.write() = DataflowState::Error {
                    message: message.clone(),
                };
                return Err(BridgeError::StopFailed(message));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = format!(
                "Dora stop failed for owned flow {dataflow_id}; ownership retained: {}",
                stderr.trim()
            );
            *self.state.write() = DataflowState::Error {
                message: message.clone(),
            };
            return Err(BridgeError::StopFailed(message));
        }

        self.owned_dataflow_id = None;
        *self.state.write() = DataflowState::Stopped;
        info!("Dataflow stopped");

        Ok(())
    }

    /// Get dataflow status
    pub fn get_status(&self) -> BridgeResult<DataflowStatus> {
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
        if self.owned_dataflow_id.is_some() {
            if let Err(e) = self.stop() {
                error!("Failed to stop dataflow on drop: {}", e);
            }
        }
        // The shared coordinator/daemon is deliberately not owned. In
        // particular, never call `dora destroy`, kill a launcher, or scan
        // other flow IDs during cleanup.
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

    // These tests execute only this private shell fixture. They never change
    // process-wide PATH/env, invoke the real Dora CLI, start models, or capture.
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
                std::fs::create_dir_all(&root).unwrap();
                let root = root.canonicalize().unwrap();
                let executable = root.join("fake-dora");
                std::fs::write(
                    &executable,
                    r#"#!/bin/sh
for arg in "$@"; do printf '%s\t' "$arg" >> "$FAKE_DORA_ROOT/calls"; done
printf '\n' >> "$FAKE_DORA_ROOT/calls"
pwd >> "$FAKE_DORA_ROOT/directories"
printf '%s\n' "$FORUM_TEST_MARKER" >> "$FAKE_DORA_ROOT/markers"
case "$1" in
  list|start|stop)
    /bin/cat "$FAKE_DORA_ROOT/$1.stdout"
    /bin/cat "$FAKE_DORA_ROOT/$1.stderr" >&2
    exit "$(/bin/cat "$FAKE_DORA_ROOT/$1.exit")"
    ;;
  *) printf 'Forbidden fake Dora command\n' >&2; exit 97 ;;
esac
"#,
                )
                .unwrap();
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                for name in ["list", "start", "stop"] {
                    std::fs::write(root.join(format!("{name}.stdout")), "").unwrap();
                    std::fs::write(root.join(format!("{name}.stderr")), "").unwrap();
                    std::fs::write(root.join(format!("{name}.exit")), "0").unwrap();
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
                controller.runtime_dir = self.root.join(&controller.flow_name);
                controller.set_env("FAKE_DORA_ROOT", self.root.to_string_lossy());
                controller.set_env("FORUM_TEST_MARKER", "private-instance-env");
                controller
            }

            fn write(&self, file: &str, text: &str) {
                std::fs::write(self.root.join(file), text).unwrap();
            }

            fn calls(&self) -> Vec<String> {
                std::fs::read_to_string(self.root.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .map(|line| line.trim_end_matches('\t').to_string())
                    .collect()
            }
        }

        impl Drop for FakeDora {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn existing_flow_blocks_start_without_stopping_or_inspecting_its_nodes() {
            let fake = FakeDora::new();
            fake.write(
                "list.stdout",
                &format!(r#"{{"uuid":"{OTHER_FLOW}","name":"Hen Translator","status":"Running"}}"#),
            );
            let mut controller = fake.controller();
            let error = controller.start().unwrap_err().to_string();
            assert!(error.contains("duplicate dynamic nodes"));
            assert!(controller.owned_dataflow_id.is_none());
            drop(controller);
            assert_eq!(fake.calls(), ["list\t--format\tjson"]);
        }

        #[test]
        fn unavailable_coordinator_is_not_automatically_started_or_destroyed() {
            let fake = FakeDora::new();
            fake.write("list.exit", "1");
            fake.write("list.stderr", "connection refused");
            let mut controller = fake.controller();
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("attaches only"));
            assert!(matches!(controller.state(), DataflowState::Error { .. }));
            drop(controller);
            assert_eq!(fake.calls(), ["list\t--format\tjson"]);
        }

        #[test]
        fn malformed_preflight_blocks_start() {
            let fake = FakeDora::new();
            fake.write("list.stdout", "not JSON");
            let mut controller = fake.controller();
            assert!(controller.start().is_err());
            drop(controller);
            assert_eq!(fake.calls(), ["list\t--format\tjson"]);
        }

        #[test]
        fn start_and_drop_only_manage_the_returned_flow_in_one_private_directory() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            let flow_name = controller.flow_name.clone();
            let directory = controller.runtime_dir.clone();
            assert_eq!(controller.start().unwrap(), OWNED_FLOW);
            drop(controller);
            let calls = fake.calls();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0], "list\t--format\tjson");
            assert!(calls[1].contains(&format!("\t--name\t{flow_name}\t--detach")));
            assert_eq!(calls[2], format!("stop\t{OWNED_FLOW}"));
            let directories = std::fs::read_to_string(fake.root.join("directories")).unwrap();
            assert!(directories.lines().all(|line| Path::new(line) == directory));
            let markers = std::fs::read_to_string(fake.root.join("markers")).unwrap();
            assert!(markers.lines().all(|line| line == "private-instance-env"));
        }

        #[test]
        fn failed_stop_retains_ownership_for_retry_and_blocks_another_start() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.start().unwrap();
            fake.write("stop.exit", "1");
            fake.write("stop.stderr", "cannot reach coordinator");
            assert!(controller.stop().is_err());
            assert_eq!(controller.owned_dataflow_id.as_deref(), Some(OWNED_FLOW));
            assert!(matches!(controller.state(), DataflowState::Error { .. }));
            let calls_before = fake.calls().len();
            assert!(controller.start().is_err());
            assert_eq!(fake.calls().len(), calls_before);
            fake.write("stop.exit", "0");
            controller
                .stop_with_grace_duration(Duration::from_secs(2))
                .unwrap();
            assert!(controller.owned_dataflow_id.is_none());
            assert!(controller.state().is_stopped());
            assert_eq!(
                fake.calls().last().unwrap(),
                &format!("stop\t{OWNED_FLOW}\t--grace-duration\t2s")
            );
        }

        #[test]
        fn failed_start_with_a_real_receipt_can_clean_up_only_that_owned_flow() {
            let fake = FakeDora::new();
            fake.write("start.exit", "1");
            let mut controller = fake.controller();
            assert!(controller.start().is_err());
            assert_eq!(controller.owned_dataflow_id.as_deref(), Some(OWNED_FLOW));
            drop(controller);
            assert_eq!(fake.calls().last().unwrap(), &format!("stop\t{OWNED_FLOW}"));
        }

        #[test]
        fn unknown_start_receipt_never_invents_or_stops_a_flow() {
            let fake = FakeDora::new();
            fake.write(
                "start.stderr",
                &format!("some unrelated log UUID {OTHER_FLOW}"),
            );
            let mut controller = fake.controller();
            assert!(controller.start().is_err());
            assert!(controller.start_outcome_uncertain);
            assert!(controller.owned_dataflow_id.is_none());
            assert!(controller.start().is_err());
            drop(controller);
            assert_eq!(fake.calls().len(), 2);
        }

        #[test]
        fn status_requires_the_exact_uuid_and_running_state() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.start().unwrap();
            fake.write(
                "list.stdout",
                &format!(r#"{{"uuid":"{OWNED_FLOW}","status":"Finished"}}"#),
            );
            assert!(controller.get_status().unwrap().state.is_stopped());
            fake.write("list.exit", "1");
            assert!(controller.get_status().is_err());
            controller.stop().unwrap();
        }

        #[test]
        fn an_explicit_absolute_executable_is_required_without_path_fallback() {
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
            assert!(fake.calls().is_empty());
        }

        #[test]
        fn relative_runtime_roots_cannot_follow_a_later_working_directory_change() {
            let fake = FakeDora::new();
            let mut controller = fake.controller();
            controller.runtime_dir = PathBuf::from("relative-forum-runtime");
            assert!(controller
                .start()
                .unwrap_err()
                .to_string()
                .contains("FORUM_AGENT_DORA_RUNTIME_DIR must be an absolute"));
            assert!(fake.calls().is_empty());
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
