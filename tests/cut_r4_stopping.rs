use inflight::{try_run, Flag, Outcome, Reports, Stop, Stopping};

#[derive(Clone, Copy, Debug, PartialEq)]
struct A;
impl Reports for A {
    type Event = u64;
}

#[derive(Debug)]
enum Lossy {
    Own,
}
impl From<Stop> for Lossy {
    fn from(_: Stop) -> Self {
        Lossy::Own
    }
}
impl Stopping for Lossy {
    fn stop(&self) -> Option<Stop> {
        None
    }
}

// From<Stop> and stop() disagree: the call's stop would surface, silently, as the algorithm's own error.
#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "Stopping"))]
fn disagreeing_stopping_is_caught() {
    let flag = Flag::default();
    flag.raise(Stop::Requested);
    let result: Result<Outcome<()>, Lossy> = try_run(&flag, |call| {
        call.within(A, |scope| {
            scope.check()?;
            Ok(())
        })
    });
    assert!(matches!(result, Err(Lossy::Own)));
}
