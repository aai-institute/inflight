use std::ops::ControlFlow;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use inflight::{
    run, Closed, IterationLimit, Observer, Opened, Outcome, Progress, Reports, Standard, Stop,
};

// A solver's own vocabulary: each step reports its residual.
#[derive(Clone, Debug)]
struct Solve;
#[derive(Debug)]
enum SolveEvent {
    Step { residual: f64 },
}
impl Reports for Solve {
    type Event = SolveEvent;
}
impl Standard for SolveEvent {
    fn is_iteration(&self) -> bool {
        matches!(self, SolveEvent::Step { .. })
    }
    fn residual(&self) -> Option<f64> {
        match self {
            SolveEvent::Step { residual } => Some(*residual),
        }
    }
}

// Generic ETA bar: needs only Progress and time; no Standard.
struct Eta(Mutex<Option<(Instant, String)>>);
impl<S: Reports> Observer<S> for Eta {
    fn progress(&self, p: Progress) {
        let mut g = self.0.lock().unwrap();
        let start = g.get_or_insert((Instant::now(), String::new())).0;
        if let Some(total) = p.total.filter(|_| p.done > 0) {
            let left = start
                .elapsed()
                .mul_f64((total - p.done) as f64 / p.done as f64);
            g.as_mut().unwrap().1 = format!("{}/{} eta {:?}", p.done, total, left);
        }
    }
}

// Generic max wall time per iteration, per scope: Iteration alone suffices.
struct PerIteration {
    budget: Duration,
    last: Mutex<Option<Instant>>,
}
impl<S: Reports> Observer<S> for PerIteration
where
    S::Event: Standard,
{
    fn check(&self) -> ControlFlow<Stop> {
        match *self.last.lock().unwrap() {
            Some(t) if t.elapsed() > self.budget => ControlFlow::Break(Stop::Deadline),
            _ => ControlFlow::Continue(()),
        }
    }
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::With(Box::new(PerIteration {
            budget: self.budget,
            last: Mutex::new(Some(Instant::now())),
        }))
    }
    fn event(&self, e: &S::Event) {
        if e.is_iteration() {
            *self.last.lock().unwrap() = Some(Instant::now());
        }
    }
    fn closed(&self, _: Closed) {}
}

#[test]
fn eta_and_per_iteration_need_no_payload() {
    let eta = Eta(Mutex::new(None));
    let per = PerIteration {
        budget: Duration::from_millis(5),
        last: Mutex::new(None),
    };
    let out = run(&(&eta, &per), |call| {
        call.within(Solve, |s| {
            for k in 0..10u64 {
                if k == 3 {
                    std::thread::sleep(Duration::from_millis(10));
                }
                s.check()?;
                s.event(|| SolveEvent::Step { residual: 1.0 });
                s.progress(k + 1, Some(10));
            }
            Ok(())
        })
    });
    assert_eq!(
        out,
        Outcome::Stopped {
            reason: Stop::Deadline,
            residue: ()
        }
    );
    assert!(eta
        .0
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .1
        .starts_with("3/10"));
}

// Generic stagnation: stop once `patience` residuals in a row did not beat the best by `factor`.
struct Stagnation {
    patience: u32,
    factor: f64,
    state: Mutex<(f64, u32)>,
}
impl<S: Reports> Observer<S> for Stagnation
where
    S::Event: Standard,
{
    fn check(&self) -> ControlFlow<Stop> {
        match self.state.lock().unwrap().1 >= self.patience {
            true => ControlFlow::Break(Stop::Requested),
            false => ControlFlow::Continue(()),
        }
    }
    fn open(&self, _: &S) -> Opened<'_, S> {
        Opened::With(Box::new(Stagnation {
            patience: self.patience,
            factor: self.factor,
            state: Mutex::new((f64::INFINITY, 0)),
        }))
    }
    fn event(&self, e: &S::Event) {
        if let Some(r) = e.residual() {
            let mut s = self.state.lock().unwrap();
            *s = match r < s.0 * self.factor {
                true => (r, 0),
                false => (s.0, s.1 + 1),
            };
        }
    }
}

#[test]
fn one_step_is_both_an_iteration_and_a_residual() {
    let stall = Stagnation {
        patience: 3,
        factor: 0.9,
        state: Mutex::new((f64::INFINITY, 0)),
    };
    let limit = IterationLimit::new(100);
    let mut steps = 0;
    let out = run(&(&limit, &stall), |call| {
        call.within(Solve, |s| -> Result<(), Stop> {
            loop {
                s.check()?;
                steps += 1;
                let residual = if steps < 5 { 1.0 / steps as f64 } else { 0.2 };
                s.event(|| SolveEvent::Step { residual });
            }
        })
    });
    assert_eq!(
        out,
        Outcome::Stopped {
            reason: Stop::Requested,
            residue: ()
        }
    );
    assert_eq!(steps, 8);
    let (limit, mut steps) = (IterationLimit::new(4), 0);
    let _ = run(&limit, |call| {
        call.within(Solve, |s| -> Result<(), Stop> {
            loop {
                s.check()?;
                steps += 1;
                s.event(|| SolveEvent::Step { residual: 1.0 });
            }
        })
    });
    assert_eq!(steps, 4);
}
