//! Real native Dora, production dispatcher/controller, synthetic text only.
//! Run with FORUM_AGENT_DORA_BIN set to a verified native Dora 0.4.1 binary.
use anyhow::{anyhow, ensure, Context, Result};
use arrow::array::StringArray;
use dora_node_api::{DoraNode, Event, Parameter};
use moxin_dora_bridge::{BridgeState, DataflowController, DynamicNodeDispatcher};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    net::{SocketAddr, TcpStream},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn synthetic_node() -> Result<()> {
    let label = std::env::var("PROBE_LABEL")?;
    let stubborn = std::env::var("PROBE_STUBBORN").as_deref() == Ok("1");
    if stubborn {
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    let (mut node, mut events) = DoraNode::init_from_env().map_err(|error| anyhow!("{error:#}"))?;
    println!(
        "{}",
        json!({"event":"synthetic_attached","pid":std::process::id(),"ppid":unsafe{libc::getppid()},"pgid":unsafe{libc::getpgrp()},"flow":node.dataflow_id(),"node":node.id(),"label":label})
    );
    std::io::stdout().flush()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut sequence = 0i64;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_secs(1)) {
            Some(Event::Input { .. }) => {
                sequence += 1;
                let metadata: BTreeMap<String, Parameter> = [
                    (
                        "session_status".into(),
                        Parameter::String("complete".into()),
                    ),
                    ("commit_id".into(), Parameter::Integer(sequence)),
                    (
                        "source_text".into(),
                        Parameter::String(format!("synthetic-{label}-{sequence}")),
                    ),
                ]
                .into_iter()
                .collect();
                if let Err(error) = node.send_output(
                    "translation".to_string().into(),
                    metadata,
                    StringArray::from(vec![format!("translation-{label}-{sequence}")]),
                ) {
                    eprintln!("synthetic {label} output error: {error:#}");
                    break;
                }
            }
            Some(Event::Stop(_)) => break,
            Some(Event::Error(error)) => {
                eprintln!("synthetic {label} event error: {error}");
                break;
            }
            None => thread::sleep(Duration::from_millis(20)),
            _ => {}
        }
    }
    println!(
        "{}",
        json!({"event":"synthetic_loop_ended","label":label,"sequence":sequence,"stubborn":stubborn})
    );
    std::io::stdout().flush()?;
    if stubborn {
        // Keep the owned process alive even after daemon failure so containment
        // must demonstrably reap this directly held child (TERM is ignored).
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }
    drop(events);
    drop(node);
    Ok(())
}

fn graph(path: &Path, executable: &Path, label: &str, stubborn: bool) -> Result<()> {
    let graph = json!({"nodes":[
        {"id":"synthetic-translator","path":executable,"inputs":{"tick":"dora/timer/millis/100"},"outputs":["translation"],"env":{"PROBE_SYNTHETIC":"1","PROBE_LABEL":label,"PROBE_STUBBORN":if stubborn{"1"}else{"0"}}},
        {"id":"moxin-translation-listener","path":"dynamic","inputs":{"translation":"synthetic-translator/translation"}}
    ]});
    fs::write(path, serde_yaml::to_string(&graph)?)?;
    Ok(())
}

fn count(dispatcher: &DynamicNodeDispatcher, label: &str) -> Result<u64> {
    let snapshot = dispatcher.shared_state().translation.read();
    let Some(update) = snapshot.as_ref() else {
        return Ok(0);
    };
    ensure!(
        update.history.iter().all(|entry| entry
            .source_text
            .starts_with(&format!("synthetic-{label}-"))
            && entry
                .translation
                .starts_with(&format!("translation-{label}-"))),
        "cross-instance text contamination"
    );
    Ok(update.completed_count)
}
fn wait_count(dispatcher: &DynamicNodeDispatcher, label: &str, minimum: u64) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let value = count(dispatcher, label)?;
        if value >= minimum {
            return Ok(value);
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic {label} text did not reach listener: {value}/{minimum}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}
fn pid_gone(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

fn listening_ports(pids: &[u32], directory: &Path) -> Result<Vec<SocketAddr>> {
    let mut result = BTreeSet::new();
    for pid in pids {
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
        let output = moxin_dora_bridge::owned_process::run_bounded(
            command,
            directory,
            Duration::from_secs(2),
        )?;
        ensure!(
            output.status.success() || output.status.code() == Some(1),
            "owned PID socket lookup failed"
        );
        for line in std::str::from_utf8(&output.stdout)?.lines() {
            if let Some(address) = line.strip_prefix('n') {
                let address: SocketAddr = address.parse()?;
                ensure!(
                    address.ip().is_loopback()
                        && ![6012, 53290, 53291, 5456, 7447].contains(&address.port()),
                    "non-private listener {address}"
                );
                result.insert(address);
            }
        }
    }
    Ok(result.into_iter().collect())
}
fn all_ports_closed(addresses: &[SocketAddr]) -> bool {
    addresses
        .iter()
        .all(|address| TcpStream::connect_timeout(address, Duration::from_millis(200)).is_err())
}

fn child_exit_probe(directory: &Path, executable: &Path) -> Result<Value> {
    graph(&directory.join("c.yml"), executable, "C", false)?;
    let mut c = DynamicNodeDispatcher::new(DataflowController::new(directory.join("c.yml"))?);
    c.start()?;
    wait_count(&c, "C", 3)?;
    let pids = c.controller().read().owned_process_pids();
    let ports = listening_ports(&pids, directory)?;
    let node_pid = c.controller().read().owned_node_pids()[0];
    ensure!(
        unsafe { libc::kill(node_pid as i32, libc::SIGKILL) } == 0,
        "cannot inject owned child exit"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let failure = loop {
        match c.controller().read().get_status() {
            Err(error) => break error.to_string(),
            Ok(_) => ensure!(Instant::now() < deadline, "child exit was not detected"),
        }
        thread::sleep(Duration::from_millis(20));
    };
    ensure!(
        failure.contains("Owned executable node") && failure.contains("exited"),
        "child failure masked by another error: {failure}"
    );
    c.stop()?;
    let shutdown = c
        .controller()
        .read()
        .last_shutdown_report()
        .context("C stop report")?;
    ensure!(
        shutdown.contained && pids.iter().all(|pid| pid_gone(*pid)) && all_ports_closed(&ports),
        "child failure runtime leaked"
    );
    Ok(
        json!({"injected_child_pid":node_pid,"all_owned_pids":pids,"listening_addresses":ports,"health_error":failure,"shutdown":shutdown,"all_owned_pids_absent":true,"all_recorded_listening_ports_closed":true}),
    )
}

fn run(directory: &Path) -> Result<Value> {
    let executable = std::env::current_exe()?.canonicalize()?;
    graph(&directory.join("a.yml"), &executable, "A", false)?;
    graph(&directory.join("b.yml"), &executable, "B", true)?;
    let mut a = DynamicNodeDispatcher::new(DataflowController::new(directory.join("a.yml"))?);
    let mut b = DynamicNodeDispatcher::new(DataflowController::new(directory.join("b.yml"))?);
    a.start().context("start A")?;
    b.start().context("start B")?;
    let ca = a.controller().read().dynamic_node_context()?;
    let cb = b.controller().read().dynamic_node_context()?;
    ensure!(
        ca.dataflow_id != cb.dataflow_id
            && ca.daemon_pid != cb.daemon_pid
            && ca.daemon_addr != cb.daemon_addr,
        "private instances share identity"
    );
    ensure!(
        a.get_bridge("moxin-translation-listener").unwrap().state() == BridgeState::Connected
            && b.get_bridge("moxin-translation-listener").unwrap().state()
                == BridgeState::Connected,
        "production bridges disconnected"
    );
    let pids_a = a.controller().read().owned_process_pids();
    let pids_b = b.controller().read().owned_process_pids();
    let ports_a = listening_ports(&pids_a, directory)?;
    let ports_b = listening_ports(&pids_b, directory)?;
    ensure!(
        pids_a.len() == 3 && pids_b.len() == 3 && ports_a.len() >= 5 && ports_b.len() >= 5,
        "missing owned process/listener evidence"
    );
    ensure!(
        ports_a.iter().all(|p| !ports_b.contains(p)),
        "instances share listening endpoint"
    );
    let nodes_a = a.controller().read().owned_node_pids();
    let nodes_b = b.controller().read().owned_node_pids();
    ensure!(
        nodes_a.len() == 1 && nodes_b.len() == 1,
        "executable node ownership not retained"
    );
    let runtime_a = a.controller().read().runtime_directory().to_path_buf();
    let runtime_b = b.controller().read().runtime_directory().to_path_buf();
    let count_a = wait_count(&a, "A", 5)?;
    let count_b = wait_count(&b, "B", 5)?;
    let attached = |directory: &Path| -> Result<Value> {
        let text = fs::read_to_string(directory.join("node-0.stdout.log"))?;
        let receipt: Value = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|v| v["event"] == "synthetic_attached")
            .context("missing synthetic identity receipt")?;
        ensure!(
            receipt["ppid"] == std::process::id(),
            "node was not directly spawned by this controller process"
        );
        ensure!(
            receipt["pid"] == receipt["pgid"],
            "node lacks a separately owned group"
        );
        Ok(receipt)
    };
    let receipt_a = attached(&runtime_a)?;
    let receipt_b = attached(&runtime_b)?;
    let started = Instant::now();
    a.stop().context("stop A")?;
    let stop_a_ms = started.elapsed().as_millis();
    let stop_a = a
        .controller()
        .read()
        .last_shutdown_report()
        .context("A shutdown report")?;
    ensure!(stop_a.contained, "A runtime not contained");
    ensure!(
        !stop_a.acknowledged,
        "unexpected pure dynamic CLI acknowledgement; review upstream behavior"
    );
    ensure!(
        a.get_bridge("moxin-translation-listener").unwrap().state() == BridgeState::Disconnected,
        "A listener join not completed"
    );
    ensure!(
        pids_a.iter().all(|pid| pid_gone(*pid)) && all_ports_closed(&ports_a),
        "A owned PID/listener remains"
    );
    let before_peer = count(&b, "B")?;
    let after_peer = wait_count(&b, "B", before_peer + 5)?;
    ensure!(
        b.controller().read().get_status()?.state.is_running(),
        "B stopped with A"
    );
    // The PID comes from this still-owned runtime; no process-name discovery.
    ensure!(
        unsafe { libc::kill(cb.daemon_pid as i32, libc::SIGKILL) } == 0,
        "cannot inject own daemon crash"
    );
    thread::sleep(Duration::from_millis(150));
    ensure!(
        !pid_gone(nodes_b[0]),
        "stubborn synthetic exited before containment test"
    );
    let started = Instant::now();
    b.stop().context("stop B after daemon crash")?;
    let stop_b_ms = started.elapsed().as_millis();
    let stop_b = b
        .controller()
        .read()
        .last_shutdown_report()
        .context("B shutdown report")?;
    ensure!(
        !stop_b.acknowledged && stop_b.contained,
        "daemon-crash fallback was not reported accurately"
    );
    ensure!(
        pids_b.iter().all(|pid| pid_gone(*pid)) && all_ports_closed(&ports_b),
        "B owned PID/listener remains"
    );
    ensure!(
        TcpStream::connect_timeout(&ca.daemon_addr, Duration::from_millis(200)).is_err()
            && TcpStream::connect_timeout(&cb.daemon_addr, Duration::from_millis(200)).is_err(),
        "owned lookup port remains open"
    );
    let child_exit = child_exit_probe(directory, &executable)?;
    Ok(
        json!({"passed":true,"child_exit_detection":child_exit,"audio_or_model_used":false,"controller_pid":std::process::id(),"a":{"flow":ca.dataflow_id,"daemon_pid":ca.daemon_pid,"lookup":ca.daemon_addr,"node_pids":nodes_a,"all_owned_pids":pids_a,"listening_addresses":ports_a,"node_identity":receipt_a,"runtime_dir":runtime_a,"received":count_a,"shutdown_ms":stop_a_ms,"shutdown":stop_a},"b":{"flow":cb.dataflow_id,"daemon_pid":cb.daemon_pid,"lookup":cb.daemon_addr,"node_pids":nodes_b,"all_owned_pids":pids_b,"listening_addresses":ports_b,"node_identity":receipt_b,"runtime_dir":runtime_b,"received_before_a_stop":count_b,"count_after_a_stopped":before_peer,"count_later":after_peer,"daemon_crash_injected":true,"shutdown_ms":stop_b_ms,"shutdown":stop_b},"all_owned_pids_absent":true,"all_recorded_listening_ports_closed":true,"production_listener_joined":true}),
    )
}

fn main() -> Result<()> {
    if std::env::var("PROBE_SYNTHETIC").as_deref() == Ok("1") {
        return synthetic_node();
    }
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: forum_owned_controller_probe ARTIFACT_DIRECTORY")?,
    )
    .canonicalize()?;
    let directory = root.join(uuid::Uuid::new_v4().to_string());
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let result = run(&directory);
    let report = match &result {
        Ok(value) => value.clone(),
        Err(error) => {
            json!({"passed":false,"error":format!("{error:#}"),"cleanup":"dispatcher/controller Drop attempted owned cleanup; inspect logs"})
        }
    };
    fs::write(
        directory.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"artifact_directory":directory,"report":report})
    );
    result.map(|_| ())
}
