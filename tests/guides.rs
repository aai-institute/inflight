use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Duration;

use inflight::{
    hear, run, try_run, Closed, Deadline, Flag, Observer, Opened, Outcome, Progress, Reports, Stop,
    Stopping, Within,
};
mod common;
use common::{Recorded, Recorder};

// ---- instrumenting: vocabulary

#[derive(Clone, Debug, PartialEq)]
pub enum SolveScope {
    Sweep { block: usize },
    Smoothing(SmoothScope),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SolveEvent {
    Step { residual: f64 },
    Smoothing(SmoothEvent),
}

impl Reports for SolveScope {
    type Event = SolveEvent;
}

// ---- instrumenting: errors

#[derive(Debug, PartialEq)]
pub enum SolveError {
    Singular { block: usize },
    Stopped(Stop),
}

impl From<Stop> for SolveError {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

impl Stopping for SolveError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Singular { .. } => None,
        }
    }
}

// ---- instrumenting: a third-party inner algorithm

#[derive(Clone, Debug, PartialEq)]
pub enum SmoothScope {
    Pass,
}
#[derive(Clone, Debug, PartialEq)]
pub enum SmoothEvent {
    Swept,
}
impl Reports for SmoothScope {
    type Event = SmoothEvent;
}

pub fn smooth(x: &mut [f64], observer: &dyn Observer<SmoothScope>) -> Outcome<()> {
    run(observer, |call| {
        call.within(SmoothScope::Pass, |pass| {
            for v in x.iter_mut() {
                *v *= 0.5;
            }
            pass.event(|| SmoothEvent::Swept);
            pass.check()
        })
    })
}

impl Within<SmoothScope> for SolveScope {
    fn scope(inner: &SmoothScope) -> Self {
        Self::Smoothing(inner.clone())
    }
    fn event(inner: &SmoothEvent) -> Option<SolveEvent> {
        Some(SolveEvent::Smoothing(inner.clone()))
    }
}

// ---- instrumenting: the entry point

pub fn solve(
    blocks: &mut [Vec<f64>],
    observer: &dyn Observer<SolveScope>,
) -> Result<Outcome<f64>, SolveError> {
    try_run(observer, |call| {
        let total = blocks.len() as u64;
        let mut worst = 0.0_f64;
        for (block, x) in blocks.iter_mut().enumerate() {
            if x.is_empty() {
                return Err(SolveError::Singular { block });
            }
            let residual = call.within(SolveScope::Sweep { block }, |sweep| {
                let mut residual = 1.0;
                for step in 0..10 {
                    residual /= 2.0;
                    sweep.progress(step + 1, Some(10));
                    sweep.event(|| SolveEvent::Step { residual });
                    sweep.check()?;
                }
                Ok::<_, SolveError>(residual)
            })?;
            call.nest(SolveScope::Smoothing(SmoothScope::Pass), |nest| {
                smooth(x, nest).into_result()
            })?;
            call.advance(1, Some(total));
            worst = worst.max(residual);
        }
        Ok(worst)
    })
}

// ---- instrumenting: parallel blocks

pub fn solve_parallel(blocks: &mut [Vec<f64>], observer: &dyn Observer<SolveScope>) -> Outcome<()> {
    let total = blocks.len() as u64;
    run(observer, |call| {
        std::thread::scope(|threads| {
            let handles: Vec<_> = blocks
                .iter_mut()
                .enumerate()
                .map(|(block, x)| {
                    threads.spawn(move || {
                        call.within(SolveScope::Sweep { block }, |sweep| {
                            x.iter_mut().for_each(|v| *v *= 0.5);
                            sweep.check()
                        })?;
                        call.advance(1, Some(total));
                        Ok(())
                    })
                })
                .collect();
            handles.into_iter().try_for_each(|h| h.join().unwrap())
        })
    })
}

// ---- instrumenting: residue

#[derive(Debug, PartialEq)]
pub struct Iterate {
    pub x: f64,
    pub k: u64,
}

pub fn iterate(
    mut state: Iterate,
    observer: &dyn Observer<SolveScope>,
) -> Outcome<Iterate, Iterate> {
    run(observer, |call| {
        while state.k < 100 {
            state.x /= 2.0;
            state.k += 1;
            let residual = state.x;
            call.event(|| SolveEvent::Step { residual });
            call.check()?;
        }
        Ok(())
    })
    .holding(state)
}

// ---- instrumenting: check-only library

pub fn count_to(n: u64, observer: &dyn Observer<std::convert::Infallible>) -> Outcome<u64> {
    run(observer, |call| {
        for i in 0..n {
            call.progress(i, Some(n));
            call.check()?;
        }
        Ok(n)
    })
}

// ---- observing: a rule

pub struct FillCap {
    budget: u64,
    seen: AtomicU64,
}

impl FillCap {
    pub const STOP: Stop = Stop::Rule("fill cap");
}

impl Observer<SolveScope> for FillCap {
    fn check(&self) -> ControlFlow<Stop> {
        match self.seen.load(Relaxed) > self.budget {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    fn event(&self, event: &SolveEvent) {
        if let SolveEvent::Step { .. } = event {
            self.seen.fetch_add(1, Relaxed);
        }
    }
}

// ---- observing: a rule per scope

pub struct Stagnation {
    patience: u32,
    best: Mutex<(f64, u32)>,
}

impl Stagnation {
    pub const STOP: Stop = Stop::Rule("stagnation");

    pub fn new(patience: u32) -> Self {
        Self {
            patience,
            best: Mutex::new((f64::INFINITY, 0)),
        }
    }
}

impl Observer<SolveScope> for Stagnation {
    fn check(&self) -> ControlFlow<Stop> {
        match self.best.lock().unwrap().1 >= self.patience {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    // Each scope converges on its own, so each gets a fresh count.
    fn open(&self, _scope: &SolveScope) -> Opened<'_, SolveScope> {
        Opened::With(Box::new(Stagnation::new(self.patience)))
    }
    fn event(&self, event: &SolveEvent) {
        if let SolveEvent::Step { residual } = *event {
            let mut best = self.best.lock().unwrap();
            *best = match residual < best.0 {
                true => (residual, 0),
                false => (best.0, best.1 + 1),
            };
        }
    }
}

// ---- observing: a listener

#[derive(Default)]
pub struct Closes(AtomicU64);

impl<S: Reports> Observer<S> for Closes {
    fn closed(&self, _closed: Closed) {
        self.0.fetch_add(1, Relaxed);
    }
}

// ---- observing: state per scope

pub struct SweepTimer {
    slow: AtomicU64,
}

struct OneSweep<'t> {
    timer: &'t SweepTimer,
}

impl Observer<SolveScope> for SweepTimer {
    fn open(&self, scope: &SolveScope) -> Opened<'_, SolveScope> {
        match scope {
            SolveScope::Sweep { .. } => Opened::With(Box::new(OneSweep { timer: self })),
            SolveScope::Smoothing(_) => Opened::Skip,
        }
    }
}

impl Observer<SolveScope> for OneSweep<'_> {
    fn closed(&self, closed: Closed) {
        if closed.elapsed > Duration::from_millis(100) {
            self.timer.slow.fetch_add(1, Relaxed);
        }
    }
}

// ---- observing: a wrapper of the owner's own

pub struct Quiet<O>(pub O);

impl<S: Reports, O: Observer<S>> Observer<S> for Quiet<O> {
    fn check(&self) -> ControlFlow<Stop> {
        self.0.check()
    }
    fn open(&self, scope: &S) -> Opened<'_, S> {
        match hear(&self.0, scope) {
            Some(child) => Opened::With(Box::new(Quiet(child))),
            None => Opened::Skip,
        }
    }
    fn closed(&self, closed: Closed) {
        self.0.closed(closed)
    }
}

// ---- tests that run the examples

#[test]
fn solve_under_a_flag_and_a_deadline() {
    let cancel = Flag::new();
    let mut blocks = vec![vec![1.0; 4]; 3];
    let outcome = solve(
        &mut blocks,
        &(&cancel, Deadline::after(Duration::from_secs(30))),
    );
    assert!(matches!(outcome, Ok(Outcome::Completed(_))));
}

#[test]
fn a_rule_stop_is_named() {
    let cap = FillCap {
        budget: 3,
        seen: AtomicU64::new(0),
    };
    let mut blocks = vec![vec![1.0; 4]; 3];
    match solve(&mut blocks, &cap) {
        Ok(Outcome::Stopped {
            reason: FillCap::STOP,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_own_error_wins() {
    let mut blocks = vec![vec![1.0; 4], vec![]];
    assert_eq!(
        solve(&mut blocks, &()),
        Err(SolveError::Singular { block: 1 })
    );
}

#[test]
fn rules_and_listeners_compose() {
    let closes = Closes::default();
    let rules = (
        FillCap {
            budget: 1000,
            seen: AtomicU64::new(0),
        },
        Stagnation::new(5),
    );
    let mut blocks = vec![vec![1.0; 4]; 2];
    let outcome = solve(&mut blocks, &(&rules, &closes)).unwrap();
    assert!(matches!(outcome, Outcome::Completed(_)));
    assert!(closes.0.load(Relaxed) > 0);
}

#[test]
fn an_optional_observer() {
    let bar: Option<Closes> = None;
    let shown: &dyn Observer<SolveScope> = bar.as_ref().map_or(&(), |b| b);
    let mut blocks = vec![vec![1.0; 4]; 2];
    assert!(solve(&mut blocks, &(Flag::new(), shown)).is_ok());
}

#[test]
fn state_per_scope_and_a_wrapper() {
    let timer = SweepTimer {
        slow: AtomicU64::new(0),
    };
    let mut blocks = vec![vec![1.0; 4]; 2];
    assert!(solve(&mut blocks, &Quiet(&timer)).is_ok());
    assert_eq!(timer.slow.load(Relaxed), 0);
}

#[test]
fn parallel_blocks_report_ordered_progress() {
    let (forward, outbox) = Recorder::<SolveScope>::new();
    let mut blocks = vec![vec![1.0; 4]; 8];
    assert_eq!(
        solve_parallel(&mut blocks, &forward),
        Outcome::Completed(())
    );
    let record: Recorded<SolveScope> = outbox.record();
    assert_eq!(
        record.progress,
        Some(Progress {
            done: 8,
            total: Some(8)
        })
    );
    assert_eq!(record.children.len(), 8);
}

#[test]
fn a_stopped_iteration_resumes_from_its_residue() {
    let cap = FillCap {
        budget: 9,
        seen: AtomicU64::new(0),
    };
    let Outcome::Stopped { residue, .. } = iterate(Iterate { x: 1.0, k: 0 }, &cap) else {
        panic!("the cap stops it");
    };
    assert_eq!(residue.k, 10);
    assert!(matches!(
        iterate(residue, &()),
        Outcome::Completed(Iterate { k: 100, .. })
    ));
}

#[test]
fn a_check_only_library_takes_any_control() {
    assert_eq!(count_to(5, &Flag::new()), Outcome::Completed(5));
}
