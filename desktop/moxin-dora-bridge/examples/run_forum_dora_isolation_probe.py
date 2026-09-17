#!/usr/bin/env python3
"""Own two private Dora instances and timer-only dynamic nodes; never use default ports."""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import uuid

DEFAULT_PORTS = {6012, 53290, 53291, 5456, 7447}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dora", type=Path, required=True)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--shutdown-policy", choices=("strict-cli-ack", "owned-runtime-fallback"), default="strict-cli-ack", help="Select the acceptance criterion explicitly; fallback never reports a CLI acknowledgement")
    args = parser.parse_args()
    for executable in (args.dora, args.node):
        if not executable.is_absolute() or not executable.is_file():
            raise ValueError("explicit absolute executable paths are required")
    run_dir = args.output.resolve() / str(uuid.uuid4())
    run_dir.mkdir(parents=True)
    children, handles, reservations, instances = [], [], [], []
    report = {"scope": "two private timer-only Dora instances; no production controller, models or audio", "run_dir": str(run_dir), "shutdown_policy": args.shutdown_policy, "passed": False}

    def launch(name, command, cwd, env, stdin=None):
        stdout = (cwd / f"{name}.stdout.log").open("w")
        stderr = (cwd / f"{name}.stderr.log").open("w")
        handles.extend((stdout, stderr))
        child = subprocess.Popen(command, cwd=cwd, env=env, stdin=stdin, stdout=stdout, stderr=stderr)
        children.append((name, child))
        return child

    def await_condition(predicate, seconds=8):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = predicate()
            if result:
                return result
            time.sleep(0.05)
        raise TimeoutError("private instance condition did not become ready")

    def endpoints(child):
        if child.poll() is not None:
            raise RuntimeError(f"owned child {child.pid} exited: {child.returncode}")
        found = subprocess.run(["/usr/sbin/lsof", "-nP", "-a", "-p", str(child.pid), "-iTCP", "-sTCP:LISTEN", "-Fn"], capture_output=True, text=True, timeout=3)
        if found.returncode not in (0, 1):
            raise RuntimeError(found.stderr)
        names = [line[1:] for line in found.stdout.splitlines() if line.startswith("n")]
        for endpoint in names:
            host, port = endpoint.rsplit(":", 1)
            if not ipaddress.ip_address(host.strip("[]")).is_loopback or int(port) in DEFAULT_PORTS:
                raise RuntimeError(f"owned process unexpectedly bound non-private endpoint: {endpoint}")
        return names

    def rows(instance):
        path = instance["work"] / "node-events.jsonl"
        if not path.exists():
            return []
        # The child flushes complete rows; tolerate only a temporarily incomplete last line.
        data = path.read_text()
        return [json.loads(line) for line in data.splitlines() if line and (data.endswith("\n") or line != data.splitlines()[-1])]

    def cli(instance, verb, *extra):
        command = [str(args.dora), verb, *extra, "--coordinator-addr", "127.0.0.1", "--coordinator-port", str(instance["control_port"])]
        result = subprocess.run(command, cwd=instance["work"], env=instance["env"], capture_output=True, text=True, timeout=12)
        with (instance["work"] / "cli.log").open("a") as log:
            log.write(json.dumps({"command": command, "returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr}) + "\n")
        if result.returncode:
            raise RuntimeError(f"private Dora {verb} failed: {result.stderr}")
        return result

    def stop(instance):
        if instance.get("flow") and not instance.get("stop_attempted"):
            instance["stop_attempted"] = True
            command = [str(args.dora), "stop", instance["flow"], "--grace-duration", "2s", "--coordinator-addr", "127.0.0.1", "--coordinator-port", str(instance["control_port"])]
            started_at_unix_ms = time.time_ns() // 1_000_000
            started = time.monotonic()
            try:
                result = subprocess.run(command, cwd=instance["work"], env=instance["env"], capture_output=True, text=True, timeout=3)
                instance["stop_acknowledged"] = result.returncode == 0
                evidence = {"command": command, "returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            except subprocess.TimeoutExpired as error:
                # subprocess.run kills and waits for this owned CLI child on timeout.
                # Keep testing delivery/isolation, but never report an acknowledged stop.
                instance["stop_acknowledged"] = False
                evidence = {"command": command, "timeout_seconds": 3, "stdout": str(error.stdout), "stderr": str(error.stderr)}
            evidence["started_at_unix_ms"] = started_at_unix_ms
            evidence["elapsed_ms"] = (time.monotonic() - started) * 1000
            instance["stop_cli_evidence"] = evidence
            with (instance["work"] / "cli.log").open("a") as log:
                log.write(json.dumps(evidence) + "\n")

    def reap_owned(child):
        """Signal only a Popen child that this invocation created and still holds."""
        action = "already_exited"
        if child.poll() is None:
            action = "terminate"
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                action = "kill_after_terminate_timeout"
                child.kill()
                child.wait(timeout=3)
        return {"pid": child.pid, "returncode": child.returncode, "action": action}

    def verify_node_shutdown(instance):
        instance["node"].wait(timeout=6)
        events = {row["event"] for row in rows(instance)}
        required = {"stop", "event_stream_drop_completed", "node_drop_completed"}
        if instance["node"].returncode != 0 or not required.issubset(events):
            raise RuntimeError(f"{instance['label']} did not complete the official Drop protocol and exit cleanly")
        instance["node_stopped_cleanly"] = True
        instance["node_drop_protocol_completed"] = True
        drop_row = next(row for row in rows(instance) if row["event"] == "node_drop_completed")
        instance["node_drop_after_stop_request_ms"] = drop_row["observed_at_unix_ms"] - instance["stop_cli_evidence"]["started_at_unix_ms"]

    def contain_runtime(instance):
        """No daemon-wide destroy or process scan: reap exact private children, then verify."""
        started = time.monotonic()
        owned_endpoints = set(instance["socket_evidence"]["daemon_listeners"])
        owned_endpoints.update(endpoints(instance["coordinator"]))
        evidence = [reap_owned(instance[role]) for role in ("daemon", "coordinator")]
        for endpoint in owned_endpoints:
            host, port = endpoint.rsplit(":", 1)
            with socket.socket() as check:
                check.settimeout(0.2)
                if check.connect_ex((host, int(port))) == 0:
                    raise RuntimeError(f"previous private endpoint is still accepting connections: {endpoint}")
        for role in ("node", "daemon", "coordinator"):
            child = instance[role]
            if child.poll() is None:
                raise RuntimeError(f"owned {role} has not exited")
            try:
                os.kill(child.pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise RuntimeError(f"PID {child.pid} still exists or was reused; containment is unverified")
        instance["contained"] = True
        instance["shutdown_outcome"] = "StoppedGracefully" if instance.get("stop_acknowledged") else "StoppedByOwnedRuntimeFallback"
        instance["containment_evidence"] = {"services": evidence, "closed_endpoints": sorted(owned_endpoints), "duration_ms": (time.monotonic() - started) * 1000}

    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    try:
        version = subprocess.run([str(args.dora), "--version"], capture_output=True, text=True, timeout=8)
        report["dora_version"] = version.stdout
        if version.returncode or "dora-cli 0.4.1" not in version.stdout:
            raise RuntimeError("this spike requires native dora-cli 0.4.1")
        for label in ("a", "b"):
            work = run_dir / label
            work.mkdir()
            ports = []
            for _ in range(3):
                reservation = socket.socket()
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
                if port in DEFAULT_PORTS:
                    raise RuntimeError("OS selected a forbidden default port")
                reservations.append(reservation)
                ports.append((port, reservation))
            coordinator_port, control_port, daemon_port = [port for port, _ in ports]
            zenoh = work / "zenoh.json5"
            zenoh.write_text(json.dumps({"mode": "peer", "connect": {"endpoints": []}, "listen": {"endpoints": ["tcp/127.0.0.1:0"]}, "scouting": {"multicast": {"enabled": False}, "gossip": {"enabled": False}}}))
            env = os.environ.copy()
            for key in ("DORA_NODE_CONFIG", "DORA_TEST_WITH_INPUTS", "DORA_TEST_WRITE_OUTPUTS_TO", "ZENOH_CONFIG_OVERRIDE"):
                env.pop(key, None)
            env.update({"ZENOH_CONFIG": str(zenoh), "RUST_LOG": "info,dora_daemon=trace,dora_coordinator=debug", "NO_COLOR": "1"})
            instance = {"label": label, "work": work, "env": env, "control_port": control_port, "daemon_port": daemon_port}
            instances.append(instance)
            ports[0][1].close()
            ports[1][1].close()
            coordinator = launch("coordinator", [str(args.dora), "coordinator", "--interface", "127.0.0.1", "--port", str(coordinator_port), "--control-interface", "127.0.0.1", "--control-port", str(control_port)], work, env)
            instance["coordinator"] = coordinator
            await_condition(lambda: {f"127.0.0.1:{coordinator_port}", f"127.0.0.1:{control_port}"}.issubset(endpoints(coordinator)))
            ports[2][1].close()
            daemon = launch("daemon", [str(args.dora), "daemon", "--coordinator-addr", "127.0.0.1", "--coordinator-port", str(coordinator_port), "--local-listen-port", str(daemon_port)], work, env)
            instance["daemon"] = daemon
            await_condition(lambda: f"127.0.0.1:{daemon_port}" in endpoints(daemon))
            descriptor = work / "timer.yml"
            descriptor.write_text("communication:\n  _unstable_local: Tcp\nnodes:\n  - id: same-dynamic-node\n    path: dynamic\n    inputs:\n      tick: dora/timer/millis/100\n")
            cli(instance, "start", str(descriptor), "--detach", "--name", f"forum-isolation-{label}")
            listed = cli(instance, "list", "--format", "json")
            flows = [json.loads(line) for line in listed.stdout.splitlines() if line.strip()]
            if len(flows) != 1 or flows[0]["status"] != "Running":
                raise RuntimeError(f"private coordinator must have exactly one running flow: {flows}")
            instance["flow"] = str(uuid.UUID(flows[0]["uuid"]))
            child = launch("node", [str(args.node), f"127.0.0.1:{daemon_port}", instance["flow"], str(work / "node-events.jsonl")], work, env, stdin=subprocess.PIPE)
            instance["node"] = child
            configs = await_condition(lambda: [row for row in rows(instance) if row["event"] == "config_validated"])
            config = configs[0]
            if config["dataflow_id"] != instance["flow"] or config["node_id"] != "same-dynamic-node":
                raise RuntimeError("dynamic NodeConfig identity mismatch")
            if config["event_endpoint"] not in endpoints(daemon):
                raise RuntimeError("node event endpoint is not owned by this daemon PID")
            instance["socket_evidence"] = {"daemon_pid": daemon.pid, "coordinator_pid": coordinator.pid, "daemon_listeners": endpoints(daemon), "event_endpoint": config["event_endpoint"]}
            child.stdin.write(b"CONTINUE\n")
            child.stdin.flush()
            await_condition(lambda: len([row for row in rows(instance) if row["event"] == "tick"]) >= 3)
        a, b = instances
        if a["flow"] == b["flow"]:
            raise RuntimeError("private dataflow UUIDs unexpectedly match")
        stop(a)
        verify_node_shutdown(a)
        contain_runtime(a)
        before = len([row for row in rows(b) if row["event"] == "tick"])
        await_condition(lambda: len([row for row in rows(b) if row["event"] == "tick"]) >= before + 5, seconds=4)
        listed_b = cli(b, "list", "--format", "json")
        b_flows = [json.loads(line) for line in listed_b.stdout.splitlines() if line.strip()]
        if not any(flow["uuid"] == b["flow"] and flow["status"] == "Running" for flow in b_flows) or b["node"].poll() is not None:
            raise RuntimeError("B was affected by stopping A")
        report["b_ticks_after_a_stopped"] = len([row for row in rows(b) if row["event"] == "tick"]) - before
        stop(b)
        verify_node_shutdown(b)
        contain_runtime(b)
        report["isolation_behaviors_passed"] = True
        report["cli_acknowledgement_passed"] = all(item.get("stop_acknowledged") for item in instances)
        report["containment_passed"] = all(item.get("contained") for item in instances)
        report["passed"] = report["containment_passed"] and (report["cli_acknowledgement_passed"] or args.shutdown_policy == "owned-runtime-fallback")
        if not report["cli_acknowledgement_passed"]:
            report["unresolved"] = "Dora 0.4.1 pure-dynamic flows do not produce the spawned-process completion event needed for CLI stop acknowledgement. Acknowledgement remains false; only exact owned children and their listener endpoints were contained."
    except BaseException as error:
        report["error"] = repr(error)
    finally:
        for instance in instances:
            try:
                stop(instance)
            except BaseException as error:
                report.setdefault("cleanup_errors", []).append(repr(error))
        for name, child in reversed(children):
            reap_owned(child)
        for reservation in reservations:
            reservation.close()
        for handle in handles:
            handle.close()
        report["instances"] = [{"label": item["label"], "flow": item.get("flow"), "daemon_lookup_port": item["daemon_port"], "control_port": item["control_port"], "sockets": item.get("socket_evidence"), "acknowledged": item.get("stop_acknowledged", False), "contained": item.get("contained", False), "shutdown_outcome": item.get("shutdown_outcome", "StopFailed"), "node_stopped_cleanly": item.get("node_stopped_cleanly", False), "node_drop_protocol_completed": item.get("node_drop_protocol_completed", False), "node_drop_after_stop_request_ms": item.get("node_drop_after_stop_request_ms"), "stop_cli_elapsed_ms": item.get("stop_cli_evidence", {}).get("elapsed_ms"), "containment_evidence": item.get("containment_evidence")} for item in instances]
        report["owned_children"] = [{"name": name, "pid": child.pid, "returncode": child.poll()} for name, child in children]
        report["all_owned_children_exited"] = all(child.poll() is not None for _, child in children)
        if not report["all_owned_children_exited"] or report.get("cleanup_errors"):
            report["passed"] = False
        (run_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
