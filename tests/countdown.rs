use inflight::{run, Countdown, Outcome, Scope, Stop};

#[test]
fn a_countdown_stops_at_its_nth_check_and_counts_them() {
    let countdown = Countdown::new(3);
    let outcome = run(&countdown, |call: &Scope<'_, std::convert::Infallible>| {
        (0..10).try_for_each(|_| call.check())
    });
    assert_eq!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        }
    );
    // Later checks repeat the latched stop without asking it.
    assert_eq!(countdown.checks(), 3);
}
