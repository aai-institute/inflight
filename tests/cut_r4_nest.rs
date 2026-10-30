use std::sync::Mutex;

use inflight::{run, Observer, Progress, Reports, Stop};

#[derive(Clone, Copy, Debug, PartialEq)]
struct A;
impl Reports for A {
    type Event = u64;
}

#[derive(Default)]
struct Seen(Mutex<Vec<(u64, Option<u64>)>>);
impl Observer<A> for Seen {
    fn progress(&self, p: Progress) {
        self.0.lock().unwrap().push((p.done, p.total));
    }
}

#[test]
fn inner_root_progress_leaves_outer_count_alone() {
    let seen = Seen::default();
    let _ = run(&seen, |call| {
        call.within(A, |blocks| {
            for _ in 0..2 {
                let _ = blocks.nest(A, |nest| {
                    run(nest, |inner| {
                        inner.advance(500, Some(1000));
                        Ok::<(), Stop>(())
                    })
                    .into_result()
                });
                blocks.advance(1, Some(2));
            }
            Ok::<(), Stop>(())
        })
    });
    assert_eq!(
        *seen.0.lock().unwrap(),
        vec![
            (500, Some(1000)),
            (1, Some(2)),
            (500, Some(1000)),
            (2, Some(2))
        ]
    );
}
