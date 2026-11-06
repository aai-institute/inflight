use std::ops::ControlFlow;
use std::sync::Mutex;

use inflight::{
    hear, run, try_run, Closed, Flag, Observer, Opened, Outcome, Progress, Reports, Scope, Stop,
    Within,
};

#[derive(Clone, Copy, Debug, PartialEq)]
struct A;
impl Reports for A {
    type Event = u64;
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum B {
    Mid,
    Leaf,
}
impl Reports for B {
    type Event = u64;
}
impl Within<A> for B {
    fn scope(_: &A) -> Self {
        B::Leaf
    }
    fn event(e: &u64) -> Option<u64> {
        Some(e + 10)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum C {
    Top,
    Mid,
    Leaf,
}
impl Reports for C {
    type Event = u64;
}
impl Within<B> for C {
    fn scope(b: &B) -> Self {
        match b {
            B::Mid => C::Mid,
            B::Leaf => C::Leaf,
        }
    }
    fn event(e: &u64) -> Option<u64> {
        Some(e + 100)
    }
}

fn alg_a(observer: &dyn Observer<A>) -> Outcome<()> {
    run(observer, |call| {
        call.within(A, |a| {
            (0..3).try_for_each(|k| {
                a.event(|| k);
                a.check()
            })
        })
    })
}

fn alg_b(observer: &dyn Observer<B>) -> Outcome<()> {
    run(observer, |call| {
        call.nest(B::Mid, |n| alg_a(n).into_result())
    })
}

#[derive(Default)]
struct Log(Mutex<Vec<String>>);

impl Log {
    fn push(&self, s: String) {
        self.0.lock().unwrap().push(s);
    }
    fn count(&self, s: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|x| *x == s).count()
    }
}

struct Tap<'l> {
    log: &'l Log,
    name: String,
}

impl<S: Reports + std::fmt::Debug> Observer<S> for Tap<'_>
where
    S::Event: std::fmt::Debug,
{
    fn check(&self) -> ControlFlow<Stop> {
        self.log.push(format!("check {}", self.name));
        ControlFlow::Continue(())
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        Opened::With(Box::new(Tap {
            log: self.log,
            name: format!("{scope:?}"),
        }))
    }
    fn event(&self, e: &S::Event) {
        self.log.push(format!("event {} {e:?}", self.name));
    }
    fn closed(&self, c: Closed) {
        self.log.push(format!("closed {} {:?}", self.name, c.ended));
    }
}

struct SameTap<'l>(&'l Log);

impl<S: Reports> Observer<S> for SameTap<'_> {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.push("check same".into());
        ControlFlow::Continue(())
    }
    fn closed(&self, _: Closed) {
        self.0.push("closed same".into());
    }
}

#[test]
fn three_vocabularies_nest_and_each_check_is_asked_once_per_safe_point() {
    let log = Log::default();
    let tap = Tap {
        log: &log,
        name: "root".into(),
    };
    let outcome = run(&tap, |call: &Scope<'_, C>| {
        call.nest(C::Top, |n| alg_b(n).into_result())
    });
    assert_eq!(outcome, Outcome::Completed(()));
    for name in ["root", "Top", "Mid", "Leaf"] {
        assert_eq!(
            log.count(&format!("check {name}")),
            3,
            "{name}: {:?}",
            log.0
        );
    }
    for name in ["Top", "Mid", "Leaf"] {
        assert_eq!(
            log.count(&format!("closed {name} Completed")),
            1,
            "{name}: {:?}",
            log.0
        );
    }
    assert_eq!(log.count("event Leaf 110"), 1, "{:?}", log.0);
}

#[test]
fn a_same_observer_in_a_tuple_under_a_wrapper_is_asked_once_per_safe_point_at_depth() {
    let log = Log::default();
    let observer = Wrap((
        SameTap(&log),
        Tap {
            log: &log,
            name: "root".into(),
        },
    ));
    let outcome = run(&observer, |call: &Scope<'_, C>| {
        call.within(C::Top, |t| {
            t.within(C::Mid, |m| {
                m.within(C::Leaf, |l| (0..3).try_for_each(|_| l.check()))
            })
        })
    });
    assert_eq!(outcome, Outcome::Completed(()));
    assert_eq!(log.count("check same"), 3, "{:?}", log.0);
    assert_eq!(log.count("closed same"), 4, "{:?}", log.0);
    for name in ["root", "Top", "Mid", "Leaf"] {
        assert_eq!(
            log.count(&format!("check {name}")),
            3,
            "{name}: {:?}",
            log.0
        );
    }
}

#[test]
fn same_observer_through_nest_at_depth_asked_once() {
    let log = Log::default();
    let observer = (
        SameTap(&log),
        Tap {
            log: &log,
            name: "root".into(),
        },
    );
    let outcome = run(&observer, |call: &Scope<'_, C>| {
        call.nest(C::Top, |n| alg_b(n).into_result())
    });
    assert_eq!(outcome, Outcome::Completed(()));
    assert_eq!(log.count("check same"), 3, "{:?}", log.0);
}

// An inner part with its own error, wrapped by the outer algorithm's error as any `?` does.
#[derive(Debug, PartialEq)]
enum InnerError {
    Stopped(Stop),
    Singular,
}
impl From<Stop> for InnerError {
    fn from(s: Stop) -> Self {
        Self::Stopped(s)
    }
}

#[derive(Debug, PartialEq)]
enum SolveError {
    Stopped(Stop),
    Factor(InnerError),
}
impl From<Stop> for SolveError {
    fn from(s: Stop) -> Self {
        Self::Stopped(s)
    }
}
impl From<InnerError> for SolveError {
    fn from(e: InnerError) -> Self {
        Self::Factor(e)
    }
}

fn factor(scope: &Scope<'_, C>) -> Result<(), InnerError> {
    scope.check()?;
    Err(InnerError::Singular)
}

#[test]
fn a_stop_wrapped_by_an_outer_error_ends_the_call_stopped() {
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    let result = try_run(&flag, |call: &Scope<'_, C>| -> Result<(), SolveError> {
        call.within(C::Top, |t| Ok(factor(t)?))
    });
    assert_eq!(
        result,
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        })
    );
}

// An error that records where the stop hit.
#[derive(Debug, PartialEq)]
enum IterError {
    Interrupted { stop: Stop, iteration: u64 },
}
impl From<Stop> for IterError {
    fn from(stop: Stop) -> Self {
        Self::Interrupted { stop, iteration: 0 }
    }
}

#[test]
fn a_stop_with_context_ends_the_call_stopped() {
    let flag = Flag::new();
    let result = try_run(&flag, |call: &Scope<'_, C>| -> Result<(), IterError> {
        (1..5).try_for_each(|iteration| {
            if iteration == 3 {
                flag.raise(Stop::Requested);
            }
            call.check()
                .map_err(|stop| IterError::Interrupted { stop, iteration })
        })
    });
    assert_eq!(
        result,
        Ok(Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        })
    );
}

impl inflight::Stopping for InnerError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Singular => None,
        }
    }
}

impl inflight::Stopping for SolveError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Factor(error) => error.stop(),
        }
    }
}

impl inflight::Stopping for IterError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Interrupted { stop, .. } => Some(*stop),
        }
    }
}

// Passes everything on and wraps each child it hears, as an observer that thins or filters does.
struct Wrap<O>(O);

impl<S: Reports, O: Observer<S>> Observer<S> for Wrap<O> {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        match hear(&self.0, scope) {
            Some(child) => Opened::With(Box::new(Wrap(child))),
            None => Opened::Skip,
        }
    }
    fn progress(&self, progress: Progress) {
        self.0.progress(progress)
    }
    fn event(&self, event: &S::Event) {
        self.0.event(event)
    }
    fn closed(&self, closed: Closed) {
        self.0.closed(closed)
    }
}
