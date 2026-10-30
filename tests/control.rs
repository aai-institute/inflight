use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use inflight::{Control, Deadline, Flag, Latched, Stop};

/// Breaks only on its `at`-th check: a control that does not latch by itself.
struct Once {
    at: usize,
    checks: AtomicUsize,
}

impl Once {
    fn new(at: usize) -> Self {
        Self {
            at,
            checks: AtomicUsize::new(0),
        }
    }
}

impl Control for Once {
    fn check(&self) -> ControlFlow<Stop> {
        match self.checks.fetch_add(1, Ordering::Relaxed) + 1 == self.at {
            true => ControlFlow::Break(Stop::Requested),
            false => ControlFlow::Continue(()),
        }
    }
}

#[test]
fn a_flag_keeps_its_first_reason() {
    let flag = Flag::new();
    assert_eq!(flag.check(), ControlFlow::Continue(()));
    flag.raise(Stop::Signal);
    flag.raise(Stop::Requested);
    assert_eq!(flag.check(), ControlFlow::Break(Stop::Signal));
}

#[test]
fn a_pair_reports_its_first_member_that_breaks() {
    let (flag, past) = (Flag::new(), Deadline::at(Instant::now()));
    flag.raise(Stop::Requested);
    assert_eq!((&flag, past).check(), ControlFlow::Break(Stop::Requested));
    assert_eq!((past, &flag).check(), ControlFlow::Break(Stop::Deadline));
    assert_eq!(((), ()).check(), ControlFlow::Continue(()));
}

#[test]
fn an_unrepresentable_budget_never_runs_out() {
    assert_eq!(
        Deadline::after(Duration::MAX).check(),
        ControlFlow::Continue(())
    );
    assert_eq!(
        Deadline::after(Duration::ZERO).check(),
        ControlFlow::Break(Stop::Deadline)
    );
}

#[test]
fn a_latch_holds_a_stop_its_control_forgets() {
    let control = Once::new(2);
    let latched = Latched::new(&control);
    assert_eq!(latched.check(), ControlFlow::Continue(()));
    assert_eq!(latched.stopped(), None);
    assert_eq!(latched.check(), ControlFlow::Break(Stop::Requested));
    assert_eq!(latched.check(), ControlFlow::Break(Stop::Requested));
    assert_eq!(latched.stopped(), Some(Stop::Requested));
    assert_eq!(
        control.checks.load(Ordering::Relaxed),
        2,
        "a latched stop never asks again"
    );
}

#[test]
fn an_inner_latch_trips_the_outer_one() {
    let control = Once::new(1);
    let outer = Latched::new(&control);
    let inner = Latched::new(&outer);
    assert_eq!(inner.check(), ControlFlow::Break(Stop::Requested));
    assert_eq!(outer.stopped(), Some(Stop::Requested));
}

#[test]
fn every_thread_sees_a_latched_stop() {
    let control = Once::new(1);
    let latched = Latched::new(&control);
    assert!(latched.check().is_break());
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| assert_eq!(latched.check(), ControlFlow::Break(Stop::Requested)));
        }
    });
}
