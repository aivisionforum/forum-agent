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
            stopped: false,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send(&mut self, value: &Value, timeout: Duration) -> anyhow::Result<()> {
        ensure!(!self.stopped, "worker already exited");
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

    /// An RPC acknowledgement is not proof of process exit. Reap the owned child.
    pub fn stop(&mut self, grace: Duration) -> anyhow::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let _ = self.send(&serde_json::json!({"jsonrpc":"2.0","id":"host-shutdown","method":"shutdown","params":{}}), Duration::from_millis(250));
        let deadline = Instant::now() + grace;
        while !self.reader.as_ref().is_some_and(|r| r.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        // Do not reap the leader before containing its group. Even an exited
        // leader retains its PID as our zombie, preventing PID/group reuse.
        // Descendants that kept stdout open after a worker crash are contained too.
        let group = -(self.child.id() as i32);
        let rc = unsafe { libc::kill(group, libc::SIGKILL) };
        if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            let error = std::io::Error::last_os_error();
            // Darwin can return EPERM for a group containing only our zombie
            // leader. Reap it, then require that the entire group is absent.
            // No signal is sent after reaping, when the numeric PID could reuse.
            if error.raw_os_error() == Some(libc::EPERM) && self.child.try_wait()?.is_some() {
                self.stopped = true;
                let absent = unsafe { libc::kill(group, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                ensure!(
                    absent,
                    "worker leader exited but its process group still exists"
                );
            } else {
                return Err(error.into());
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                self.stopped = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        ensure!(self.stopped, "owned worker did not exit after termination");
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.is_finished())
        {
            let _ = self.reader.take().unwrap().join();
        }
        Ok(())
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop(Duration::ZERO);
        }
    }
}
