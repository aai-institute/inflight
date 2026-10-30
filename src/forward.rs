use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use crate::{Closed, Observer, Opened, Progress, Reports, Within};

/// What a [`Forward`] sends; scope `0` is the call itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Message<S: Reports> {
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
    /// A scope's event.
    Event {
        /// The scope.
        id: u64,
        /// What happened.
        event: S::Event,
    },
    /// A scope ended.
    Closed {
        /// The scope.
        id: u64,
        /// How it ended.
        closed: Closed,
    },
}

/// Sends what a call of any vocabulary `S` reads as [`Within`] reports to its [`Outbox`].
#[derive(Debug)]
pub struct Forward<S: Reports> {
    // Shared by the call's scopes, so each gets its own id.
    ids: Arc<AtomicU64>,
    id: u64,
    sender: Sender<Message<S>>,
}

/// Receives what a [`Forward`] sent, on whichever thread drains it.
#[derive(Debug)]
pub struct Outbox<S: Reports>(Receiver<Message<S>>);

impl<S: Reports> Forward<S> {
    /// A forward and the outbox it sends to.
    pub fn new() -> (Self, Outbox<S>) {
        let (sender, receiver) = mpsc::channel();
        let forward = Self {
            ids: Arc::new(AtomicU64::new(1)),
            id: 0,
            sender,
        };
        (forward, Outbox(receiver))
    }
}

impl<S: Reports> Outbox<S> {
    /// Hands over everything sent so far, in order, but of each scope's progress only the latest.
    pub fn drain<E>(&self, deliver: impl FnMut(Message<S>) -> Result<(), E>) -> Result<(), E> {
        Self::deliver(self.0.try_iter().collect(), deliver)
    }

    /// As [`drain`](Self::drain), waiting for each next message, until every [`Forward`] is dropped.
    pub fn follow<E>(&self, mut deliver: impl FnMut(Message<S>) -> Result<(), E>) -> Result<(), E> {
        while let Ok(first) = self.0.recv() {
            let sent = std::iter::once(first).chain(self.0.try_iter()).collect();
            Self::deliver(sent, &mut deliver)?;
        }
        Ok(())
    }

    fn deliver<E>(
        sent: Vec<Message<S>>,
        mut deliver: impl FnMut(Message<S>) -> Result<(), E>,
    ) -> Result<(), E> {
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

impl<S: Reports> Forward<S> {
    // A dropped outbox only means nobody listens any more.
    fn send(&self, message: Message<S>) {
        let _ = self.sender.send(message);
    }
}

impl<V: Reports, S: Within<V> + Send> Observer<V> for Forward<S>
where
    S::Event: Send,
{
    fn open(&self, scope: &V) -> Opened<'_, V> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.send(Message::Opened {
            id,
            parent: self.id,
            scope: S::scope(scope),
        });
        Opened::With(Box::new(Forward {
            ids: Arc::clone(&self.ids),
            id,
            sender: self.sender.clone(),
        }))
    }
    fn progress(&self, progress: Progress) {
        self.send(Message::Progress {
            id: self.id,
            progress,
        });
    }
    fn event(&self, event: &V::Event) {
        if let Some(event) = S::event(event) {
            self.send(Message::Event { id: self.id, event });
        }
    }
    fn closed(&self, closed: Closed) {
        self.send(Message::Closed {
            id: self.id,
            closed,
        });
    }
}
