use forum_runtime::worker::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
fn directory() -> PathBuf {
    let root = std::env::temp_dir().join(format!("forum-worker-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn python(source: &str) -> Command {
    let mut command = Command::new("python3");
    command.args(["-u", "-c", source]);
    command
}
#[test]
fn immutable_snapshot_hash_and_path_boundaries() {
    let root = directory();
    let attempt = attempt_directory(&root, uuid::Uuid::new_v4(), 1).unwrap();
    let hash = write_snapshot(&attempt, b"{\"text\":\"synthetic\"}").unwrap();
    assert_eq!(
        read_result(&attempt, "input.json", Some(&hash)).unwrap(),
        b"{\"text\":\"synthetic\"}"
    );
    assert!(write_snapshot(&attempt, b"different").is_err());
    assert!(read_result(&attempt, "../input.json", None).is_err());
    assert!(read_result(&attempt, "input.json", Some(&"0".repeat(64))).is_err());
    symlink(attempt.join("input.json"), attempt.join("link.json")).unwrap();
    assert!(read_result(&attempt, "link.json", None).is_err());
    let fifo = attempt.join("result.fifo");
    let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let started = Instant::now();
    assert!(read_result(&attempt, "result.fifo", None).is_err());
    assert!(started.elapsed() < Duration::from_millis(100));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_and_oversized_stdout_fail_closed() {
    for output in ["print('not-json')", "print('x'*1048577)"] {
        let root = directory();
        let mut worker = WorkerProcess::spawn(python(output), &root.join("worker.log")).unwrap();
        assert!(worker.poll(Duration::from_secs(3)).is_err());
        worker.stop(Duration::ZERO).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn cooperative_exit_reaps_zombie_without_false_containment_failure() {
    let root = directory();
    let mut worker = WorkerProcess::spawn(
        python("print('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":true}',flush=True)"),
        &root.join("worker.log"),
    )
    .unwrap();
    assert!(worker.poll(Duration::from_secs(3)).unwrap().is_some());
    std::thread::sleep(Duration::from_millis(50));
    worker.stop(Duration::from_secs(1)).unwrap();
    assert_eq!(unsafe { libc::kill(worker.pid() as i32, 0) }, -1);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn incomplete_frame_is_bounded_by_host_deadline_and_owned_teardown() {
    let root = directory();
    let mut worker = WorkerProcess::spawn(
        python("import sys,time\nsys.stdout.write('{');sys.stdout.flush();time.sleep(60)"),
        &root.join("worker.log"),
    )
    .unwrap();
    assert!(worker.poll(Duration::from_millis(100)).unwrap().is_none());
    let started = Instant::now();
    worker.stop(Duration::from_millis(20)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(unsafe { libc::kill(worker.pid() as i32, 0) }, -1);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn crashed_leader_does_not_leave_owned_compute_child_running() {
    let root = directory();
    let source="import os,time,json\npid=os.fork()\nif pid==0:\n time.sleep(60)\nelse:\n print(json.dumps({'jsonrpc':'2.0','id':1,'result':pid}),flush=True)\n os._exit(1)";
    let mut worker = WorkerProcess::spawn(python(source), &root.join("worker.log")).unwrap();
    let child = worker.poll(Duration::from_secs(3)).unwrap().unwrap()["result"]
        .as_i64()
        .unwrap() as i32;
    assert!(child > 1);
    worker.stop(Duration::ZERO).unwrap();
    // Grandchild held stdout after its parent crashed; containment closes it.
    assert!(worker.poll(Duration::from_secs(3)).is_err());
    fs::remove_dir_all(root).unwrap();
}
