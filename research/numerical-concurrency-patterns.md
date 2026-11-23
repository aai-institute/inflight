# Concurrency, stop and observation patterns in numerical libraries

Research for [inflight#8](https://github.com/aai-institute/inflight/issues/8), feeding [inflight#5](https://github.com/aai-institute/inflight/issues/5) (a parallel map whose elements wait on lazily built shared values). Sources read on 2026-11-23. A claim with a link comes from that source. A claim marked **(analysis)** is my own conclusion from the cited facts.

## Question

Which concurrency, stop and observation patterns do established numerical libraries use, and which are generic enough for inflight? For each: the problem it solves, what it needs from the runtime, and whether it maps onto rayon.

## Gist

1. oneTBB's `collaborative_call_once` lets waiters help build a shared value. It needs an arena the waiters can join plus *work isolation* keyed to that one build. rayon has neither. On rayon the substitute is to **park** waiting elements (return them to the scheduler as state, not as a blocked stack frame). A parked element's worker then goes back to the pool, where rayon's plain work stealing already runs the build's jobs. Parking is the only option from this survey that needs no `unsafe`, no second pool and no stackful coroutines.
2. Stops in numerical libraries are cooperative everywhere: oneTBB, OpenMP, MKL, PETSc, Belos, Ceres, SciPy, scikit-learn and argmin. Unstarted work is skipped, running work notices at its next check, and the first failure wins. inflight already has the per-call latch, the nesting and the part-local stop (`under`) that these libraries use.
3. Monitors (observe only) and convergence tests (decide) are separate hooks in PETSc and Belos. inflight's `event`/`progress` vs `check` split matches that. Status-test combinators (Belos AND/SEQ, quorum) and convergence reasons (PETSc, Ceres) need solver state, so they belong in the solver, not in inflight.
4. Thread-count-independent reductions all use the same recipe: a partition that does not depend on the thread count and a combine order that is fixed (oneTBB `parallel_deterministic_reduce`, MKL strict CNR). rayon's `reduce` splits by `current_num_threads()` and by stealing, so only fixed chunks combined in order are reproducible. For inflight, only the *ordering* obligations of #5 are generic: result order, which failure is reported, and progress order.
5. `enough::Stop` is a polled `check() -> Result<(), StopReason>` trait with two reasons. It maps onto inflight with an adapter in each direction; the mapping loses detail because inflight has four stop kinds and enough has two.

## Starting point: what inflight has

- Cooperative stop at library-chosen safe points; the first stop in a call latches for the whole call ([README](https://github.com/aai-institute/inflight/blob/9bd9c88/README.md#design), latch in [`src/observe.rs` L248](https://github.com/aai-institute/inflight/blob/9bd9c88/src/observe.rs#L248)).
- Observer with `check` (decide), `event`/`progress`/`closed` (listen) and `open` (what hears a child scope) ([`guides/observing.md`](https://github.com/aai-institute/inflight/blob/9bd9c88/guides/observing.md)).
- `Scope::under` gives a part a stop of its own that ends only that part ([`src/observe.rs` L345](https://github.com/aai-institute/inflight/blob/9bd9c88/src/observe.rs#L345)).
- Python: work runs beside the main thread, which polls signals ([README](https://github.com/aai-institute/inflight/blob/9bd9c88/README.md#python-pyo3-feature)).
- No scheduler of any kind. #5 asks for one behind a `rayon` feature.

---

## 1. Lazy shared values with helping waiters: oneTBB `collaborative_call_once`

**Problem.** A value is built once, the build is itself parallel, and several threads may need it at the same moment. With `std::call_once`/`OnceLock` the losers block, so their cores sit idle while the winner's parallel build runs, or, on a work-stealing pool, they deadlock (§2). The spec: it "executes the `Func` object only once, even if it is called concurrently" and "allows other threads blocked on the same `collaborative_once_flag` to join oneTBB parallel construction called within the `Func` object" ([spec](https://uxlfoundation.github.io/oneAPI-spec/spec/elements/oneTBB/source/algorithms/functions/collaborative_call_once_func.html)).

**Failure semantics.** "In case of the exception thrown from the `Func` object, the thread calling the `Func` object receives this exception. One of the threads blocked on the same `collaborative_once_flag` calls the `Func` object again" ([spec](https://uxlfoundation.github.io/oneAPI-spec/spec/elements/oneTBB/source/algorithms/functions/collaborative_call_once_func.html)). The implementation resets the flag to `uninitialized` on exception "to allow other threads to try initialization again" ([source L185–L192](https://github.com/uxlfoundation/oneTBB/blob/e497f24/include/oneapi/tbb/collaborative_call_once.h#L185-L192)).

**Mechanism** ([source L60–L150](https://github.com/uxlfoundation/oneTBB/blob/e497f24/include/oneapi/tbb/collaborative_call_once.h#L60-L150)):
- The flag is one atomic word: `uninitialized`, `done`, or a pointer to the running build's *runner*, whose alignment leaves low bits free for a waiter refcount.
- The runner holds a `task_arena` constructed with `task_arena::attach{}`, which connects to the arena the calling thread already uses, so no new threads ([task_arena spec](https://oneapi-spec.uxlfoundation.org/specifications/oneapi/latest/elements/onetbb/source/task_scheduler/task_arena/task_arena_cls)). It also holds a `wait_context`.
- The winner runs the function inside `isolate_within_arena(delegate, reinterpret_cast<std::intptr_t>(this))`: the isolation tag is the runner's address (L76–L86).
- A waiter (`assist`, L138–L149) joins the same arena under the *same* isolation tag and calls `wait(m_storage.m_wait_context, ...)`. While it waits it can run only tasks spawned inside this build. It never runs the build function itself, and exceptions stay with the winner.
- If a waiter cannot get an arena slot, `task_arena::execute` "enqueues it into the arena, waits using an OS kernel synchronization object for another opportunity to join" ([task_arena spec](https://oneapi-spec.uxlfoundation.org/specifications/oneapi/latest/elements/onetbb/source/task_scheduler/task_arena/task_arena_cls)), so a waiter can still block in the worst case.

**Runtime requirement.** (a) A waiter can join the build's arena. (b) Isolation keyed per build, so a waiting thread runs only the build's tasks. (c) A completion counter (`wait_context`) that waiters can wait on while executing tasks.

**rayon fit: no.** rayon has no isolation (§2). Its wait helps with *any* job, and the only scoping it offers is by pool. A waiter cannot join one particular `join`/`scope` and help only that.

**Generic for inflight: the goal is, the mechanism is not (analysis).** "Built once, may fail, built in parallel, waiters retry after a failed build" is the contract #5 asks for, and nothing about it is specific to one solver. The mechanism depends on TBB runtime internals that cannot be built on rayon's public API without `unsafe`. On rayon the same effect comes from parking (§4).

## 2. Task isolation: `this_task_arena::isolate`

**Problem.** A thread waiting inside a nested parallel construct may pick up outer-level tasks: "The blocked thread is allowed to take tasks belonging to the first parallel loop", which can change thread-local state mid-iteration and "might lead to deadlocks" ([oneTBB guide, Work Isolation](https://uxlfoundation.github.io/oneTBB/main/tbb_userguide/work_isolation.html)). Inside `isolate`, the waiting thread runs only tasks spawned in the isolated region. "The isolation region imposes restrictions only upon the thread that called it" (same page).

**The same hazard on rayon.**
- rayon's wait loop runs whatever it finds: local jobs, then stolen jobs, then injected jobs ([`registry.rs` `wait_until_cold` L780, `find_work` L833](https://github.com/rayon-rs/rayon/blob/v1.12.0/rayon-core/src/registry.rs#L780-L842)). `find_work`'s comment: "finish what we started before we take on something new". That is a preference, not a restriction.
- The maintainers' description of the resulting deadlock under a `Mutex`: the waiting thread "goes to steal more work itself while it's waiting... finds another of the `for_each(mutex_and_par)` calls, tries to grab the lock (again) ... DEADLOCK". Their advice: "don't call rayon under a mutex". The issue is still open ([rayon#592](https://github.com/rayon-rs/rayon/issues/592)). A `OnceLock` held by a builder that waits in its own `join` is the same shape, which is #5's first bullet.
- `ThreadPool::install` from another pool's worker "will try to keep busy while the `op` completes in its target pool, similar to calling `ThreadPool::yield_now()` in a loop" ([`thread_pool/mod.rs` L68–L110](https://github.com/rayon-rs/rayon/blob/v1.12.0/rayon-core/src/thread_pool/mod.rs#L68-L110)). A separate pool therefore does not isolate the calling thread either. It only keeps the build's own jobs on other threads, and those threads are the oversubscription.
- `rayon::yield_now()` "looks for a single unit of pending work in the pool, then executes it"; `yield_local()` "does not steal from other threads" ([`thread_pool/mod.rs` L459–L493](https://github.com/rayon-rs/rayon/blob/v1.12.0/rayon-core/src/thread_pool/mod.rs#L459-L493)). Both are unscoped.

**Runtime requirement.** The scheduler must tag tasks with an isolation region and filter on that tag when a thread waits.

**rayon fit: no.** No tag, no filter. A search of rayon's issue tracker for "isolation"/"isolate" finds no request or design for it (searched 2026-11-23).

**Generic for inflight: no.** It is a scheduler feature. inflight cannot add it without owning a scheduler, which #5 rules out ("no pool of its own"). The design goal follows from this **(analysis)**: no pool job may ever wait while it holds something another job needs.

## 3. Suspending a running task: oneTBB resumable tasks

**Problem.** A task that must wait for an external event releases its thread without finishing. `suspend(func)` hands `func` a `suspend_point`; the thread goes on to other work; `resume(tag)` may be called "even on a separate thread", and "there are no guarantees that the same thread that called `suspend` continues execution after the suspended point" ([spec, Resumable tasks](https://spec.oneapi.io/versions/latest/elements/oneTBB/source/task_scheduler/scheduling_controls/resumable_tasks.html)).

**Runtime requirement.** Stackful coroutines. oneTBB implements them with `ucontext` (`makecontext`/`swapcontext`) or a thread per coroutine under sanitizers ([`co_context.h` L23–L66](https://github.com/uxlfoundation/oneTBB/blob/e497f24/src/tbb/co_context.h#L23-L66)). The feature is compiled out on Android, QNX and non-glibc Linux ([`_config.h` L286](https://github.com/uxlfoundation/oneTBB/blob/e497f24/include/oneapi/tbb/detail/_config.h#L286)).

**rayon fit: no.** rayon jobs are stack frames on the worker's stack. Swapping stacks needs `unsafe` and platform code, which inflight forbids (`unsafe_code = "forbid"`, [Cargo.toml](https://github.com/aai-institute/inflight/blob/9bd9c88/Cargo.toml)).

**Generic for inflight: no**, as a mechanism. What it achieves, a waiting element that holds no thread, is exactly what parking achieves without a second stack (§4).

## 4. Park and resume: the stackless equivalent on rayon

No library in this survey ships this for rayon, so this section is **(analysis)** built on the facts above, not a sourced pattern.

An element that advances in steps returns at each step either `Done`, `Failed`, or `Waiting(key)`. A parked element is plain data, not a suspended stack. A call runs rounds:
1. Advance every ready element in parallel (`par_iter_mut` on the caller's pool).
2. For each key that parked elements wait on and nobody has built yet, build it on the same pool (the build may use `join`/`par_iter` freely).
3. Unpark the elements whose key is ready, and repeat.

Why this sidesteps §1–§3:
- No pool job ever blocks on a value: an element that needs one returns. A worker that would have blocked goes back to rayon's wait loop, and that loop runs the build's jobs. That is the "help while waiting" of `collaborative_call_once`, obtained without isolation, because no waiting stack frame holds anything.
- A failed build leaves the key unbuilt. A later round or a later call retries, which matches TBB's retry rule.
- A parked element sits at a safe point by construction, so the call can stop there and hand the parked state back as residue (inflight's `Outcome::holding`).

The open part is concurrent calls, as #5 already says. If call B needs a key that call A is building, B's root must wait somewhere:
- If B's root is not a pool job, it can block, because it holds no worker.
- If B's root is a pool job, it can loop on `rayon::yield_now()` until the key is ready. That is TBB's `assist` without isolation. It is safe if B holds no lock and the key dependencies are acyclic, because whatever B steals cannot wait on B. A build that itself parks on another key keeps the graph a DAG only if keys are ordered. The alternative is a duplicate build, which never waits but repeats the work.
- To stop while waiting, the loop polls `check()` between `yield_now` calls. inflight observers are polled, not callback-driven, so this needs nothing new.

**Runtime requirement.** `par_iter`/`join` on one pool and `yield_now`. All of it is public, safe rayon API.

**rayon fit: yes.** **Generic for inflight: yes**, because nothing in it names a solver. It is #5's design.

## 5. Cancellation: contexts, cancellation points, first failure wins

**oneTBB `task_group_context`.** `cancel_group_execution()` "cancels the tasks in the `task_group` that have been submitted but have not started to execute". Running tasks "can query the cancellation status... and exit early" via `is_current_task_group_canceling()` ([guide, Cancellation Without An Exception](https://uxlfoundation.github.io/oneTBB/main/tbb_userguide/Cancellation_Without_An_Exception.html)). "Cancelling a `task_group_context` cancels all of its child `task_group_context` objects", and an "isolated" context blocks downward propagation. "Exceptions propagate upwards. Cancellation propagates downwards" ([guide, Cancellation and Nested Parallelism](https://uxlfoundation.github.io/oneTBB/main/tbb_userguide/Cancellation_and_Nested_Parallelism.html)). On an exception, "Any further exceptions inside the algorithm are ignored... Pending iterations are not executed... Once all parts of the algorithm stop, an exception is thrown on the thread that invoked the algorithm" ([guide, Exceptions and Cancellation](https://uxlfoundation.github.io/oneTBB/main/tbb_userguide/Exceptions_and_Cancellation.html)).

**OpenMP `cancel` / `cancellation point`.** "If the cancel-var ICV is false, the cancel construct is ignored." Cancellation is observed only at cancellation points (including barriers). A not-yet-started task in a cancelled taskgroup "may be discarded"; started tasks run to completion or to a cancellation point. After cancellation, reduction and lastprivate values "are undefined", and "the programmer is responsible for releasing locks" ([OpenMP 5.0 §2.18.1](https://www.openmp.org/spec-html/5.0/openmpsu101.html)).

**Eigen thread pool.** `Cancel()`: "stop processing the closures that have been enqueued. Currently running closures may still be processed" ([`ThreadPoolInterface.h` L34–L37](https://gitlab.com/libeigen/eigen/-/blob/master/Eigen/src/ThreadPool/ThreadPoolInterface.h#L34-L37)). It is pool-wide and sets `done_`, which shuts the pool down ([`NonBlockingThreadPool.h` L209–L222](https://gitlab.com/libeigen/eigen/-/blob/master/Eigen/src/ThreadPool/NonBlockingThreadPool.h#L209-L222)).

**rayon.** No cancel for queued jobs. The fallible iterators short-circuit cooperatively: a consumer's `full()` is a "Hint whether this `Consumer` would like to stop processing further items" ([`plumbing/mod.rs` L143–L145](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/plumbing/mod.rs#L143-L145)). For `try_for_each`, rayon "will attempt to stop processing the rest of the items... If there are multiple errors in parallel, it is not specified which will be returned" ([`iter/mod.rs` L445–L452](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/mod.rs#L445-L452)).

**Runtime requirement.** A shared flag per cancellable unit, polled at check points; parent→child links for propagation; a way to skip unstarted work. Skipping unstarted work needs scheduler support (TBB, OpenMP). Without it, each unstarted job starts and returns at its first check.

**rayon fit: yes, minus skipping.** A queued rayon job cannot be dropped, but it can check a flag on entry, which costs one atomic load per job **(analysis)**.

**Generic for inflight: yes, and already there.** The call latch is a per-call context; `nest` shares the call's stop (a bound child); `under` is a part-local stop that does not end the call. TBB's isolated context runs the other way: it shields a child from the parent's cancel. inflight has no equivalent; the call's stop reaches every part, `under` included ([`src/observe.rs` L345–L370](https://github.com/aai-institute/inflight/blob/9bd9c88/src/observe.rs#L345-L370)). Where inflight departs from TBB on purpose: TBB keeps the *first in time* exception. #5 asks for the failure of the *lowest-index* element, which is the deterministic choice (§8).

## 6. Monitors and convergence tests: PETSc, Trilinos Belos, Ceres, SciPy, scikit-learn, argmin

**PETSc** keeps observing and deciding in separate hooks:
- Monitors: `KSPMonitorSet(ksp, monitor, ctx, destroy)`; several can be set, and "they will be called in the order in which they were set" ([KSPMonitorSet](https://petsc.org/release/manualpages/KSP/KSPMonitorSet/)). Signature `(KSP, PetscInt it, PetscReal rnorm, PetscCtx)`, which returns no decision ([KSPMonitorFn](https://petsc.org/release/manualpages/KSP/KSPMonitorFn/)).
- Convergence test: `KSPSetConvergenceTest(ksp, converge, ctx, destroy)` replaces the default `KSPConvergedDefault`, which also "aborts if the residual grows to more than 10000 times the initial residual" ([KSPSetConvergenceTest](https://petsc.org/release/manualpages/KSP/KSPSetConvergenceTest/)). The test writes a `KSPConvergedReason *reason` ([KSPConvergenceTestFn](https://petsc.org/release/manualpages/KSP/KSPConvergenceTestFn/)).
- Reasons: positive = converged, negative = diverged, 0 = iterating. These include `KSP_CONVERGED_USER` ("the user has indicated convergence for an arbitrary reason") and `KSP_DIVERGED_USER` ([KSPConvergedReason](https://petsc.org/release/manualpages/KSP/KSPConvergedReason/)). Time stepping has `TS_CONVERGED_USER` "user requested termination" ([TSConvergedReason](https://petsc.org/release/manualpages/TS/TSConvergedReason/)), set by `TSSetConvergedReason`, which "Can only be called while TSSolve() is active" ([TSSetConvergedReason](https://petsc.org/release/manualpages/TS/TSSetConvergedReason/)).

**Trilinos Belos**:
- `StatusTest::checkStatus(Iteration* iSolver)` gets the whole iteration object ([`BelosStatusTest.hpp` L74](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosStatusTest.hpp#L74)). It returns `Passed` ("the iteration needs to stop"), `Failed` or `Undefined` ([`BelosTypes.hpp` L220–L223](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosTypes.hpp#L220-L223)). The iteration loops `while (stest_->checkStatus(this) != Passed)` ([`BelosPseudoBlockCGIter.hpp` L441](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosPseudoBlockCGIter.hpp#L441)).
- `StatusTestCombo` combines tests with `AND`, `OR` or `SEQ`. SEQ runs subtests in order and stops at the first that is not converged, "to allow the addition of expensive but more rigorous convergence tests", e.g. an explicit residual only once the implicit one is small ([`BelosStatusTestCombo.hpp` L30–L55](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosStatusTestCombo.hpp#L30-L55)).
- For several right-hand sides, `StatusTestGenResNorm` takes a `quorum`: how many residuals must pass, -1 meaning all ([`BelosStatusTestGenResNorm.hpp` L85, L136–L137](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosStatusTestGenResNorm.hpp#L85)).
- Output is a status test too: `StatusTestOutput` wraps a test and prints every `mod` calls in chosen states ([`BelosStatusTestOutput.hpp` L52–L66](https://github.com/trilinos/Trilinos/blob/578a854/packages/belos/src/BelosStatusTestOutput.hpp#L52-L66)).

**Ceres.** An `IterationCallback` runs after each minimizer step and returns `SOLVER_CONTINUE`, `SOLVER_ABORT` (termination type `USER_ABORT`; parameters are not updated unless `update_state_every_iteration`) or `SOLVER_TERMINATE_SUCCESSFULLY` (`USER_SUCCESS`) ([`iteration_callback.h` L138–L157](https://github.com/ceres-solver/ceres-solver/blob/c39d3cd/include/ceres/iteration_callback.h#L138-L157)). Logging is implemented as one of these callbacks (same file).

**SciPy.** `minimize(callback=...)`: "A callable called after each iteration"; with the signature `callback(intermediate_result: OptimizeResult)`, the methods "terminate if the callback raises `StopIteration`" ([scipy.optimize.minimize](https://docs.scipy.org/doc/scipy/reference/generated/scipy.optimize.minimize.html)). The sparse Krylov solvers only observe: `cg(callback=...)` is "called as `callback(xk)`", and `info > 0` is "number of iterations" without convergence ([scipy.sparse.linalg.cg](https://docs.scipy.org/doc/scipy/reference/generated/scipy.sparse.linalg.cg.html)).

**scikit-learn (1.9, experimental).** Fitting is a tree of tasks: "tasks are divided into subtasks... giving them a natural tree structure where fitting the estimator is the root task". Auto-propagated callbacks have a `max_propagation_depth`, where 0 means the callback is not propagated to sub-estimators ([Callbacks guide](https://scikit-learn.org/stable/callbacks.html)). Hooks are `setup`, `on_fit_task_begin`, `on_fit_task_end` and `teardown`. "`call_on_fit_task_end`... returns a boolean... True if any of the callbacks' `on_fit_task_end` methods return True. This enables the interruption of the `fit` process". For joblib parallelism, sub-contexts "must be all created before the parallelization" ([third-party estimator example](https://scikit-learn.org/1.9/auto_examples/callbacks/plot_callback_support.html)).

**argmin (Rust).** Observers have `observe_init`, `observe_iter` and `observe_final`, each returning `Result` ([`observers/mod.rs` L154–L173](https://github.com/argmin-rs/argmin/blob/b300285/crates/argmin/src/core/observers/mod.rs#L154-L173)), with `ObserverMode` `Always`, `Never`, `NewBest` or `Every(i)` ([docs](https://docs.rs/argmin/latest/argmin/core/observers/index.html)). The executor installs a `ctrlc` handler that sets an `AtomicBool`, checks it each iteration and terminates with `TerminationReason::Interrupt` ([`executor.rs` L148–L193, L274–L276](https://github.com/argmin-rs/argmin/blob/b300285/crates/argmin/src/core/executor.rs#L148-L193); [`termination.rs` L54–L67](https://github.com/argmin-rs/argmin/blob/b300285/crates/argmin/src/core/termination.rs#L54-L67)).

**Kokkos profiling regions.** `ScopedRegion` "is a RAII wrapper that 'pushes' a user-defined profiling region... and properly 'pops' that region upon destruction" ([ScopedRegion](https://kokkos.org/kokkos-core-wiki/API/core/profiling/scoped_region.html)): scopes for tools, with no stop.

**Problem solved.** Callers watch and steer an iterative method without changing it: logging, progress, user stop rules and custom convergence criteria.

**Runtime requirement.** None from the scheduler; a callback slot in the algorithm. Under threads the callback must be thread-safe (stated for MKL in §7; implied in sklearn by the "created before the parallelization" rule).

**rayon fit: yes.** It is independent of the scheduler. inflight's observers are `Sync` and reach rayon workers as a parameter ([README](https://github.com/aai-institute/inflight/blob/9bd9c88/README.md#design)).

**Generic for inflight (analysis):**
- *Monitor list, observe-only:* yes, already there (`event`, `progress`, pairs `(a, b)`).
- *A user rule that stops:* yes, already there (`Stop::Rule(name)`). PETSc and Ceres separate a user stop that means *converged* (`KSP_CONVERGED_USER`, `USER_SUCCESS`) from one that means *abort* (`KSP_DIVERGED_USER`, `USER_ABORT`). inflight returns `Outcome::Stopped` with a residue for both, and the caller matches on the rule's `STOP` constant to tell them apart. That covers both meanings without inflight assigning one.
- *Task tree with a propagation depth (sklearn) and RAII regions (Kokkos):* yes, already there (scopes; `Opened::Skip` cuts a subtree).
- *Pluggable convergence test, reason codes, AND/SEQ combinators, quorum:* no. Each needs solver state (`Iteration*`, residual vectors), and the reason taxonomy is per method. inflight's rule is "observing means choosing, not changing" and observers see only reported events. A solver that wants a quorum over right-hand sides computes it and reports it as an event; an observer may then stop on it. OR over stop rules is what inflight's pair already does.

## 7. Progress and stop in factorizations and direct solvers: MKL, CHOLMOD, Eigen, HYPRE

**MKL `mkl_progress`.** `int mkl_progress(int* thread_process, int* step, char* stage, int lstage)`. `step` is a "linear progress indicator... Increases from 0 to the linear size of the problem"; "A non-zero return value may be supplied by the redefined function to break the computation"; "The user-defined mkl_progress function must be thread-safe"; LAPACK returns `info = -1002` when interrupted; ScaLAPACK ignores the return value. Called by "Some... functions from LAPACK, ScaLAPACK, DSS/PARDISO" ([oneMKL C reference](https://www.intel.com/content/www/us/en/docs/onemkl/developer-reference-c/2025-2/mkl-progress.html)). One callback both reports progress and decides the stop, at a coarse grain chosen by the library.

**CHOLMOD.** The `cholmod_common` object has `error_handler(status, file, line, message)` and print settings. A text search of `cholmod.h` finds no progress or cancel hook ([`cholmod.h` L606–L620](https://github.com/DrTimothyAldenDavis/SuiteSparse/blob/39d0c10/CHOLMOD/Include/cholmod.h#L606-L620)).

**Eigen iterative solvers.** Control is `setTolerance`, `setMaxIterations`, and after the solve `info()`, `iterations()` and `error()`. There is no callback ([`IterativeSolverBase.h` L292, L312, L368](https://gitlab.com/libeigen/eigen/-/blob/master/Eigen/src/IterativeLinearSolvers/IterativeSolverBase.h#L292)).

**HYPRE Krylov.** Tolerances, max iterations and print/logging levels. A text search of `HYPRE_krylov.h` for "callback" and "monitor" finds nothing ([`HYPRE_krylov.h`](https://github.com/hypre-space/hypre/blob/1da70df/src/krylov/HYPRE_krylov.h)).

**Problem solved.** A long factorization can be interrupted without a separate thread and a kill.

**Runtime requirement.** None beyond a thread-safe callback.

**rayon fit: yes.**

**Generic for inflight: yes, already there.** MKL's design matches inflight's guidance to report and check between chunks of a hot loop ([`guides/instrumenting.md` §7](https://github.com/aai-institute/inflight/blob/9bd9c88/guides/instrumenting.md#7-choose-your-safe-points)). MKL's `-1002` is inflight's `Outcome::Stopped`. That CHOLMOD, Eigen and HYPRE expose no stop is the gap inflight exists to fill.

## 8. Reductions that do not depend on the thread count

**oneTBB `parallel_deterministic_reduce`** "executes the same set of split and join operations no matter how many threads participate". It requires `simple_partitioner` or `static_partitioner`, and the result "might differ from that obtained with an equivalent sequential (linear) algorithm" ([spec](https://uxlfoundation.github.io/oneAPI-spec/spec/elements/oneTBB/source/algorithms/functions/parallel_deterministic_reduce_func.html)).

**OpenMP.** "The location in the OpenMP program at which values are combined and the order in which values are combined are unspecified"; "there is no guarantee that bitwise-identical results will be obtained" ([OpenMP 5.0 §2.19.5.2](https://www.openmp.org/spec-html/5.0/openmpsu107.html)).

**Kokkos.** `parallel_reduce`: "Neither concurrency nor order of execution are guaranteed" ([parallel_reduce](https://kokkos.org/kokkos-core-wiki/API/core/parallel-dispatch/parallel_reduce.html)).

**MKL CNR.** Reproducible only when "The number of computational threads used by the library does not change in the run" ([Obtaining Numerically Reproducible Results](https://www.intel.com/content/www/us/en/docs/onemkl/developer-guide-linux/2025-2/obtaining-numerically-reproducible-results.html)). Strict CNR gives bitwise results "even when the number of threads changes", but only for `?gemm`, `?symm`, `?hemm` and `?trsm`, and "can reduce performance". "with the Intel® Threading Building Blocks parallelization, numerical reproducibility is not guaranteed" ([Reproducibility Conditions](https://www.intel.com/content/www/us/en/docs/onemkl/developer-guide-linux/2025-2/reproducibility-conditions.html)).

**rayon.**
- `reduce`: "the order in which `op` will be applied to reduce the result is not fully specified" ([`iter/mod.rs` L975–L981](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/mod.rs#L975-L981)).
- `fold`: "The number of results is nondeterministic, as is the point where the breaks occur" ([L1131](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/mod.rs#L1131)).
- The cause: the splitter starts with `splits: current_num_threads()` and resets on steal ([`plumbing/mod.rs` L260–L283](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/plumbing/mod.rs#L260-L283)).
- Deterministic tools that exist: `fold_chunks(n, ..)`, whose boundaries are fixed by `n` ([L2690–L2713](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/mod.rs#L2690-L2713)); ordered `collect` into a `Vec`; and `find_first`, which returns "the sequentially **first**" match, where "attempts to the right of the match will be stopped, while attempts to the left must continue" ([L1666–L1692](https://github.com/rayon-rs/rayon/blob/v1.12.0/src/iter/mod.rs#L1666-L1692)).

**Problem solved.** Results that are bitwise-identical across thread counts and runs.

**Runtime requirement.** A partition independent of the thread count, plus a fixed combine tree. No scheduler support beyond that.

**rayon fit: yes, with discipline (analysis).** Fixed-size chunks (`fold_chunks`/`par_chunks` with a constant size), results collected in order, then combined sequentially or by a fixed tree. Any use of `reduce`, `sum` or `fold` on floats is thread-count dependent. The same holds for errors: `try_for_each` picks an unspecified error, while collecting per-element results and scanning from the left (or `find_map_first`) gives the lowest index.

**Generic for inflight: partly (analysis).** Floating-point reductions are the math library's business and need no inflight API. The ordering obligations of a parallel map are generic and belong to #5's map: result order by index, the reported failure is the lowest-index one, and parked elements are resumed in index order within a round. inflight's `advance` already keeps progress reports in order across threads ([`src/observe.rs` L379](https://github.com/aai-institute/inflight/blob/9bd9c88/src/observe.rs#L379)). The latch keeps the first stop *in time*. That is fine because stops come from outside and carry no data.

## 9. Stop tokens and Rust crates

**C++20 `std::stop_token`** is "a thread-safe 'view' of the associated stop-state". It is polled with `stop_requested()`, and `stop_callback` runs a callback when a stop is requested, which can interrupt `condition_variable_any` waits ([cppreference](https://en.cppreference.com/w/cpp/thread/stop_token)).

**`enough` (0.4.x).**
- `pub trait Stop: Send + Sync { fn check(&self) -> Result<(), StopReason>; fn should_stop(&self) -> bool; fn may_stop(&self) -> bool }`, with `Unstoppable` (`may_stop() == false`) and blanket impls for `&T`, `&mut T`, `Box<T>`, `Arc<T>` and `Option<T>`. The docs advise checking "every 16-1000 iterations" ([`lib.rs`](https://docs.rs/enough/latest/src/enough/lib.rs.html)).
- `StopReason` is `#[non_exhaustive]` with `Cancelled` and `TimedOut`. `AsStopReason::as_stop_reason(&self) -> Option<StopReason>` lets an error say whether it is a stop ([`reason.rs`](https://docs.rs/enough/latest/src/enough/reason.rs.html)).
- Concrete stoppers live in `almost-enough`; `enough-tokio` bridges tokio's `CancellationToken` ([lib.rs](https://lib.rs/crates/enough)).

**Interop with inflight (analysis).** The two are close relatives: `check() -> Result<(), StopReason>` is `Observer::check() -> ControlFlow<Stop>`, and `AsStopReason` is `Stopping`. Two adapters are enough:
- *enough → inflight:* any `T: enough::Stop` as a checks-only `Observer<S>` mapping `Cancelled → Stop::Requested` and `TimedOut → Stop::Deadline`, like inflight's `Flag`/`Deadline`.
- *inflight → enough:* a `&Scope` as `enough::Stop`, so a third-party inner library that takes `impl Stop` can be called at an inflight safe point. `Signal`, `Requested` and `Rule` map to `Cancelled` and `Deadline` maps to `TimedOut`. The mapping loses detail, but the inner call's stop latches the outer call with the full `Stop`, so the caller still sees the precise reason. `may_stop` maps to "the observer is `()`".

**rayon.**
- No futures, no isolation. Waiting helps with any job (§2). `yield_now`/`yield_local` exist. `try_*` iterators short-circuit with an unspecified error (§5).

**chili.**
- A port of Spice: "Very low-overhead parallelization primitive, almost identical to `rayon::join`" ([README](https://github.com/dragostis/chili/blob/main/README.md)). Spice uses "heartbeat scheduling" ([Spice README](https://github.com/judofyr/spice)).
- The API is `Scope::join` and `ThreadPool`, with no parallel iterators, spawn or isolation ([docs](https://docs.rs/chili/latest/chili/)).
- It does not help with #5 **(analysis)**: the waiting problem is the same and the primitives are fewer.

**forte (1.0.0-beta.2).**
- Pools are `const`-constructed "intended to be defined as `static`"; "Up to 32 threads can participate in a pool at a time"; spawn of closures and futures is supported ([README](https://github.com/NthTensor/Forte/blob/main/README.md)).
- Already rejected in #5 for these reasons.

**faer.**
- `pub enum Par { Seq, Rayon(NonZeroUsize) }` makes parallelism an explicit argument of each call ([`faer/src/lib.rs` L929–L936, faer 0.24.4](https://docs.rs/faer/0.24.4/src/faer/lib.rs.html#929)).
- This is the pattern behind per-call thread budgets. It is generic, but it is the math library's parameter, not inflight's.

**argmin.** See §6.

## Summary table

| Pattern | Problem | Runtime requirement | rayon fit | Generic enough for inflight |
|---|---|---|---|---|
| oneTBB `collaborative_call_once` (§1) | Build a shared value once, in parallel, while concurrent callers wait without idling or deadlocking; retry after a failed build | Waiters join the build's arena; isolation keyed per build; a `wait_context` waiters execute tasks on | No: no arena join, no isolation | Goal yes (it is #5's contract); mechanism no (needs TBB internals) |
| Task isolation, `this_task_arena::isolate` (§2) | A waiting thread steals outer work and re-enters a lock or changes thread-local state | Isolation tags on tasks, filtered when a thread waits | No: rayon's wait runs any job ([rayon#592](https://github.com/rayon-rs/rayon/issues/592) open) | No: a scheduler feature; inflight has no scheduler. Design rule: no pool job waits while holding anything |
| Resumable tasks, `task::suspend`/`resume` (§3) | A task waits for an event without holding its thread | Stackful coroutines (`ucontext` or a thread per coroutine); platform-limited | No: needs `unsafe` stack switching | No: violates `forbid(unsafe_code)`; parking does the same job |
| Park and resume stepped elements (§4, analysis) | Elements wait on lazily built values without blocking a worker, on the caller's pool | `par_iter`/`join` on one pool; `yield_now` for cross-call waits | Yes: public, safe API only | Yes: #5's design. Open: wait vs duplicate build across calls; keys must be acyclic |
| Hierarchical cancel context (TBB `task_group_context`, OpenMP `cancel`) (§5) | Stop a whole parallel computation, including nested work, at check points | Shared flag polled at check points; parent→child propagation; skipping unstarted tasks | Yes, minus skipping: a queued job starts and returns at its first check | Yes, already there: call latch, `nest`, `under` |
| First failure wins, cancel the rest (TBB exceptions, rayon `try_*`) (§5) | One failure ends a parallel algorithm promptly | Capture one error; cancel siblings | Yes: `try_for_each` short-circuits; the chosen error is unspecified | Yes for stops (latch exists). For element errors, use the lowest index instead (§8) |
| Observe-only monitor list (PETSc `KSPMonitorSet`, argmin observers, Belos `StatusTestOutput`) (§6) | Watch an iterative method without changing it | Thread-safe callback slot | Yes | Yes, already there: `event`, `progress`, pairs |
| User stop callback with a reason (Ceres `IterationCallback`, SciPy `StopIteration`, sklearn `on_fit_task_end`, PETSc `*_USER` reasons) (§6) | Caller-defined stop rules, early stopping | Per-iteration callback whose return value can end the loop | Yes | Yes, already there: `check` → `Stop::Rule(name)`; converged-vs-abort meaning is left to the caller |
| Pluggable convergence test, reason codes, AND/OR/SEQ combinators, quorum (PETSc, Belos) (§6) | Custom and staged convergence criteria; multi-RHS "k of n converged" | Test gets solver state (`Iteration*`, residuals) | Yes | No: needs solver state; per-method taxonomy. OR over stop rules exists as pairs |
| Task tree with propagation depth (sklearn), RAII regions (Kokkos `ScopedRegion`) (§6) | Attribute callbacks to nested work; limit how deep they reach | Context passed down; sub-contexts created before parallel dispatch | Yes | Yes, already there: scopes, `Opened::Skip` |
| Progress + stop in one coarse callback (MKL `mkl_progress`) (§7) | Interrupt a long factorization; report linear progress | Thread-safe callback at chunk boundaries | Yes | Yes, already there: chunked `progress` + `check` |
| Pool-wide cancel (Eigen `ThreadPool::Cancel`) (§5) | Drop queued closures, typically at shutdown | Owning the pool | n/a (rayon has none) | No: inflight stops calls, never the caller's pool |
| Thread-count-independent reduction (TBB `parallel_deterministic_reduce`, MKL strict CNR) (§8) | Bitwise-identical results across thread counts | Partition independent of threads; fixed combine tree | Yes, with fixed chunks (`fold_chunks`) and an ordered combine; `reduce`/`sum` are not | No for float reductions (math library). Yes for the map's ordering obligations: result order, lowest-index failure, progress order |
| Deterministic error choice (rayon `find_first` vs `try_for_each`) (§8) | Same reported failure regardless of scheduling | Leftmost-wins search, or ordered collect | Yes | Yes: #5 requires lowest-index failure |
| Polled stop token (C++ `stop_token`, `enough::Stop`) (§9) | A library accepts "stop me" without knowing the source | An atomic flag; optionally a callback to wake blocked waits | Yes | Yes: interoperate via two adapters; lossy (4 reasons vs 2) |
| Explicit parallelism argument (faer `Par`) (§9) | Per-call thread budget instead of a global setting | Pool or split-count parameter | Yes | No: the math library's parameter; #5's map just runs on the pool it is called on |
| Heartbeat scheduling (chili/Spice), static async pools (forte) (§9) | Lower fork-join overhead; async on a compute pool | Own scheduler | Replaces rayon | No: neither solves waiting on shared values; forte already rejected |
