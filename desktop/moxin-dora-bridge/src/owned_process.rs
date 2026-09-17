//! Bounded child commands and exclusively created Unix process groups.
//! No PID discovery, global process-name matching or shared-service cleanup.
use crate::{BridgeError, BridgeResult};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_COMMAND_BYTES: u64 = 1024 * 1024;

pub struct OwnedProcess {
    child: Child,
    group_id: libc::pid_t,
    status: Option<ExitStatus>,
    contained: bool,
}

impl OwnedProcess {
    /// Each child becomes leader of a new process group before exec.
    pub fn spawn(mut command: Command) -> BridgeResult<Self> {
        command.process_group(0);
        let child = command.spawn()?;
        Ok(Self {
            group_id: child.id() as libc::pid_t,
            child,
            status: None,
            contained: false,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn poll(&mut self) -> BridgeResult<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        if self.status.is_some() && !self.contained && !self.group_exists()? {
            // Once observed gone, never signal this numerical group ID again.
            self.contained = true;
        }
        Ok(self.status)
    }

    fn signal(&self, signal: libc::c_int) -> BridgeResult<()> {
        // The ID is obtained solely from our just-spawned group leader.
        // It is never supplied by a config file, process listing or old PID file.
        let result = unsafe { libc::killpg(self.group_id, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error.into())
        }
    }

    fn group_exists(&self) -> BridgeResult<bool> {
        let result = unsafe { libc::killpg(self.group_id, 0) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            _ => Err(error.into()),
        }
    }

    /// A direct child may have exited while its descendants still run.
    /// Cleanup is complete only when both the child is reaped and its group is gone.
    pub fn terminate(&mut self) -> BridgeResult<()> {
        self.poll()?;
        if self.contained {
            return Ok(());
        }
        for (signal, budget) in [
            (libc::SIGTERM, Duration::from_millis(700)),
            (libc::SIGKILL, Duration::from_secs(2)),
        ] {
            self.signal(signal)?;
            let deadline = Instant::now() + budget;
            loop {
                let exited = self.poll()?.is_some();
                if exited && !self.group_exists()? {
                    self.contained = true;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        Err(BridgeError::StopFailed(format!(
            "Owned process group {} has not exited; ownership retained",
            self.group_id
        )))
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            tracing::error!("Owned child cleanup failed: {error}");
        }
    }
}

/// Files prevent a blocked stdout pipe from making timeout enforcement useless.
/// A private unique pair is removed after collecting bounded output.
pub fn run_bounded(
    mut command: Command,
    directory: &Path,
    budget: Duration,
) -> BridgeResult<Output> {
    if budget.is_zero() || budget > Duration::from_secs(60) {
        return Err(BridgeError::InvalidData(
            "CLI timeout must be in (0, 60s]".into(),
        ));
    }
    let id = uuid::Uuid::new_v4();
    let stdout_path = directory.join(format!("command-{id}.stdout"));
    let stderr_path = directory.join(format!("command-{id}.stderr"));
    let result = (|| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(File::create_new(&stdout_path)?))
            .stderr(Stdio::from(File::create_new(&stderr_path)?));
        let mut process = OwnedProcess::spawn(command)?;
        let deadline = Instant::now() + budget;
        let status = loop {
            let bytes = fs::metadata(&stdout_path)?.len() + fs::metadata(&stderr_path)?.len();
            if bytes > MAX_COMMAND_BYTES {
                process.terminate()?;
                return Err(BridgeError::InvalidData(
                    "Dora CLI output exceeds 1 MiB".into(),
                ));
            }
            if let Some(status) = process.poll()? {
                break status;
            }
            if Instant::now() >= deadline {
                process.terminate()?;
                return Err(BridgeError::Timeout(format!(
                    "Dora CLI exceeded {} ms; owned child reaped",
                    budget.as_millis()
                )));
            }
            thread::sleep(Duration::from_millis(15));
        };
        // A short command must not leave a detached helper behind.
        process.terminate()?;
        let read = |path: &Path| -> BridgeResult<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(path)?
                .take(MAX_COMMAND_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_COMMAND_BYTES {
                return Err(BridgeError::InvalidData("CLI output exceeds limit".into()));
            }
            Ok(bytes)
        };
        Ok(Output {
            status,
            stdout: read(&stdout_path)?,
            stderr: read(&stderr_path)?,
        })
    })();
    let _ = fs::remove_file(stdout_path);
    let _ = fs::remove_file(stderr_path);
    result
}

/// Keep the handle on timeout so a retry cannot silently replace a live worker.
pub fn join_worker(
    handle: &mut Option<thread::JoinHandle<()>>,
    budget: Duration,
) -> BridgeResult<()> {
    let deadline = Instant::now() + budget;
    while handle.as_ref().is_some_and(|worker| !worker.is_finished()) {
        if Instant::now() >= deadline {
            return Err(BridgeError::Timeout(
                "Audio bridge has not exited; worker ownership retained".into(),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    if let Some(worker) = handle.take() {
        worker.join().map_err(|_| BridgeError::ThreadJoinFailed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_observed_gone_group_is_never_signalled_again() {
        let mut finished = OwnedProcess::spawn(Command::new("/usr/bin/true")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while finished.poll().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert!(finished.contained);
        let mut command = Command::new("/bin/sleep");
        command.arg("20");
        let mut peer = OwnedProcess::spawn(command).unwrap();
        // Simulate numerical reuse after the original group was observed gone.
        finished.group_id = peer.group_id;
        finished.terminate().unwrap();
        drop(finished);
        assert!(peer.poll().unwrap().is_none());
        peer.terminate().unwrap();
    }

    #[test]
    fn timed_out_command_is_reaped_and_does_not_touch_another_group() {
        let directory =
            std::env::temp_dir().join(format!("forum-process-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let mut peer = Command::new("/bin/sleep");
        peer.arg("20");
        let mut peer = OwnedProcess::spawn(peer).unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("20");
        let start = Instant::now();
        assert!(matches!(
            run_bounded(command, &directory, Duration::from_millis(60)),
            Err(BridgeError::Timeout(_))
        ));
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(peer.poll().unwrap().is_none());
        peer.terminate().unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn command_output_and_nonzero_status_are_preserved() {
        let directory =
            std::env::temp_dir().join(format!("forum-output-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf answer; printf failure >&2; exit 7"]);
        let output = run_bounded(command, &directory, Duration::from_secs(2)).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"answer");
        assert_eq!(output.stderr, b"failure");
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn a_timed_out_thread_handle_is_retained_until_exit() {
        let (send, recv) = std::sync::mpsc::channel();
        let mut handle = Some(thread::spawn(move || {
            recv.recv().unwrap();
        }));
        assert!(join_worker(&mut handle, Duration::from_millis(10)).is_err());
        assert!(handle.is_some());
        send.send(()).unwrap();
        join_worker(&mut handle, Duration::from_secs(1)).unwrap();
        assert!(handle.is_none());
    }
}
