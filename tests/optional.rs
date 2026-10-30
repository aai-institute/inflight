use std::sync::atomic::{AtomicU64, Ordering};

use inflight::{run, Flag, Observer, Outcome, Progress, Reports, Scope, Stop};

#[derive(Debug)]
struct Pass;
impl Reports for Pass {
    type Event = ();
}

#[derive(Default)]
struct Bar(AtomicU64);
impl<S: Reports> Observer<S> for Bar {
    fn progress(&self, p: Progress) {
        self.0.store(p.done, Ordering::Relaxed);
    }
}

fn body(call: &Scope<'_, Pass>) -> Result<u64, Stop> {
    call.within(Pass, |s| {
        s.check()?;
        s.progress(7, Some(7));
        Ok(7)
    })
}

#[test]
fn an_optional_bar() {
    for verbose in [false, true] {
        let flag = Flag::new();
        let bar = verbose.then(Bar::default);
        let shown: &dyn Observer<Pass> = bar.as_ref().map_or(&(), |b| b);
        assert_eq!(run(&(&flag, shown), body), Outcome::Completed(7));
        assert_eq!(bar.map(|b| b.0.into_inner()), verbose.then_some(7));
    }
}
