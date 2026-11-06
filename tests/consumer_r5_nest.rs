use inflight::{run, Reports, Stop};
mod common;
use common::{Recorded, Recorder};

#[derive(Clone, Debug, PartialEq)]
enum Host {
    Blocks,
    Lp,
}
impl Reports for Host {
    type Event = ();
}
#[derive(Clone, Debug, PartialEq)]
struct Lp;
impl Reports for Lp {
    type Event = ();
}
impl inflight::Within<Lp> for Host {
    fn scope(_: &Lp) -> Self {
        Host::Lp
    }
    fn event(_: &()) -> Option<()> {
        None
    }
}

#[test]
fn a_nested_root_reports_into_its_own_scope() {
    let (forward, outbox) = Recorder::<Host>::new();
    let _ = run(&forward, |call| {
        call.within(Host::Blocks, |blocks| {
            for _ in 0..2 {
                let _ = blocks.nest(Host::Lp, |nest| {
                    run(nest, |lp: &inflight::Scope<'_, Lp>| {
                        lp.advance(500, Some(1000));
                        Ok::<(), Stop>(())
                    })
                    .into_result()
                });
                blocks.advance(1, Some(2));
            }
            Ok::<(), Stop>(())
        })
    });
    let record = outbox.record();
    let (_, blocks) = &record.children[0];
    let done = |r: &Recorded<Host>| r.progress.map(|p| (p.done, p.total));
    assert_eq!(done(blocks), Some((2, Some(2))));
    assert_eq!(blocks.children.len(), 2);
    assert!(blocks
        .children
        .iter()
        .all(|(s, lp)| *s == Host::Lp && done(lp) == Some((500, Some(1000)))));
}
