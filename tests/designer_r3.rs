use std::ops::ControlFlow;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use inflight::{
    run, try_run, Closed, Deadline, Flag, Observer, Opened, Outcome, Progress, Reports, Scope,
    Stop, Within,
};
mod common;
use common::{Message, Recorder, Standard};

// ---------- algorithm 1: a resumable, fallible optimizer ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opt {
    Descend,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OptEvent {
    Step { residual: f64 },
}

impl Reports for Opt {
    type Event = OptEvent;
}

impl Standard for OptEvent {
    fn is_iteration(&self) -> bool {
        true
    }
    fn residual(&self) -> Option<f64> {
        let OptEvent::Step { residual } = self;
        Some(*residual)
    }
}

#[derive(Debug, PartialEq)]
pub enum OptError {
    Diverged { at: u64 },
    Stopped(Stop),
}

impl From<Stop> for OptError {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

impl inflight::Stopping for OptError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Diverged { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Iterate {
    pub x: f64,
    pub k: u64,
}

// Minimises (x - 3)^2 + floor; the floor makes the residual stagnate.
pub fn optimize(
    observer: &dyn Observer<Opt>,
    mut state: Iterate,
    iterations: u64,
    floor: f64,
) -> Result<Outcome<Iterate, Iterate>, OptError> {
    let outcome = try_run(observer, |call| {
        call.within(Opt::Descend, |s| {
            while state.k < iterations {
                state.x -= 0.25 * (state.x - 3.0);
                state.k += 1;
                let residual = (state.x - 3.0).abs().max(floor);
                if !residual.is_finite() {
                    return Err(OptError::Diverged { at: state.k });
                }
                s.event(|| OptEvent::Step { residual });
                s.progress(state.k, Some(iterations));
                s.check()?;
            }
            Ok(())
        })
    })?;
    Ok(outcome.holding(state))
}

// ---------- third-party inner algorithm, foreign event type ----------

pub mod smoother {
    use inflight::{run, Observer, Outcome, Reports};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Phase {
        Sweep,
    }
    impl Reports for Phase {
        type Event = u64;
    }

    pub fn smooth(observer: &dyn Observer<Phase>, block: &mut [f64], sweeps: u64) -> Outcome<()> {
        run(observer, |call| {
            call.within(Phase::Sweep, |s| {
                for k in 0..sweeps {
                    for i in 1..block.len().saturating_sub(1) {
                        block[i] = 0.5 * block[i] + 0.25 * (block[i - 1] + block[i + 1]);
                    }
                    s.event(|| k);
                    s.check()?;
                }
                Ok(())
            })
        })
    }
}

// ---------- algorithm 2: a parallel pipeline nesting the smoother ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipe {
    Blocks,
    Block(usize),
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PipeEvent {
    BlockDone { block: usize, residual: f64 },
}

impl Reports for Pipe {
    type Event = PipeEvent;
}

impl Standard for PipeEvent {
    fn is_iteration(&self) -> bool {
        true
    }
    fn residual(&self) -> Option<f64> {
        let PipeEvent::BlockDone { residual, .. } = self;
        Some(*residual)
    }
}

impl Within<smoother::Phase> for Pipe {
    fn scope(_: &smoother::Phase) -> Self {
        Pipe::Smooth
    }
    fn event(_: &u64) -> Option<PipeEvent> {
        None
    }
}

pub fn pipeline(
    observer: &dyn Observer<Pipe>,
    blocks: &mut [Vec<f64>],
    sweeps: u64,
) -> Outcome<()> {
    run(observer, |call| {
        call.within(Pipe::Blocks, |all| {
            let total = blocks.len() as u64;
            thread::scope(|t| {
                let handles: Vec<_> = blocks
                    .iter_mut()
                    .enumerate()
                    .map(|(i, block)| {
                        t.spawn(move || {
                            all.within(Pipe::Block(i), |b| {
                                b.nest(Pipe::Smooth, |n| {
                                    smoother::smooth(n, block, sweeps).into_result()
                                })?;
                                let residual = block.iter().map(|v| v.abs()).sum::<f64>();
                                b.event(|| PipeEvent::BlockDone { block: i, residual });
                                all.advance(1, Some(total));
                                b.check()
                            })
                        })
                    })
                    .collect();
                handles.into_iter().try_for_each(|h| h.join().unwrap())
            })
        })
    })
}

// ---------- owner: a generic stagnation rule ----------

pub struct Stagnation {
    patience: u64,
    tolerance: f64,
    best: Mutex<(f64, u64)>,
}

impl Stagnation {
    pub const STOP: Stop = Stop::Rule("stagnation");
    pub fn new(patience: u64, tolerance: f64) -> Self {
        Self {
            patience,
            tolerance,
            best: Mutex::new((f64::INFINITY, 0)),
        }
    }
}

impl<S: Reports> Observer<S> for Stagnation
where
    S::Event: Standard,
{
    fn check(&self) -> ControlFlow<Stop> {
        match self.best.lock().unwrap().1 >= self.patience {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    fn event(&self, event: &S::Event) {
        let Some(r) = event.residual() else { return };
        let mut best = self.best.lock().unwrap();
        if r < best.0 - self.tolerance {
            *best = (r, 0);
        } else {
            best.1 += 1;
        }
    }
}

// ---------- owner: a progress bar over any algorithm ----------

#[derive(Default)]
pub struct Bar {
    depth: usize,
    lines: Mutex<Vec<String>>,
}

impl<S: Reports + std::fmt::Debug> Observer<S> for Bar {
    fn open(&self, scope: &S) -> Opened<'_, S> {
        self.lines
            .lock()
            .unwrap()
            .push(format!("{:>w$}{scope:?}", "", w = self.depth * 2));
        Opened::Same
    }
    fn progress(&self, p: Progress) {
        self.lines
            .lock()
            .unwrap()
            .push(format!("{}/{:?}", p.done, p.total));
    }
    fn closed(&self, c: Closed) {
        self.lines
            .lock()
            .unwrap()
            .push(format!("closed {:?}", c.ended));
    }
}

// ---------- the library owner's stable API ----------

pub enum Why {
    Done,
    Stagnated,
    Interrupted(Stop),
}

pub fn owner_optimize(
    flag: &Flag,
    budget: Duration,
    start: Iterate,
) -> Result<(Why, Iterate), OptError> {
    let rule = Stagnation::new(5, 1e-9);
    let observer = (flag, (Deadline::after(budget), &rule));
    Ok(classify(optimize(&observer, start, 10_000, 1e-3)?))
}

pub fn classify(outcome: Outcome<Iterate, Iterate>) -> (Why, Iterate) {
    match outcome {
        Outcome::Completed(x) => (Why::Done, x),
        Outcome::Stopped {
            reason: Stagnation::STOP,
            residue,
        } => (Why::Stagnated, residue),
        Outcome::Stopped { reason, residue } => (Why::Interrupted(reason), residue),
    }
}

#[test]
fn optimizer_resumes_after_deadline() {
    let past = Deadline::after(Duration::ZERO);
    let out = optimize(&past, Iterate { x: 0.0, k: 0 }, 50, 0.0).unwrap();
    let Outcome::Stopped {
        reason: Stop::Deadline,
        residue,
    } = out
    else {
        panic!("{out:?}")
    };
    assert_eq!(residue.k, 1);
    let out = optimize(&(), residue, 50, 0.0).unwrap();
    assert_eq!(out.completed().map(|i| i.k), Some(50));
}

#[test]
fn optimizer_stagnates_and_owner_tells_why() {
    let (why, it) = owner_optimize(
        &Flag::new(),
        Duration::from_secs(60),
        Iterate { x: 0.0, k: 0 },
    )
    .unwrap();
    assert!(matches!(why, Why::Stagnated));
    assert!(it.k < 10_000);
}

#[test]
fn pipeline_stagnation_and_bar() {
    let mut blocks = vec![vec![1.0; 64]; 8];
    let bar = Bar::default();
    assert_eq!(pipeline(&bar, &mut blocks, 20), Outcome::Completed(()));
    let lines = bar.lines.into_inner().unwrap();
    assert!(lines.iter().any(|l| l.contains("Smooth")), "{lines:?}");

    let rule = Stagnation::new(2, 1e9);
    let out = pipeline(&rule, &mut blocks, 20);
    assert!(matches!(
        out,
        Outcome::Stopped {
            reason: Stagnation::STOP,
            ..
        }
    ));
}

#[test]
fn forward_progress_over_both() {
    let (fwd, outbox) = Recorder::<Opt>::new();
    optimize(&fwd, Iterate { x: 0.0, k: 0 }, 10, 0.0).unwrap();
    let (fwd2, outbox2) = Recorder::<Pipe>::new();
    pipeline(&fwd2, &mut vec![vec![1.0; 8]; 3], 4);
    let mut n = 0;
    outbox
        .drain(|m| {
            if let Message::Progress { .. } = m {
                n += 1
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    outbox2
        .drain(|m| {
            if let Message::Progress { .. } = m {
                n += 1
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    assert!(n >= 2);
}

#[allow(dead_code)]
fn scope_is_sync<'a>(s: &'a Scope<'a, Pipe>) -> &'a (dyn Sync + 'a) {
    s
}

#[test]
fn a_rule_stop_is_told_apart_from_a_later_cancel() {
    let flag = Flag::new();
    let rule = Stagnation::new(5, 1e-9);
    let out = optimize(&(&flag, &rule), Iterate { x: 0.0, k: 0 }, 10_000, 1e-3).unwrap();
    flag.raise(Stop::Requested); // the user hits cancel just as the rule fired
    let (why, _) = classify(out);
    assert!(matches!(why, Why::Stagnated));
}

#[test]
fn parallel_progress_ends_at_its_total() {
    let mut stale = 0;
    for _ in 0..2000 {
        let (forward, outbox) = Recorder::<Pipe>::new();
        pipeline(&forward, &mut vec![vec![1.0; 4]; 8], 1);
        let r = outbox.record();
        let p = r.children[0].1.progress.unwrap();
        if p.done != 8 {
            stale += 1
        }
    }
    assert_eq!(stale, 0);
}
