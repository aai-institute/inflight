use std::ops::ControlFlow;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{Flag, Stop};

/// Stops once the process's resident memory passes a cap; a thread samples it, so a check stays one load.
#[derive(Debug)]
pub struct MemoryCap {
    over: Arc<Flag>,
    stop_sampling: Sender<()>,
    sampler: Option<JoinHandle<()>>,
}

impl MemoryCap {
    /// Samples now and every `interval`; `None` where the platform does not report resident memory.
    ///
    /// # Panics
    /// If `interval` is zero.
    pub fn start(bytes: usize, interval: Duration) -> Option<Self> {
        assert!(!interval.is_zero(), "a zero interval would spin a core");
        let over = Arc::new(Flag::new());
        if resident()? > bytes {
            over.raise(Stop::Memory);
        }
        let (stop_sampling, stopped) = mpsc::channel();
        let sampler = thread::spawn({
            let over = Arc::clone(&over);
            move || {
                while stopped.recv_timeout(interval) == Err(RecvTimeoutError::Timeout) {
                    if resident().is_some_and(|now| now > bytes) {
                        over.raise(Stop::Memory);
                        return;
                    }
                }
            }
        });
        Some(Self {
            over,
            stop_sampling,
            sampler: Some(sampler),
        })
    }

    /// `Break` once a sample passed the cap.
    pub fn check(&self) -> ControlFlow<Stop> {
        self.over.check()
    }
}

fn resident() -> Option<usize> {
    memory_stats::memory_stats().map(|stats| stats.physical_mem)
}

impl Drop for MemoryCap {
    fn drop(&mut self) {
        // Fails only if the sampler already returned, which the join then sees at once.
        let _ = self.stop_sampling.send(());
        if let Some(sampler) = self.sampler.take() {
            let _ = sampler.join();
        }
    }
}
