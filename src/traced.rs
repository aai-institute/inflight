use std::fmt::Debug;

use tracing::Span;

use crate::{Closed, Observer, Progress, Vocabulary};

/// Reports to `tracing`: a scope is a span under its parent's, its events, progress and end are events in it.
#[derive(Clone, Debug)]
pub struct Traced(Span);

impl Traced {
    /// Under `span`, often [`Span::current`].
    pub fn new(span: Span) -> Self {
        Self(span)
    }
}

impl<V> Observer<V> for Traced
where
    V: Vocabulary,
    V::Scope: Debug,
    for<'a> V::Event<'a>: Debug,
{
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        let span = tracing::info_span!(parent: &self.0, "scope", scope = ?scope);
        Some(Box::new(Traced(span)))
    }
    fn progress(&self, progress: Progress) {
        tracing::trace!(parent: &self.0, done = progress.done, total = ?progress.total, "progress");
    }
    fn event(&self, event: &V::Event<'_>) {
        tracing::debug!(parent: &self.0, event = ?event, "event");
    }
    fn closed(&self, closed: Closed) {
        tracing::info!(parent: &self.0, ended = ?closed.ended, elapsed = ?closed.elapsed, "closed");
    }
}
