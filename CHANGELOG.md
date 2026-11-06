# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First release.

### Added

- `Observer`, which hears what a computation reports and answers its checks with `Continue` or `Break(Stop)`; pairs combine observers.
- `run`, `try_run` and `run_fallible`, which run a computation as one call whose first stop holds for the rest of the call.
- Scopes: `within`, `within_as`, `under` and `nest` open typed parts that report progress and events and end as completed, stopped, failed or unwound; `Observer::open` picks what hears each part.
- `Outcome::Completed` and `Outcome::Stopped { reason, residue }`, with `Outcome::holding` for state a stopped computation can resume from.
- `Stop` causes `Signal`, `Deadline`, `Requested` and `Rule(name)`, and the `Flag` and `Deadline` observers.
- A debug build panics when a body swallows a `check()` stop and returns `Ok`.
- `pyo3` feature: `interruptible` and `interruptible_with` stop a computation on Ctrl-C while it runs with the GIL released; `Limits` turns a `timeout` into a stop; `ProgressCallback` hands progress to a Python `progress(done, total)`; `Stop` converts to `PyErr`.
