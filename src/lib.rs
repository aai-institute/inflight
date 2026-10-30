//! Stop a running computation at the safe points it chooses, and learn why it stopped.

#![deny(missing_docs)]

use std::ops::ControlFlow;

mod controls;
mod latched;
#[cfg(feature = "memory")]
mod memory;
#[cfg(feature = "pyo3")]
mod python;

pub use controls::{Deadline, Flag};
pub use latched::Latched;
#[cfg(feature = "memory")]
pub use memory::MemoryCap;
#[cfg(feature = "pyo3")]
pub use python::interruptible;

/// Why a call stopped.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stop {
    /// An OS signal, such as Ctrl-C.
    Signal,
    /// A time budget ran out.
    Deadline,
    /// The process's resident memory passed a cap.
    Memory,
    /// The caller asked for it.
    Requested,
}

/// Asked at a computation's safe points whether to go on.
pub trait Control: Sync {
    /// `Break` ends the call at this safe point.
    fn check(&self) -> ControlFlow<Stop>;
}

/// Never stops.
impl Control for () {
    fn check(&self) -> ControlFlow<Stop> {
        ControlFlow::Continue(())
    }
}

impl<C: Control + ?Sized> Control for &C {
    fn check(&self) -> ControlFlow<Stop> {
        (**self).check()
    }
}

/// Stops when either does; the first is asked first.
impl<A: Control, B: Control> Control for (A, B) {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()?;
        self.1.check()
    }
}

/// What a call that may stop returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<R, S = ()> {
    /// Ran to the end.
    Completed(R),
    /// Ended at a safe point.
    Stopped {
        /// The first stop the call saw.
        reason: Stop,
        /// What the call leaves behind for its caller.
        residue: S,
    },
}

impl<R, S> Outcome<R, S> {
    /// The result, if the call ran to the end.
    pub fn completed(self) -> Option<R> {
        match self {
            Self::Completed(result) => Some(result),
            Self::Stopped { .. } => None,
        }
    }
}
