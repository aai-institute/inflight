use std::sync::Mutex;

use inflight::{run, Observer, Progress, Reports, Stop};

#[derive(Clone, Copy, Debug, PartialEq)]
struct A;
impl Reports for A {
    type Event = u64;
}

#[derive(Default)]
struct Seen(Mutex<Vec<u64>>);
impl Observer<A> for Seen {
    fn progress(&self, progress: Progress) {
        self.0.lock().unwrap().push(progress.done);
    }
}

// A resumed call starts from its residue's count, then advances.
#[test]
fn advance_continues_from_reported_progress() {
    let seen = Seen::default();
    let _ = run(&seen, |call| {
        call.within(A, |scope| {
            scope.progress(500, Some(1000));
            scope.advance(1, Some(1000));
            Ok::<(), Stop>(())
        })
    });
    assert_eq!(*seen.0.lock().unwrap(), vec![500, 501]);
}
