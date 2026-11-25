//! Counts the threads inside a test's closure at once: unit tests check that a computation keeps
//! its budget.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

/// One [`peak`]'s count: the threads inside a task now, and the most at once.
#[derive(Default)]
struct Counters {
    active: AtomicUsize,
    peak: AtomicUsize,
}

thread_local! {
    /// The counters of the [`peak`] whose pool this thread belongs to, if any.
    static COUNTERS: RefCell<Option<Arc<Counters>>> = const { RefCell::new(None) };
    /// Tasks this thread is inside: a thread waiting on its own loop runs other items.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// `run`, on a [`peak`]'s pool counting the thread inside it.
pub(super) fn counted<R>(run: impl FnOnce() -> R) -> R {
    let Some(counters) = COUNTERS.with(|c| c.borrow().clone()) else {
        return run();
    };
    let entered = DEPTH.with(|depth| depth.replace(depth.get() + 1)) == 0;
    if entered {
        let active = counters.active.fetch_add(1, SeqCst) + 1;
        counters.peak.fetch_max(active, SeqCst);
    }
    let _left = Left { counters, entered };
    run()
}

/// Leaves a counted task, also when it unwinds.
struct Left {
    counters: Arc<Counters>,
    entered: bool,
}

impl Drop for Left {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get() - 1));
        if self.entered {
            self.counters.active.fetch_sub(1, SeqCst);
        }
    }
}

/// The most threads inside a [`counted`] at once while `run` runs on a fresh pool of `threads`;
/// measurements on other pools do not mix with it.
///
/// # Panics
/// If the pool fails to build.
pub(super) fn peak(threads: usize, run: impl FnOnce() + Send) -> usize {
    let counters = Arc::new(Counters::default());
    let mine = Arc::clone(&counters);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .start_handler(move |_| COUNTERS.with(|c| *c.borrow_mut() = Some(Arc::clone(&mine))))
        .build()
        .expect("pool")
        .install(run);
    counters.peak.load(SeqCst)
}
