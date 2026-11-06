# Writing an observer

An observer hears what an algorithm reports and, at the algorithm's safe points, may stop it. Implement `Observer<S>`, where `S` is the algorithm's scope enum, and only the methods you need. All of them have defaults.

| Method | Called | Default |
|---|---|---|
| `check(&self) -> ControlFlow<Stop>` | at each safe point | `Continue(())` |
| `event(&self, event: &S::Event)` | for each event | ignores it |
| `progress(&self, progress: Progress)` | when a scope reports progress | ignores it |
| `closed(&self, closed: Closed)` | once, when a scope ends | ignores it |
| `open(&self, scope: &S) -> Opened<'_, S>` | when a scope starts inside the call | `Opened::Same` |

## Ready-made

| Observer | Does |
|---|---|
| `()` | nothing; never stops |
| `Flag` | stops with the reason it was raised with (`flag.raise(Stop::Requested)`, or `Stop::Signal` from `interruptible`) |
| `Deadline::after(d)`, `Deadline::at(t)` | stops with `Stop::Deadline` |

The controls (`Flag`, `Deadline`) work with every algorithm and skip every scope; they also have an inherent `check()` for code that only wants to poll them.

## A rule that only stops

```rust
pub struct FillCap { budget: u64, seen: AtomicU64 }

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
```

Stop with `Stop::Rule(name)` and publish it as a `STOP` constant, so a caller can tell your stop from a user's cancel: `Ok(Outcome::Stopped { reason: FillCap::STOP, .. }) => ..`.

Update your state in `event`, decide in `check`. You can't stop from inside `event`: only the algorithm knows where stopping is safe, and it asks at its next safe point.

## A rule per scope

A rule that judges each scope on its own hands each a fresh child in `open`:

```rust
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
```

## A listener

Implement `event`, `progress` or `closed`. By default you hear every scope inside the call, with the same observer:

```rust
#[derive(Default)]
pub struct Closes(AtomicU64);

impl<S: Reports> Observer<S> for Closes {
    fn closed(&self, _closed: Closed) {
        self.0.fetch_add(1, Relaxed);
    }
}
```

## State per scope

`open` decides what hears a scope that starts:

| `open` returns | Meaning |
|---|---|
| `Opened::Same` | This observer hears it, as it hears the call. The default. |
| `Opened::Skip` | Nothing of yours hears it, nor anything inside it. Your `check` is still asked. |
| `Opened::With(Box::new(child))` | `child`, a fresh observer, hears it and everything inside it, and is asked at its safe points along with you. |

```rust
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
```

## Wrapping another observer

A wrapper decides what hears a child by asking the observer it wraps through `hear`, which returns `None` for `Skip` and the child to wrap otherwise:

```rust
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
```

Don't return `Opened::With(Box::new(&self.0))`: the wrapped observer then answers checks both as itself and as the child, so a rule that counts its checks counts twice. `hear` avoids that.

## Combining and reusing

- `&(a, b)` passes two observers; both hear everything, and `a` is asked first. Nest pairs for more: `&(a, (b, c))`.
- `&o` and `Box<dyn Observer<S>>` are observers too. An optional observer: `let shown: &dyn Observer<S> = bar.as_ref().map_or(&(), |b| b);`

## Python

`interruptible_with(py, on_wake, work)` runs `on_wake` on the main thread at every signal check. `ProgressCallback::report(py)` there hands the call's progress to a Python callable; see the README.

## Rules that hold

- **Every method takes `&self` and may be called from several threads at once.** Keep state in atomics or behind a mutex; `Observer` requires `Sync`.
- **`check` is asked once per safe point per observer**, however deep the scope. A scope's own child observer is asked first, then those of the scopes around it, and the first stop holds for the rest of the call.
- **`closed` is called once per scope you heard open, last.** It still comes if your neighbour in a pair panics in its `open` or `closed`.
- **Events of one scope arrive in order.** Events of scopes running in parallel interleave.
- **A panic in your observer** propagates like a panic in the algorithm. If one happens while another panic is already unwinding, inflight swallows the second so that the process does not abort.
