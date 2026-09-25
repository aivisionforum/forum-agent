use parking_lot::Mutex;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

pub fn default_inputs() -> Vec<String> {
    ["__system_audio__", "__default_microphone__", "__dual_audio__"]
        .into_iter().map(String::from).collect()
}

/// CoreAudio capability enumeration may wait indefinitely for a device driver.
/// Keep it off the UI thread and allow at most one outstanding enumeration.
pub struct AudioInputs {
    devices: Arc<Mutex<Vec<String>>>,
    started: AtomicBool,
}

impl AudioInputs {
    pub fn new() -> Self {
        Self { devices: Arc::new(Mutex::new(default_inputs())), started: AtomicBool::new(false) }
    }

    pub fn get_or_start(
        &self,
        selected: &str,
        discover: impl FnOnce() -> Vec<String> + Send + 'static,
        completed: impl FnOnce(Vec<String>) + Send + 'static,
    ) -> Vec<String> {
        if !self.started.swap(true, Ordering::AcqRel) {
            let cache = self.devices.clone();
            if let Err(error) = std::thread::Builder::new().name("audio-input-discovery".into()).spawn(move || {
                let devices = discover();
                *cache.lock() = devices.clone();
                completed(devices);
            }) {
                self.started.store(false, Ordering::Release);
                log::warn!("Audio input discovery could not start: {error}");
            }
        }
        let mut devices = self.devices.lock().clone();
        if !selected.is_empty() && !devices.iter().any(|device| device == selected) {
            devices.push(selected.into());
        }
        devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn stalled_discovery_keeps_settings_available_and_never_spawns_duplicates() {
        let inputs = AudioInputs::new();
        let (release, blocked) = mpsc::channel();
        let (completed, result) = mpsc::channel();
        let devices = inputs.get_or_start("Saved microphone", move || {
            blocked.recv().unwrap();
            let mut devices = default_inputs();
            devices.push("External microphone".into());
            devices
        }, move |devices| { completed.send(devices).unwrap(); });
        assert!(devices.contains(&"__default_microphone__".into()));
        assert!(devices.contains(&"Saved microphone".into()));
        let cached = inputs.get_or_start("Saved microphone", || panic!("duplicate enumeration"), |_| {});
        assert_eq!(cached, devices);
        release.send(()).unwrap();
        assert!(result.recv_timeout(Duration::from_secs(2)).unwrap().contains(&"External microphone".into()));
        assert!(inputs.get_or_start("", || panic!("cached enumeration"), |_| {}).contains(&"External microphone".into()));
    }
}
