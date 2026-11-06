#![allow(dead_code)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use inflight::{Closed, Observer, Opened, Progress, Reports, Within};

/// What a [`Recorder`] sends; scope `0` is the call itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Message<S: Reports> {
    Opened { id: u64, parent: u64, scope: S },
    Progress { id: u64, progress: Progress },
    Event { id: u64, event: S::Event },
    Closed { id: u64, closed: Closed },
}

/// Sends what a call of any vocabulary `S` reads as [`Within`] reports to its [`Recording`].
#[derive(Debug)]
pub struct Recorder<S: Reports> {
    ids: Arc<AtomicU64>,
    id: u64,
    sender: Sender<Message<S>>,
}

/// Receives what a [`Recorder`] sent, on whichever thread drains it.
#[derive(Debug)]
pub struct Recording<S: Reports>(Receiver<Message<S>>);

/// What one scope reported.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded<S: Reports> {
    pub events: Vec<S::Event>,
    pub progress: Option<Progress>,
    pub children: Vec<(S, Recorded<S>)>,
    pub closed: Option<Closed>,
}

impl<S: Reports> Default for Recorded<S> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            progress: None,
            children: Vec::new(),
            closed: None,
        }
    }
}

impl<S: Reports> Recorder<S> {
    pub fn new() -> (Self, Recording<S>) {
        let (sender, receiver) = mpsc::channel();
        let recorder = Self {
            ids: Arc::new(AtomicU64::new(1)),
            id: 0,
            sender,
        };
        (recorder, Recording(receiver))
    }

    fn send(&self, message: Message<S>) {
        let _ = self.sender.send(message);
    }
}

impl<S: Reports> Recording<S> {
    /// Everything sent so far, in order, but of each scope's progress only the latest.
    pub fn drain<E>(&self, deliver: impl FnMut(Message<S>) -> Result<(), E>) -> Result<(), E> {
        Self::deliver(self.0.try_iter().collect(), deliver)
    }

    /// As `drain`, waiting for each next message, until every recorder is dropped.
    pub fn follow<E>(&self, mut deliver: impl FnMut(Message<S>) -> Result<(), E>) -> Result<(), E> {
        while let Ok(first) = self.0.recv() {
            let sent = std::iter::once(first).chain(self.0.try_iter()).collect();
            Self::deliver(sent, &mut deliver)?;
        }
        Ok(())
    }

    /// What was sent and not yet drained, as the call's tree of scopes.
    pub fn record(&self) -> Recorded<S> {
        let mut records: HashMap<u64, Recorded<S>> = HashMap::new();
        let mut opened: HashMap<u64, Vec<(u64, S)>> = HashMap::new();
        let Ok(()) = self.drain(|message| {
            match message {
                Message::Opened { id, parent, scope } => {
                    opened.entry(parent).or_default().push((id, scope))
                }
                Message::Progress { id, progress } => {
                    records.entry(id).or_default().progress = Some(progress)
                }
                Message::Event { id, event } => records.entry(id).or_default().events.push(event),
                Message::Closed { id, closed } => {
                    records.entry(id).or_default().closed = Some(closed)
                }
            }
            Ok::<(), Infallible>(())
        });
        assemble(0, &mut records, &mut opened)
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

fn assemble<S: Reports>(
    id: u64,
    records: &mut HashMap<u64, Recorded<S>>,
    opened: &mut HashMap<u64, Vec<(u64, S)>>,
) -> Recorded<S> {
    let mut record = records.remove(&id).unwrap_or_default();
    record.children = opened
        .remove(&id)
        .unwrap_or_default()
        .into_iter()
        .map(|(child, scope)| (scope, assemble(child, records, opened)))
        .collect();
    record
}

impl<V: Reports, S: Within<V> + Send> Observer<V> for Recorder<S>
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
        Opened::With(Box::new(Recorder {
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

/// An event that a caller's generic rules understand, whatever the algorithm calls it.
pub trait Standard {
    fn is_iteration(&self) -> bool {
        false
    }
    fn residual(&self) -> Option<f64> {
        None
    }
}
