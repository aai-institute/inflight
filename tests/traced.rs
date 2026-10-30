use std::fmt::{Debug, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use inflight::{Progress, Scope, Traced, Vocabulary};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

struct Test;

impl Vocabulary for Test {
    type Scope = &'static str;
    type Event<'a> = &'a [u32];
}

/// What a subscriber saw: each span with its parent, each event with its span.
#[derive(Default)]
struct Seen {
    next: AtomicU64,
    spans: Mutex<Vec<(u64, Option<u64>, String)>>,
    events: Mutex<Vec<(Option<u64>, String)>>,
}

#[derive(Clone, Default)]
struct Capture(Arc<Seen>);

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut fields = Fields(format!("{} ", span.metadata().name()));
        span.record(&mut fields);
        let parent = span.parent().map(Id::into_u64);
        self.0.spans.lock().unwrap().push((id, parent, fields.0));
        Id::from_u64(id)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        let parent = event.parent().map(Id::into_u64);
        self.0.events.lock().unwrap().push((parent, fields.0));
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn scopes_become_nested_spans_and_reports_their_events() {
    let capture = Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        let call = tracing::info_span!("call");
        let traced = Traced::new(call);
        let root = Scope::<Test>::root(&traced);
        let block = root.open(&"block");
        let run = block.open(&"run");
        run.event(|| &[1, 2]);
        run.progress(Progress {
            done: 2,
            total: None,
        });
        run.close();
        block.close();
        root.close();
    });
    let spans = capture.0.spans.lock().unwrap().clone();
    let events = capture.0.events.lock().unwrap().clone();
    let [(call, None, _), (block, Some(block_parent), block_fields), (run, Some(run_parent), run_fields)] =
        &spans[..]
    else {
        panic!("three spans: {spans:?}");
    };
    assert_eq!((block_parent, run_parent), (call, block));
    assert!(block_fields.contains("scope=\"block\""), "{block_fields}");
    assert!(run_fields.contains("scope=\"run\""), "{run_fields}");
    let in_run: Vec<_> = events
        .iter()
        .filter(|(span, _)| *span == Some(*run))
        .collect();
    assert!(in_run[0].1.contains("event=[1, 2]"), "{in_run:?}");
    assert!(in_run[1].1.contains("done=2"), "{in_run:?}");
    assert!(in_run[2].1.contains("ended=Completed"), "{in_run:?}");
    let call_ends = events
        .iter()
        .filter(|(span, fields)| *span == Some(*call) && fields.contains("ended=Completed"));
    assert_eq!(call_ends.count(), 1);
}
