//! Private coordinator/daemon ownership. Never attaches to Dora's default network.
use crate::{
    owned_process::{run_bounded, OwnedProcess},
    BridgeError, BridgeResult,
};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct DynamicNodeContext {
    pub daemon_addr: SocketAddr,
    pub dataflow_id: uuid::Uuid,
    pub daemon_pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ShutdownOutcome {
    StoppedGracefully,
    StoppedByOwnedRuntimeFallback,
    StopFailed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StopReport {
    pub acknowledged: bool,
    pub contained: bool,
    pub outcome: ShutdownOutcome,
    pub detail: Option<String>,
}

/// Local executable delegated by the private graph to this direct-child supervisor.
#[derive(Debug,Clone,serde::Serialize)]
pub struct NodeHealth { pub node_id:String, pub pid:u32, pub running:bool, pub exit_status:Option<String> }

pub struct ExecutableNode {
    pub id: String,
    pub program: PathBuf,
    pub env: HashMap<String, String>,
}

pub struct OwnedRuntime {
    directory: PathBuf,
    coordinator_addr: SocketAddr,
    control_addr: SocketAddr,
    daemon_addr: SocketAddr,
    reservations: Vec<TcpListener>,
    coordinator: Option<OwnedProcess>,
    daemon: Option<OwnedProcess>,
    nodes: parking_lot::Mutex<Vec<(String,OwnedProcess)>>,
    started: bool,
}

impl OwnedRuntime {
    pub fn new(directory: &Path) -> BridgeResult<Self> {
        if !directory.is_absolute() {
            return Err(BridgeError::StartFailed(
                "runtime directory must be absolute".into(),
            ));
        }
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir()
            || metadata.mode() & 0o777 != 0o700
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(BridgeError::StartFailed(
                "Runtime requires a controller-created private 0700 directory".into(),
            ));
        }
        let mut reservations = Vec::new();
        for _ in 0..3 {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            if [6012, 53290, 53291, 5456, 7447].contains(&listener.local_addr()?.port()) {
                return Err(BridgeError::StartFailed(
                    "OS allocated a reserved Dora port".into(),
                ));
            }
            reservations.push(listener);
        }
        Ok(Self {
            directory: directory.into(),
            coordinator_addr: reservations[0].local_addr()?,
            control_addr: reservations[1].local_addr()?,
            daemon_addr: reservations[2].local_addr()?,
            reservations,
            coordinator: None,
            daemon: None,
            nodes: parking_lot::Mutex::new(Vec::new()),
            started: false,
        })
    }

    pub fn daemon_addr(&self) -> SocketAddr {
        self.daemon_addr
    }
    pub fn daemon_pid(&self) -> BridgeResult<u32> {
        self.daemon
            .as_ref()
            .map(OwnedProcess::id)
            .ok_or_else(|| BridgeError::NotConnected)
    }
    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn command(&self, program: &Path, env: &HashMap<String, String>) -> Command {
        let mut command = Command::new(program);
        command.current_dir(&self.directory).envs(env);
        for key in [
            "DORA_NODE_CONFIG",
            "DORA_TEST_WITH_INPUTS",
            "DORA_TEST_WRITE_OUTPUTS_TO",
            "ZENOH_CONFIG_OVERRIDE",
        ] {
            command.env_remove(key);
        }
        command.env("ZENOH_CONFIG", self.directory.join("zenoh.json5"));
        command
    }

    pub fn append_control_args(&self, command: &mut Command) {
        command
            .arg("--coordinator-addr")
            .arg(self.control_addr.ip().to_string())
            .arg("--coordinator-port")
            .arg(self.control_addr.port().to_string());
    }

    pub fn start(&mut self, program: &Path, env: &HashMap<String, String>) -> BridgeResult<()> {
        if self.started {
            return Ok(());
        }
        if self.coordinator.is_some() || self.daemon.is_some() {
            return Err(BridgeError::StartFailed(
                "partial runtime still owned; stop it before retrying".into(),
            ));
        }
        std::fs::write(
            self.directory.join("zenoh.json5"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mode": "peer", "connect": {"endpoints": []},
                "listen": {"endpoints": ["tcp/127.0.0.1:0"]},
                "scouting": {"multicast": {"enabled": false}, "gossip": {"enabled": false}}
            }))?,
        )?;
        // Retain each handle immediately so any failure remains cleanable.
        self.reservations.drain(0..2);
        let mut coordinator = self.command(program, env);
        coordinator
            .args([
                "coordinator",
                "--interface",
                "127.0.0.1",
                "--control-interface",
                "127.0.0.1",
            ])
            .arg("--port")
            .arg(self.coordinator_addr.port().to_string())
            .arg("--control-port")
            .arg(self.control_addr.port().to_string());
        log_to_files(&mut coordinator, &self.directory, "coordinator")?;
        self.coordinator = Some(OwnedProcess::spawn(coordinator)?);
        wait_for_owned_ports(
            self.coordinator.as_mut().unwrap(),
            &[self.coordinator_addr, self.control_addr],
            &self.directory,
        )?;
        self.reservations.clear();
        let mut daemon = self.command(program, env);
        daemon
            .arg("daemon")
            .arg("--coordinator-addr")
            .arg("127.0.0.1")
            .arg("--coordinator-port")
            .arg(self.coordinator_addr.port().to_string())
            .arg("--local-listen-port")
            .arg(self.daemon_addr.port().to_string());
        log_to_files(&mut daemon, &self.directory, "daemon")?;
        self.daemon = Some(OwnedProcess::spawn(daemon)?);
        wait_for_owned_ports(
            self.daemon.as_mut().unwrap(),
            &[self.daemon_addr],
            &self.directory,
        )?;
        self.started = true;
        Ok(())
    }

    pub fn owned_process_pids(&self) -> Vec<u32> {
        self.coordinator
            .iter()
            .chain(self.daemon.iter())
            .map(OwnedProcess::id)
            .chain(self.node_pids())
            .collect()
    }

    pub fn node_pids(&self) -> Vec<u32> {
        self.nodes.lock().iter().map(|(_,process)|process.id()).collect()
    }

    pub fn node_health(&self)->BridgeResult<Vec<NodeHealth>> {
        self.nodes.lock().iter_mut().map(|(node_id,process)|{
            let status=process.poll()?;
            Ok(NodeHealth{node_id:node_id.clone(),pid:process.id(),running:status.is_none(),exit_status:status.map(|value|value.to_string())})
        }).collect()
    }

    pub fn check_node_health(&self) -> BridgeResult<()> {
        for (node_id,process) in self.nodes.lock().iter_mut() {
            if let Some(status) = process.poll()? {
                return Err(BridgeError::ConnectionFailed(format!(
                    "Owned executable node {node_id} ({}) exited: {status}",
                    process.id()
                )));
            }
        }
        Ok(())
    }

    /// The daemon never spawns model binaries. Each original executable node is
    /// registered as dynamic and launched here with an explicit verified config.
    pub fn spawn_nodes(
        &mut self,
        dataflow_id: uuid::Uuid,
        nodes: &[ExecutableNode],
        env: &HashMap<String, String>,
    ) -> BridgeResult<()> {
        if !self.nodes.get_mut().is_empty() {
            return Err(BridgeError::StartFailed(
                "Executable node children already owned".into(),
            ));
        }
        let context = DynamicNodeContext {
            daemon_addr: self.daemon_addr(),
            dataflow_id,
            daemon_pid: self.daemon_pid()?,
        };
        for (index, node) in nodes.iter().enumerate() {
            let config = crate::dynamic_node_endpoint::request_owned_node_config(
                &context,
                node.id.clone().into(),
            )
            .map_err(|error| {
                BridgeError::ConnectionFailed(format!("Owned node {} config: {error:#}", node.id))
            })?;
            let mut command = self.command(&node.program, env);
            command
                .envs(&node.env)
                .env("DORA_NODE_CONFIG", serde_yaml::to_string(&config)?)
                .env("PYTHONUNBUFFERED", "1");
            log_to_files(&mut command, &self.directory, &format!("node-{index}"))?;
            self.nodes.get_mut().push((node.id.clone(),OwnedProcess::spawn(command)?));
        }
        Ok(())
    }

    /// Reap only the groups that this runtime created. On any error, retain all handles.
    pub fn contain(&mut self) -> BridgeResult<()> {
        let mut failures = Vec::new();
        for process in self.nodes.get_mut().iter_mut().map(|(_,process)|process).chain(
            [&mut self.daemon, &mut self.coordinator]
                .into_iter()
                .flatten(),
        ) {
            if let Err(error) = process.terminate() {
                failures.push(error.to_string());
            }
        }
        self.reservations.clear();
        if !failures.is_empty() {
            return Err(BridgeError::StopFailed(failures.join("; ")));
        }
        self.nodes.get_mut().clear();
        self.daemon = None;
        self.coordinator = None;
        self.started = false;
        Ok(())
    }
}

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.contain() {
            tracing::error!("private runtime cleanup failed: {error}");
        }
    }
}

fn log_to_files(command: &mut Command, directory: &Path, name: &str) -> BridgeResult<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(
            directory.join(format!("{name}.stdout.log")),
        )?))
        .stderr(Stdio::from(File::create(
            directory.join(format!("{name}.stderr.log")),
        )?));
    Ok(())
}

fn wait_for_owned_ports(
    process: &mut OwnedProcess,
    required: &[SocketAddr],
    directory: &Path,
) -> BridgeResult<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = process.poll()? {
            return Err(BridgeError::StartFailed(format!(
                "private Dora service exited before readiness: {status}"
            )));
        }
        let mut command = Command::new("/usr/sbin/lsof");
        command
            .args(["-nP", "-a", "-p"])
            .arg(process.id().to_string())
            .args(["-iTCP", "-sTCP:LISTEN", "-Fn"]);
        let output = run_bounded(command, directory, Duration::from_secs(2))?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(BridgeError::StartFailed(
                "cannot verify private service socket ownership".into(),
            ));
        }
        let mut listening = HashSet::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(address) = line.strip_prefix('n') {
                let address: SocketAddr = address.parse().map_err(|_| {
                    BridgeError::StartFailed(format!("unexpected service listener: {address}"))
                })?;
                if address.ip() != Ipv4Addr::LOCALHOST
                    || [6012, 53290, 53291, 5456, 7447].contains(&address.port())
                {
                    return Err(BridgeError::StartFailed(format!(
                        "service bound a non-private endpoint: {address}"
                    )));
                }
                listening.insert(address);
            }
        }
        if required.iter().all(|address| listening.contains(address)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(BridgeError::Timeout(
                "private service did not bind its owned endpoints".into(),
            ));
        }
        thread::sleep(Duration::from_millis(30));
    }
}
