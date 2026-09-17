//! No-audio dynamic node used only by run_forum_dora_isolation_probe.py.
//! The parent verifies socket ownership before writing CONTINUE to stdin.
use anyhow::{anyhow, bail, ensure, Result};
use dora_message::{daemon_to_node::DaemonCommunication, DataflowId};
use dora_node_api::{DoraNode, Event};
use moxin_dora_bridge::dynamic_node_endpoint::request_dynamic_node_config;
use std::{
    io::{BufRead, Write},
    net::SocketAddr,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: forum_dynamic_node_probe DAEMON_ADDR FLOW_UUID OUTPUT_JSONL"
    );
    let daemon_addr: SocketAddr = args[0].parse()?;
    ensure!(
        ![6012, 53290, 53291, 5456].contains(&daemon_addr.port()),
        "default Dora ports are forbidden in this spike"
    );
    let flow_id: DataflowId = args[1].parse()?;
    let node_id = "same-dynamic-node".parse()?;
    let config =
        request_dynamic_node_config(daemon_addr, flow_id, node_id, Duration::from_secs(3))?;
    let event_endpoint = match &config.daemon_communication {
        Some(DaemonCommunication::Tcp { socket_addr }) => *socket_addr,
        _ => bail!("spike requires TCP node communication"),
    };
    let mut output = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    let mut record = |value: serde_json::Value| -> Result<()> {
        serde_json::to_writer(&mut output, &value)?;
        writeln!(output)?;
        output.flush()?;
        Ok(())
    };
    record(serde_json::json!({
        "event": "config_validated", "dataflow_id": config.dataflow_id,
        "node_id": config.node_id, "daemon_lookup_endpoint": daemon_addr,
        "event_endpoint": event_endpoint,
    }))?;
    // DoraNode::init itself has no upstream deadline. The parent supervises
    // this whole child process and grants this step only after checking lsof.
    let mut confirmation = String::new();
    std::io::stdin().lock().read_line(&mut confirmation)?;
    ensure!(
        confirmation.trim() == "CONTINUE",
        "parent did not verify daemon socket ownership"
    );
    let (node, mut events) = DoraNode::init(config).map_err(|error| anyhow!("{error:?}"))?;
    ensure!(
        *node.dataflow_id() == flow_id && node.id().as_ref() == "same-dynamic-node",
        "initialized node identity mismatch"
    );
    record(
        serde_json::json!({"event": "attached", "dataflow_id": node.dataflow_id(), "node_id": node.id()}),
    )?;
    let start = Instant::now();
    let mut ticks = 0;
    while start.elapsed() < Duration::from_secs(30) {
        match events.recv_timeout(Duration::from_secs(2)) {
            Some(Event::Input { id, .. }) if id.to_string() == "tick" => {
                ticks += 1;
                record(
                    serde_json::json!({"event": "tick", "dataflow_id": node.dataflow_id(), "sequence": ticks}),
                )?;
            }
            Some(Event::Stop(cause)) => {
                record(
                    serde_json::json!({"event": "stop", "dataflow_id": node.dataflow_id(), "cause": format!("{cause:?}"), "ticks": ticks}),
                )?;
                return Ok(());
            }
            Some(Event::Error(error)) => bail!("Dora event error: {error}"),
            None => bail!("event stream closed before an explicit Stop"),
            _ => {}
        }
    }
    bail!("spike node exceeded 30-second lifecycle deadline")
}
