use inflight::{Closed, Ended, Forward, Message, Progress, Scope, Vocabulary};

struct Test;

impl Vocabulary for Test {
    type Scope = &'static str;
    type Event<'a> = &'a [u32];
}

type Sent = Message<&'static str, Vec<u32>>;

type Outbox = inflight::Outbox<&'static str, Vec<u32>>;

type Keep = fn(&&[u32]) -> Vec<u32>;

fn forward() -> (Forward<Keep, &'static str, Vec<u32>>, Outbox) {
    Forward::new(|event| event.to_vec())
}

fn drained(outbox: &Outbox) -> Vec<Sent> {
    let mut sent = Vec::new();
    outbox
        .drain(|message| {
            sent.push(message);
            Ok::<_, ()>(())
        })
        .expect("delivers");
    sent
}

fn ended(message: &Sent) -> Option<(u64, Ended)> {
    match message {
        Message::Closed {
            id,
            closed: Closed { ended, .. },
        } => Some((*id, *ended)),
        _ => None,
    }
}

fn progress(done: u64) -> Progress {
    Progress {
        done,
        total: Some(3),
    }
}

#[test]
fn a_drain_keeps_the_order_and_only_the_latest_progress() {
    let (forward, outbox) = forward();
    {
        let call = Scope::<Test>::root(&forward);
        let block = call.open(&"block");
        for done in 1..=3 {
            block.progress(progress(done));
        }
        block.event(|| &[9]);
        block.close();
        call.close();
    }
    let sent = drained(&outbox);
    assert_eq!(
        sent[..3],
        [
            Message::Opened {
                id: 1,
                parent: 0,
                scope: "block"
            },
            Message::Progress {
                id: 1,
                progress: progress(3)
            },
            Message::Event {
                id: 1,
                event: vec![9]
            },
        ]
    );
    let ends: Vec<_> = sent[3..].iter().map(ended).collect();
    assert_eq!(
        ends,
        [Some((1, Ended::Completed)), Some((0, Ended::Completed))]
    );
}

#[test]
fn each_drain_delivers_the_progress_since_the_last() {
    let (forward, outbox) = forward();
    let call = Scope::<Test>::root(&forward);
    call.progress(progress(1));
    assert_eq!(
        drained(&outbox),
        [Message::Progress {
            id: 0,
            progress: progress(1)
        }]
    );
    call.progress(progress(2));
    assert_eq!(
        drained(&outbox),
        [Message::Progress {
            id: 0,
            progress: progress(2)
        }]
    );
}

#[test]
fn a_delivery_error_ends_the_drain() {
    let (forward, outbox) = forward();
    let call = Scope::<Test>::root(&forward);
    call.event(|| &[1]);
    call.event(|| &[2]);
    let mut delivered = 0;
    let result = outbox.drain(|_| {
        delivered += 1;
        Err("callback raised")
    });
    assert_eq!((result, delivered), (Err("callback raised"), 1));
}
