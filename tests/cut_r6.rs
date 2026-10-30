use std::ops::ControlFlow;
use std::sync::Mutex;

use inflight::{run, Closed, Flag, Observer, Opened, Reports, Scope, Stop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Out(&'static str);
impl Reports for Out {
    type Event = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct In(&'static str);
impl Reports for In {
    type Event = ();
}

impl inflight::Within<In> for Out {
    fn scope(inner: &In) -> Self {
        Out(inner.0)
    }
    fn event(_: &()) -> Option<()> {
        Some(())
    }
}

#[derive(Default)]
struct Log(Mutex<Vec<String>>);
struct Named<'l>(&'l Log, &'static str);

impl Observer<Out> for Log {
    fn open(&self, scope: &Out) -> Opened<'_, Out> {
        self.0.lock().unwrap().push(format!("open {}", scope.0));
        Opened::With(Box::new(Named(self, scope.0)))
    }
}
impl Observer<Out> for Named<'_> {
    fn open(&self, scope: &Out) -> Opened<'_, Out> {
        self.0.open(scope)
    }
    fn closed(&self, closed: Closed) {
        let line = format!("close {} {:?}", self.1, closed.ended);
        self.0 .0.lock().unwrap().push(line);
    }
}

fn inner(observer: &dyn Observer<In>, flag: &Flag) -> inflight::Outcome<()> {
    run(&(observer, flag), |call| {
        call.within(In("work"), |w| w.check())
    })
}

// approx-chol's `factorize_complement` shape: the nest is let-bound and outlives the inner call.
#[test]
fn a_let_bound_nest_closes_before_its_sibling_opens() {
    let log = Log::default();
    run(&log, |call: &Scope<'_, Out>| {
        let _ = call.nest(Out("reducing"), |reducing| {
            inner(reducing, &Flag::new()).into_result()
        });
        call.within(Out("factoring"), |_| Ok::<_, Stop>(()))
    });
    let log = log.0.into_inner().unwrap();
    let close = log
        .iter()
        .position(|l| l.starts_with("close reducing"))
        .unwrap();
    let open = log.iter().position(|l| l == "open factoring").unwrap();
    assert!(close < open, "{log:#?}");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "a Nest hears one inner call")]
fn a_reused_nest_reports_how_the_last_inner_call_ended() {
    let log = Log::default();
    let stop = Flag::new();
    stop.raise(Stop::Requested);
    run(&log, |call: &Scope<'_, Out>| {
        call.nest(Out("phase"), |nest| {
            let _ = inner(nest, &Flag::new());
            let second = inner(nest, &stop);
            assert!(matches!(second, inflight::Outcome::Stopped { .. }));
            Ok::<_, Stop>(())
        })?;
        Ok(())
    });
    let log = log.0.into_inner().unwrap();
    assert!(
        log.contains(&"close phase Stopped(Requested)".to_string()),
        "{log:#?}"
    );
}

#[derive(Default)]
struct CountsChecks(Mutex<u32>);
impl Observer<Out> for CountsChecks {
    fn check(&self) -> ControlFlow<Stop> {
        *self.0.lock().unwrap() += 1;
        ControlFlow::Continue(())
    }
}

#[test]
fn parallel_inner_safe_points_ask_the_outer_check_once_each() {
    let checks = CountsChecks::default();
    run(&checks, |call: &Scope<'_, Out>| {
        let _ = call.nest(Out("phase"), |nest| {
            run(nest, |inner: &Scope<'_, In>| {
                std::thread::scope(|t| {
                    for _ in 0..4 {
                        t.spawn(|| inner.within(In("block"), |b| b.check()));
                    }
                });
                Ok(())
            })
            .into_result()
        });
        Ok(())
    });
    assert_eq!(*checks.0.lock().unwrap(), 4);
}
