//! Stop a running computation at the safe points it chooses, and learn why it stopped.

#![deny(missing_docs)]

mod controls;
mod observe;
#[cfg(feature = "pyo3")]
mod python;

pub use controls::{Deadline, Flag};
pub use observe::{
    hear, run, run_fallible, try_run, Closed, Ended, Nest, Observer, Opened, Progress, Reports,
    Scope, Within,
};
#[cfg(feature = "pyo3")]
pub use python::{interruptible, interruptible_with, Limits, ProgressCallback};

/// Why a call stopped.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stop {
    /// An OS signal, such as Ctrl-C.
    Signal,
    /// A time budget ran out.
    Deadline,
    /// The caller asked for it.
    Requested,
    /// A rule the call ran under, by its name.
    Rule(&'static str),
}

/// An error that is either a call's stop or the algorithm's own.
pub trait Stopping: From<Stop> {
    /// The stop this error carries, however wrapped; `None` for the algorithm's own error.
    fn stop(&self) -> Option<Stop>;
}

impl Stopping for Stop {
    fn stop(&self) -> Option<Stop> {
        Some(*self)
    }
}

/// The call's stop, or the algorithm's own error, which then needs no stop of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupted<E> {
    /// The call stopped.
    Stop(Stop),
    /// The algorithm failed.
    Own(E),
}

impl<E> Interrupted<E> {
    /// The own error mapped by `f`; a stop passes through.
    pub fn map<F>(self, f: impl FnOnce(E) -> F) -> Interrupted<F> {
        match self {
            Self::Stop(stop) => Interrupted::Stop(stop),
            Self::Own(error) => Interrupted::Own(f(error)),
        }
    }
}

impl<E> From<Stop> for Interrupted<E> {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

impl<E> Stopping for Interrupted<E> {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stop(stop) => Some(*stop),
            Self::Own(_) => None,
        }
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

    /// The result mapped by `f`; a stop and its residue pass through.
    pub fn map<T>(self, f: impl FnOnce(R) -> T) -> Outcome<T, S> {
        match self {
            Self::Completed(result) => Outcome::Completed(f(result)),
            Self::Stopped { reason, residue } => Outcome::Stopped { reason, residue },
        }
    }
}

impl<R> Outcome<R> {
    /// The result, or the stop, so that `?` carries an inner call's stop out.
    pub fn into_result(self) -> Result<R, Stop> {
        match self {
            Self::Completed(result) => Ok(result),
            Self::Stopped { reason, .. } => Err(reason),
        }
    }
}

impl Outcome<()> {
    /// `state` as the result, or as the residue a stopped call resumes from.
    pub fn holding<T>(self, state: T) -> Outcome<T, T> {
        match self {
            Self::Completed(()) => Outcome::Completed(state),
            Self::Stopped { reason, .. } => Outcome::Stopped {
                reason,
                residue: state,
            },
        }
    }
}
