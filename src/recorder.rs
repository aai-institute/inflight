use std::collections::HashMap;
use std::convert::Infallible;

use crate::{Closed, Message, Outbox, Progress, Reports};

/// What one scope reported.
#[derive(Clone, Debug, PartialEq)]
pub struct Record<S: Reports> {
    /// Its events, in order.
    pub events: Vec<S::Event>,
    /// Its last progress.
    pub progress: Option<Progress>,
    /// The scopes opened inside it, in the order they opened.
    pub children: Vec<(S, Record<S>)>,
    /// Its end; `None` only while it is open.
    pub closed: Option<Closed>,
}

impl<S: Reports> Default for Record<S> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            progress: None,
            children: Vec::new(),
            closed: None,
        }
    }
}

impl<S: Reports> Outbox<S> {
    /// What was sent and not yet drained, as the call's tree of scopes; to also drain live, record from a second `Forward`.
    pub fn record(&self) -> Record<S> {
        let mut records: HashMap<u64, Record<S>> = HashMap::new();
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
}

fn assemble<S: Reports>(
    id: u64,
    records: &mut HashMap<u64, Record<S>>,
    opened: &mut HashMap<u64, Vec<(u64, S)>>,
) -> Record<S> {
    let mut record = records.remove(&id).unwrap_or_default();
    record.children = opened
        .remove(&id)
        .unwrap_or_default()
        .into_iter()
        .map(|(child, scope)| (scope, assemble(child, records, opened)))
        .collect();
    record
}
