use std::fmt;
use std::ops::ControlFlow;
use std::sync::OnceLock;

use crate::{Control, Stop};

/// A call's view of its control: once a check breaks, every later one breaks with the same reason.
pub struct Latched<'c> {
    inner: &'c dyn Control,
    stop: OnceLock<Stop>,
}

impl<'c> Latched<'c> {
    /// Not yet stopped.
    pub fn new(inner: &'c dyn Control) -> Self {
        Self {
            inner,
            stop: OnceLock::new(),
        }
    }

    /// The reason of the first check that broke, without asking the control again.
    pub fn stopped(&self) -> Option<Stop> {
        self.stop.get().copied()
    }
}

impl Control for Latched<'_> {
    fn check(&self) -> ControlFlow<Stop> {
        if let Some(&reason) = self.stop.get() {
            return ControlFlow::Break(reason);
        }
        match self.inner.check() {
            ControlFlow::Break(reason) => ControlFlow::Break(*self.stop.get_or_init(|| reason)),
            ControlFlow::Continue(()) => ControlFlow::Continue(()),
        }
    }
}

impl fmt::Debug for Latched<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Latched")
            .field("stop", &self.stopped())
            .finish_non_exhaustive()
    }
}
