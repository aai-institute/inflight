use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Observer, Opened, Reports, Stop};

/// Stops with [`Stop::Requested`] at its `limit`-th check and every one after, counting them: walks a library's safe points in its tests.
#[derive(Debug)]
pub struct Countdown {
    limit: usize,
    checks: AtomicUsize,
}

impl Countdown {
    /// Stops at the `limit`-th check; `0` stops at the first.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            checks: AtomicUsize::new(0),
        }
    }

    /// Checks asked so far.
    pub fn checks(&self) -> usize {
        self.checks.load(Ordering::Relaxed)
    }
}

impl<S: Reports> Observer<S> for Countdown {
    fn check(&self) -> ControlFlow<Stop> {
        match self.checks.fetch_add(1, Ordering::Relaxed) + 1 >= self.limit {
            true => ControlFlow::Break(Stop::Requested),
            false => ControlFlow::Continue(()),
        }
    }
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::Skip
    }
}
