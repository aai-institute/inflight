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

`Stop` converts to `PyErr`: `Signal` to `KeyboardInterrupt`, `Deadline` to `TimeoutError`, `Memory` to `MemoryError`, `Requested` to `InterruptedError`.

## Development

```sh
pixi install
pixi run test   # cargo test --all-features
pixi run lint   # clippy, warnings denied
```

## License

MIT
