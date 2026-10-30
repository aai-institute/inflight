use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use pyo3::exceptions::{
    PyInterruptedError, PyKeyboardInterrupt, PyMemoryError, PyRuntimeError, PyTimeoutError,
    PyValueError,
};
use pyo3::{Bound, Py, PyAny, PyErr, PyResult, Python};

use crate::{Closed, Deadline, Ended, Flag, MemoryCap, Observer, Opened, Progress, Reports, Stop};

/// How long a signal may wait for the main thread to notice it.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// How often a memory limit samples resident memory; a check then costs one load.
const MEMORY_SAMPLE: Duration = Duration::from_millis(10);

/// Runs `work` beside the main thread with the GIL released; an exception from a signal handler stops it and is raised.
pub fn interruptible<T: Send>(py: Python<'_>, work: impl FnOnce(&Flag) -> T + Send) -> PyResult<T> {
    interruptible_with(py, |_| Ok(()), work)
}

/// As [`interruptible`], and runs `on_wake` on the main thread after each signal check and once after `work` ends; its error stops `work` too and is raised.
pub fn interruptible_with<T: Send>(
    py: Python<'_>,
    mut on_wake: impl FnMut(Python<'_>) -> PyResult<()>,
    work: impl FnOnce(&Flag) -> T + Send,
) -> PyResult<T> {
    let (stop, done, main) = (Flag::new(), AtomicBool::new(false), thread::current());
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = work(&stop);
            done.store(true, Ordering::Release);
            main.unpark();
            result
        });
        let mut raised = Ok(());
        loop {
            // Read before waking, so the last wake sees everything `work` reported.
            let finished = done.load(Ordering::Acquire);
            if raised.is_ok() {
                raised = match py.check_signals() {
                    Err(signal) => {
                        stop.raise(Stop::Signal);
                        Err(signal)
                    }
                    Ok(()) => on_wake(py).inspect_err(|_| stop.raise(Stop::Requested)),
                };
            }
            if finished {
                break;
            }
            py.detach(|| thread::park_timeout(SIGNAL_POLL));
        }
        let result = worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        raised.map(|()| result)
    })
}

impl From<Stop> for PyErr {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Signal => PyKeyboardInterrupt::new_err("stopped by a signal"),
            Stop::Deadline => PyTimeoutError::new_err("stopped at its deadline"),
            Stop::Memory => PyMemoryError::new_err("stopped at its memory cap"),
            Stop::Requested => PyInterruptedError::new_err("stopped on request"),
            Stop::Rule(rule) => PyInterruptedError::new_err(format!("stopped by its {rule}")),
        }
    }
}

/// The limits a Python caller set on one call, as `timeout` seconds and `memory_limit` bytes; stops any algorithm.
#[derive(Debug)]
pub struct Limits {
    deadline: Option<Deadline>,
    memory: Option<MemoryCap>,
}

impl Limits {
    /// `ValueError` for a timeout that is negative or not finite; `RuntimeError` for a memory limit where the platform does not report resident memory.
    pub fn new(timeout: Option<f64>, memory_limit: Option<usize>) -> PyResult<Self> {
        let deadline = timeout
            .map(|seconds| {
                Duration::try_from_secs_f64(seconds)
                    .map(Deadline::after)
                    .map_err(|_| {
                        PyValueError::new_err(
                            "timeout must be a finite, nonnegative number of seconds",
                        )
                    })
            })
            .transpose()?;
        let memory = memory_limit
            .map(|bytes| {
                MemoryCap::start(bytes, MEMORY_SAMPLE).ok_or_else(|| {
                    PyRuntimeError::new_err(
                        "memory_limit: this platform does not report resident memory",
                    )
                })
            })
            .transpose()?;
        Ok(Self { deadline, memory })
    }
}

impl<S: Reports> Observer<S> for Limits {
    fn check(&self) -> ControlFlow<Stop> {
        if let Some(deadline) = &self.deadline {
            deadline.check()?;
        }
        match &self.memory {
            Some(memory) => memory.check(),
            None => ControlFlow::Continue(()),
        }
    }
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::Skip
    }
}

/// Hands a call's progress to a Python callable as `progress(done, total)`, on the thread that calls [`report`](Self::report).
pub struct ProgressCallback<S> {
    callback: Option<Py<PyAny>>,
    // Which scopes add up; `None` reports the call's own progress.
    counted: Option<fn(&S) -> bool>,
    seen: Mutex<Vec<Progress>>,
    moved: AtomicBool,
}

impl<S: Reports> ProgressCallback<S> {
    /// The call's own progress; without a callback, it hears nothing.
    pub fn new(callback: Option<Bound<'_, PyAny>>) -> Self {
        Self {
            callback: callback.map(Bound::unbind),
            counted: None,
            seen: Mutex::new(Vec::new()),
            moved: AtomicBool::new(false),
        }
    }

    /// The summed progress of every scope `counted` picks, at any depth, instead; the total grows as such scopes start.
    pub fn summing(callback: Option<Bound<'_, PyAny>>, counted: fn(&S) -> bool) -> Self {
        Self {
            counted: Some(counted),
            ..Self::new(callback)
        }
    }

    /// Calls the callable if the progress moved since the last report; meant for [`interruptible_with`]'s `on_wake`.
    pub fn report(&self, py: Python<'_>) -> PyResult<()> {
        let Some(callback) = &self.callback else {
            return Ok(());
        };
        if !self.moved.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let (done, total) = {
            let seen = lock(&self.seen);
            match self.counted {
                None => seen.first().map_or((0, None), |p| (p.done, p.total)),
                Some(_) => seen.iter().fold((0, Some(0)), |(done, total), p| {
                    (done + p.done, total.map(|t| t + p.total.unwrap_or(p.done)))
                }),
            }
        };
        callback.call1(py, (done, total)).map(drop)
    }

    fn set(&self, slot: usize, progress: Progress) {
        lock(&self.seen)[slot] = progress;
        self.moved.store(true, Ordering::Release);
    }

    fn open_below(&self, scope: &S) -> Opened<'_, S> {
        match self.counted {
            Some(counted) if counted(scope) => {
                let slot = {
                    let mut seen = lock(&self.seen);
                    seen.push(Progress {
                        done: 0,
                        total: None,
                    });
                    seen.len() - 1
                };
                Opened::With(Box::new(Counted { owner: self, slot }))
            }
            Some(_) => Opened::With(Box::new(Below(self))),
            None => Opened::Skip,
        }
    }
}

impl<S> std::fmt::Debug for ProgressCallback<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressCallback")
            .field("summing", &self.counted.is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<S: Reports> Observer<S> for ProgressCallback<S> {
    fn open(&self, scope: &S) -> Opened<'_, S> {
        match self.callback {
            Some(_) => self.open_below(scope),
            None => Opened::Skip,
        }
    }
    fn progress(&self, progress: Progress) {
        if self.callback.is_some() && self.counted.is_none() {
            let mut seen = lock(&self.seen);
            match seen.first_mut() {
                Some(first) => *first = progress,
                None => seen.push(progress),
            }
            self.moved.store(true, Ordering::Release);
        }
    }
}

// A scope between the call and the counted ones: it only looks further down.
struct Below<'p, S>(&'p ProgressCallback<S>);

impl<S: Reports> Observer<S> for Below<'_, S> {
    fn open(&self, scope: &S) -> Opened<'_, S> {
        self.0.open_below(scope)
    }
}

// A counted scope's progress, in its slot; a scope that completes counts as done.
struct Counted<'p, S> {
    owner: &'p ProgressCallback<S>,
    slot: usize,
}

impl<S: Reports> Observer<S> for Counted<'_, S> {
    fn open(&self, scope: &S) -> Opened<'_, S> {
        self.owner.open_below(scope)
    }
    fn progress(&self, progress: Progress) {
        self.owner.set(self.slot, progress);
    }
    fn closed(&self, closed: Closed) {
        if closed.ended == Ended::Completed {
            let seen = lock(&self.owner.seen)[self.slot];
            if let Some(total) = seen.total {
                self.owner.set(
                    self.slot,
                    Progress {
                        done: total,
                        total: Some(total),
                    },
                );
            }
        }
    }
}
