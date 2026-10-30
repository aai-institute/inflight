use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use crate::{Control, Deadline, Flag, Latched, Stop};

/// A computation's own words for what it reports.
pub trait Vocabulary: 'static {
    /// What a scope is, such as a block or a run.
    type Scope;
    /// What happens inside a scope.
    type Event<'a>;
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
    /// It returned early without a stop, such as on an error.
    Failed,
    /// A panic unwound through it.
    Unwound,
}

/// A scope's end, as its observer hears of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed {
    /// How it ended.
    pub ended: Ended,
    /// From its opening to its end.
    pub elapsed: Duration,
}

/// Hears what a computation reports and may stop it at its safe points; every method's default ignores.
pub trait Observer<V: Vocabulary>: Sync {
    /// Asked at each safe point; `Break` ends the call there.
    fn check(&self) -> ControlFlow<Stop> {
        ControlFlow::Continue(())
    }

    /// The observer of a scope that opens inside this one; `None` skips everything inside it.
    fn open(&self, _scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        None
    }

    /// The scope's latest progress.
    fn progress(&self, _progress: Progress) {}

    /// Something the computation reports.
    fn event(&self, _event: &V::Event<'_>) {}

    /// The scope ended; called once, last.
    fn closed(&self, _closed: Closed) {}
}

/// Ignores everything and never stops.
impl<V: Vocabulary> Observer<V> for () {}

impl<V: Vocabulary> Observer<V> for Flag {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(self)
    }
}

impl<V: Vocabulary> Observer<V> for Deadline {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(self)
    }
}

impl<V: Vocabulary> Observer<V> for Latched<'_> {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(self)
    }
}

#[cfg(feature = "memory")]
impl<V: Vocabulary> Observer<V> for crate::MemoryCap {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(self)
    }
}

impl<V: Vocabulary> Observer<V> for dyn Control + '_ {
    fn check(&self) -> ControlFlow<Stop> {
        Control::check(self)
    }
}

/// A control of the caller's own as an observer that only checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Checked<C>(pub C);

impl<V: Vocabulary, C: Control> Observer<V> for Checked<C> {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()
    }
}

impl<V: Vocabulary, O: Observer<V> + ?Sized> Observer<V> for &O {
    fn check(&self) -> ControlFlow<Stop> {
        (**self).check()
    }
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        (**self).open(scope)
    }
    fn progress(&self, progress: Progress) {
        (**self).progress(progress)
    }
    fn event(&self, event: &V::Event<'_>) {
        (**self).event(event)
    }
    fn closed(&self, closed: Closed) {
        (**self).closed(closed)
    }
}

impl<V: Vocabulary, O: Observer<V> + ?Sized> Observer<V> for Box<O> {
    fn check(&self) -> ControlFlow<Stop> {
        (**self).check()
    }
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        (**self).open(scope)
    }
    fn progress(&self, progress: Progress) {
        (**self).progress(progress)
    }
    fn event(&self, event: &V::Event<'_>) {
        (**self).event(event)
    }
    fn closed(&self, closed: Closed) {
        (**self).closed(closed)
    }
}

/// Both hear everything; the first is asked first.
impl<V: Vocabulary, A: Observer<V>, B: Observer<V>> Observer<V> for (A, B) {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()?;
        self.1.check()
    }
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        match (self.0.open(scope), self.1.open(scope)) {
            (Some(a), Some(b)) => Some(Box::new((a, b))),
            (a, b) => a.or(b),
        }
    }
    fn progress(&self, progress: Progress) {
        self.0.progress(progress);
        self.1.progress(progress);
    }
    fn event(&self, event: &V::Event<'_>) {
        self.0.event(event);
        self.1.event(event);
    }
    fn closed(&self, closed: Closed) {
        self.0.closed(closed);
        self.1.closed(closed);
    }
}

/// A computation's side of one scope: it reports here, checks here, and the scope closes exactly once.
pub struct Scope<'s, V: Vocabulary> {
    listener: Option<Listener<'s, V>>,
    up: Up<'s, V>,
    opened: Option<Instant>,
    closed: bool,
}

enum Listener<'s, V: Vocabulary> {
    Root(&'s (dyn Observer<V> + 's)),
    Child(Box<dyn Observer<V> + 's>),
}

enum Up<'s, V: Vocabulary> {
    // The call's first stop, which every later check repeats.
    Root(OnceLock<Stop>),
    Child(&'s Scope<'s, V>),
}

impl<'s, V: Vocabulary> Scope<'s, V> {
    /// One call under `observer`.
    pub fn root(observer: &'s (dyn Observer<V> + 's)) -> Self {
        Self {
            listener: Some(Listener::Root(observer)),
            up: Up::Root(OnceLock::new()),
            opened: Some(Instant::now()),
            closed: false,
        }
    }

    /// A scope inside this one; unheard, and untimed, if no observer wants it.
    pub fn open(&self, scope: &V::Scope) -> Scope<'_, V> {
        let listener = self
            .observer()
            .and_then(|observer| observer.open(scope))
            .map(Listener::Child);
        Scope {
            opened: listener.as_ref().map(|_| Instant::now()),
            listener,
            up: Up::Child(self),
            closed: false,
        }
    }

    /// Whether anything hears this scope.
    pub fn is_heard(&self) -> bool {
        self.listener.is_some()
    }

    /// Reports progress.
    pub fn progress(&self, progress: Progress) {
        if let Some(observer) = self.observer() {
            observer.progress(progress);
        }
    }

    /// Reports the event `event` builds, building it only if something hears it.
    pub fn event<'e>(&self, event: impl FnOnce() -> V::Event<'e>) {
        if let Some(observer) = self.observer() {
            observer.event(&event());
        }
    }

    /// The call's first stop, if any check has broken.
    pub fn stopped(&self) -> Option<Stop> {
        self.latch().get().copied()
    }

    /// Ends this scope as completed; a scope dropped without it ends stopped, failed or unwound.
    pub fn close(mut self) {
        self.end(Ended::Completed);
    }

    fn observer(&self) -> Option<&(dyn Observer<V> + 's)> {
        self.listener.as_ref().map(|listener| match listener {
            Listener::Root(observer) => *observer,
            Listener::Child(observer) => &**observer,
        })
    }

    fn latch(&self) -> &OnceLock<Stop> {
        match &self.up {
            Up::Root(latch) => latch,
            Up::Child(parent) => parent.latch(),
        }
    }

    // Innermost first, so a scope's own rule decides before the call's.
    fn ask(&self) -> ControlFlow<Stop> {
        if let Some(observer) = self.observer() {
            observer.check()?;
        }
        match &self.up {
            Up::Root(_) => ControlFlow::Continue(()),
            Up::Child(parent) => parent.ask(),
        }
    }

    fn end(&mut self, ended: Ended) {
        self.closed = true;
        if let (Some(observer), Some(opened)) = (self.observer(), self.opened) {
            observer.closed(Closed {
                ended,
                elapsed: opened.elapsed(),
            });
        }
    }
}

/// Every observer from this scope out to the call's is asked; the first stop holds for the whole call.
impl<V: Vocabulary> Control for Scope<'_, V> {
    fn check(&self) -> ControlFlow<Stop> {
        let latch = self.latch();
        if let Some(&reason) = latch.get() {
            return ControlFlow::Break(reason);
        }
        match self.ask() {
            ControlFlow::Break(reason) => ControlFlow::Break(*latch.get_or_init(|| reason)),
            ControlFlow::Continue(()) => ControlFlow::Continue(()),
        }
    }
}

impl<V: Vocabulary> Drop for Scope<'_, V> {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let ended = match (thread::panicking(), self.stopped()) {
            (true, _) => Ended::Unwound,
            (false, Some(reason)) => Ended::Stopped(reason),
            (false, None) => Ended::Failed,
        };
        self.end(ended);
    }
}

impl<V: Vocabulary> std::fmt::Debug for Scope<'_, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("heard", &self.is_heard())
            .field("stopped", &self.stopped())
            .finish_non_exhaustive()
    }
}
