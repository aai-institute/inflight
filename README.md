# inflight

Stop a running computation at the safe points it chooses, and learn why it stopped.

`inflight` is the shared control layer between long-running numerical libraries and their callers, including callers in Python. A library takes a `&dyn Control` and checks it at its safe points. The caller decides what that control is: a Ctrl-C flag, a deadline, its own rule, or any combination of these.

## Design

- **The computation chooses its safe points.** These are places where stopping leaves a well-defined state. The caller only decides *whether* to stop there.
- **Control means choosing, not changing.** A control answers `Continue` or `Break(Stop)`. It never gets write access to the computation.
- **A stop is an outcome, not an error.** A call returns `Outcome::Completed(result)` or `Outcome::Stopped { reason, residue }`. The residue is whatever the computation can hand back. It may be nothing, or it may be state you can resume from.
- **Control is an explicit parameter.** There is no ambient or thread-local state, so it reaches rayon workers and nested calls unchanged.
- **One call latches.** `Latched` holds the first stop for the rest of the call, so a control that later reads `Continue` again cannot resume a half-stopped computation.

## Library side

```rust
use inflight::{Control, Latched, Outcome};

pub fn solve(control: &(impl Control + ?Sized)) -> Outcome<Solution, Resumable> {
    let control = Latched::new(&control);
    for step in 0.. {
        // ... one iteration ...
        if control.check().is_break() {
            break;
        }
    }
    match control.stopped() {
        Some(reason) => Outcome::Stopped { reason, residue: resumable },
        None => Outcome::Completed(solution),
    }
}
```

A nested call receives the outer `Latched`. A stop inside the inner call then also stops the outer one.

## Caller side

```rust
use inflight::{Deadline, Flag, Stop};
use std::time::Duration;

let cancel = Flag::new();                      // raise from another thread: cancel.raise(Stop::Requested)
let outcome = solve(&(&cancel, Deadline::after(Duration::from_secs(30))));
```

## Observation

A computation names what it reports in its own `Vocabulary`: a scope type (a block, a run) and an event type. It takes one `&dyn Observer<V>`, which hears scopes, progress and events and answers the checks, so a rule that decides from what it observes is one object.

```rust
pub fn solve(observer: &(impl Observer<Solve> + ?Sized)) -> Outcome<Solution> {
    let call = Scope::root(&observer);
    let run = call.open(&SolveScope::Run);
    run.progress(Progress { done, total: Some(n) });
    run.event(|| SolveEvent::Residual(r));      // built only if someone listens
    if run.check().is_break() { /* stop here */ }
    run.close();                                // dropped instead: stopped, failed or unwound
    // ...
}
```

`inflight`'s controls are observers of any vocabulary; a caller's own `Control` goes in `Checked`. Pairs combine observers. Also included:

- `Recorder` keeps the scope tree with its events and ends.
- `Nest::new(&scope)` hands an inner computation an observer inside an outer scope: its scopes and events join the outer vocabulary through `From`, and its stops stop the outer call.
- `Forward` sends everything through a channel; its `Outbox` drains in order, each scope's progress coalesced to the latest.
- `Traced` (`tracing` feature) turns scopes into spans and the rest into events.

## Memory cap (`memory` feature)

`MemoryCap` stops once the process's resident memory passes a byte limit. A background thread samples it at a fixed interval, so a check costs one atomic load. It is a soft cap: memory can grow past it until the computation's next safe point. `start` returns `None` where the platform does not report resident memory.

```rust
use inflight::MemoryCap;
use std::time::Duration;

let cap = MemoryCap::start(8 << 30, Duration::from_millis(1)).expect("resident memory is measurable");
let outcome = solve(&cap);
```

## Python (`pyo3` feature)

`interruptible` runs the work beside the main thread with the GIL released. The main thread checks for signals every 50 ms. If Python's signal handler raises, the work's control stops it at its next safe point, and the handler's exception is raised.

```rust
let outcome = inflight::interruptible(py, |signal| solve(&signal))?;
```

`interruptible_with` also runs a hook on the main thread at every wake and once after the work ends, such as draining an `Outbox` into a Python callback. If the hook raises, the work stops and that exception is raised.

`Stop` converts to `PyErr`: `Signal` to `KeyboardInterrupt`, `Deadline` to `TimeoutError`, `Memory` to `MemoryError`, `Requested` to `InterruptedError`.

## Development

```sh
pixi install
pixi run test   # cargo test --all-features
pixi run lint   # clippy, warnings denied
```

## License

MIT
