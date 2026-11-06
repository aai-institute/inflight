use std::thread;

use inflight::{run, try_run, Flag, Observer, Outcome, Reports, Stop, Within};
mod common;
use common::{Recorded, Recorder};

// --- (1) a resumable solver: the iterate must come back on a stop ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SolveScope {
    Solve,
}
impl Reports for SolveScope {
    type Event = u64;
}

fn step(x: &mut [f64]) {
    for v in x.iter_mut() {
        *v = 0.5 * (*v + 2.0 / *v);
    }
}

// Today: the state lives outside the closure and the outcome is rebuilt by hand.
fn solve_today(
    mut x: Vec<f64>,
    iterations: u64,
    observer: &dyn Observer<SolveScope>,
) -> Outcome<Vec<f64>, Vec<f64>> {
    let outcome = run(observer, |call| {
        call.within(SolveScope::Solve, |solve| {
            for k in 0..iterations {
                step(&mut x);
                solve.event(|| k);
                solve.check()?;
            }
            Ok(())
        })
    });
    match outcome {
        Outcome::Completed(()) => Outcome::Completed(x),
        Outcome::Stopped { reason, .. } => Outcome::Stopped { reason, residue: x },
    }
}

// With `holding`: the same solver, no hand-rebuilt outcome.
fn solve(
    mut x: Vec<f64>,
    iterations: u64,
    observer: &dyn Observer<SolveScope>,
) -> Outcome<Vec<f64>, Vec<f64>> {
    run(observer, |call| {
        call.within(SolveScope::Solve, |solve| {
            for k in 0..iterations {
                step(&mut x);
                solve.event(|| k);
                solve.check()?;
            }
            Ok(())
        })
    })
    .holding(x)
}

#[test]
fn a_solver_holding_its_state_resumes_from_its_residue() {
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    let Outcome::Stopped { residue, .. } = solve(vec![1.0; 3], 50, &flag) else {
        panic!("raised")
    };
    assert_eq!(
        Outcome::Stopped {
            reason: Stop::Requested,
            residue: residue.clone()
        },
        solve_today(vec![1.0; 3], 50, &flag)
    );
    let done = solve(residue, 50, &());
    assert!((done.completed().unwrap()[0] - 2f64.sqrt()).abs() < 1e-12);
}

#[test]
fn a_resumable_solver_resumes_from_its_residue() {
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    let stopped = solve_today(vec![1.0; 3], 50, &flag);
    let Outcome::Stopped { residue, .. } = stopped else {
        panic!("raised")
    };
    assert_ne!(residue, vec![1.0; 3], "one step ran before the check");
    let done = solve_today(residue, 50, &());
    assert!((done.completed().unwrap()[0] - 2f64.sqrt()).abs() < 1e-12);
}

// --- (2) parallel blocks under std::thread::scope ---

#[test]
fn blocks_open_scopes_from_several_threads() {
    let (recorder, outbox) = Recorder::<SolveScope>::new();
    let outcome = run(&recorder, |call| {
        thread::scope(|threads| {
            let handles: Vec<_> = (0..4u64)
                .map(|k| {
                    threads.spawn(move || {
                        call.within(SolveScope::Solve, |block| {
                            block.event(|| k);
                            block.check()
                        })
                    })
                })
                .collect();
            handles.into_iter().try_for_each(|h| h.join().unwrap())
        })
    });
    assert_eq!(outcome, Outcome::Completed(()));
    let record: Recorded<SolveScope> = outbox.record();
    assert_eq!(record.children.len(), 4);
}

// --- (3) an outer algorithm calls an inner one with a fallible error ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outer {
    Phase,
    Inner,
}
impl Reports for Outer {
    type Event = OuterEvent;
}
#[derive(Debug)]
struct OuterEvent;
impl Within<SolveScope> for Outer {
    fn scope(_: &SolveScope) -> Self {
        Outer::Inner
    }
    fn event(_: &u64) -> Option<OuterEvent> {
        Some(OuterEvent)
    }
}

#[derive(Debug, PartialEq)]
enum InnerError {
    Diverged,
    Stopped(Stop),
}
impl From<Stop> for InnerError {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

fn inner(n: u64, observer: &dyn Observer<SolveScope>) -> Result<Outcome<u64>, InnerError> {
    try_run(observer, |call| {
        call.within(SolveScope::Solve, |solve| {
            for k in 0..n {
                solve.event(|| k);
                solve.check()?;
            }
            match n {
                0 => Err(InnerError::Diverged),
                _ => Ok(n),
            }
        })
    })
}

#[derive(Debug, PartialEq)]
enum OuterError {
    Inner(InnerError),
    Stopped(Stop),
}
impl From<Stop> for OuterError {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

// Today: an inner outcome is matched by hand to carry its stop out.
fn outer_today(observer: &dyn Observer<Outer>) -> Result<Outcome<u64>, OuterError> {
    try_run(observer, |call| {
        let n = call
            .nest(Outer::Phase, |n| inner(3, n))
            .map_err(OuterError::Inner)?
            .into_result()?;
        call.check()?;
        Ok(n)
    })
}

#[test]
fn an_inner_stop_stops_the_outer_call() {
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    let outcome = outer_today(&flag).unwrap();
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Requested,
            ..
        }
    ));
    assert_eq!(outer_today(&()).unwrap(), Outcome::Completed(3));
}

impl inflight::Stopping for InnerError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Diverged => None,
        }
    }
}

impl inflight::Stopping for OuterError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Inner(error) => error.stop(),
        }
    }
}
