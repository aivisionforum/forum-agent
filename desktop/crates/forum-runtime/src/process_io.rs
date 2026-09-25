//! Bounded JSONL child adapter. Child inherits its owner's process group.
use anyhow::{ensure, Context};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    os::unix::io::AsRawFd,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{sync_channel, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub struct JsonLineProcess {
    child: Child,
    input: ChildStdin,
    output: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    max_line: usize,
}
impl JsonLineProcess {
    pub fn spawn(mut command: Command, max_line: usize) -> anyhow::Result<Self> {
        ensure!(
            max_line > 0 && max_line <= 16 * 1024 * 1024,
            "invalid adapter frame limit"
        );
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let input = child.stdin.take().context("adapter stdin")?;
        let stdout = child.stdout.take().context("adapter stdout")?;
        let (tx, rx) = sync_channel(8);
        let reader = thread::spawn(move || {
            let mut stream = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result = (|| -> anyhow::Result<Value> {
                    loop {
                        let available = stream.fill_buf()?;
                        ensure!(!available.is_empty(), "adapter stdout closed");
                        let count = available
                            .iter()
                            .position(|b| *b == b'\n')
                            .map(|i| i + 1)
                            .unwrap_or(available.len());
                        ensure!(line.len() + count <= max_line, "adapter line exceeds limit");
                        let done = available[count - 1] == b'\n';
                        line.extend_from_slice(&available[..count]);
                        stream.consume(count);
                        if done {
                            break;
                        }
                    }
                    Ok(serde_json::from_slice(&line)?)
                })()
                .map_err(|e| e.to_string());
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output: rx,
            reader: Some(reader),
            max_line,
        })
    }
    pub fn receive(&self, timeout: Duration) -> anyhow::Result<Value> {
        self.output
            .recv_timeout(timeout)
            .context("adapter output deadline or closed reader")?
            .map_err(anyhow::Error::msg)
    }
    /// Poll without treating a quiet, still-running inference as failure.
    pub fn poll(&self, timeout: Duration) -> anyhow::Result<Option<Value>> {
        match self.output.recv_timeout(timeout) {
            Ok(value) => value.map(Some).map_err(anyhow::Error::msg),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn request(&mut self, value: &Value, timeout: Duration) -> anyhow::Result<Value> {
        let deadline = Instant::now() + timeout;
        self.send(value, timeout)?;
        self.receive(deadline.checked_duration_since(Instant::now()).context("adapter response deadline")?)
    }
    pub fn send(&mut self, value: &Value, timeout: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() <= self.max_line,
            "adapter request exceeds limit"
        );
        let fd = self.input.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(flags >= 0, "adapter stdin flags");
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
            "adapter nonblocking stdin"
        );
        let result = (|| -> anyhow::Result<()> {
            let mut offset = 0;
            while offset < bytes.len() {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .context("adapter write deadline")?;
                let mut p = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                ensure!(
                    unsafe {
                        libc::poll(
                            &mut p,
                            1,
                            remaining.as_millis().min(i32::MAX as u128) as i32,
                        )
                    } > 0,
                    "adapter write timed out"
                );
                let count = unsafe {
                    libc::write(
                        fd,
                        bytes[offset..].as_ptr() as *const _,
                        bytes.len() - offset,
                    )
                };
                if count < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        continue;
                    }
                    return Err(error.into());
                }
                ensure!(count > 0, "adapter stdin closed");
                offset += count as usize;
            }
            Ok(())
        })();
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        result
    }
}
impl Drop for JsonLineProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Disconnect before joining: a full bounded channel may have blocked
        // the reader in send(), and draining buffered stdout can take seconds.
        let (_, disconnected) = sync_channel(1);
        drop(std::mem::replace(&mut self.output, disconnected));
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.reader.as_ref().is_some_and(|h| !h.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if self.reader.as_ref().is_some_and(|h| h.is_finished()) {
            let _ = self.reader.take().unwrap().join();
        }
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    #[test]
    fn streaming_burst_survives_bounded_backpressure() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "i=0; while [ $i -lt 80 ]; do echo '{\"type\":\"partial\"}'; i=$((i+1)); done; echo '{\"type\":\"complete\"}'"]);
        let process = JsonLineProcess::spawn(cmd, 1024).unwrap();
        thread::sleep(Duration::from_millis(40));
        for _ in 0..80 { assert_eq!(process.receive(Duration::from_secs(2)).unwrap()["type"], "partial"); }
        assert_eq!(process.receive(Duration::from_secs(2)).unwrap()["type"], "complete");
    }
    #[test]
    fn drop_reaps_child_even_when_stream_receiver_stops_consuming() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "while :; do echo '{}'; done"]);
        let process = JsonLineProcess::spawn(cmd, 1024).unwrap();
        let pid = process.child.id();
        thread::sleep(Duration::from_millis(40));
        let started = Instant::now();
        drop(process);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
    }
}
