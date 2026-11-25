//! Work on a pool of its own, entered from outside every pool.

use std::io;

/// Runs `f` on a fresh pool of the current pool's size, entered from a thread outside every pool,
/// and waits for it: a rayon worker that waits on a pool steals other jobs meanwhile, and a stolen
/// job that waits on `f` deadlocks. Nothing `f` runs may wait on the caller's pool. A panic in `f`
/// resumes on the caller; the error is the pool's or the bridging thread's failure to start.
pub fn isolated<R: Send>(f: impl FnOnce() -> R + Send) -> io::Result<R> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon::current_num_threads())
        .build()
        .map_err(io::Error::other)?;
    std::thread::scope(|s| {
        let bridge = std::thread::Builder::new().spawn_scoped(s, || pool.install(f))?;
        match bridge.join() {
            Ok(r) => Ok(r),
            // Carry `f`'s own panic, not `Any { .. }` from formatting the payload.
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::isolated;

    thread_local! {
        static ON_CALLER_POOL: Cell<bool> = const { Cell::new(false) };
    }

    /// Work handed to [`isolated`] runs off the caller's pool, so it can steal none of its jobs.
    #[test]
    fn isolated_leaves_the_callers_pool() {
        let caller = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .start_handler(|_| ON_CALLER_POOL.with(|f| f.set(true)))
            .build()
            .expect("caller pool");
        assert!(
            caller.install(|| ON_CALLER_POOL.with(Cell::get)),
            "the marker never reached the caller pool's workers"
        );
        assert!(
            !caller
                .install(|| isolated(|| ON_CALLER_POOL.with(Cell::get)))
                .expect("isolated"),
            "the work ran on the caller's pool"
        );
    }

    /// A worker waiting on the work must not steal: entering the new pool from the worker itself
    /// is not enough.
    #[test]
    fn a_worker_waiting_on_it_runs_nothing_else() {
        let caller = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("caller pool");
        let ran = Arc::new(AtomicBool::new(false));
        let queued = Arc::clone(&ran);
        caller.install(move || {
            rayon::spawn(move || queued.store(true, Ordering::SeqCst));
            isolated(|| std::thread::sleep(Duration::from_millis(50))).expect("isolated");
            assert!(
                !ran.load(Ordering::SeqCst),
                "the waiting worker stole a queued job meanwhile"
            );
        });
    }
}
