//! Runs without the test harness: Python delivers signals only on the main thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pyo3::exceptions::{PyKeyboardInterrupt, PyValueError};
use pyo3::prelude::*;

fn main() {
    Python::initialize();
    Python::attach(|py| {
        // Embedded Python starts without handlers; a timer thread sends SIGINT once the work runs.
        py.run(
            c"import os, signal, threading\n\
              signal.signal(signal.SIGINT, signal.default_int_handler)\n\
              threading.Timer(0.1, os.kill, (os.getpid(), signal.SIGINT)).start()",
            None,
            None,
        )
        .expect("installs the handler and the timer");
        let started = Instant::now();
        let stopped = inflight::interruptible(py, |control| {
            while control.check().is_continue() {
                if started.elapsed() > Duration::from_secs(5) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            true
        });
        let err = stopped.expect_err("the handler's exception is raised");
        assert!(err.is_instance_of::<PyKeyboardInterrupt>(py), "{err}");
        let latency = started.elapsed();
        assert!(latency < Duration::from_secs(1), "{latency:?}");
        println!("signal: KeyboardInterrupt after {latency:?}");

        // The last wake runs after `work` ends, so it sees everything `work` sent.
        let (sent, received) = mpsc::channel();
        let mut seen = Vec::new();
        inflight::interruptible_with(
            py,
            |_| {
                seen.extend(received.try_iter());
                Ok(())
            },
            |_| sent.send(7).expect("received"),
        )
        .expect("nothing raises");
        assert_eq!(seen, [7]);

        // A raising wake stops `work` and its exception is raised.
        let (started, stopped) = (Instant::now(), AtomicBool::new(false));
        let raised = inflight::interruptible_with(
            py,
            |_| match started.elapsed() > Duration::from_millis(100) {
                true => Err(PyValueError::new_err("enough")),
                false => Ok(()),
            },
            |control| {
                while control.check().is_continue() {
                    if started.elapsed() > Duration::from_secs(5) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                stopped.store(true, Ordering::Relaxed);
            },
        );
        let err = raised.expect_err("the wake's exception is raised");
        assert!(err.is_instance_of::<PyValueError>(py), "{err}");
        assert!(
            stopped.load(Ordering::Relaxed),
            "the wake's error stopped the work"
        );
    });
}
