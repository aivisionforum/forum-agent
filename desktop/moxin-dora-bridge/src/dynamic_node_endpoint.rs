//! Explicit-daemon adapter for an exclusively owned Forum runtime.
//!
//! Unlike `DoraNode::init_from_node_id`, this never chooses a default port. It
//! uses the official dora-node-api 0.4.1 / dora-message 0.7.0 request types and
//! framing. It only fetches a bounded, identity-checked config. `DoraNode::init`
//! still has upstream unbounded registration calls. Production retains each
//! worker handle and tears down its owned daemon to unblock failed shutdowns.

use anyhow::{anyhow, bail, ensure, Context, Result};
use dora_message::{
    daemon_to_node::{DaemonCommunication, DaemonReply, NodeConfig},
    id::NodeId,
    node_to_daemon::{DaemonRequest, Timestamped},
    DataflowId,
};
use dora_node_api::dora_core::uhlc::HLC;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const MAX_REPLY_BYTES: usize = 1024 * 1024;

/// Fetch a dynamic node's config from one explicit loopback daemon.
///
/// The caller must already own `expected_dataflow_id`. A NodeConfig request
/// contains only a node ID; checking the returned flow is essential when names
/// collide. Dora allocates a separate event TCP port for each node, so the
/// reply must stay on the requested loopback IP, but need not reuse its port.
/// The spike supervisor separately verifies that the owned daemon PID listens
/// on that returned port before allowing node initialization.
pub fn request_dynamic_node_config(
    daemon_addr: SocketAddr,
    expected_dataflow_id: DataflowId,
    node_id: NodeId,
    timeout: Duration,
) -> Result<NodeConfig> {
    ensure!(
        daemon_addr.ip().is_loopback() && daemon_addr.port() != 0,
        "an explicit loopback daemon endpoint is required"
    );
    ensure!(
        !expected_dataflow_id.is_nil(),
        "expected dataflow UUID must not be nil"
    );
    ensure!(
        !node_id.as_ref().is_empty() && node_id.as_ref().len() <= 128,
        "node ID must contain 1..128 bytes"
    );
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(30),
        "request timeout must be in (0, 30s]"
    );
    let deadline = Instant::now() + timeout;
    let mut stream = TcpStream::connect_timeout(&daemon_addr, timeout)
        .with_context(|| format!("connect explicit daemon {daemon_addr}"))?;
    stream.set_nodelay(true)?;
    let request = Timestamped {
        inner: DaemonRequest::NodeConfig {
            node_id: node_id.clone(),
        },
        timestamp: HLC::default().new_timestamp(),
    };
    let serialized = bincode::serialize(&request)?;
    write_before(
        &mut stream,
        &(serialized.len() as u64).to_le_bytes(),
        deadline,
    )?;
    write_before(&mut stream, &serialized, deadline)?;
    let mut length = [0_u8; 8];
    read_before(&mut stream, &mut length, deadline)?;
    let length = u64::from_le_bytes(length);
    ensure!(
        length > 0 && length <= MAX_REPLY_BYTES as u64,
        "invalid NodeConfig reply size: {length}"
    );
    let mut body = vec![0; length as usize];
    read_before(&mut stream, &mut body, deadline)?;
    let config = match serde_json::from_slice::<DaemonReply>(&body)
        .context("decode NodeConfig JSON reply")?
    {
        DaemonReply::NodeConfig { result: Ok(config) } => config,
        DaemonReply::NodeConfig { result: Err(error) } => {
            bail!("daemon refused NodeConfig: {error}")
        }
        other => bail!("unexpected daemon reply: {other:?}"),
    };
    ensure!(
        config.dataflow_id == expected_dataflow_id,
        "dataflow UUID mismatch: expected {expected_dataflow_id}, received {}",
        config.dataflow_id
    );
    ensure!(
        config.node_id == node_id,
        "node ID mismatch: expected {node_id}, received {}",
        config.node_id
    );
    ensure!(config.dynamic, "daemon returned a non-dynamic node config");
    ensure!(
        config.write_events_to.is_none(),
        "unexpected event-file write requested by daemon"
    );
    match &config.daemon_communication {
        Some(DaemonCommunication::Tcp { socket_addr })
            if socket_addr.ip() == daemon_addr.ip() && socket_addr.port() != 0 => {}
        other => {
            bail!("NodeConfig redirected daemon communication away from the requested loopback IP: {other:?}")
        }
    }
    Ok(config)
}

/// Production bridges fail closed without an explicitly owned context.
/// The registration step belongs to a retained worker thread; it must not run
/// on the UI thread. A caller may not replace the worker after a join timeout.
pub fn init_from_shared(
    shared: Option<&crate::shared_state::SharedDoraState>,
    node_id: NodeId,
) -> Result<(dora_node_api::DoraNode, dora_node_api::EventStream)> {
    let context = shared
        .and_then(|state| state.dynamic_node_context.read().clone())
        .ok_or_else(|| anyhow!("No owned Dora endpoint; refusing default-port connection"))?;
    let config = request_owned_node_config(&context, node_id)?;
    dora_node_api::DoraNode::init(config).map_err(|error| anyhow!("Dora registration: {error:#}"))
}

/// Resolve the same verified config for directly supervised binary nodes.
pub fn request_owned_node_config(
    context: &crate::owned_runtime::DynamicNodeContext,
    node_id: NodeId,
) -> Result<NodeConfig> {
    let config = request_dynamic_node_config(
        context.daemon_addr,
        context.dataflow_id,
        node_id,
        Duration::from_secs(2),
    )?;
    let Some(DaemonCommunication::Tcp { socket_addr }) = &config.daemon_communication else {
        bail!("Owned dynamic nodes require TCP communication");
    };
    verify_event_listener(context.daemon_pid, *socket_addr)?;
    Ok(config)
}

fn verify_event_listener(pid: u32, address: SocketAddr) -> Result<()> {
    use std::{fs, os::unix::fs::DirBuilderExt, process::Command};
    ensure!(
        pid > 1 && address.ip().is_loopback(),
        "invalid owned daemon identity"
    );
    let directory = std::env::temp_dir().join(format!("forum-endpoint-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let result = (|| {
        let mut command = Command::new("/usr/sbin/lsof");
        command.args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ]);
        let output =
            crate::owned_process::run_bounded(command, &directory, Duration::from_secs(2))?;
        let expected = format!("n{address}");
        ensure!(
            output.status.success()
                && std::str::from_utf8(&output.stdout)?
                    .lines()
                    .any(|line| line == expected),
            "dynamic-node event port is not owned by the expected daemon PID"
        );
        Ok(())
    })();
    let _ = fs::remove_dir(&directory);
    result
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| anyhow!("NodeConfig request deadline expired"))
}

fn write_before(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        let count = stream
            .write(bytes)
            .context("write NodeConfig request before deadline")?;
        ensure!(count > 0, "daemon closed while writing NodeConfig request");
        bytes = &bytes[count..];
    }
    Ok(())
}

fn read_before(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let count = stream
            .read(bytes)
            .context("read NodeConfig reply before deadline")?;
        ensure!(
            count > 0,
            "daemon closed before completing NodeConfig reply"
        );
        bytes = &mut bytes[count..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dora_message::config::NodeRunConfig;
    use std::{net::TcpListener, thread};

    fn config(addr: SocketAddr, flow: DataflowId) -> NodeConfig {
        NodeConfig {
            dataflow_id: flow,
            node_id: "shared-node".parse().unwrap(),
            run_config: NodeRunConfig {
                inputs: Default::default(),
                outputs: Default::default(),
            },
            daemon_communication: Some(DaemonCommunication::Tcp { socket_addr: addr }),
            dataflow_descriptor: serde_yaml::Value::Null,
            dynamic: true,
            write_events_to: None,
        }
    }

    fn with_reply(
        reply: impl FnOnce(SocketAddr) -> Vec<u8> + Send + 'static,
        flow: DataflowId,
    ) -> Result<NodeConfig> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut length = [0; 8];
            socket.read_exact(&mut length).unwrap();
            let mut request = vec![0; u64::from_le_bytes(length) as usize];
            socket.read_exact(&mut request).unwrap();
            let request: Timestamped<DaemonRequest> = bincode::deserialize(&request).unwrap();
            assert!(
                matches!(request.inner, DaemonRequest::NodeConfig { node_id } if node_id.as_ref() == "shared-node")
            );
            let _ = socket.write_all(&reply(addr));
        });
        let result = request_dynamic_node_config(
            addr,
            flow,
            "shared-node".parse().unwrap(),
            Duration::from_secs(1),
        );
        server.join().unwrap();
        result
    }

    fn frame(config: NodeConfig) -> Vec<u8> {
        let body = serde_json::to_vec(&DaemonReply::NodeConfig { result: Ok(config) }).unwrap();
        let mut frame = (body.len() as u64).to_le_bytes().to_vec();
        frame.extend(body);
        frame
    }

    #[test]
    fn official_wire_format_accepts_only_matching_identity() {
        let flow = DataflowId::new_v4();
        assert_eq!(
            with_reply(move |addr| frame(config(addr, flow)), flow)
                .unwrap()
                .dataflow_id,
            flow
        );
        let other = DataflowId::new_v4();
        assert!(with_reply(move |addr| frame(config(addr, other)), flow)
            .unwrap_err()
            .to_string()
            .contains("UUID mismatch"));
        assert!(with_reply(
            move |addr| {
                let mut reply = config(addr, flow);
                reply.node_id = "other-node".parse().unwrap();
                frame(reply)
            },
            flow
        )
        .unwrap_err()
        .to_string()
        .contains("node ID mismatch"));
    }

    #[test]
    fn rejects_oversized_or_invalid_json_reply() {
        let flow = DataflowId::new_v4();
        assert!(with_reply(
            |_| (MAX_REPLY_BYTES as u64 + 1).to_le_bytes().to_vec(),
            flow
        )
        .unwrap_err()
        .to_string()
        .contains("reply size"));
        assert!(with_reply(
            |_| {
                let mut data = 1_u64.to_le_bytes().to_vec();
                data.push(b'!');
                data
            },
            flow
        )
        .is_err());
    }

    #[test]
    fn rejects_redirected_daemon_endpoint() {
        let flow = DataflowId::new_v4();
        assert!(with_reply(
            move |addr| {
                let mut reply = config(addr, flow);
                reply.daemon_communication = Some(DaemonCommunication::Tcp {
                    socket_addr: "192.0.2.1:53291".parse().unwrap(),
                });
                frame(reply)
            },
            flow
        )
        .unwrap_err()
        .to_string()
        .contains("redirected"));
    }

    #[test]
    fn rejects_nonloopback_without_connecting() {
        assert!(request_dynamic_node_config(
            "192.0.2.1:53291".parse().unwrap(),
            DataflowId::new_v4(),
            "shared-node".parse().unwrap(),
            Duration::from_secs(1)
        )
        .unwrap_err()
        .to_string()
        .contains("loopback"));
    }

    #[test]
    fn stalled_reply_has_a_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let start = Instant::now();
        assert!(request_dynamic_node_config(
            addr,
            DataflowId::new_v4(),
            "shared-node".parse().unwrap(),
            Duration::from_millis(50)
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }
}
