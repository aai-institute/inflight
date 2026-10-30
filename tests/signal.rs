//! Runs without the test harness: Python delivers signals only on the main thread.

use std::time::{Duration, Instant};

use pyo3::exceptions::PyKeyboardInterrupt;
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
    });
}
