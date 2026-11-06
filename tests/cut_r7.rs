use inflight::{run, try_run, Ended, Nest, Outcome, Reports, Stop};
mod common;
use common::Recorder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct P(&'static str);
impl Reports for P {
    type Event = ();
}

// A library entry point with a fast path that never starts a call.
fn solve(observer: &Nest<'_, P>, n: usize) -> Result<Outcome<()>, Stop> {
    if n == 0 {
        return Ok(Outcome::Completed(()));
    }
    try_run(observer, |call| {
        call.check()?;
        Ok(())
    })
}

fn nested_end(n: usize) -> Ended {
    let (fwd, outbox) = Recorder::<P>::new();
    let out = run(&fwd, |call| {
        call.nest(P("inner"), |nest| solve(nest, n))?;
        Ok(())
    });
    assert_eq!(out, Outcome::Completed(()));
    let record = outbox.record();
    record.children[0].1.closed.unwrap().ended
}

#[test]
fn nest_with_a_call_completes() {
    assert_eq!(nested_end(3), Ended::Completed);
}

#[test]
fn nest_whose_library_skips_the_call_completes() {
    assert_eq!(nested_end(0), Ended::Completed);
}

#[test]
fn nest_whose_body_fails_after_the_call_fails() {
    #[derive(Debug)]
    struct Mine;
    #[derive(Debug)]
    enum E {
        Stop(Stop),
        Mine(Mine),
    }
    impl From<Stop> for E {
        fn from(s: Stop) -> Self {
            E::Stop(s)
        }
    }
    impl inflight::Stopping for E {
        fn stop(&self) -> Option<Stop> {
            match self {
                E::Stop(s) => Some(*s),
                E::Mine(_) => None,
            }
        }
    }
    let (fwd, outbox) = Recorder::<P>::new();
    let out = try_run(&fwd, |call| {
        call.nest(P("inner"), |nest| {
            solve(nest, 3)?;
            Err::<(), E>(E::Mine(Mine))
        })
    });
    assert!(matches!(out, Err(E::Mine(_))));
    let record = outbox.record();
    assert_eq!(record.closed.unwrap().ended, Ended::Failed);
    assert_eq!(record.children[0].1.closed.unwrap().ended, Ended::Failed);
}
