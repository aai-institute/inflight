use std::ops::ControlFlow;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Deadline, Flag, Interrupted, Outcome, Stop, Stopping};

/// An algorithm's scopes, and the events that happen inside them.
pub trait Reports: 'static {
    /// What happens inside a scope.
    type Event;
}

/// A vocabulary that reads another's scopes and events as its own; an event read as `None` is dropped.
pub trait Within<I: Reports>: Reports {
    /// The inner scope as one of this vocabulary.
    fn scope(inner: &I) -> Self;
    /// The inner event as one of this vocabulary, if it is kept.
    fn event(inner: &I::Event) -> Option<Self::Event>;
}

/// Every vocabulary reads itself unchanged.
impl<S: Reports + Clone> Within<S> for S
where
    S::Event: Clone,
{
    fn scope(inner: &S) -> S {
        inner.clone()
    }
    fn event(inner: &S::Event) -> Option<S::Event> {
        Some(inner.clone())
    }
}

/// No scopes and no events: a call that only checks, and may report its progress.
impl Reports for std::convert::Infallible {
    type Event = std::convert::Infallible;
}

/// How far a scope has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Units of work done.
    pub done: u64,
    /// Units of work in all, when known.
    pub total: Option<u64>,
}

/// How a scope ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It ran to the end.
    Completed,
    /// The call stopped while it ran.
    Stopped(Stop),
    /// It returned an error without a stop.
    Failed,
    /// A panic unwound through it.
    Unwound,
}

/// A scope's end, as its observer hears of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed {
    /// How it ended.
    pub ended: Ended,
    /// From its start to its end.
    pub elapsed: Duration,
}

/// Hears what an algorithm reports and may stop it at its safe points; every method has a default.
pub trait Observer<S: Reports>: Sync {
    /// Asked at each safe point; `Break` stops the call there.
    fn check(&self) -> ControlFlow<Stop> {
        ControlFlow::Continue(())
    }

    /// What hears a scope that starts inside this one.
    fn open(&self, _scope: &S) -> Opened<'_, S> {
        Opened::Same
    }

    /// The scope's latest progress.
    fn progress(&self, _progress: Progress) {}

    /// Something the algorithm reports.
    fn event(&self, _event: &S::Event) {}

    /// The scope ended; called once, last.
    fn closed(&self, _closed: Closed) {}
}

/// What [`Observer::open`] decides for a scope that starts.
pub enum Opened<'o, S: Reports> {
    /// This observer hears the scope, as it hears its parent.
    Same,
    /// Nothing of this observer hears the scope or anything inside it; its `check` is still asked.
    Skip,
    /// This child hears the scope and everything inside it.
    With(Box<dyn Observer<S> + 'o>),
}

impl<'o, S: Reports> Opened<'o, S> {
    fn heard_by<O: Observer<S> + ?Sized>(
        self,
        observer: &'o O,
    ) -> Option<Box<dyn Observer<S> + 'o>> {
        match self {
            Self::Same => Some(Box::new(Hears(observer))),
            Self::Skip => None,
            Self::With(child) => Some(child),
        }
    }
}

/// What hears a scope that starts inside the one `observer` hears, as `observer` decides; its check stays with `observer` unless it chose a child.
pub fn hear<'o, S: Reports, O: Observer<S> + ?Sized>(
    observer: &'o O,
    scope: &S,
) -> Option<Box<dyn Observer<S> + 'o>> {
    observer.open(scope).heard_by(observer)
}

/// Ignores everything and never stops.
impl<S: Reports> Observer<S> for () {
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::Skip
    }
}

macro_rules! checks_only {
    ($($control:ty),* $(,)?) => {$(
        impl<S: Reports> Observer<S> for $control {
            fn check(&self) -> ControlFlow<Stop> {
                <$control>::check(self)
            }
            fn open(&self, _: &S) -> Opened<'_, S> {
                Opened::Skip
            }
        }
    )*};
}

checks_only!(Flag, Deadline);

macro_rules! forwards {
    ($($wrapper:ty),*) => {$(
        impl<S: Reports, O: Observer<S> + ?Sized> Observer<S> for $wrapper {
            fn check(&self) -> ControlFlow<Stop> {
                (**self).check()
            }
            fn open(&self, scope: &S) -> Opened<'_, S> {
                (**self).open(scope)
            }
            fn progress(&self, progress: Progress) {
                (**self).progress(progress)
            }
            fn event(&self, event: &S::Event) {
                (**self).event(event)
            }
            fn closed(&self, closed: Closed) {
                (**self).closed(closed)
            }
        }
    )*};
}

forwards!(&O, Box<O>);

// An observer heard in a child scope but asked in its parent's, so its check is never asked twice.
struct Hears<'o, O: ?Sized>(&'o O);

impl<S: Reports, O: Observer<S> + ?Sized> Observer<S> for Hears<'_, O> {
    fn open(&self, scope: &S) -> Opened<'_, S> {
        self.0.open(scope)
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

/// Both hear everything; the first is asked first.
impl<S: Reports, A: Observer<S>, B: Observer<S>> Observer<S> for (A, B) {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()?;
        self.1.check()
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        let a = self.0.open(scope);
        // A first that already heard the scope open hears it end, as the scope's drop would have told it.
        let b = match panic::catch_unwind(AssertUnwindSafe(|| self.1.open(scope))) {
            Ok(b) => b,
            Err(panic) => {
                if let Some(a) = a.heard_by(&self.0) {
                    let ended = Ended::Unwound;
                    a.closed(Closed {
                        ended,
                        elapsed: Duration::ZERO,
                    });
                }
                panic::resume_unwind(panic)
            }
        };
        match (a, b) {
            (Opened::Same, Opened::Same) => Opened::Same,
            (Opened::Skip, Opened::Skip) => Opened::Skip,
            (a, b) => Opened::With(Box::new((
                a.heard_by(&self.0).unwrap_or_else(|| Box::new(())),
                b.heard_by(&self.1).unwrap_or_else(|| Box::new(())),
            ))),
        }
    }
    fn progress(&self, progress: Progress) {
        self.0.progress(progress);
        self.1.progress(progress);
    }
    fn event(&self, event: &S::Event) {
        self.0.event(event);
        self.1.event(event);
    }
    fn closed(&self, closed: Closed) {
        // Each hears its close even if the other panics, so a recording's tree stays whole.
        let first = panic::catch_unwind(AssertUnwindSafe(|| self.0.closed(closed)));
        self.1.closed(closed);
        first.unwrap_or_else(|panic| panic::resume_unwind(panic));
    }
}

// A parent's observer is borrowed, unboxed, and asked only by the parent.
enum Listener<'s, S: Reports> {
    Own(Box<dyn Observer<S> + 's>),
    Parents(&'s (dyn Observer<S> + 's)),
}

/// An algorithm's side of one scope: it reports here and checks here.
pub struct Scope<'s, S: Reports> {
    // Heard here since the instant; a parent's observer heard as `Hears`, so only the parent asks it.
    listener: Option<(Listener<'s, S>, Instant)>,
    parent: Option<&'s Scope<'s, S>>,
    // The call's first stop, which every later check repeats.
    latch: &'s OnceLock<Stop>,
    // How it ended, when known before the drop; otherwise the drop infers it.
    ended: OnceLock<Ended>,
    // Summed by `advance`, so reports from several threads stay in order.
    done: Mutex<u64>,
}

/// Runs `body` as one call under `observer`; an error carrying the call's stop makes it [`Outcome::Stopped`], any other error wins. A stop for one part of the call belongs in [`Scope::under`].
pub fn try_run<S: Reports, R, E: Stopping>(
    observer: &(dyn Observer<S> + '_),
    body: impl FnOnce(&Scope<'_, S>) -> Result<R, E>,
) -> Result<Outcome<R>, E> {
    match call(observer, body) {
        (Ok(result), _) => Ok(Outcome::Completed(result)),
        (Err(_), Some(reason)) => Ok(Outcome::Stopped {
            reason,
            residue: (),
        }),
        (Err(error), None) => Err(error),
    }
}

/// As [`try_run`], for a body whose error is [`Interrupted`]: every stop ends the call as [`Outcome::Stopped`], so only the algorithm's own error comes out.
pub fn run_fallible<S: Reports, R, E>(
    observer: &(dyn Observer<S> + '_),
    body: impl FnOnce(&Scope<'_, S>) -> Result<R, Interrupted<E>>,
) -> Result<Outcome<R>, E> {
    match call(observer, body).0 {
        Ok(result) => Ok(Outcome::Completed(result)),
        Err(Interrupted::Stop(reason)) => Ok(Outcome::Stopped {
            reason,
            residue: (),
        }),
        Err(Interrupted::Own(error)) => Err(error),
    }
}

// The body's result, and the call's stop if its error carries it.
fn call<S: Reports, R, E: Stopping>(
    observer: &(dyn Observer<S> + '_),
    body: impl FnOnce(&Scope<'_, S>) -> Result<R, E>,
) -> (Result<R, E>, Option<Stop>) {
    let latch = OnceLock::new();
    let call = Scope {
        listener: Some((Listener::Own(Box::new(observer)), Instant::now())),
        parent: None,
        latch: &latch,
        ended: OnceLock::new(),
        done: Mutex::new(0),
    };
    let result = body(&call);
    debug_assert!(
        result.is_err() || latch.get().is_none(),
        "a check returned Err({:?}), but the body returned Ok: propagate check()'s Err with `?`",
        latch.get()
    );
    let stopped = call.close(&result);
    (result, stopped)
}

/// As [`try_run`], for a body whose only error is a stop.
pub fn run<S: Reports, R>(
    observer: &(dyn Observer<S> + '_),
    body: impl FnOnce(&Scope<'_, S>) -> Result<R, Stop>,
) -> Outcome<R> {
    try_run(observer, body).unwrap_or_else(|reason| Outcome::Stopped {
        reason,
        residue: (),
    })
}

impl<'s, S: Reports> Scope<'s, S> {
    /// Runs `body` as a part with its own start and end, which follows from what `body` returns.
    pub fn within<R, E: Stopping>(
        &self,
        scope: S,
        body: impl FnOnce(&Scope<'_, S>) -> Result<R, E>,
    ) -> Result<R, E> {
        let child = self.open(&scope);
        let result = body(&child);
        child.close(&result);
        result
    }

    /// As [`within`](Self::within), for a body in vocabulary `I` that this one reads [`Within`]: its scopes join this tree, and its stops are this call's.
    pub fn within_as<I: Reports, R, E: Stopping>(
        &self,
        scope: S,
        body: impl FnOnce(&Scope<'_, I>) -> Result<R, E>,
    ) -> Result<R, E>
    where
        S: Within<I>,
    {
        self.nest(scope, |nest| call(nest, body).0)
    }

    /// As [`within`](Self::within), with `control` asked too: its stop ends only this part, as [`Outcome::Stopped`]; the call's stop is `Err`.
    pub fn under<R, E: Stopping>(
        &self,
        scope: S,
        control: &(dyn Observer<S> + '_),
        body: impl FnOnce(&Scope<'_, S>) -> Result<R, E>,
    ) -> Result<Outcome<R>, E> {
        let part = Part {
            scope: self.open(&scope),
            ask: Scope::ask,
        };
        match call(&(&part, control), body) {
            (Ok(result), _) => Ok(Outcome::Completed(result)),
            // Decided by the stop the error carries, as the call's may latch after the part's.
            (Err(error), _)
                if error
                    .stop()
                    .is_some_and(|stop| self.stopped() == Some(stop)) =>
            {
                Err(error)
            }
            (Err(_), Some(reason)) => Ok(Outcome::Stopped {
                reason,
                residue: (),
            }),
            (Err(error), None) => Err(error),
        }
    }

    /// Reports how far this scope got; a later `advance` adds to it.
    pub fn progress(&self, done: u64, total: Option<u64>) {
        self.report(|_| done, total);
    }

    /// Adds `by` units to this scope's progress and reports it, in order across threads.
    pub fn advance(&self, by: u64, total: Option<u64>) {
        self.report(|done| done + by, total);
    }

    // Under the lock, so reports reach the observer in the order they were counted.
    fn report(&self, next: impl FnOnce(u64) -> u64, total: Option<u64>) {
        let mut done = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *done = next(*done);
        if let Some(observer) = self.observer() {
            observer.progress(Progress { done: *done, total });
        }
    }

    /// Reports the event `event` builds, building it only if something hears it.
    pub fn event(&self, event: impl FnOnce() -> S::Event) {
        if let Some(observer) = self.observer() {
            observer.event(&event());
        }
    }

    /// A safe point: `Err` if an observer, here or in a scope around this one, stops the call.
    pub fn check(&self) -> Result<(), Stop> {
        match self.latched(Self::ask) {
            ControlFlow::Break(reason) => Err(reason),
            ControlFlow::Continue(()) => Ok(()),
        }
    }

    /// A scope `scope` inside this one, as the observer of an inner call: its scopes and events join this tree, its progress is this new scope's, and its checks and stops this call's.
    pub fn nest<R, E: Stopping>(
        &self,
        scope: S,
        body: impl FnOnce(&Nest<'_, S>) -> Result<R, E>,
    ) -> Result<R, E> {
        let nest = Nest {
            scope: self.open(&scope),
            ask: Scope::ask,
        };
        let result = body(&nest);
        let Nest { mut scope, .. } = nest;
        // The inner call's end stands unless none ran or the body's own error followed it.
        if result.as_ref().is_err_and(|error| error.stop().is_none()) {
            scope.ended.take();
        }
        scope.close(&result);
        result
    }

    fn open(&self, scope: &S) -> Scope<'_, S> {
        let listener = self
            .observer()
            .and_then(|observer| match observer.open(scope) {
                Opened::Same => Some(Listener::Parents(observer)),
                Opened::Skip => None,
                Opened::With(child) => Some(Listener::Own(child)),
            });
        Scope {
            listener: listener.map(|listener| (listener, Instant::now())),
            parent: Some(self),
            latch: self.latch,
            ended: OnceLock::new(),
            done: Mutex::new(0),
        }
    }

    fn is_heard(&self) -> bool {
        self.observer().is_some()
    }

    fn stopped(&self) -> Option<Stop> {
        self.latch.get().copied()
    }

    // Ends as `result` says, closing on drop; the latched stop if `result`'s error carries it, as an algorithm's own error does not.
    fn close<R, E: Stopping>(self, result: &Result<R, E>) -> Option<Stop> {
        let stopped = result.as_ref().err().and_then(|error| {
            let reason = self.stopped()?;
            debug_assert_eq!(
                E::from(reason).stop(),
                Some(reason),
                "Stopping::stop must find the stop From<Stop> wrapped"
            );
            (error.stop() == Some(reason)).then_some(reason)
        });
        let _ = self.ended.set(match (result, stopped) {
            (Ok(_), _) => Ended::Completed,
            (Err(_), Some(reason)) => Ended::Stopped(reason),
            (Err(_), None) => Ended::Failed,
        });
        stopped
    }

    // The call's stop, or else what `ask` says, which then latches.
    fn latched(&self, ask: fn(&Self) -> ControlFlow<Stop>) -> ControlFlow<Stop> {
        if let Some(&reason) = self.latch.get() {
            return ControlFlow::Break(reason);
        }
        ask(self).map_break(|reason| *self.latch.get_or_init(|| reason))
    }

    fn ask_own(&self) -> ControlFlow<Stop> {
        match &self.listener {
            Some((Listener::Own(own), _)) => own.check(),
            _ => ControlFlow::Continue(()),
        }
    }

    fn observer(&self) -> Option<&(dyn Observer<S> + 's)> {
        self.listener.as_ref().map(|(listener, _)| match listener {
            Listener::Own(own) => &**own,
            Listener::Parents(parents) => *parents,
        })
    }

    // Innermost first, so a scope's own rule decides before the call's.
    fn ask(&self) -> ControlFlow<Stop> {
        self.ask_own()?;
        self.parent.map_or(ControlFlow::Continue(()), Scope::ask)
    }
}

impl<S: Reports> Drop for Scope<'_, S> {
    fn drop(&mut self) {
        let ended = match (self.ended.get(), thread::panicking(), self.stopped()) {
            (Some(&ended), _, _) => ended,
            (None, true, _) => Ended::Unwound,
            (None, false, Some(reason)) => Ended::Stopped(reason),
            (None, false, None) => Ended::Failed,
        };
        if let (Some(observer), Some((_, opened))) = (self.observer(), &self.listener) {
            let closed = Closed {
                ended,
                elapsed: opened.elapsed(),
            };
            // A second panic out of a drop while unwinding aborts; the first is already on its way.
            match thread::panicking() {
                true => drop(panic::catch_unwind(AssertUnwindSafe(|| {
                    observer.closed(closed)
                }))),
                false => observer.closed(closed),
            }
        }
    }
}

impl<S: Reports> std::fmt::Debug for Scope<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("heard", &self.is_heard())
            .field("stopped", &self.stopped())
            .finish_non_exhaustive()
    }
}

/// An inner call's scope as one of this call, which ends as the inner one ended.
pub struct Nest<'n, O: Reports> {
    scope: Scope<'n, O>,
    // The inner call's root asks every scope around it; its inner scopes ask only their own, as their parents ask the rest.
    ask: fn(&Scope<'n, O>) -> ControlFlow<Stop>,
}

impl<I: Reports, O: Within<I>> Observer<I> for Nest<'_, O> {
    fn check(&self) -> ControlFlow<Stop> {
        self.scope.latched(self.ask)
    }
    fn open(&self, scope: &I) -> Opened<'_, I> {
        let scope = self.scope.open(&O::scope(scope));
        match scope.is_heard() {
            true => Opened::With(Box::new(Nest {
                scope,
                ask: Scope::ask_own,
            })),
            false => Opened::Skip,
        }
    }
    fn progress(&self, progress: Progress) {
        self.scope.progress(progress.done, progress.total);
    }
    fn event(&self, event: &I::Event) {
        if let Some(event) = self.scope.is_heard().then(|| O::event(event)).flatten() {
            self.scope.event(|| event);
        }
    }
    fn closed(&self, closed: Closed) {
        let first = self.scope.ended.set(closed.ended).is_ok();
        debug_assert!(first, "a Nest hears one inner call");
    }
}

// A part's scope as the root observer of its own call, in the same vocabulary.
struct Part<'p, S: Reports> {
    scope: Scope<'p, S>,
    ask: fn(&Scope<'p, S>) -> ControlFlow<Stop>,
}

impl<S: Reports> Observer<S> for Part<'_, S> {
    fn check(&self) -> ControlFlow<Stop> {
        self.scope.latched(self.ask)
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        let scope = self.scope.open(scope);
        match scope.is_heard() {
            true => Opened::With(Box::new(Part {
                scope,
                ask: Scope::ask_own,
            })),
            false => Opened::Skip,
        }
    }
    fn progress(&self, progress: Progress) {
        self.scope.progress(progress.done, progress.total);
    }
    fn event(&self, event: &S::Event) {
        if let Some(observer) = self.scope.observer() {
            observer.event(event);
        }
    }
    fn closed(&self, closed: Closed) {
        let _ = self.scope.ended.set(closed.ended);
    }
}
