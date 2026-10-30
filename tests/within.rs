use std::fmt::Debug;

use inflight::{run, Forward, Message, Outcome, Reports, Scope, Stop, Within};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Solve;
impl Reports for Solve {
    type Event = u64;
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sweep;
impl Reports for Sweep {
    type Event = f64;
}

fn solve(observer: &dyn inflight::Observer<Solve>) -> Outcome<u64> {
    run(observer, |call| {
        call.within(Solve, |solve| {
            (0..4).try_for_each(|k| {
                solve.event(|| k);
                solve.check()
            })?;
            Ok(4)
        })
    })
}

fn sweep(observer: &dyn inflight::Observer<Sweep>) -> Outcome<()> {
    run(observer, |call| {
        call.within(Sweep, |sweep| {
            sweep.event(|| 0.5);
            sweep.check()
        })
    })
}

// An outer vocabulary whose events are a primitive: `From<&u64> for u64` is the identity, so `From` could not map.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Counted {
    Phase,
    Solve,
}
impl Reports for Counted {
    type Event = u64;
}
impl Within<Solve> for Counted {
    fn scope(_: &Solve) -> Self {
        Counted::Solve
    }
    fn event(k: &u64) -> Option<u64> {
        (k % 2 == 0).then_some(k * 10)
    }
}

#[test]
fn a_nest_maps_and_drops_events_of_a_primitive_type() {
    let (recorder, outbox) = Forward::<Counted>::new();
    let outcome = run(&recorder, |call: &Scope<'_, Counted>| {
        call.nest(Counted::Phase, |n| solve(n).into_result())
    });
    assert_eq!(outcome, Outcome::Completed(4));
    let record = outbox.record();
    let (_, phase) = &record.children[0];
    let (scope, solve) = &phase.children[0];
    assert_eq!((*scope, &solve.events[..]), (Counted::Solve, &[0, 20][..]));
}

// One message vocabulary a library owner's sink hears every algorithm in.
#[derive(Debug, PartialEq)]
struct Said(String);
impl Reports for Said {
    type Event = String;
}
impl<S: Reports + Debug> Within<S> for Said
where
    S::Event: Debug,
{
    fn scope(scope: &S) -> Self {
        Said(format!("{scope:?}"))
    }
    fn event(event: &S::Event) -> Option<String> {
        Some(format!("{event:?}"))
    }
}

#[test]
fn one_forward_hears_two_vocabularies_with_distinct_ids() {
    let (forward, outbox) = Forward::<Said>::new();
    solve(&forward).completed().expect("never stops");
    sweep(&forward).completed().expect("never stops");
    let mut opened = Vec::new();
    outbox
        .drain(|message| {
            if let Message::Opened { id, scope, .. } = message {
                opened.push((id, scope.0));
            }
            Ok::<_, Stop>(())
        })
        .unwrap();
    assert_eq!(opened, [(1, "Solve".into()), (2, "Sweep".into())]);
}
