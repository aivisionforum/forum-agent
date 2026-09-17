//! Shared foreground admission gate. It never terminates a process itself.
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    live: bool,
    preparing: bool,
    pressure: Option<String>,
    background: bool,
    background_live_eligible: bool,
    shutdown: bool,
}

#[derive(Clone, Default)]
pub struct ResourceBudget(Arc<(Mutex<State>, Condvar)>);

impl ResourceBudget {
    pub fn enter_live(&self, timeout: Duration) -> Result<LivePermit, String> {
        match self.prepare_live(timeout) {
            Ok(()) => Ok(LivePermit(self.clone())),
            Err(error) => {
                self.finish_live();
                Err(error)
            }
        }
    }
    /// Set the preemption flag before waiting. Capture may start only after the
    /// background owner confirms exit by dropping its permit.
    pub fn prepare_live(&self, timeout: Duration) -> Result<(), String> {
        let (mutex, changed) = &*self.0;
        let mut state = mutex.lock().map_err(|_| "模型调度状态不可用")?;
        state.live = true;
        state.preparing = true;
        let deadline = Instant::now() + timeout;
        while state.background {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err("后台模型尚未退出，暂不能开始采音".into());
            };
            state = changed
                .wait_timeout(state, remaining)
                .map_err(|_| "模型调度等待失败")?
                .0;
        }
        Ok(())
    }

    pub fn live_ready(&self) {
        if let Ok(mut state) = self.0 .0.lock() {
            state.preparing = false;
        }
    }
    pub fn finish_live(&self) {
        if let Ok(mut state) = self.0 .0.lock() {
            state.live = false;
            state.preparing = false;
            state.pressure = None;
        }
        self.0 .1.notify_all();
    }
    pub fn set_pressure(&self, reason: Option<String>) {
        if let Ok(mut state) = self.0 .0.lock() {
            state.pressure = reason;
        }
    }
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.0 .0.lock() {
            state.shutdown = true;
        }
        self.0 .1.notify_all();
    }

    /// Only the small insights profile is live eligible; minutes/32B wait for
    /// capture to stop. Runtime backlog/health measurements can revoke a lease.
    pub fn admit(&self, live_eligible: bool) -> Result<BackgroundPermit, String> {
        let mut state = self.0 .0.lock().map_err(|_| "模型调度状态不可用")?;
        let reason = if state.shutdown {
            Some("应用正在退出")
        } else if state.preparing {
            Some("正在准备实时字幕")
        } else if state.background {
            Some("已有分析任务正在运行")
        } else if state.live && !live_eligible {
            Some("等待采音及字幕收尾完成")
        } else {
            state.pressure.as_deref()
        };
        if let Some(reason) = reason {
            return Err(reason.to_string());
        }
        state.background = true;
        state.background_live_eligible = live_eligible;
        Ok(BackgroundPermit(self.clone()))
    }

    pub fn yield_reason(&self) -> Option<String> {
        let state = self.0 .0.lock().ok()?;
        if state.shutdown {
            Some("应用正在退出".into())
        } else if state.preparing {
            Some("新会议优先，已请求后台模型退出".into())
        } else if state.live && !state.background_live_eligible {
            Some("实时字幕优先".into())
        } else {
            state.pressure.clone()
        }
    }
}

pub struct BackgroundPermit(ResourceBudget);
pub struct LivePermit(ResourceBudget);
impl Drop for LivePermit {
    fn drop(&mut self) {
        self.0.finish_live();
    }
}
impl Drop for BackgroundPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0 .0 .0.lock() {
            state.background = false;
        }
        self.0 .0 .1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_waits_for_actual_background_release() {
        let gate = ResourceBudget::default();
        let permit = gate.admit(false).unwrap();
        assert!(gate.prepare_live(Duration::from_millis(10)).is_err());
        assert!(gate.yield_reason().is_some());
        assert!(gate.admit(true).is_err());
        drop(permit);
        gate.prepare_live(Duration::from_millis(10)).unwrap();
        gate.live_ready();
        assert!(gate.admit(false).is_err());
        let insight = gate.admit(true).unwrap();
        gate.set_pressure(Some("字幕积压".into()));
        assert_eq!(gate.yield_reason().as_deref(), Some("字幕积压"));
        drop(insight);
        gate.finish_live();
        assert!(gate.admit(false).is_ok());
    }
}
