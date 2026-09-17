#[cfg(target_os = "macos")]
mod platform {
    use objc2_core_foundation::CFString;
    use objc2_io_kit::{
        kIOPMAssertionLevelOn, IOPMAssertionCreateWithName, IOPMAssertionDeclareUserActivity,
        IOPMAssertionID, IOPMAssertionRelease, IOPMUserActiveType,
    };
    use parking_lot::Mutex;
    use std::{
        sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
        thread::{self, JoinHandle},
        time::Duration,
    };

    const USER_ACTIVITY_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
    const ASSERTION_TYPE: &str = "PreventUserIdleDisplaySleep";
    const ASSERTION_REASON: &str = "AI Vision Forum live translation";
    const IO_SUCCESS: i32 = 0;

    struct WakeSession {
        stop_sender: Sender<()>,
        worker: JoinHandle<()>,
    }

    /// Owns the macOS power assertions used by a live translation session.
    ///
    /// The display-sleep assertion also prevents idle system sleep. A periodic
    /// user-activity declaration keeps the screen-saver idle timer from firing.
    pub struct WakeLock {
        session: Mutex<Option<WakeSession>>,
    }

    impl WakeLock {
        pub fn new() -> Self {
            Self {
                session: Mutex::new(None),
            }
        }

        pub fn start(&self) -> Result<(), String> {
            let mut session = self.session.lock();
            if session.is_some() {
                return Ok(());
            }

            let (stop_sender, stop_receiver) = mpsc::channel();
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            let worker = thread::Builder::new()
                .name("forum-agent-wake-lock".into())
                .spawn(move || run_wake_session(stop_receiver, ready_sender))
                .map_err(|error| format!("Could not start the screen wake lock: {error}"))?;

            match ready_receiver.recv() {
                Ok(Ok(())) => {
                    *session = Some(WakeSession {
                        stop_sender,
                        worker,
                    });
                    Ok(())
                }
                Ok(Err(error)) => {
                    let _ = worker.join();
                    Err(error)
                }
                Err(error) => {
                    let _ = worker.join();
                    Err(format!(
                        "Screen wake lock stopped before it was ready: {error}"
                    ))
                }
            }
        }

        pub fn stop(&self) {
            let Some(session) = self.session.lock().take() else {
                return;
            };
            let _ = session.stop_sender.send(());
            let _ = session.worker.join();
        }

        #[cfg(test)]
        fn is_active(&self) -> bool {
            self.session.lock().is_some()
        }
    }

    impl Default for WakeLock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for WakeLock {
        fn drop(&mut self) {
            self.stop();
        }
    }

    fn run_wake_session(stop_receiver: Receiver<()>, ready_sender: SyncSender<Result<(), String>>) {
        let assertion_type = CFString::from_static_str(ASSERTION_TYPE);
        let assertion_reason = CFString::from_static_str(ASSERTION_REASON);
        let mut display_assertion: IOPMAssertionID = 0;

        let result = unsafe {
            IOPMAssertionCreateWithName(
                Some(&assertion_type),
                kIOPMAssertionLevelOn,
                Some(&assertion_reason),
                &mut display_assertion,
            )
        };
        if result != IO_SUCCESS {
            let _ = ready_sender.send(Err(format!(
                "macOS rejected the display sleep assertion (IOKit error {result})"
            )));
            return;
        }

        let mut user_activity_assertion: IOPMAssertionID = 0;
        let result = declare_user_activity(&assertion_reason, &mut user_activity_assertion);
        if result != IO_SUCCESS {
            let _ = IOPMAssertionRelease(display_assertion);
            let _ = ready_sender.send(Err(format!(
                "macOS rejected the screen-saver activity assertion (IOKit error {result})"
            )));
            return;
        }

        if ready_sender.send(Ok(())).is_err() {
            release_assertions(display_assertion, user_activity_assertion);
            return;
        }

        loop {
            match stop_receiver.recv_timeout(USER_ACTIVITY_REFRESH_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    let result =
                        declare_user_activity(&assertion_reason, &mut user_activity_assertion);
                    if result != IO_SUCCESS {
                        log::warn!(
                            "Could not refresh the screen-saver activity assertion: IOKit error {result}"
                        );
                    }
                }
            }
        }

        release_assertions(display_assertion, user_activity_assertion);
    }

    fn declare_user_activity(reason: &CFString, assertion_id: &mut IOPMAssertionID) -> i32 {
        unsafe {
            IOPMAssertionDeclareUserActivity(Some(reason), IOPMUserActiveType::Local, assertion_id)
        }
    }

    fn release_assertions(
        display_assertion: IOPMAssertionID,
        user_activity_assertion: IOPMAssertionID,
    ) {
        if user_activity_assertion != 0 {
            let _ = IOPMAssertionRelease(user_activity_assertion);
        }
        if display_assertion != 0 {
            let _ = IOPMAssertionRelease(display_assertion);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wake_lock_starts_and_stops_idempotently() {
            let wake_lock = WakeLock::new();
            wake_lock.start().expect("wake lock should start");
            wake_lock.start().expect("second start should be a no-op");
            assert!(wake_lock.is_active());
            wake_lock.stop();
            wake_lock.stop();
            assert!(!wake_lock.is_active());
        }

        #[test]
        #[ignore = "holds the macOS assertion long enough for pmset inspection"]
        fn wake_lock_is_visible_to_macos_power_management() {
            let wake_lock = WakeLock::new();
            wake_lock.start().expect("wake lock should start");
            assert!(wake_lock.is_active());
            thread::sleep(Duration::from_secs(10));
            wake_lock.stop();
            assert!(!wake_lock.is_active());
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::WakeLock;

#[cfg(not(target_os = "macos"))]
pub struct WakeLock;

#[cfg(not(target_os = "macos"))]
impl WakeLock {
    pub fn new() -> Self {
        Self
    }

    pub fn start(&self) -> Result<(), String> {
        Ok(())
    }

    pub fn stop(&self) {}
}
