use inflight::{run, IterationLimit, Observer, Outcome, Reports, Standard};

/// An algorithm with its own events, one of which counts as an iteration.
#[derive(Debug)]
struct Pass;

enum SweepEvent {
    Pass,
    Rescaled,
}

impl Reports for Pass {
    type Event = SweepEvent;
}

impl Standard for SweepEvent {
    fn is_iteration(&self) -> bool {
        matches!(self, SweepEvent::Pass)
    }
}

/// Runs up to `passes` passes, each in its own scope, and how many finished.
fn sweep(passes: u64, observer: &dyn Observer<Pass>) -> (Outcome<()>, u64) {
    let mut done = 0;
    let outcome = run(observer, |call| {
        for _ in 0..passes {
            call.within(Pass, |pass| {
                pass.event(|| SweepEvent::Rescaled);
                pass.event(|| SweepEvent::Pass);
                Ok(())
            })?;
            done += 1;
            call.check()?;
        }
        Ok(())
    });
    (outcome, done)
}

#[test]
fn an_iteration_limit_stops_any_algorithm_that_names_its_iterations() {
    let limit = IterationLimit::new(3);
    assert_eq!(
        sweep(10, &limit),
        (
            Outcome::Stopped {
                reason: IterationLimit::STOP,
                residue: ()
            },
            3
        )
    );
}

#[test]
fn an_iteration_limit_lets_a_shorter_run_complete() {
    assert_eq!(
        sweep(2, &IterationLimit::new(3)),
        (Outcome::Completed(()), 2)
    );
}
