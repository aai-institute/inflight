# inflight

Stop a running computation at the safe points it chooses, and learn why it stopped.

`inflight` is the shared control and observation layer between long-running numerical libraries and their callers, including callers in Python. A library takes one `&dyn Observer<SolveScope>`, reports what it does, and checks it at its safe points. The caller decides what that observer is: a Ctrl-C flag, a deadline, a progress bar, a rule of its own, or any combination of these.

## Design

- **The computation chooses its safe points.** These are places where stopping leaves a well-defined state. The caller only decides *whether* to stop there. Stopping is cooperative: inflight never preempts a thread, so a stop takes effect at the next safe point on each thread, and the library must return the stop it is handed. The contract is in [`guides/instrumenting.md`](guides/instrumenting.md#7-choose-your-safe-points).
- **Observing means choosing, not changing.** An observer hears what the computation reports and answers checks with `Continue` or `Break(Stop)`. It never gets write access to the computation.
- **A stop is an outcome, not an error.** A call returns `Outcome::Completed(result)` or `Outcome::Stopped { reason, residue }`. The residue is whatever the computation can hand back: nothing, or state you can resume from. An error the computation raised on its own still comes back as an error.
- **The observer is an explicit parameter.** There is no ambient or thread-local state, so it reaches rayon workers and nested calls unchanged.
- **One call latches.** The first stop holds for the rest of the call, so an observer that later reads `Continue` again cannot resume a half-stopped computation.

## Library side

A computation names what it reports with two enums, its scopes and its events, and runs its body inside inflight:

```rust
impl inflight::Reports for SolveScope {
    type Event = SolveEvent;
}

pub fn solve(observer: &dyn Observer<SolveScope>) -> Result<Outcome<Solution>, SolveError> {
    inflight::try_run(observer, |call| {
        call.within(SolveScope::Sweep { block }, |sweep| {
            sweep.progress(done, Some(n));
            sweep.event(|| SolveEvent::Step { residual });  // built only if someone listens
            sweep.check()?;                                  // a safe point
            Ok(())
        })?;
        Ok(solution)
    })
}
```

A scope ends as its closure returns: completed, stopped, failed or unwound. An error that carries the call's stop (`SolveError: Stopping`) makes the call `Outcome::Stopped`; any other error is returned as is. `run` is the same for a body whose only error is a stop, `Outcome::holding(state)` hands a resumable computation's state back, and `scope.nest(..)` runs another instrumented algorithm inside a scope of its own. See [`guides/instrumenting.md`](guides/instrumenting.md).

## Caller side

```rust
use inflight::{Deadline, Flag, Stop};
use std::time::Duration;

let cancel = Flag::new();                      // raise from another thread: cancel.raise(Stop::Requested)
let outcome = solve(&(&cancel, Deadline::after(Duration::from_secs(30))))?;
match outcome {
    Outcome::Completed(solution) => ..,
    Outcome::Stopped { reason: Stop::Deadline, .. } => ..,
    Outcome::Stopped { reason, .. } => ..,
}
```

Observers implement only the `Observer` methods they need: `check`, `event`, `progress`, `closed`, and `open`, which decides what hears a scope that starts. Pairs combine observers. Also included:

- `Flag` and `Deadline` stop any computation.
- A rule of your own stops with `Stop::Rule(name)`, so its stop can be told apart from a cancel.

See [`guides/observing.md`](guides/observing.md).

## Python (`pyo3` feature)

`interruptible` runs the work beside the main thread with the GIL released. The main thread checks for signals every 50 ms. If Python's signal handler raises, the work's `Flag` stops it at its next safe point, and the handler's exception is raised.

```rust
let outcome = inflight::interruptible(py, |signal| solve(signal))?;
```

`interruptible_with` also runs a hook on the main thread at every wake and once after the work ends. If the hook raises, the work stops and that exception is raised.

Two observers turn a Python function's keyword arguments into inflight:

- `Limits::new(timeout)?` stops any computation after `timeout` seconds, raising `TimeoutError`; a bad `timeout` is a `ValueError`.
- `ProgressCallback::new(progress)` hands the call's own progress to a Python `progress(done, total)`, and `ProgressCallback::summing(progress, |scope| ..)` the summed progress of the scopes it picks. Call its `report(py)` from `interruptible_with`'s hook; it calls Python only when the progress moved, and does nothing without a callable.

```rust
let limits = inflight::Limits::new(timeout)?;
let progress = inflight::ProgressCallback::new(progress);
let outcome = inflight::interruptible_with(py, |py| progress.report(py), |signal| {
    solve(&(signal, (&limits, &progress)))
})?;
```

`Stop` converts to `PyErr`: `Signal` to `KeyboardInterrupt`, `Deadline` to `TimeoutError`, `Requested` and `Rule` to `InterruptedError`.

## Development

```sh
pixi install
pixi run test   # cargo test --all-features
pixi run lint   # clippy, warnings denied
```

## License

MIT
