use inflight::{run, Closed, Ended, Forward, Message, Outcome, Progress, Reports, Scope, Stop};

#[derive(Clone, Debug, PartialEq)]
struct Named(&'static str);

impl Reports for Named {
    type Event = Vec<u32>;
}

type Sent = Message<Named>;

type Outbox = inflight::Outbox<Named>;

fn forward() -> (Forward<Named>, Outbox) {
    Forward::new()
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
    run(&forward, |call| {
        call.within(Named("block"), |block| {
            for done in 1..=3 {
                block.progress(done, Some(3));
            }
            block.event(|| vec![9]);
            Ok(())
        })
    });
    let sent = drained(&outbox);
    assert_eq!(
        sent[..3],
        [
            Message::Opened {
                id: 1,
                parent: 0,
                scope: Named("block")
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
    let outcome = run(&forward, |call: &Scope<'_, Named>| {
        call.progress(1, Some(3));
        assert_eq!(
            drained(&outbox),
            [Message::Progress {
                id: 0,
                progress: progress(1)
            }]
        );
        call.progress(2, Some(3));
        assert_eq!(
            drained(&outbox),
            [Message::Progress {
                id: 0,
                progress: progress(2)
            }]
        );
        Ok::<_, Stop>(())
    });
    assert_eq!(outcome, Outcome::Completed(()));
}

#[test]
fn a_delivery_error_ends_the_drain() {
    let (forward, outbox) = forward();
    run(&forward, |call: &Scope<'_, Named>| {
        call.event(|| vec![1]);
        call.event(|| vec![2]);
        Ok(())
    });
    let mut delivered = 0;
    let result = outbox.drain(|_| {
        delivered += 1;
        Err("callback raised")
    });
    assert_eq!((result, delivered), (Err("callback raised"), 1));
}
