use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Observer, Reports, Stop};

/// An event that generic rules understand, whatever the algorithm calls it.
pub trait Standard {
    /// One iteration finished.
    fn is_iteration(&self) -> bool {
        false
    }

    /// The residual this reports, smaller being better.
    fn residual(&self) -> Option<f64> {
        None
    }
}

/// Stops once it has heard `limit` iterations, in any scope.
#[derive(Debug)]
pub struct IterationLimit {
    limit: u64,
    heard: AtomicU64,
}

impl IterationLimit {
    /// How its stop reads.
    pub const STOP: Stop = Stop::Rule("iteration limit");

    /// Stops after `limit` iterations.
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            heard: AtomicU64::new(0),
        }
    }
}

impl<S: Reports> Observer<S> for IterationLimit
where
    S::Event: Standard,
{
    fn check(&self) -> ControlFlow<Stop> {
        match self.heard.load(Ordering::Relaxed) >= self.limit {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    fn event(&self, event: &S::Event) {
        if event.is_iteration() {
            self.heard.fetch_add(1, Ordering::Relaxed);
        }
    }
}
