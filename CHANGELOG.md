# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Threads::map_chunks` maps each chunk of an input to one result and returns the results in input order: under a count, one chunk a thread, as `for_each_chunk` cuts; under `All`, a few a thread, none shorter than a grain.

## [0.3.0] - 2026-11-25

### Removed

- `parallel::instrument_task` and `parallel::testing::peak`: the thread count inside tasks is now a check of inflight's own tests. The `testing` feature no longer instruments every task of a loop, which cost its users' benchmarks; it keeps `assert_pool_independent` and `in_pool`.

## [0.2.0] - 2026-11-25

### Added

- `rayon` feature: `parallel`, parallel work whose results do not depend on the thread count, moved from schwarz-precond, approx-chol and within.
  - `Threads` is a thread budget, and each loop that splits work is a method on it. `for_each_chunk` runs a closure over chunks of any `Split` input, such as zipped slices. `for_each_item` and `try_for_each_item` hand each item to a closure, at most the budget's threads at once. `schedule` maps items in tasks of at least a grain, each item under the budget its task leaves it.
  - `fold_tree` sums an input's items in a binary tree fixed by their count. `Blocks` cuts an input into `fold_leaves` blocks of a length, each told where it starts; zipped with state, such as `Accumulators` whose first is the output, each leaf owns its own.
  - `Split` is what the loops cut, and its `Item` what a loop over items hands; `Pieces`, `Chunks` and `Blocks` cut buffers alongside. `par_split` cuts an input adaptively on rayon's whole pool.
  - `Workspaces` and `Regions` lay out stack room for tasks that run at once.
  - `Disjoint` lets tasks write one slice at once. `Partition::new` checks a partition's sets disjoint once, or says why not in a `PartitionError`; `Disjoint::claims` hands each set's `Claim` out once, and `Partition::claim` one set's claim of a slice held alone.
  - `isolated` runs work on a pool of its own, entered from outside every pool, so a worker waiting on it steals nothing. `instrument_task` marks a task of a loop that splits work on rayon itself.
- `testing` feature: `parallel::testing::peak` counts the threads inside tasks at once on a pool of its own, and `assert_pool_independent` checks that a result does not depend on the pool.

### Changed

- `unsafe_code` is denied rather than forbidden: `parallel::disjoint` alone allows it.
- docs.rs documents every feature.
- `Deadline::at` and `Outcome::holding` are `const`; pure getters and constructors are `#[must_use]`; fallible and panicking functions document when in `# Errors` and `# Panics`.

## [0.1.1] - 2026-11-23

No changes to the crate. Released to check publishing from CI through crates.io trusted publishing.

## [0.1.0] - 2026-11-23

First release.

### Added

- `Observer`, which hears what a computation reports and answers its checks with `Continue` or `Break(Stop)`; pairs combine observers.
- `run`, `try_run` and `run_fallible`, which run a computation as one call whose first stop holds for the rest of the call.
- Scopes: `within`, `within_as`, `under` and `nest` open typed parts that report progress and events and end as completed, stopped, failed or unwound; `Observer::open` picks what hears each part.
- `Outcome::Completed` and `Outcome::Stopped { reason, residue }`, with `Outcome::holding` for state a stopped computation can resume from.
- `Stop` causes `Signal`, `Deadline`, `Requested` and `Rule(name)`, and the `Flag` and `Deadline` observers.
- A debug build panics when a body swallows a `check()` stop and returns `Ok`.
- `pyo3` feature: `interruptible` and `interruptible_with` stop a computation on Ctrl-C while it runs with the GIL released; `Limits` turns a `timeout` into a stop; `ProgressCallback` hands progress to a Python `progress(done, total)`; `Stop` converts to `PyErr`.

[Unreleased]: https://github.com/aai-institute/inflight/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/aai-institute/inflight/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aai-institute/inflight/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/aai-institute/inflight/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/aai-institute/inflight/releases/tag/v0.1.0
