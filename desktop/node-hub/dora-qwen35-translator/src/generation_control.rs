use anyhow::{bail, Result};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct GenerationControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
    pub require_eos: bool,
}
impl GenerationControl {
    pub fn new(budget: Duration, require_eos: bool) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + budget,
            require_eos,
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!("CANCELLED: translation generation was cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("DEADLINE_EXCEEDED: translation generation timed out");
        }
        Ok(())
    }
    pub fn finish(&self, saw_eos: bool) -> Result<()> {
        self.check()?;
        if self.require_eos && !saw_eos {
            bail!("TOKEN_BUDGET_EXHAUSTED: incomplete translation is not a final result");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_during_generation_reaches_all_clones_and_cannot_finalize() {
        let control = GenerationControl::new(Duration::from_secs(2), true);
        let worker = control.clone();
        for token in 0..10 {
            if token == 3 {
                control.cancel();
            }
            if token < 3 {
                worker.check().unwrap();
            } else {
                assert!(worker.check().is_err());
            }
        }
        assert!(worker.finish(true).is_err());
    }
    #[test]
    fn exhausted_budget_is_failure_even_when_some_text_exists() {
        let control = GenerationControl::new(Duration::from_secs(2), true);
        assert!(control.finish(false).is_err());
        control.finish(true).unwrap();
        assert!(GenerationControl::new(Duration::ZERO, true)
            .check()
            .is_err());
    }
}
