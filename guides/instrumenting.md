# Instrumenting an algorithm

You write four things: what your algorithm reports, how its errors carry a stop, an observer parameter on each entry point, and a call to inflight around each entry point's body. Your algorithm never learns what any observer does with a report.

## 1. Name what you report

Write two plain enums: the parts of a run that have a start and an end (scopes), and the things that happen inside them (events). Link them once.

```rust
#[derive(Clone, Debug)]
pub enum SolveScope {
    Sweep { block: usize },
    Smoothing(SmoothScope),          // a third-party inner algorithm, see step 6
}

#[derive(Clone, Debug)]
pub enum SolveEvent {
    Step { residual: f64 },
    Smoothing(SmoothEvent),
}

impl inflight::Reports for SolveScope {
    type Event = SolveEvent;
}
```

- Events are owned values. Building one costs nothing when no observer listens (step 4).
- Give each entry point that reports something different its own scope enum: `reduce` has `ReductionScope`, `factor` has `FactorizationScope`. An observer written for one then cannot be passed to the other.
- An entry point that reports nothing and only checks takes `&dyn Observer<Infallible>`: inflight implements `Reports` for `std::convert::Infallible`.
- Optional: say which of your events are standard facts, and generic rules such as `IterationLimit` or an owner's stagnation rule work with your algorithm. Both methods default to "no":

  ```rust
  impl inflight::Standard for SolveEvent {
      fn is_iteration(&self) -> bool {
          matches!(self, Self::Step { .. })
      }
      fn residual(&self) -> Option<f64> {
          match self {
              Self::Step { residual } => Some(*residual),
              Self::Smoothing(_) => None,
          }
      }
  }
  ```

## 2. Let your error carry a stop

`check()?` turns a stop into your error through `From<Stop>`. `Stopping` tells inflight which of your errors *are* that stop, so it can return `Outcome::Stopped` for them and pass every other error through:

```rust
#[derive(Debug)]
pub enum SolveError {
    Singular { block: usize },
    Stopped(Stop),
}

impl From<Stop> for SolveError {
    fn from(stop: Stop) -> Self {
        Self::Stopped(stop)
    }
}

impl inflight::Stopping for SolveError {
    fn stop(&self) -> Option<Stop> {
        match self {
            Self::Stopped(stop) => Some(*stop),
            Self::Singular { .. } => None,
        }
    }
}
```

An error that wraps another one forwards `stop()` to it, so a stop deep inside a wrapped error is still recognized. `From<Stop>` and `stop()` must agree (`E::from(s).stop() == Some(s)`); debug builds assert it.

## 3. Take an observer

```rust
pub fn solve(blocks: &mut [Vec<f64>], observer: &dyn Observer<SolveScope>)
    -> Result<Outcome<f64>, SolveError>
```

Callers pass `&()` for nothing, `&flag` for a Ctrl-C flag, or `&(a, b)` for two observers.

## 4. Run the body inside inflight

```rust
inflight::try_run(observer, |call| {
    let total = blocks.len() as u64;
    let mut worst = 0.0_f64;
    for (block, x) in blocks.iter_mut().enumerate() {
        if x.is_empty() {
            return Err(SolveError::Singular { block });     // your own error, as usual
        }
        let residual = call.within(SolveScope::Sweep { block }, |sweep| {
            let mut residual = 1.0;
            for step in 0..10 {
                residual /= 2.0;
                sweep.progress(step + 1, Some(10));
                sweep.event(|| SolveEvent::Step { residual });
                sweep.check()?;                             // a safe point: the call may stop here
            }
            Ok::<_, SolveError>(residual)
        })?;
        call.advance(1, Some(total));
        worst = worst.max(residual);
    }
    Ok(worst)
})
```

Inside a scope you have six calls:

| Call | Meaning |
|---|---|
| `scope.within(child, \|child\| ..)` | Run a part with its own start and end. |
| `scope.progress(done, total)` | Set how far this scope got. `total` is `Option<u64>`. |
| `scope.advance(by, total)` | Add `by` to this scope's progress; reports from several threads stay in order. |
| `scope.event(\|\| event)` | Report an event. The closure runs only if someone listens. |
| `scope.check()?` | A safe point. Returns `Err(Stop)` if an observer wants the call to stop; observers answer with `ControlFlow`, and inflight turns a `Break` into this `Err` so that it sits next to your own `?`. |
| `scope.nest(child, \|nest\| inner(nest))` | Run an instrumented inner algorithm in a scope of its own (step 6). |

A scope has one progress count: `progress` sets it and `advance` adds to it, so a resumed scope can set where it starts and then advance.

How a scope ends follows from what its closure returns; you never close one by hand:

| The closure | The scope ends | `try_run` returns |
|---|---|---|
| returns `Ok` | completed | `Ok(Outcome::Completed(..))` |
| returns an error whose `stop()` is the call's stop | stopped | `Ok(Outcome::Stopped { reason, .. })` |
| returns any other error | failed | `Err(e)`, even if the call also stopped (e.g. a parallel sibling stopped it) |
| panics | unwound | the panic continues |

An infallible body uses `inflight::run`, whose closure returns `Result<R, Stop>` and which returns `Outcome<R>`. `Outcome::into_result()` turns an inner call's `Outcome` back into `Result<R, Stop>` for `?`, and `Outcome::map` changes the result and keeps a stop.

## 5. Hand back what a stopped call leaves

A resumable algorithm keeps its state outside the closure and attaches it afterwards. `holding` makes it the result when the call completes and the residue when it stops:

```rust
pub fn iterate(mut state: Iterate, observer: &dyn Observer<SolveScope>) -> Outcome<Iterate, Iterate> {
    inflight::run(observer, |call| {
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
```

The caller resumes by passing the residue back in: `iterate(residue, observer)`.

## 6. Call another instrumented algorithm

An inner algorithm takes an observer of *its* vocabulary. Say once how its scopes and events read in yours, then hand it a `Nest`:

```rust
impl inflight::Within<SmoothScope> for SolveScope {
    fn scope(inner: &SmoothScope) -> Self {
        Self::Smoothing(inner.clone())
    }
    fn event(inner: &SmoothEvent) -> Option<SolveEvent> {
        Some(SolveEvent::Smoothing(inner.clone()))       // `None` drops a chatty event
    }
}

call.nest(SolveScope::Smoothing(SmoothScope::Pass), |nest| {
    smooth(x, nest).into_result()
})?;
```

- `nest` opens a scope of its own for the inner call. The inner call's scopes and events join your tree beneath it, its root progress is that scope's progress, and its checks and stops are your call's.
- One `Nest` hears one inner call. To call the inner algorithm twice, nest twice.
- The `Within` impl lives on your type, so it works even when the inner events are another crate's or a primitive like `u64`. Every vocabulary already reads itself unchanged.

## 7. Choose your safe points

- Call `check()` where stopping leaves nothing half-built that the caller would see.
- Scopes are `Sync`: blocks run in parallel each call `call.within(..)` from their own thread, and `call.advance(1, Some(blocks))` counts finished blocks in order.
- **Keep scopes out of hot loops.** Passing a scope into a per-pivot loop cost approx-chol 2–3% of its instructions, even with no observer. Run the hot loop in chunks, and report and check between chunks:

  ```rust
  while done < total {
      let n = CHUNK.min(total - done);
      pivot(n);                                  // no scope in here
      done += n;
      elimination.progress(done, Some(total));
      elimination.event(|| FactorizationEvent::Fill { entries: stored() });
      elimination.check()?;
  }
  ```

- To report about a chunk, sum it up inside the loop (a counter or a maximum) and report the summary at the boundary. Sampling single iterations costs the same and says less.
- Report at the finest grain that is still cheap. Callers who want less wrap their observer in `Every`; nobody can get more than you report.
- Many short tasks: one scope for the scheduler with `advance(1, ..)` and a lazy `event`, rather than a scope per task.
