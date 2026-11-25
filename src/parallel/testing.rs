//! Checks that a result does not depend on the pool. Behind the `testing` feature.

/// Runs `f` in a fresh pool of `threads`.
///
/// # Panics
/// If the pool fails to build.
pub fn in_pool<R: Send>(threads: usize, f: impl FnOnce() -> R + Send) -> R {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("pool")
        .install(f)
}

/// Asserts `f` returns the same value in pools of 1, 2, 3 and 8 threads.
///
/// # Panics
/// If a pool's value differs from one thread's, or a pool fails to build.
pub fn assert_pool_independent<R: PartialEq + Send>(f: impl Fn() -> R + Sync) {
    let reference = in_pool(1, &f);
    for threads in [2, 3, 8] {
        // Not `assert_eq!`: a printed solution vector would bury the thread count.
        assert!(in_pool(threads, &f) == reference, "{threads} threads");
    }
}
