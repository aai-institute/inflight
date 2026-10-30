use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use pyo3::exceptions::{PyInterruptedError, PyKeyboardInterrupt, PyMemoryError, PyTimeoutError};
use pyo3::{PyErr, PyResult, Python};

use crate::{Control, Flag, Stop};

/// How long a signal may wait for the main thread to notice it.
const SIGNAL_POLL: Duration = Duration::from_millis(50);

/// Runs `work` beside the main thread with the GIL released; an exception from a signal handler stops it and is raised.
pub fn interruptible<T: Send>(
    py: Python<'_>,
    work: impl FnOnce(&dyn Control) -> T + Send,
) -> PyResult<T> {
    let (signal, done, main) = (Flag::new(), AtomicBool::new(false), thread::current());
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = work(&signal);
            done.store(true, Ordering::Release);
            main.unpark();
            result
        });
        let mut raised = Ok(());
        while !done.load(Ordering::Acquire) {
            py.detach(|| thread::park_timeout(SIGNAL_POLL));
            if raised.is_ok() {
                raised = py.check_signals();
                if raised.is_err() {
                    signal.raise(Stop::Signal);
                }
            }
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
        }
    }
}
