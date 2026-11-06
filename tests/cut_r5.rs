use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use inflight::{run, Closed, Ended, Observer, Progress, Reports, Stop};
mod common;
use common::Recorder;

#[derive(Clone, Debug, PartialEq)]
struct Part;
impl Reports for Part {
    type Event = u64;
}

// A usual test observer: a log behind a Mutex, unwrapped.
#[derive(Default)]
struct Log(Mutex<Vec<String>>);
impl Observer<Part> for Log {
    fn event(&self, event: &u64) {
        let mut log = self.0.lock().unwrap();
        log.push(format!("event {event}"));
        assert!(*event < 3, "observer bug");
    }
    fn closed(&self, closed: Closed) {
        self.0
            .lock()
            .unwrap()
            .push(format!("closed {:?}", closed.ended));
    }
}

#[test]
fn an_observer_panic_under_its_lock_is_a_panic_not_an_abort() {
    let log = Log::default();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        run::<Part, _>(&log, |call| {
            call.within(Part, |part| {
                for k in 0..5 {
                    part.event(|| k);
                }
                Ok::<_, Stop>(())
            })
        })
    }));
    assert!(caught.is_err());
}

struct Counts(AtomicU64);
impl Observer<Part> for Counts {
    fn progress(&self, progress: Progress) {
        if progress.done == 2 {
            panic!("observer bug at 2");
        }
        self.0.store(progress.done, Ordering::Relaxed);
    }
}

#[test]
fn a_progress_panic_keeps_the_count() {
    let counts = Counts(AtomicU64::new(0));
    let _ = run::<Part, _>(&counts, |call| {
        call.advance(1, None);
        let _ = catch_unwind(AssertUnwindSafe(|| call.advance(1, None)));
        call.advance(1, None);
        Ok(())
    });
    assert_eq!(counts.0.load(Ordering::Relaxed), 3);
}

struct ClosedOnce(AtomicU64, AtomicU64);
impl Observer<Part> for ClosedOnce {
    fn closed(&self, closed: Closed) {
        self.0.fetch_add(1, Ordering::Relaxed);
        if closed.ended == Ended::Completed {
            self.1.fetch_add(1, Ordering::Relaxed);
            panic!("observer bug in closed");
        }
    }
}

#[test]
fn a_closed_panic_closes_each_scope_once() {
    let seen = ClosedOnce(AtomicU64::new(0), AtomicU64::new(0));
    let caught = catch_unwind(AssertUnwindSafe(|| {
        run::<Part, _>(&seen, |call| call.within(Part, |_| Ok::<_, Stop>(())))
    }));
    assert!(caught.is_err());
    assert_eq!(seen.0.load(Ordering::Relaxed), 2);
}

#[test]
fn a_forward_beside_a_panicking_closed_still_hears_the_close() {
    let (forward, outbox) = Recorder::<Part>::new();
    let seen = ClosedOnce(AtomicU64::new(0), AtomicU64::new(0));
    let both = (seen, forward);
    let _ = catch_unwind(AssertUnwindSafe(|| {
        run::<Part, _>(&both, |call| call.within(Part, |_| Ok::<_, Stop>(())))
    }));
    let record = outbox.record();
    assert!(record.closed.is_some());
    assert_eq!(record.children.len(), 1);
    assert!(record.children[0].1.closed.is_some(), "{record:?}");
}
