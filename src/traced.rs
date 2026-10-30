use std::fmt::Debug;

use tracing::Span;

use crate::{Closed, Observer, Opened, Progress, Reports};

/// Reports to `tracing` under its span, often [`Span::current`]: a scope is a span under its parent's, its events, progress and end are events in it.
#[derive(Clone, Debug)]
pub struct Traced(pub Span);

impl<S> Observer<S> for Traced
where
    S: Reports + Debug,
    S::Event: Debug,
{
    fn open(&self, scope: &S) -> Opened<'_, S> {
        let span = tracing::info_span!(parent: &self.0, "scope", scope = ?scope);
        Opened::With(Box::new(Traced(span)))
    }
    fn progress(&self, progress: Progress) {
        tracing::trace!(parent: &self.0, done = progress.done, total = ?progress.total, "progress");
    }
    fn event(&self, event: &S::Event) {
        tracing::debug!(parent: &self.0, event = ?event, "event");
    }
    fn closed(&self, closed: Closed) {
        tracing::info!(parent: &self.0, ended = ?closed.ended, elapsed = ?closed.elapsed, "closed");
    }
}
