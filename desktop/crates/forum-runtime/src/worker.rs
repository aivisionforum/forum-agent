//! Bounded, host-owned analysis sidecar. No database or model selection here.
use anyhow::{bail, ensure, Context};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{sync_channel, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_CONTROL_BYTES: usize = 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

/// Every component below the private root is a regular directory, never a link.
pub fn attempt_directory(root: &Path, job: uuid::Uuid, attempt: u32) -> anyhow::Result<PathBuf> {
    ensure!(attempt > 0, "invalid attempt");
    super::private_directory(root)?;
    let job_dir = root.join(job.to_string());
    super::private_directory(&job_dir)?;
    let path = job_dir.join(attempt.to_string());
    super::private_directory(&path)?;
    Ok(path)
}

pub fn write_snapshot(directory: &Path, bytes: &[u8]) -> anyhow::Result<String> {
    ensure!(bytes.len() <= MAX_ARTIFACT_BYTES, "snapshot exceeds limit");
    let path = directory.join("input.json");
    if path.exists() {
        ensure!(
            read_result(directory, "input.json", None)? == bytes,
            "immutable snapshot conflict"
        );
    } else {
        super::atomic_write(&path, bytes)?;
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Read fixed bytes once; validate these same bytes before the final core commit.
pub fn read_result(
    directory: &Path,
    relative: &str,
    expected: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let relative_path = Path::new(relative);
    ensure!(!relative.is_empty(), "empty result path");
    ensure!(
        relative_path
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "result path escapes attempt"
    );
    let mut current = directory.to_path_buf();
    for component in relative_path.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "result path contains symlink"
        );
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&current)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_ARTIFACT_BYTES as u64,
        "invalid result file"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_ARTIFACT_BYTES, "result exceeds limit");
    if let Some(expected) = expected {
        ensure!(
            expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid result hash"
        );
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == expected,
            "result hash mismatch"
        );
    }
    Ok(bytes)
}

pub struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    // A reaped leader no longer reserves its numeric PID. No subsequent retry
    // may send a signal using that PID/PGID, even if containment is incomplete.
    leader_reaped: bool,
    group_absent: bool,
    stopped: bool,
}

impl WorkerProcess {
    pub fn spawn(mut command: Command, log_path: &Path) -> anyhow::Result<Self> {
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(log_path)?;
        // A new process group contains only this worker and children it creates.
        command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log);
        let mut child = command.spawn().context("start analysis worker")?;
        let stdin = child.stdin.take().context("worker stdin")?;
        let stdout = child.stdout.take().context("worker stdout")?;
        let (tx, messages) = sync_channel(32);
        let reader = thread::spawn(move || {
            let mut stream = BufReader::new(stdout);
            loop {
                let result = (|| -> anyhow::Result<Value> {
                    let mut line = Vec::new();
                    loop {
                        let buffer = stream.fill_buf()?;
                        ensure!(!buffer.is_empty(), "worker stdout closed");
                        let size = buffer
                            .iter()
                            .position(|b| *b == b'\n')
                            .map_or(buffer.len(), |i| i + 1);
                        ensure!(
                            line.len() + size <= MAX_CONTROL_BYTES,
                            "worker frame exceeds 1 MiB"
                        );
                        let done = buffer[size - 1] == b'\n';
                        line.extend_from_slice(&buffer[..size]);
                        stream.consume(size);
                        if done {
                            break;
                        }
                    }
                    let value: Value = serde_json::from_slice(&line)?;
                    ensure!(
                        value.is_object() && value["jsonrpc"] == "2.0",
                        "invalid worker JSON-RPC"
                    );
                    Ok(value)
                })()
                .map_err(|e| e.to_string());
                let failed = result.is_err();
                // An overproducing worker fails closed; stdout never blocks core.
                if tx.try_send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            messages,
            reader: Some(reader),
            leader_reaped: false,
            group_absent: false,
            stopped: false,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send(&mut self, value: &Value, timeout: Duration) -> anyhow::Result<()> {
        ensure!(
            !self.stopped && !self.leader_reaped,
            "worker already exited"
        );
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= MAX_CONTROL_BYTES, "host frame exceeds 1 MiB");
        let fd = self.stdin.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
            "worker pipe flags"
        );
        let deadline = Instant::now() + timeout;
        let result = (|| {
            let mut offset = 0;
            while offset < bytes.len() {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .context("worker write deadline")?;
                let mut p = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let count = unsafe {
                    libc::poll(
                        &mut p,
                        1,
                        remaining.as_millis().min(i32::MAX as u128) as i32,
                    )
                };
                if count < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                ensure!(count > 0, "worker write deadline");
                let count = unsafe {
                    libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset)
                };
                if count < 0 {
                    let e = std::io::Error::last_os_error();
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) {
                        continue;
                    }
                    return Err(e.into());
                }
                ensure!(count > 0, "worker pipe closed");
                offset += count as usize;
            }
            Ok(())
        })();
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
        result
    }

    pub fn poll(&self, timeout: Duration) -> anyhow::Result<Option<Value>> {
        match self.messages.recv_timeout(timeout) {
            Ok(value) => value.map(Some).map_err(anyhow::Error::msg),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("worker protocol reader exited"),
        }
    }

    /// Success means the leader was reaped, the original process group was
    /// observed absent, and every protocol reader finished. Failure preserves
    /// these separate facts so an owner can retry without releasing its lease.
    pub fn stop(&mut self, grace: Duration) -> anyhow::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let group = -(self.child.id() as i32);
        if !self.leader_reaped {
            let _ = self.send(&serde_json::json!({"jsonrpc":"2.0","id":"host-shutdown","method":"shutdown","params":{}}), Duration::from_millis(250));
            let deadline = Instant::now() + grace;
            while !self.reader.as_ref().is_none_or(|r| r.is_finished()) && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(10));
            }
            // The unreaped child, including a zombie, reserves the PID/PGID.
            // This is the only state in which a destructive group signal is safe.
            let rc = unsafe { libc::kill(group, libc::SIGKILL) };
            if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                let error = std::io::Error::last_os_error();
                // Darwin can return EPERM for a group containing only zombies.
                // Reaping is allowed, but does not by itself prove containment.
                if error.raw_os_error() == Some(libc::EPERM) && self.child.try_wait()?.is_some() {
                    self.leader_reaped = true;
                } else {
                    return Err(error.into());
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if !self.leader_reaped && self.child.try_wait()?.is_some() {
                self.leader_reaped = true;
            }
            if self.leader_reaped && !self.group_absent {
                // Signal zero is observation only. Never signal again after
                // reaping: the numeric PGID may already name unrelated work.
                let rc = unsafe { libc::kill(group, 0) };
                if rc == -1 {
                    let error = std::io::Error::last_os_error();
                    match error.raw_os_error() {
                        Some(libc::ESRCH) => self.group_absent = true,
                        Some(libc::EPERM) => (), // existence without permission
                        _ => return Err(error.into()),
                    }
                }
            }
            let reader_finished = self.reader.as_ref().is_none_or(|r| r.is_finished());
            if self.leader_reaped && self.group_absent && reader_finished {
                if let Some(reader) = self.reader.take() {
                    let _ = reader.join();
                }
                self.stopped = true;
                return Ok(());
            }
            ensure!(Instant::now() < deadline,
                "worker containment incomplete: leader_reaped={}, original_group_absent={}, reader_finished={}",
                self.leader_reaped,self.group_absent,reader_finished);
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop(Duration::ZERO);
        }
    }
}

#[cfg(test)]
mod containment_tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("forum-containment-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    fn wait_until(mut test: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !test() {
            assert!(Instant::now() < end, "test readiness timeout");
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn unreaped_group_member_prevents_success_until_entire_group_disappears() {
        let root = Temp::new();
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c","import json,time;print(json.dumps({'jsonrpc':'2.0','method':'ready'}),flush=True);time.sleep(60)"]);
        let mut worker = WorkerProcess::spawn(command, &root.0.join("worker.log")).unwrap();
        assert!(worker.poll(Duration::from_secs(5)).unwrap().is_some());
        // The test owns this auxiliary child, deliberately joins it to the
        // worker group, and retains its zombie until we explicitly reap it.
        // It has no stdout handle, so reader EOF alone cannot prove group exit.
        let mut helper = Command::new("/usr/bin/python3");
        helper
            .args(["-c", "import time;time.sleep(60)"])
            .process_group(worker.pid() as i32)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut helper = OwnedChild(helper.spawn().unwrap());
        let first = worker.stop(Duration::ZERO);
        let was_stopped = worker.stopped;
        let retained_leader_state = worker.leader_reaped;
        // Always reap before assertions, so even a regression leaves no process.
        helper.0.wait().unwrap();
        assert!(
            first.is_err(),
            "a remaining process-group member must not release the owner"
        );
        assert!(!was_stopped);
        assert!(retained_leader_state);
        worker.stop(Duration::ZERO).unwrap();
        assert!(worker.stopped);
        assert!(worker.group_absent);
        assert!(worker.reader.is_none());
    }
    #[test]
    fn outstanding_reader_blocks_success_and_retry_does_not_signal_escaped_child() {
        let root = Temp::new();
        let release = root.0.join("release");
        let ready = root.0.join("reader-ready");
        struct Release(PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = fs::write(&self.0, b"release");
            }
        }
        // A deliberately escaped test descendant keeps the protocol pipe open.
        // The owner must report failure rather than kill an unrelated group.
        let script="import sys,subprocess,time,pathlib\nchild=\"import pathlib,time,sys;p=pathlib.Path(sys.argv[1]);pathlib.Path(sys.argv[2]).write_text('ready');end=time.monotonic()+15\\nwhile not p.exists() and time.monotonic()<end: time.sleep(.01)\"\nsubprocess.Popen([sys.executable,'-c',child,sys.argv[1],sys.argv[2]],start_new_session=True)\nwhile not pathlib.Path(sys.argv[2]).exists():time.sleep(.01)\n";
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", script]).arg(&release).arg(&ready);
        let mut worker = WorkerProcess::spawn(command, &root.0.join("worker.log")).unwrap();
        let release_on_drop = Release(release.clone());
        wait_until(|| ready.is_file());
        let first = worker.stop(Duration::ZERO);
        assert!(first.is_err());
        assert!(worker.leader_reaped);
        assert!(worker.group_absent);
        assert!(!worker.stopped);
        assert!(
            !worker.reader.as_ref().unwrap().is_finished(),
            "escaping stdout holder was not signalled"
        );
        drop(release_on_drop);
        worker.stop(Duration::ZERO).unwrap();
        assert!(worker.stopped);
        assert!(worker.reader.is_none());
    }
}
