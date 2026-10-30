use std::rc::Rc;

use inflight::{run, Forward, Reports, Within};

#[derive(Clone, Debug, PartialEq)]
struct Algo;
impl Reports for Algo {
    type Event = Rc<str>;
}

// The owner's owned, sendable form: same scopes, events as `String`.
#[derive(Clone, Debug, PartialEq)]
struct Sent;
impl Reports for Sent {
    type Event = String;
}
impl Within<Algo> for Sent {
    fn scope(_: &Algo) -> Self {
        Sent
    }
    fn event(inner: &Rc<str>) -> Option<String> {
        Some(inner.to_string())
    }
}

#[test]
fn a_non_send_vocabulary_forwards_through_an_owned_one() {
    let (forward, outbox) = Forward::<Sent>::new();
    let _ = run::<Algo, _>(&forward, |call| {
        call.within(Algo, |part| {
            part.event(|| Rc::from("x"));
            Ok::<_, inflight::Stop>(())
        })
    });
    assert_eq!(outbox.record().children[0].1.events, vec!["x".to_string()]);
}
