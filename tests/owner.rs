use std::ops::ControlFlow;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use inflight::{run, Closed, Observer, Opened, Progress, Reports, Stop};

// A library owner's time-based Every: pass on progress at most once per `period`, per scope.
struct Throttle<O> {
    period: Duration,
    inner: O,
    last: Mutex<Option<Instant>>,
}

impl<S: Reports, O: Observer<S>> Observer<S> for Throttle<O> {
    fn check(&self) -> ControlFlow<Stop> {
        self.inner.check()
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        // Must wrap whatever `inner` decided; Opened cannot be inspected.
        match inflight::hear(&self.inner, scope) {
            None => Opened::Skip,
            Some(inner) => Opened::With(Box::new(Throttle {
                period: self.period,
                inner,
                last: Mutex::new(None),
            })),
        }
    }
    fn progress(&self, progress: Progress) {
        let mut last = self.last.lock().unwrap();
        if last.is_none_or(|t| t.elapsed() >= self.period) {
            *last = Some(Instant::now());
            self.inner.progress(progress);
        }
    }
    fn closed(&self, closed: Closed) {
        self.inner.closed(closed)
    }
}

// A runtime-sized list, e.g. observers a user registered.
struct Many<'a, S: Reports>(Vec<Box<dyn Observer<S> + 'a>>);

impl<S: Reports> Observer<S> for Many<'_, S> {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.iter().try_for_each(|o| o.check())
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        let heard: Vec<_> = self
            .0
            .iter()
            .filter_map(|o| inflight::hear(&**o, scope))
            .collect();
        match heard.is_empty() {
            true => Opened::Skip,
            false => Opened::With(Box::new(Many(heard))),
        }
    }
    fn progress(&self, progress: Progress) {
        self.0.iter().for_each(|o| o.progress(progress))
    }
    fn closed(&self, closed: Closed) {
        self.0.iter().for_each(|o| o.closed(closed))
    }
}

#[derive(Clone, Debug)]
struct Named;
impl Reports for Named {
    type Event = ();
}

struct Counting(std::sync::atomic::AtomicU64);
impl<S: Reports> Observer<S> for Counting {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ControlFlow::Continue(())
    }
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::Skip
    }
}

#[test]
fn workaround_asks_a_rule_once_per_safe_point() {
    let rule = Counting(Default::default());
    let t = Throttle {
        period: Duration::ZERO,
        inner: &rule,
        last: Mutex::new(None),
    };
    let _ = run(&t, |call: &inflight::Scope<'_, Named>| {
        call.within(Named, |a| a.within(Named, |b| b.check()))
    });
    assert_eq!(rule.0.into_inner(), 1, "one safe point");
}

#[test]
fn a_runtime_list_asks_each_rule_once_per_safe_point() {
    let (a, b) = (Counting(Default::default()), Counting(Default::default()));
    let many = Many(vec![
        Box::new(&a),
        Box::new(Throttle {
            period: Duration::ZERO,
            inner: &b,
            last: Mutex::new(None),
        }),
    ]);
    let _ = run(&many, |call: &inflight::Scope<'_, Named>| {
        call.within(Named, |x| x.within(Named, |y| y.check()))
    });
    drop(many);
    assert_eq!((a.0.into_inner(), b.0.into_inner()), (1, 1));
}
