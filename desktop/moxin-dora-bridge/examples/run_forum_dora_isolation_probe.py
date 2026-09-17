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
    args = parser.parse_args()
    for executable in (args.dora, args.node):
        if not executable.is_absolute() or not executable.is_file():
            raise ValueError("explicit absolute executable paths are required")
    run_dir = args.output.resolve() / str(uuid.uuid4())
    run_dir.mkdir(parents=True)
    children, handles, reservations, instances = [], [], [], []
    report = {"scope": "two private timer-only Dora instances; no production controller, models or audio", "run_dir": str(run_dir), "passed": False}

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
            try:
                result = subprocess.run(command, cwd=instance["work"], env=instance["env"], capture_output=True, text=True, timeout=3)
                instance["stop_acknowledged"] = result.returncode == 0
                evidence = {"command": command, "returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            except subprocess.TimeoutExpired as error:
                # subprocess.run kills and waits for this owned CLI child on timeout.
                # Keep testing delivery/isolation, but never report an acknowledged stop.
                instance["stop_acknowledged"] = False
                evidence = {"command": command, "timeout_seconds": 3, "stdout": str(error.stdout), "stderr": str(error.stderr)}
            with (instance["work"] / "cli.log").open("a") as log:
                log.write(json.dumps(evidence) + "\n")

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
            env.update({"ZENOH_CONFIG": str(zenoh), "RUST_LOG": "info", "NO_COLOR": "1"})
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
        a["node"].wait(timeout=6)
        if a["node"].returncode != 0 or not any(row["event"] == "stop" for row in rows(a)):
            raise RuntimeError("A did not receive Stop and exit cleanly")
        a["node_stopped_cleanly"] = True
        before = len([row for row in rows(b) if row["event"] == "tick"])
        await_condition(lambda: len([row for row in rows(b) if row["event"] == "tick"]) >= before + 5, seconds=4)
        listed_b = cli(b, "list", "--format", "json")
        b_flows = [json.loads(line) for line in listed_b.stdout.splitlines() if line.strip()]
        if not any(flow["uuid"] == b["flow"] and flow["status"] == "Running" for flow in b_flows) or b["node"].poll() is not None:
            raise RuntimeError("B was affected by stopping A")
        report["b_ticks_after_a_stopped"] = len([row for row in rows(b) if row["event"] == "tick"]) - before
        stop(b)
        b["node"].wait(timeout=6)
        if b["node"].returncode != 0 or not any(row["event"] == "stop" for row in rows(b)):
            raise RuntimeError("B did not stop cleanly")
        b["node_stopped_cleanly"] = True
        report["isolation_behaviors_passed"] = True
        report["passed"] = all(item.get("stop_acknowledged") for item in instances)
        if not report["passed"]:
            report["unresolved"] = "Stop reached both nodes and they exited, but Dora CLI did not confirm flow completion within 3 seconds. Full lifecycle acceptance remains unpassed."
    except BaseException as error:
        report["error"] = repr(error)
    finally:
        for instance in instances:
            try:
                stop(instance)
            except BaseException as error:
                report.setdefault("cleanup_errors", []).append(repr(error))
        for name, child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=3)
        for reservation in reservations:
            reservation.close()
        for handle in handles:
            handle.close()
        report["instances"] = [{"label": item["label"], "flow": item.get("flow"), "daemon_lookup_port": item["daemon_port"], "control_port": item["control_port"], "sockets": item.get("socket_evidence"), "stop_acknowledged": item.get("stop_acknowledged", False), "node_stopped_cleanly": item.get("node_stopped_cleanly", False)} for item in instances]
        report["owned_children"] = [{"name": name, "pid": child.pid, "returncode": child.poll()} for name, child in children]
        report["all_owned_children_exited"] = all(child.poll() is not None for _, child in children)
        if not report["all_owned_children_exited"] or report.get("cleanup_errors"):
            report["passed"] = False
        (run_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
