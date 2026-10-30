// r5 consumer probe: a branch-and-bound search and a resumable Monte-Carlo sampler,
// plus owner adapters (JSON-lines logger, stall rule) and stable wrappers.
use std::fmt::Debug;
use std::io::Write;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use inflight::{
    run, try_run, Closed, Flag, Observer, Opened, Outcome, Progress, Reports, Scope, Standard,
    Stop, Stopping,
};

// ---------- designer: 0/1 knapsack branch-and-bound, dynamic tree of scopes ----------
#[derive(Clone, Debug, PartialEq)]
pub enum Bnb {
    Search,
    Subtree { depth: u32, take: bool },
}
#[derive(Clone, Debug, PartialEq)]
pub enum BnbEvent {
    Incumbent(f64),
    Pruned { bound: f64 },
}
impl Reports for Bnb {
    type Event = BnbEvent;
}
impl Standard for BnbEvent {}

#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub value: f64,
    pub weight: f64,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Best {
    pub value: f64,
    pub taken: Vec<usize>,
}

#[derive(Debug, PartialEq)]
pub enum KnapsackError {
    NegativeWeight(usize),
    Stopped(Stop),
}
impl From<Stop> for KnapsackError {
    fn from(s: Stop) -> Self {
        Self::Stopped(s)
    }
}
impl Stopping for KnapsackError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(s) => Some(*s),
            _ => None,
        }
    }
}

const SCOPED_DEPTH: u32 = 3;

struct Search<'a> {
    items: &'a [Item],
    best: Best,
    taken: Vec<usize>,
}

impl Search<'_> {
    fn bound(&self, k: usize, room: f64, value: f64) -> f64 {
        value
            + self.items[k..]
                .iter()
                .filter(|i| i.weight <= room)
                .map(|i| i.value)
                .sum::<f64>()
    }
    fn branch(&mut self, at: &Scope<'_, Bnb>, k: usize, room: f64, value: f64) -> Result<(), Stop> {
        at.check()?;
        at.advance(1, None);
        if value > self.best.value {
            self.best = Best {
                value,
                taken: self.taken.clone(),
            };
            at.event(|| BnbEvent::Incumbent(value));
        }
        if k == self.items.len() {
            return Ok(());
        }
        let bound = self.bound(k, room, value);
        if bound <= self.best.value {
            at.event(|| BnbEvent::Pruned { bound });
            return Ok(());
        }
        let item = self.items[k];
        for take in [true, false] {
            if take && item.weight > room {
                continue;
            }
            let (room, value) = match take {
                true => (room - item.weight, value + item.value),
                false => (room, value),
            };
            if take {
                self.taken.push(k);
            }
            let depth = k as u32;
            if depth < SCOPED_DEPTH {
                at.within(Bnb::Subtree { depth, take }, |sub| {
                    self.branch(sub, k + 1, room, value)
                })?;
            } else {
                self.branch(at, k + 1, room, value)?;
            }
            if take {
                self.taken.pop();
            }
        }
        Ok(())
    }
}

/// Stable public entry point: optimum when completed, the incumbent when stopped.
pub fn knapsack(
    items: &[Item],
    capacity: f64,
    observer: &dyn Observer<Bnb>,
) -> Result<Outcome<Best, Best>, KnapsackError> {
    if let Some(k) = items.iter().position(|i| i.weight < 0.0) {
        return Err(KnapsackError::NegativeWeight(k));
    }
    let mut search = Search {
        items,
        best: Best::default(),
        taken: Vec::new(),
    };
    let outcome = try_run(observer, |call| {
        call.within(Bnb::Search, |s| {
            search
                .branch(s, 0, capacity, 0.0)
                .map_err(KnapsackError::from)
        })
    })?;
    Ok(outcome.holding(search.best))
}

// ---------- designer: resumable Monte-Carlo with checkpoints, no scopes ----------
#[derive(Clone, Debug, PartialEq)]
pub struct Sampler {
    pub seed: u64,
    pub n: u64,
    pub hits: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum McEvent {
    Checkpoint(Sampler),
}
#[derive(Clone, Debug, PartialEq)]
pub enum Mc {}
impl Reports for Mc {
    type Event = McEvent;
}

fn next(seed: &mut u64) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 11) as f64 / (1u64 << 53) as f64
}

pub fn estimate_pi(
    mut state: Sampler,
    samples: u64,
    observer: &dyn Observer<Mc>,
) -> Outcome<f64, Sampler> {
    run(observer, |call| {
        while state.n < samples {
            if state.n % 1000 == 0 {
                call.check()?;
                call.progress(state.n, Some(samples));
                call.event(|| McEvent::Checkpoint(state.clone()));
            }
            let (x, y) = (next(&mut state.seed), next(&mut state.seed));
            state.hits += u64::from(x * x + y * y <= 1.0);
            state.n += 1;
        }
        call.progress(state.n, Some(samples));
        Ok(())
    })
    .holding(state)
    .map(|s| 4.0 * s.hits as f64 / s.n as f64)
}

// ---------- owner: JSON-lines logger for any vocabulary ----------
pub struct JsonLines<'w, W: Write + Send> {
    out: &'w Mutex<W>,
    ids: &'w AtomicU64,
    id: u64,
}
impl<'w, W: Write + Send> JsonLines<'w, W> {
    pub fn new(out: &'w Mutex<W>, ids: &'w AtomicU64) -> Self {
        Self { out, ids, id: 0 }
    }
    fn line(&self, what: std::fmt::Arguments<'_>) {
        let _ = writeln!(self.out.lock().unwrap(), "{{\"id\":{},{what}}}", self.id);
    }
}
impl<S: Reports + Debug, W: Write + Send> Observer<S> for JsonLines<'_, W>
where
    S::Event: Debug,
{
    fn open(&self, scope: &S) -> Opened<'_, S> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed) + 1;
        self.line(format_args!("\"open\":{id},\"scope\":\"{scope:?}\""));
        Opened::With(Box::new(JsonLines {
            out: self.out,
            ids: self.ids,
            id,
        }))
    }
    fn progress(&self, p: Progress) {
        self.line(format_args!("\"done\":{},\"total\":{:?}", p.done, p.total));
    }
    fn event(&self, e: &S::Event) {
        self.line(format_args!("\"event\":\"{e:?}\""));
    }
    fn closed(&self, c: Closed) {
        self.line(format_args!(
            "\"ended\":\"{:?}\",\"ms\":{}",
            c.ended,
            c.elapsed.as_millis()
        ));
    }
}

// ---------- owner: stop after `window` without progress in any scope ----------
pub struct Stall {
    window: Duration,
    last: Mutex<Instant>,
}
impl Stall {
    pub const STOP: Stop = Stop::Rule("stalled");
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last: Mutex::new(Instant::now()),
        }
    }
}
impl<S: Reports> Observer<S> for Stall {
    fn check(&self) -> ControlFlow<Stop> {
        match self.last.lock().unwrap().elapsed() > self.window {
            true => ControlFlow::Break(Self::STOP),
            false => ControlFlow::Continue(()),
        }
    }
    fn progress(&self, _: Progress) {
        *self.last.lock().unwrap() = Instant::now();
    }
}

// ---------- tests ----------
fn items() -> Vec<Item> {
    (0..14)
        .map(|k| Item {
            value: (k * 7 % 11 + 1) as f64,
            weight: (k * 5 % 9 + 1) as f64,
        })
        .collect()
}

#[test]
fn knapsack_completes_and_logs_a_tree() {
    let out = Mutex::new(Vec::new());
    let ids = AtomicU64::new(0);
    let log = JsonLines::new(&out, &ids);
    let outcome = knapsack(&items(), 20.0, &log).unwrap();
    let best = outcome.completed().unwrap();
    assert!(best.value > 0.0);
    let text = String::from_utf8(out.into_inner().unwrap()).unwrap();
    assert!(text.lines().any(|l| l.contains("Subtree")), "{text}");
}

#[test]
fn knapsack_stops_with_incumbent_from_a_flag() {
    let flag = Flag::new();
    flag.raise(Stop::Requested);
    let outcome = knapsack(&items(), 20.0, &flag).unwrap();
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stop::Requested,
            ..
        }
    ));
    assert_eq!(
        knapsack(
            &[Item {
                value: 1.0,
                weight: -1.0
            }],
            1.0,
            &()
        ),
        Err(KnapsackError::NegativeWeight(0))
    );
}

#[test]
fn stall_and_logger_compose() {
    let out = Mutex::new(Vec::new());
    let ids = AtomicU64::new(0);
    let both = (Stall::new(Duration::ZERO), JsonLines::new(&out, &ids));
    std::thread::sleep(Duration::from_millis(2));
    let outcome = knapsack(&items(), 20.0, &both).unwrap();
    assert!(matches!(
        outcome,
        Outcome::Stopped {
            reason: Stall::STOP,
            ..
        }
    ));
}

#[test]
fn monte_carlo_resumes_from_its_residue() {
    let start = Sampler {
        seed: 7,
        n: 0,
        hits: 0,
    };
    let calls = AtomicU64::new(0);
    struct StopAt<'a>(&'a AtomicU64, u64);
    impl Observer<Mc> for StopAt<'_> {
        fn check(&self) -> ControlFlow<Stop> {
            match self.0.fetch_add(1, Ordering::Relaxed) >= self.1 {
                true => ControlFlow::Break(Stop::Requested),
                false => ControlFlow::Continue(()),
            }
        }
    }
    let Outcome::Stopped { residue, .. } = estimate_pi(start.clone(), 100_000, &StopAt(&calls, 30))
    else {
        panic!("should stop")
    };
    assert_eq!(residue.n, 30_000);
    let resumed = estimate_pi(residue, 100_000, &()).completed().unwrap();
    let straight = estimate_pi(start, 100_000, &()).completed().unwrap();
    assert_eq!(resumed, straight);
}
