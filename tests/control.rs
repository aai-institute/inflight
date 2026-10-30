use std::ops::ControlFlow;
use std::time::Duration;

use inflight::{Deadline, Flag, Stop};

#[test]
fn a_flag_keeps_its_first_reason() {
    let flag = Flag::new();
    assert_eq!(flag.check(), ControlFlow::Continue(()));
    flag.raise(Stop::Signal);
    flag.raise(Stop::Requested);
    assert_eq!(flag.check(), ControlFlow::Break(Stop::Signal));
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
