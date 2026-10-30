use std::sync::Mutex;

use crate::{Closed, Observer, Progress, Vocabulary};

/// What one scope reported.
#[derive(Clone, Debug, PartialEq)]
pub struct Record<S, R> {
    /// Its events, in order.
    pub events: Vec<R>,
    /// Its last progress.
    pub progress: Option<Progress>,
    /// The scopes opened inside it, in the order they opened.
    pub children: Vec<(S, Record<S, R>)>,
    /// Its end; `None` only while it is open.
    pub closed: Option<Closed>,
}

impl<S, R> Default for Record<S, R> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            progress: None,
            children: Vec::new(),
            closed: None,
        }
    }
}

/// Keeps every scope, event and end of a call, each event as `keep` turns it into an `R`.
#[derive(Debug)]
pub struct Recorder<F, S, R> {
    keep: F,
    record: Mutex<Record<S, R>>,
}

impl<F, S, R> Recorder<F, S, R> {
    /// Nothing recorded yet.
    pub fn new(keep: F) -> Self {
        Self {
            keep,
            record: Mutex::new(Record::default()),
        }
    }

    /// The call's record.
    pub fn into_record(self) -> Record<S, R> {
        self.record
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// Each scope fills its own record and hands it to its slot in the parent's once it closes.
struct Child<'p, F, S, R> {
    keep: &'p F,
    parent: &'p Mutex<Record<S, R>>,
    slot: usize,
    record: Mutex<Record<S, R>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open<'p, V, F, R>(
    keep: &'p F,
    record: &'p Mutex<Record<V::Scope, R>>,
    scope: &V::Scope,
) -> Option<Box<dyn Observer<V> + 'p>>
where
    V: Vocabulary,
    V::Scope: Clone + Send,
    F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
    R: Send + 'p,
{
    let slot = {
        let mut parent = lock(record);
        parent.children.push((scope.clone(), Record::default()));
        parent.children.len() - 1
    };
    Some(Box::new(Child {
        keep,
        parent: record,
        slot,
        record: Mutex::new(Record::default()),
    }))
}

impl<V, F, R> Observer<V> for Recorder<F, V::Scope, R>
where
    V: Vocabulary,
    V::Scope: Clone + Send,
    F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
    R: Send,
{
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        open::<V, F, R>(&self.keep, &self.record, scope)
    }
    fn progress(&self, progress: Progress) {
        lock(&self.record).progress = Some(progress);
    }
    fn event(&self, event: &V::Event<'_>) {
        let kept = (self.keep)(event);
        lock(&self.record).events.push(kept);
    }
    fn closed(&self, closed: Closed) {
        lock(&self.record).closed = Some(closed);
    }
}

impl<V, F, R> Observer<V> for Child<'_, F, V::Scope, R>
where
    V: Vocabulary,
    V::Scope: Clone + Send,
    F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
    R: Send,
{
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        open::<V, F, R>(self.keep, &self.record, scope)
    }
    fn progress(&self, progress: Progress) {
        lock(&self.record).progress = Some(progress);
    }
    fn event(&self, event: &V::Event<'_>) {
        let kept = (self.keep)(event);
        lock(&self.record).events.push(kept);
    }
    fn closed(&self, closed: Closed) {
        let mut record = std::mem::take(&mut *lock(&self.record));
        record.closed = Some(closed);
        lock(self.parent).children[self.slot].1 = record;
    }
}
