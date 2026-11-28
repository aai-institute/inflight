//! Checks that a result does not depend on the pool. Behind the `testing` feature.

/// Thread counts that run `f` once each, after the one-thread reference.
const COUNTS: [usize; 2] = [2, 3];

/// The fewest threads the last pool has, on any machine.
const LAST_AT_LEAST: usize = 8;

/// How often the last pool runs `f`: a rerun may catch a result that depends on the order in which threads finish.
const RUNS: usize = 2;

/// Runs `f` in a fresh pool of `threads`.
///
/// # Panics
/// If the pool fails to build.
pub fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    pool(threads).install(f)
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("pool")
}

/// The machine's cores; 1 when they are unknown, so the last pool has [`LAST_AT_LEAST`] threads.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// The last pool's threads on a machine of `cores`.
fn last_threads(cores: usize) -> usize {
    (cores + 1).max(LAST_AT_LEAST)
}

/// Asserts `f` returns the same value in pools of 2 and 3 threads as in one thread, and in two runs of one pool
/// with more threads than cores and at least 8; the rerun may catch a result that depends on the order in which
/// threads finish.
///
/// Return bit patterns, such as floats through `f64::to_bits`: `R: Eq` rules out bare floats, where
/// `0.0 == -0.0` and a NaN never equals itself. It does not certify bits by itself: a float wrapper
/// with its own `Eq` would pass.
///
/// ```
/// # use inflight::parallel::testing::assert_pool_independent;
/// assert_pool_independent(|| [0.1_f64, -0.0].map(f64::to_bits));
/// ```
///
/// ```compile_fail,E0277
/// # use inflight::parallel::testing::assert_pool_independent;
/// assert_pool_independent(|| -0.0_f64);
/// ```
///
/// Returns the one-thread value, so a caller checks it further without another run. It stands for the other
/// pools' values as far as `Eq` means identical, as it does for bit patterns.
///
/// # Panics
/// If a run's value differs from one thread's, or a pool fails to build.
pub fn assert_pool_independent<R: Eq + Send>(f: impl Fn() -> R + Sync) -> R {
    let reference = in_pool(1, &f);
    for threads in COUNTS {
        // Not `assert_eq!`: a printed solution vector would bury the thread count.
        assert!(in_pool(threads, &f) == reference, "{threads} threads");
    }
    let last = pool(last_threads(cores()));
    for run in 1..=RUNS {
        let threads = last.current_num_threads();
        assert!(
            last.install(&f) == reference,
            "{threads} threads, run {run}"
        );
    }
    reference
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn the_last_pool_has_more_threads_than_cores_and_at_least_the_floor() {
        for cores in [1, 2, LAST_AT_LEAST - 1, LAST_AT_LEAST, 12, 64] {
            let threads = last_threads(cores);
            assert!(threads > cores && threads >= LAST_AT_LEAST, "{cores} cores");
        }
    }

    #[test]
    fn runs_the_last_pool() {
        let counts = Mutex::new(Vec::new());
        assert_pool_independent(|| counts.lock().unwrap().push(rayon::current_num_threads()));
        let most = counts.into_inner().unwrap().into_iter().max().unwrap();
        assert_eq!(most, last_threads(cores()));
    }

    #[test]
    #[should_panic(expected = "threads, run 2")]
    fn catches_a_value_that_changes_on_a_rerun_of_the_last_pool() {
        // Stands in for a result that depends on the order in which threads finish.
        let calls = Mutex::new(0);
        assert_pool_independent(|| {
            let mut calls = calls.lock().unwrap();
            *calls += 1;
            *calls > COUNTS.len() + 2
        });
    }
}
