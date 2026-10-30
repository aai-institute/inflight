use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::Stop;

/// Raised from outside the call; the first reason sticks.
#[derive(Debug, Default)]
pub struct Flag(OnceLock<Stop>);

impl Flag {
    /// Not raised.
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// Stops every later check with `reason`, unless an earlier one was raised.
    pub fn raise(&self, reason: Stop) {
        let _ = self.0.set(reason);
    }

    /// `Break` once raised.
    pub fn check(&self) -> ControlFlow<Stop> {
        match self.0.get() {
            Some(&reason) => ControlFlow::Break(reason),
            None => ControlFlow::Continue(()),
        }
    }
}

/// Stops once its instant has passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    /// At `instant`.
    pub fn at(instant: Instant) -> Self {
        Self(Some(instant))
    }

    /// `budget` from now; a budget past what `Instant` holds never runs out.
    pub fn after(budget: Duration) -> Self {
        Self(Instant::now().checked_add(budget))
    }

    /// `Break` once its instant has passed.
    pub fn check(&self) -> ControlFlow<Stop> {
        match self.0 {
            Some(instant) if Instant::now() >= instant => ControlFlow::Break(Stop::Deadline),
            _ => ControlFlow::Continue(()),
        }
    }
}
