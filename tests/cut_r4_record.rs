use inflight::{run, Forward, Message, Reports, Stop};

#[derive(Clone, Copy, Debug, PartialEq)]
struct A;
impl Reports for A {
    type Event = u64;
}

// A live display drains while the call runs; a second forward keeps the whole tree for the record.
#[test]
fn live_drain_and_record_use_two_outboxes() {
    let (live, display) = Forward::<A>::new();
    let (kept, outbox) = Forward::<A>::new();
    let _ = run(&(live, kept), |call| {
        call.within(A, |scope| {
            scope.event(|| 7);
            let mut shown = 0;
            display
                .drain(|_: Message<A>| {
                    shown += 1;
                    Ok::<(), ()>(())
                })
                .unwrap();
            assert!(shown > 0);
            scope.event(|| 8);
            Ok::<(), Stop>(())
        })
    });
    let record = outbox.record();
    assert_eq!(record.children.len(), 1);
    assert_eq!(record.children[0].1.events, vec![7, 8]);
}

// One outbox drained live, then recorded: the record holds only what came after the drain.
#[test]
fn record_after_drain_holds_only_the_rest() {
    let (forward, outbox) = Forward::<A>::new();
    let _ = run(&forward, |call| {
        call.within(A, |scope| {
            scope.event(|| 7);
            outbox.drain(|_| Ok::<(), ()>(())).unwrap();
            scope.event(|| 8);
            Ok::<(), Stop>(())
        })
    });
    assert!(outbox.record().children.is_empty());
}
