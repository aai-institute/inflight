use std::ops::ControlFlow;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use inflight::{
    run, try_run, Deadline, Ended, Forward, Message, Observer, Outcome, Progress, Reports, Scope,
    Standard, Stop, Stopping, Within,
};

// ---------- a third-party instrumented library ----------
mod lib3 {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub struct Sweep;
    #[derive(Clone, Debug, PartialEq)]
    pub struct Residual(pub f64);
    impl Reports for Sweep {
        type Event = Residual;
    }
    impl Standard for Residual {
        fn is_iteration(&self) -> bool {
            true
        }
        fn residual(&self) -> Option<f64> {
            Some(self.0)
        }
    }

    #[derive(Debug, PartialEq)]
    pub enum Error {
        Diverged,
        Stopped(Stop),
    }
    impl From<Stop> for Error {
        fn from(s: Stop) -> Self {
            Error::Stopped(s)
        }
    }
    impl Stopping for Error {
        fn stop(&self) -> Option<Stop> {
            match self {
                Error::Stopped(s) => Some(*s),
                Error::Diverged => None,
            }
        }
    }

    pub fn solve(
        observer: &dyn Observer<Sweep>,
        x: &mut f64,
        sweeps: u64,
    ) -> Result<Outcome<()>, Error> {
        try_run(observer, |s| {
            for k in 0..sweeps {
                s.check()?;
                *x *= 0.5;
                if !x.is_finite() {
                    return Err(Error::Diverged);
                }
                s.event(|| Residual(x.abs()));
                s.progress(k + 1, Some(sweeps));
            }
            Ok(())
        })
    }
}

// ---------- 1. designer: setup -> iterate (seq or parallel) -> polish, lib called twice ----------
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Setup,
    Iterate,
    Block(usize),
    Polish,
    Lib(&'static str),
    Sweep,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    Residual(f64),
}
impl Reports for Phase {
    type Event = Ev;
}
impl Standard for Ev {
    fn is_iteration(&self) -> bool {
        true
    }
    fn residual(&self) -> Option<f64> {
        let Ev::Residual(r) = self;
        Some(*r)
    }
}
impl Within<lib3::Sweep> for Phase {
    fn scope(_: &lib3::Sweep) -> Self {
        Phase::Sweep
    }
    fn event(e: &lib3::Residual) -> Option<Ev> {
        Some(Ev::Residual(e.0))
    }
}

#[derive(Debug)]
pub enum PipeError {
    Lib(lib3::Error),
    Stopped(Stop),
}
impl From<Stop> for PipeError {
    fn from(s: Stop) -> Self {
        PipeError::Stopped(s)
    }
}
impl From<lib3::Error> for PipeError {
    fn from(e: lib3::Error) -> Self {
        PipeError::Lib(e)
    }
}
impl Stopping for PipeError {
    fn stop(&self) -> Option<Stop> {
        match self {
            PipeError::Stopped(s) => Some(*s),
            PipeError::Lib(e) => e.stop(),
        }
    }
}

fn iterate_block(s: &Scope<'_, Phase>, block: &mut [f64]) -> Result<(), Stop> {
    for v in block.iter_mut() {
        s.check()?;
        *v *= 0.9;
        s.event(|| Ev::Residual(v.abs()));
    }
    Ok(())
}

pub fn pipeline(
    observer: &dyn Observer<Phase>,
    data: &mut [f64],
) -> Result<Outcome<()>, PipeError> {
    try_run(observer, |s| {
        let mut x = s.within(Phase::Setup, |_| {
            Ok::<_, PipeError>(data.iter().sum::<f64>())
        })?;
        // first library call
        s.nest(Phase::Lib("pre"), |n| lib3::solve(n, &mut x, 3))?
            .into_result()?;
        let parallel = data.len() > 4;
        s.within(Phase::Iterate, |it| {
            let n = data.len() as u64;
            if parallel {
                thread::scope(|t| {
                    let hs: Vec<_> = data
                        .chunks_mut(2)
                        .enumerate()
                        .map(|(i, c)| {
                            t.spawn(move || {
                                let r = it.within(Phase::Block(i), |b| iterate_block(b, c));
                                it.advance(c.len() as u64, Some(n));
                                r
                            })
                        })
                        .collect();
                    hs.into_iter().try_for_each(|h| h.join().unwrap())
                })
            } else {
                for (i, c) in data.chunks_mut(2).enumerate() {
                    it.within(Phase::Block(i), |b| iterate_block(b, c))?;
                    it.advance(c.len() as u64, Some(n));
                }
                Ok(())
            }
        })?;
        // second library call
        s.within(Phase::Polish, |p| {
            p.nest(Phase::Lib("post"), |n| lib3::solve(n, &mut x, 2))?
                .into_result()?;
            Ok::<_, PipeError>(())
        })
    })
}

#[test]
fn pipeline_seq_and_parallel_record_the_same_shape() {
    for len in [4, 8] {
        let (fwd, outbox) = Forward::<Phase>::new();
        let mut data = vec![1.0; len];
        let out = pipeline(&fwd, &mut data).unwrap();
        assert_eq!(out, Outcome::Completed(()));
        let rec = outbox.record();
        let labels: Vec<_> = rec.children.iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(
            labels,
            [
                Phase::Setup,
                Phase::Lib("pre"),
                Phase::Iterate,
                Phase::Polish
            ]
        );
        assert_eq!(
            rec.children[1].1.progress,
            Some(Progress {
                done: 3,
                total: Some(3)
            })
        );
        assert_eq!(
            rec.children[2].1.progress,
            Some(Progress {
                done: len as u64,
                total: Some(len as u64)
            })
        );
    }
}

// ---------- 2. owner: python-ish bridge without pyo3 ----------
pub struct ResidualTarget(pub f64, std::sync::atomic::AtomicU64);
impl ResidualTarget {
    pub const STOP: Stop = Stop::Rule("residual target");
    pub fn new(t: f64) -> Self {
        Self(
            t,
            std::sync::atomic::AtomicU64::new(f64::INFINITY.to_bits()),
        )
    }
}
impl<S: Reports> Observer<S> for ResidualTarget
where
    S::Event: Standard,
{
    fn check(&self) -> ControlFlow<Stop> {
        match f64::from_bits(self.1.load(std::sync::atomic::Ordering::Relaxed)) <= self.0 {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    fn event(&self, e: &S::Event) {
        if let Some(r) = e.residual() {
            self.1
                .store(r.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum PyMsg {
    Started(String),
    Progress(String, u64),
    Finished(String, bool),
}

pub fn bridge(budget: Duration, target: f64, data: Vec<f64>) -> (Vec<PyMsg>, String) {
    let (fwd, outbox) = Forward::<Phase>::new();
    let (tx, rx) = mpsc::channel::<PyMsg>();
    let rule = (Deadline::after(budget), ResidualTarget::new(target));
    let drainer = thread::spawn(move || {
        let mut names = std::collections::HashMap::from([(0u64, "call".to_string())]);
        let _ = outbox.follow(|m| {
            let msg = match m {
                Message::Opened { id, scope, .. } => {
                    names.insert(id, format!("{scope:?}"));
                    PyMsg::Started(format!("{scope:?}"))
                }
                Message::Progress { id, progress } => {
                    PyMsg::Progress(names[&id].clone(), progress.done)
                }
                Message::Event { .. } => return Ok(()),
                Message::Closed { id, closed } => {
                    PyMsg::Finished(names[&id].clone(), closed.ended == Ended::Completed)
                }
            };
            tx.send(msg).map_err(|_| ())
        });
    });
    let mut data = data;
    let out = pipeline(&(&rule, &fwd), &mut data);
    drop(fwd);
    drainer.join().unwrap();
    let why = match out {
        Ok(Outcome::Completed(())) => "ok".to_string(),
        Ok(Outcome::Stopped { reason, .. }) if reason == ResidualTarget::STOP => "converged".into(),
        Ok(Outcome::Stopped { reason, .. }) => format!("{reason:?}"),
        Err(e) => format!("{e:?}"),
    };
    (rx.try_iter().collect(), why)
}

#[test]
fn bridge_converges_and_drains_live() {
    let (msgs, why) = bridge(Duration::from_secs(10), 0.6, vec![1.0; 8]);
    assert_eq!(why, "converged");
    assert_eq!(msgs.first(), Some(&PyMsg::Started("Setup".into())));
    assert_eq!(msgs.last(), Some(&PyMsg::Finished("call".into(), false)));
}

// ---------- 3. downstream re-export with own stable types ----------
pub mod stable {
    use super::*;

    #[non_exhaustive]
    #[derive(Debug, PartialEq)]
    pub enum Why {
        Cancelled,
        OutOfTime,
        Converged,
        Other(String),
    }
    #[derive(Debug, PartialEq)]
    pub enum Solved {
        Done(f64),
        Partial { why: Why, sum: f64 },
    }
    #[derive(Debug)]
    pub enum SolveError {
        Diverged,
    }

    pub fn solve(observer: &dyn Observer<Phase>, data: Vec<f64>) -> Result<Solved, SolveError> {
        let mut data = data;
        let out = pipeline(observer, &mut data).map_err(|e| match e {
            PipeError::Lib(lib3::Error::Diverged) => SolveError::Diverged,
            PipeError::Lib(lib3::Error::Stopped(_)) | PipeError::Stopped(_) => {
                unreachable!("a stop is an outcome")
            }
        })?;
        let sum = data.iter().sum();
        Ok(match out.holding(sum) {
            Outcome::Completed(sum) => Solved::Done(sum),
            Outcome::Stopped { reason, residue } => Solved::Partial {
                why: match reason {
                    Stop::Signal | Stop::Requested => Why::Cancelled,
                    Stop::Deadline => Why::OutOfTime,
                    r if r == ResidualTarget::STOP => Why::Converged,
                    r => Why::Other(format!("{r:?}")),
                },
                sum: residue,
            },
        })
    }
}

#[test]
fn stable_api_reports_why() {
    let r = stable::solve(&ResidualTarget::new(0.3), vec![1.0; 4]).unwrap();
    assert!(
        matches!(
            r,
            stable::Solved::Partial {
                why: stable::Why::Converged,
                ..
            }
        ),
        "{r:?}"
    );
    let r = stable::solve(&(), vec![1.0; 4]).unwrap();
    assert!(matches!(r, stable::Solved::Done(_)));
    let _ = run::<Phase, ()>(&(), |_| Ok(()));
}
