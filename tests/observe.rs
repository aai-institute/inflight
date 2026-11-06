use std::ops::ControlFlow;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use inflight::{
    run, try_run, Closed, Ended, Flag, Observer, Opened, Outcome, Progress, Reports, Scope, Stop,
    Within,
};
mod common;
use common::{Recorded, Recorder, Recording};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Named(&'static str);

impl Reports for Named {
    type Event = Vec<u32>;
}

type Kept = Recorded<Named>;

fn recorder() -> (Recorder<Named>, Recording<Named>) {
    Recorder::new()
}

fn ended(record: &Kept) -> Option<Ended> {
    record.closed.map(|Closed { ended, .. }| ended)
}

#[test]
fn nothing_is_built_for_an_unheard_scope() {
    let outcome = run(&(), |call: &Scope<'_, Named>| {
        call.within(Named("block"), |block| {
            block.event(|| unreachable!("no observer hears this scope"));
            block.check()
        })
    });
    assert_eq!(outcome, Outcome::Completed(()));
}

#[test]
fn a_recorder_keeps_the_tree_in_starting_order() {
    let (recorder, outbox) = recorder();
    let outcome = run(&recorder, |call| {
        call.event(|| vec![1]);
        for name in ["first", "second"] {
            call.within(Named(name), |block| {
                block.within(Named("run"), |run| {
                    run.progress(3, Some(4));
                    run.event(|| vec![1, 2]);
                    Ok(())
                })
            })?;
        }
        Ok(())
    });
    assert_eq!(outcome, Outcome::Completed(()));
    let record = outbox.record();
    assert_eq!(record.events, [vec![1]]);
    assert_eq!(ended(&record), Some(Ended::Completed));
    let names: Vec<_> = record.children.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, [Named("first"), Named("second")]);
    let (_, block) = &record.children[1];
    assert_eq!(ended(block), Some(Ended::Completed));
    let (name, run) = &block.children[0];
    assert_eq!(*name, Named("run"));
    assert_eq!(run.events, [vec![1, 2]]);
    assert_eq!(
        run.progress,
        Some(Progress {
            done: 3,
            total: Some(4)
        })
    );
}

#[test]
fn parallel_scopes_each_keep_their_own_record() {
    let (recorder, outbox) = recorder();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    run(&recorder, |call| {
        thread::scope(|threads| {
            for (k, name) in names.into_iter().enumerate() {
                threads.spawn(move || {
                    call.within(Named(name), |block| {
                        block.event(|| vec![k as u32]);
                        block.progress(k as u64, None);
                        Ok::<_, Stop>(())
                    })
                });
            }
        });
        Ok(())
    });
    let record = outbox.record();
    assert_eq!(record.children.len(), names.len());
    for (Named(name), block) in &record.children {
        let k = names.iter().position(|n| n == name).expect("one of ours") as u32;
        assert_eq!(block.events, [vec![k]]);
        assert_eq!(block.progress.map(|p| p.done), Some(u64::from(k)));
        assert_eq!(ended(block), Some(Ended::Completed));
    }
}

#[derive(Debug, PartialEq)]
enum Failure {
    Bad,
    Stopped(Stop),
}

impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        Failure::Stopped(stop)
    }
}

#[test]
fn a_scope_ends_as_its_body_returns() {
    let (recorder, outbox) = recorder();
    let flag = Flag::new();
    let watch = (&flag, &recorder);
    let outcome = try_run(&watch, |call| {
        assert_eq!(
            call.within(Named("failed"), |_| Err::<(), _>(Failure::Bad)),
            Err(Failure::Bad)
        );
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            call.within(Named("unwound"), |_| -> Result<(), Failure> {
                panic!("inside a scope")
            })
        }));
        assert!(unwound.is_err());
        flag.raise(Stop::Requested);
        call.within(Named("stopped"), |block| {
            block.check()?;
            Ok::<_, Failure>(())
        })
    });
    assert_eq!(
        outcome,
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        }),
        "an error a stop caused is moot"
    );
    let record = outbox.record();
    let ends: Vec<_> = record.children.iter().map(|(_, r)| ended(r)).collect();
    assert_eq!(
        ends,
        [
            Some(Ended::Failed),
            Some(Ended::Unwound),
            Some(Ended::Stopped(Stop::Requested))
        ]
    );
    assert_eq!(ended(&record), Some(Ended::Stopped(Stop::Requested)));
}

#[test]
fn an_error_without_a_stop_is_returned() {
    let outcome = try_run(&(), |_: &Scope<'_, Named>| Err::<(), _>(Failure::Bad));
    assert_eq!(outcome, Err(Failure::Bad));
}

/// Stops once it has heard `after` events: a rule that decides from what it hears.
struct Budget {
    after: u32,
    heard: AtomicU32,
}

impl Observer<Named> for Budget {
    fn check(&self) -> ControlFlow<Stop> {
        match self.heard.load(Ordering::Relaxed) >= self.after {
            true => ControlFlow::Break(Stop::Requested),
            false => ControlFlow::Continue(()),
        }
    }
    fn event(&self, _: &Vec<u32>) {
        self.heard.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn a_rule_hears_inside_every_scope_by_default_and_its_stop_holds() {
    let budget = Budget {
        after: 2,
        heard: AtomicU32::new(0),
    };
    let outcome = run(&budget, |call| {
        call.within(Named("first"), |first| {
            first.event(Vec::new);
            first.check()?;
            first.event(Vec::new);
            first.check()
        })?;
        panic!("the second event stopped the call");
        #[allow(unreachable_code)]
        Ok(())
    });
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Requested,
            ..
        }
    ));
}

/// Counts the checks it is asked: once per safe point, however deep.
#[derive(Default)]
struct Checks(AtomicU32);

impl Observer<Named> for Checks {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.fetch_add(1, Ordering::Relaxed);
        ControlFlow::Continue(())
    }
}

fn three_deep_check(observer: &dyn Observer<Named>) {
    run(observer, |call| {
        call.within(Named("block"), |block| {
            block.within(Named("run"), |run| run.check())
        })
    });
}

#[test]
fn an_observer_is_asked_once_per_safe_point_however_deep() {
    let checks = Checks::default();
    three_deep_check(&checks);
    assert_eq!(checks.0.load(Ordering::Relaxed), 1);
}

#[test]
fn beside_one_that_opens_its_own_child_it_is_still_asked_once() {
    let (checks, (recorder, outbox)) = (Checks::default(), recorder());
    three_deep_check(&(&checks, &recorder));
    assert_eq!(checks.0.load(Ordering::Relaxed), 1);
    let record = outbox.record();
    assert_eq!(record.children[0].1.children[0].0, Named("run"));
}

/// Skips every scope but stops the call: checks reach scopes it does not hear.
struct Deaf(Flag);

impl Observer<Named> for Deaf {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()
    }
    fn open(&self, _: &Named) -> Opened<'_, Named> {
        Opened::Skip
    }
}

#[test]
fn checks_reach_the_call_through_skipped_scopes() {
    let deaf = Deaf(Flag::new());
    let outcome = run(&deaf, |call| {
        call.within(Named("block"), |block| {
            block.within(Named("run"), |run| {
                run.event(|| unreachable!("skipped"));
                deaf.0.raise(Stop::Signal);
                run.check()
            })
        })
    });
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Signal,
            ..
        }
    ));
}

#[test]
fn a_pair_hears_both_and_asks_the_first_first() {
    let ((left, left_out), (right, right_out)) = (recorder(), recorder());
    let (first, second) = (Flag::new(), Flag::new());
    first.raise(Stop::Signal);
    second.raise(Stop::Requested);
    let watch = ((&first, &left), (&second, &right));
    let outcome = run(&watch, |call| {
        call.within(Named("block"), |block| {
            block.event(|| vec![5]);
            block.check()
        })
    });
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Signal,
            ..
        }
    ));
    for record in [left_out.record(), right_out.record()] {
        assert_eq!(record.children[0].1.events, [vec![5]]);
    }
}

/// An outer algorithm that nests `Named` calls, as within nests approx-chol's builds.
#[derive(Clone, Debug, PartialEq)]
enum OuterScope {
    Rung(u32),
    Inner(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
enum OuterEvent {
    Iteration,
    Inner(Vec<u32>),
}

impl Reports for OuterScope {
    type Event = OuterEvent;
}

impl Within<Named> for OuterScope {
    fn scope(Named(name): &Named) -> Self {
        OuterScope::Inner(name)
    }
    fn event(event: &Vec<u32>) -> Option<OuterEvent> {
        Some(OuterEvent::Inner(event.clone()))
    }
}

type OuterRecord = Recorded<OuterScope>;

fn outer_recorder() -> (Recorder<OuterScope>, Recording<OuterScope>) {
    Recorder::new()
}

fn outer_ended(record: &OuterRecord) -> Option<Ended> {
    record.closed.map(|closed| closed.ended)
}

#[test]
fn a_nested_call_joins_the_outer_tree() {
    let (recorder, outbox) = outer_recorder();
    run(&recorder, |call| {
        call.within(OuterScope::Rung(1), |rung| {
            rung.event(|| OuterEvent::Iteration);
            let inner = rung.nest(OuterScope::Rung(2), |nest| {
                Ok::<_, Stop>(run(nest, |inner| {
                    inner.within(Named("block"), |block| {
                        block.event(|| vec![4, 2]);
                        block.progress(1, Some(1));
                        Ok(())
                    })
                }))
            })?;
            assert_eq!(inner, Outcome::Completed(()));
            Ok(())
        })
    });
    let record = outbox.record();
    let (scope, rung) = &record.children[0];
    assert_eq!(*scope, OuterScope::Rung(1));
    assert_eq!(rung.events, [OuterEvent::Iteration]);
    assert_eq!(outer_ended(rung), Some(Ended::Completed));
    let (scope, nest) = &rung.children[0];
    assert_eq!(*scope, OuterScope::Rung(2));
    let (scope, block) = &nest.children[0];
    assert_eq!(*scope, OuterScope::Inner("block"));
    assert_eq!(block.events, [OuterEvent::Inner(vec![4, 2])]);
    assert_eq!(block.progress.map(|p| p.done), Some(1));
    assert_eq!(outer_ended(block), Some(Ended::Completed));
}

#[test]
fn a_nested_stop_stops_the_outer_call() {
    let (recorder, outbox) = outer_recorder();
    let flag = Flag::new();
    let watch = (&flag, &recorder);
    let outcome = run(&watch, |call| {
        call.within(OuterScope::Rung(2), |rung| {
            let inner = rung.nest(OuterScope::Rung(3), |nest| {
                Ok::<_, Stop>(run(nest, |inner| {
                    inner.within(Named("block"), |block| {
                        flag.raise(Stop::Signal);
                        block.check()
                    })
                }))
            })?;
            assert!(matches!(
                inner,
                Outcome::Stopped {
                    reason: Stop::Signal,
                    ..
                }
            ));
            rung.check()
        })
    });
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Signal,
            ..
        }
    ));
    let record = outbox.record();
    let (_, rung) = &record.children[0];
    let (_, block) = &rung.children[0].1.children[0];
    assert_eq!(outer_ended(block), Some(Ended::Stopped(Stop::Signal)));
    assert_eq!(outer_ended(rung), Some(Ended::Stopped(Stop::Signal)));
}

#[derive(Default)]
struct OuterChecks(AtomicU32);

impl Observer<OuterScope> for OuterChecks {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.fetch_add(1, Ordering::Relaxed);
        ControlFlow::Continue(())
    }
}

#[test]
fn a_nested_safe_point_asks_the_outer_observer_once() {
    let checks = OuterChecks::default();
    run(&checks, |call| {
        let _ = call.nest(OuterScope::Rung(1), |nest| {
            run(nest, |inner| {
                inner.within(Named("block"), |block| {
                    block.within(Named("run"), |run| run.check())
                })
            })
            .into_result()
        });
        Ok(())
    });
    assert_eq!(checks.0.load(Ordering::Relaxed), 1);
}

#[test]
fn an_inner_only_stop_closes_the_outer_mirror_as_stopped() {
    let (recorder, outbox) = outer_recorder();
    let flag = Flag::new();
    flag.raise(Stop::Signal);
    let outcome = run(&recorder, |call| {
        call.within(OuterScope::Rung(1), |rung| {
            let inner = rung.nest(OuterScope::Rung(2), |nest| {
                Ok::<_, Stop>(run(&(nest, &flag), |inner| {
                    inner.within(Named("block"), |block| block.check())
                }))
            })?;
            assert!(matches!(
                inner,
                Outcome::Stopped {
                    reason: Stop::Signal,
                    ..
                }
            ));
            Ok(())
        })
    });
    assert_eq!(outcome, Outcome::Completed(()));
    let record = outbox.record();
    let (_, rung) = &record.children[0];
    let (_, block) = &rung.children[0].1.children[0];
    assert_eq!(outer_ended(block), Some(Ended::Stopped(Stop::Signal)));
}

impl inflight::Stopping for Failure {
    fn stop(&self) -> Option<Stop> {
        match self {
            Failure::Stopped(stop) => Some(*stop),
            Failure::Bad => None,
        }
    }
}

#[test]
fn a_parts_own_stop_ends_only_that_part() {
    let outcome = run(&(), |call: &Scope<'_, Named>| {
        let budget = Flag::new();
        let probe = call.under(Named("probe"), &budget, |probe| {
            budget.raise(Stop::Rule("probe budget"));
            probe.check()
        })?;
        call.check()?;
        Ok(probe)
    });
    assert_eq!(
        outcome,
        Outcome::Completed(Outcome::Stopped {
            reason: Stop::Rule("probe budget"),
            residue: ()
        })
    );
}

#[derive(Debug, PartialEq)]
struct At(Stop, u32);
impl From<Stop> for At {
    fn from(stop: Stop) -> Self {
        At(stop, 0)
    }
}
impl inflight::Stopping for At {
    fn stop(&self) -> Option<Stop> {
        Some(self.0)
    }
}

#[test]
fn the_calls_stop_leaves_a_part_with_the_bodys_error() {
    let (forward, outbox) = recorder();
    let call_flag = Flag::new();
    let mut part = None;
    let outcome = try_run(&(&forward, &call_flag), |call: &Scope<'_, Named>| {
        part = Some(call.under(Named("probe"), &Flag::new(), |probe| {
            call_flag.raise(Stop::Requested);
            probe.check().map_err(|stop| At(stop, 7))
        }));
        call.check()?;
        Ok::<_, At>(())
    });
    assert_eq!(part, Some(Err(At(Stop::Requested, 7))));
    assert_eq!(
        outcome,
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        })
    );
    let record = outbox.record();
    assert_eq!(
        ended(&record.children[0].1),
        Some(Ended::Stopped(Stop::Requested))
    );
}

#[test]
fn a_stopped_part_is_heard_by_the_call_and_its_control() {
    let (forward, outbox) = recorder();
    let (control, heard) = recorder();
    let budget = (&control, Flag::new());
    let outcome = run(&forward, |call: &Scope<'_, Named>| {
        let _ = call.under(Named("probe"), &budget, |probe| {
            probe.event(|| vec![1]);
            budget.1.raise(Stop::Rule("probe budget"));
            probe.check()
        })?;
        Ok(())
    });
    assert_eq!(outcome, Outcome::Completed(()));
    let probe = &outbox.record().children[0].1;
    assert_eq!(
        ended(probe),
        Some(Ended::Stopped(Stop::Rule("probe budget")))
    );
    assert_eq!(probe.events, vec![vec![1]]);
    assert_eq!(heard.record().events, vec![vec![1]]);
}

#[test]
fn a_call_stop_after_the_parts_own_leaves_the_part_stopped() {
    let call_flag = Flag::new();
    let mut part = None;
    let outcome = try_run(&call_flag, |call: &Scope<'_, Named>| {
        let superseded = Flag::new();
        part = Some(call.under(Named("build"), &superseded, |build| {
            superseded.raise(Stop::Rule("superseded"));
            let own = build.check();
            // Another thread's safe point, between the part's stop and `under` returning.
            call_flag.raise(Stop::Requested);
            let _ = call.check();
            own.map_err(|stop| At(stop, 1))
        }));
        call.check()?;
        Ok::<_, At>(())
    });
    assert_eq!(
        part,
        Some(Ok(Outcome::Stopped {
            reason: Stop::Rule("superseded"),
            residue: ()
        }))
    );
    assert_eq!(
        outcome,
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        })
    );
}
