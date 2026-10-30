use std::panic::{catch_unwind, AssertUnwindSafe};

use inflight::{run, Forward, Observer, Opened, Reports, Stop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Out(&'static str);
impl Reports for Out {
    type Event = ();
}

struct PanicsOnOpen;
impl Observer<Out> for PanicsOnOpen {
    fn open(&self, _: &Out) -> Opened<'_, Out> {
        panic!("open")
    }
}

#[test]
fn a_pair_whose_second_panics_in_open_leaves_no_open_scope_recorded() {
    let (forward, outbox) = Forward::<Out>::new();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        run(&(&forward, PanicsOnOpen), |call| {
            call.within(Out("block"), |_| Ok::<_, Stop>(()))
        })
    }));
    assert!(caught.is_err());
    let record = outbox.record();
    for (name, child) in &record.children {
        assert!(child.closed.is_some(), "{name:?} opened, never closed");
    }
}
