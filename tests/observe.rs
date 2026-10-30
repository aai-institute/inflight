use std::ops::ControlFlow;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use inflight::{
    Checked, Closed, Control, Ended, Flag, Observer, Progress, Record, Recorder, Scope, Stop,
    Vocabulary,
};

struct Test;

impl Vocabulary for Test {
    type Scope = &'static str;
    type Event<'a> = &'a [u32];
}

fn recorder() -> Recorder<impl Fn(&&[u32]) -> Vec<u32> + Sync, &'static str, Vec<u32>> {
    Recorder::new(|event: &&[u32]| event.to_vec())
}

fn ended(record: &Record<&'static str, Vec<u32>>) -> Option<Ended> {
    record.closed.map(|Closed { ended, .. }| ended)
}

#[test]
fn nothing_is_built_for_an_unheard_scope() {
    let root = Scope::<Test>::root(&());
    let block = root.open(&"block");
    assert!(!block.is_heard());
    block.event(|| unreachable!("no observer hears this scope"));
    assert_eq!(block.check(), ControlFlow::Continue(()));
    block.close();
}

#[test]
fn a_recorder_keeps_the_tree_in_opening_order() {
    let recorder = recorder();
    {
        let root = Scope::<Test>::root(&recorder);
        let data = [1, 2];
        root.event(|| &data[..1]);
        for name in ["first", "second"] {
            let block = root.open(&name);
            let run = block.open(&"run");
            run.progress(Progress {
                done: 3,
                total: Some(4),
            });
            run.event(|| &data[..]);
            run.close();
            block.close();
        }
        root.close();
    }
    let record = recorder.into_record();
    assert_eq!(record.events, [vec![1]]);
    assert_eq!(ended(&record), Some(Ended::Completed));
    let names: Vec<_> = record.children.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["first", "second"]);
    let (_, block) = &record.children[1];
    assert_eq!(ended(block), Some(Ended::Completed));
    let (name, run) = &block.children[0];
    assert_eq!(*name, "run");
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
fn parallel_scopes_keep_their_slots() {
    let recorder = recorder();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    {
        let root = Scope::<Test>::root(&recorder);
        let blocks: Vec<_> = names.iter().map(|name| root.open(name)).collect();
        thread::scope(|threads| {
            for (k, block) in blocks.iter().enumerate().rev() {
                threads.spawn(move || {
                    block.event(|| &[7][..]);
                    block.progress(Progress {
                        done: k as u64,
                        total: None,
                    });
                });
            }
        });
        blocks.into_iter().rev().for_each(Scope::close);
        root.close();
    }
    let record = recorder.into_record();
    for (k, ((name, block), expected)) in record.children.iter().zip(names).enumerate() {
        assert_eq!(*name, expected);
        assert_eq!(block.progress.map(|p| p.done), Some(k as u64));
        assert_eq!(block.events, [vec![7]]);
    }
}

#[test]
fn a_scope_left_without_closing_says_why() {
    let recorder = recorder();
    let flag = Flag::new();
    let watch = (&flag, &recorder);
    {
        let root = Scope::<Test>::root(&watch);
        drop(root.open(&"failed"));
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _block = root.open(&"unwound");
            panic!("inside a scope");
        }));
        assert!(unwound.is_err());
        flag.raise(Stop::Requested);
        let block = root.open(&"stopped");
        assert_eq!(block.check(), ControlFlow::Break(Stop::Requested));
        drop(block);
    }
    let record = recorder.into_record();
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

/// Stops once it has heard `after` events: a rule that decides from what it observes.
struct Budget {
    after: u32,
    heard: AtomicU32,
}

impl Observer<Test> for Budget {
    fn check(&self) -> ControlFlow<Stop> {
        match self.heard.load(Ordering::Relaxed) >= self.after {
            true => ControlFlow::Break(Stop::Requested),
            false => ControlFlow::Continue(()),
        }
    }
    fn open(&self, _: &&'static str) -> Option<Box<dyn Observer<Test> + '_>> {
        Some(Box::new(self))
    }
    fn event(&self, _: &&[u32]) {
        self.heard.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn a_rule_that_hears_inside_a_scope_stops_the_whole_call() {
    let budget = Budget {
        after: 2,
        heard: AtomicU32::new(0),
    };
    let root = Scope::<Test>::root(&budget);
    let (first, second) = (root.open(&"first"), root.open(&"second"));
    first.event(|| &[]);
    assert_eq!(first.check(), ControlFlow::Continue(()));
    first.event(|| &[]);
    assert_eq!(first.check(), ControlFlow::Break(Stop::Requested));
    budget.heard.store(0, Ordering::Relaxed);
    assert_eq!(
        second.check(),
        ControlFlow::Break(Stop::Requested),
        "latched"
    );
    assert_eq!(root.stopped(), Some(Stop::Requested));
}

/// Declines every scope but stops the call: control reaches scopes it does not hear.
struct Deaf(Flag);

impl Observer<Test> for Deaf {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(&self.0)
    }
}

#[test]
fn checks_reach_the_call_through_unheard_scopes() {
    let deaf = Deaf(Flag::new());
    let root = Scope::<Test>::root(&deaf);
    let block = root.open(&"block");
    let run = block.open(&"run");
    assert!(!run.is_heard());
    deaf.0.raise(Stop::Signal);
    assert_eq!(run.check(), ControlFlow::Break(Stop::Signal));
}

#[test]
fn a_callers_own_control_checks_through_checked() {
    struct Never;
    impl Control for Never {
        fn check(&self) -> ControlFlow<Stop> {
            ControlFlow::Break(Stop::Deadline)
        }
    }
    let checked = Checked(Never);
    let root = Scope::<Test>::root(&checked);
    assert_eq!(root.check(), ControlFlow::Break(Stop::Deadline));
}

#[test]
fn a_pair_hears_both_and_asks_the_first_first() {
    let (left, right) = (recorder(), recorder());
    let (first, second) = (Flag::new(), Flag::new());
    first.raise(Stop::Signal);
    second.raise(Stop::Requested);
    let watch = ((&first, &left), (&second, &right));
    {
        let root = Scope::<Test>::root(&watch);
        let block = root.open(&"block");
        block.event(|| &[5]);
        assert_eq!(block.check(), ControlFlow::Break(Stop::Signal));
    }
    for record in [left.into_record(), right.into_record()] {
        assert_eq!(record.children[0].1.events, [vec![5]]);
    }
}
