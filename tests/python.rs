use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use inflight::{run, Limits, Outcome, ProgressCallback, Reports, Stop};

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Counted,
    Other,
}

impl Reports for Part {
    type Event = ();
}

fn seen(py: Python<'_>) -> (Bound<'_, PyAny>, Bound<'_, PyAny>) {
    let seen = py.eval(c"[]", None, None).unwrap();
    let append = seen.getattr("append").unwrap();
    let callback = py
        .eval(
            c"lambda append: lambda done, total: append((done, total))",
            None,
            None,
        )
        .unwrap()
        .call1((append,))
        .unwrap();
    (seen, callback)
}

#[test]
fn a_timeout_must_be_a_duration() {
    Python::initialize();
    Python::attach(|py| {
        let error = Limits::new(Some(-1.0), None).unwrap_err();
        assert!(error.is_instance_of::<PyValueError>(py));
    });
}

#[test]
fn a_spent_timeout_stops_the_call() {
    let limits = Limits::new(Some(0.0), None).unwrap();
    let outcome = run::<Part, _>(&limits, |call| call.check());
    assert_eq!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Deadline,
            residue: ()
        }
    );
}

#[test]
fn the_calls_own_progress_reaches_the_callable_once_per_move() {
    Python::initialize();
    Python::attach(|py| {
        let (seen, callback) = seen(py);
        let progress = ProgressCallback::<Part>::new(Some(callback));
        let _ = run(&progress, |call| {
            call.within(Part::Other, |other| {
                other.progress(5, Some(5));
                Ok::<(), Stop>(())
            })?;
            call.progress(1, Some(2));
            Ok::<(), Stop>(())
        });
        progress.report(py).unwrap();
        progress.report(py).unwrap();
        assert_eq!(
            seen.extract::<Vec<(u64, Option<u64>)>>().unwrap(),
            [(1, Some(2))]
        );
    });
}

#[test]
fn summing_adds_the_counted_scopes_at_any_depth() {
    Python::initialize();
    Python::attach(|py| {
        let (seen, callback) = seen(py);
        let progress = ProgressCallback::summing(Some(callback), |part| *part == Part::Counted);
        let _ = run(&progress, |call| {
            call.progress(7, Some(7));
            call.within(Part::Counted, |counted| {
                counted.progress(3, Some(10));
                Ok::<(), Stop>(())
            })?;
            call.within(Part::Other, |other| {
                other.within(Part::Counted, |counted| {
                    counted.progress(1, Some(4));
                    Ok::<(), Stop>(())
                })
            })
        });
        progress.report(py).unwrap();
        // A counted scope that completes counts as done.
        assert_eq!(
            seen.extract::<Vec<(u64, Option<u64>)>>().unwrap(),
            [(14, Some(14))]
        );
    });
}

#[test]
fn without_a_callable_nothing_is_called() {
    Python::initialize();
    Python::attach(|py| {
        let progress = ProgressCallback::<Part>::new(None);
        let _ = run(&progress, |call| {
            call.progress(1, None);
            Ok::<(), Stop>(())
        });
        assert!(progress.report(py).is_ok());
    });
}
