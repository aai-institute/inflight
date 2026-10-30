use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::{Closed, Observer, Progress, Vocabulary};

/// What a [`Forward`] sends; scope `0` is the call itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Message<S, R> {
    /// A scope opened inside `parent`.
    Opened {
        /// The new scope.
        id: u64,
        /// The scope it opened in.
        parent: u64,
        /// What it is.
        scope: S,
    },
    /// A scope's progress.
    Progress {
        /// The scope.
        id: u64,
        /// How far it got.
        progress: Progress,
    },
    /// A scope's event, as `keep` turned it into an `R`.
    Event {
        /// The scope.
        id: u64,
        /// What happened.
        event: R,
    },
    /// A scope ended.
    Closed {
        /// The scope.
        id: u64,
        /// How it ended.
        closed: Closed,
    },
}

/// Sends what a call reports to its [`Outbox`], each event as `keep` turns it into an `R`.
#[derive(Debug)]
pub struct Forward<F, S, R> {
    keep: F,
    ids: AtomicU64,
    sender: Sender<Message<S, R>>,
}

/// Receives what a [`Forward`] sent, on whichever thread drains it.
#[derive(Debug)]
pub struct Outbox<S, R>(Receiver<Message<S, R>>);

impl<F, S, R> Forward<F, S, R> {
    /// A forward and the outbox it sends to.
    pub fn new(keep: F) -> (Self, Outbox<S, R>) {
        let (sender, receiver) = mpsc::channel();
        let forward = Self {
            keep,
            ids: AtomicU64::new(1),
            sender,
        };
        (forward, Outbox(receiver))
    }
}

impl<S, R> Outbox<S, R> {
    /// Hands over everything sent so far, in order, but of each scope's progress only the latest.
    pub fn drain<E>(
        &self,
        mut deliver: impl FnMut(Message<S, R>) -> Result<(), E>,
    ) -> Result<(), E> {
        let sent: Vec<_> = self.0.try_iter().collect();
        let mut latest = HashMap::new();
        for (k, message) in sent.iter().enumerate() {
            if let Message::Progress { id, .. } = message {
                latest.insert(*id, k);
            }
        }
        for (k, message) in sent.into_iter().enumerate() {
            let superseded = matches!(message, Message::Progress { id, .. } if latest[&id] != k);
            if !superseded {
                deliver(message)?;
            }
        }
        Ok(())
    }
}

// One scope's sending end; scope ids come from the call's counter.
struct Sending<'f, F, S, R> {
    keep: &'f F,
    ids: &'f AtomicU64,
    id: u64,
    sender: Sender<Message<S, R>>,
}

impl<'f, F, S, R> Sending<'f, F, S, R> {
    // A dropped outbox only means nobody listens any more.
    fn send(&self, message: Message<S, R>) {
        let _ = self.sender.send(message);
    }

    fn open<V>(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + 'f>>
    where
        V: Vocabulary<Scope = S>,
        S: Clone + Send + 'f,
        F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
        R: Send + 'f,
    {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.send(Message::Opened {
            id,
            parent: self.id,
            scope: scope.clone(),
        });
        Some(Box::new(Sending {
            keep: self.keep,
            ids: self.ids,
            id,
            sender: self.sender.clone(),
        }))
    }
}

impl<V, F, R> Observer<V> for Sending<'_, F, V::Scope, R>
where
    V: Vocabulary,
    V::Scope: Clone + Send,
    F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
    R: Send,
{
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        Sending::open::<V>(self, scope)
    }
    fn progress(&self, progress: Progress) {
        self.send(Message::Progress {
            id: self.id,
            progress,
        });
    }
    fn event(&self, event: &V::Event<'_>) {
        self.send(Message::Event {
            id: self.id,
            event: (self.keep)(event),
        });
    }
    fn closed(&self, closed: Closed) {
        self.send(Message::Closed {
            id: self.id,
            closed,
        });
    }
}

impl<F, S, R> Forward<F, S, R> {
    fn call(&self) -> Sending<'_, F, S, R> {
        Sending {
            keep: &self.keep,
            ids: &self.ids,
            id: 0,
            sender: self.sender.clone(),
        }
    }
}

impl<V, F, R> Observer<V> for Forward<F, V::Scope, R>
where
    V: Vocabulary,
    V::Scope: Clone + Send,
    F: for<'e> Fn(&V::Event<'e>) -> R + Sync,
    R: Send,
{
    fn open(&self, scope: &V::Scope) -> Option<Box<dyn Observer<V> + '_>> {
        self.call().open::<V>(scope)
    }
    fn progress(&self, progress: Progress) {
        Observer::<V>::progress(&self.call(), progress);
    }
    fn event(&self, event: &V::Event<'_>) {
        Observer::<V>::event(&self.call(), event);
    }
    fn closed(&self, closed: Closed) {
        Observer::<V>::closed(&self.call(), closed);
    }
}
