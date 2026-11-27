//! How many threads a computation may use, and how a loop splits under that budget.

use std::num::NonZeroUsize;

use rayon::prelude::*;

/// The threads a computation may use; no result's bits depend on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Threads {
    /// Every thread of the caller's rayon pool.
    All,
    /// At most `n` threads at a time, whatever the pool's size; `Count(1)` runs on the calling
    /// thread.
    Count(NonZeroUsize),
}

impl Threads {
    /// The calling thread alone.
    pub const ONE: Self = Self::Count(NonZeroUsize::MIN);

    /// The most threads it runs at a time: no more than the current pool has. The calling thread
    /// alone asks no pool, as outside one rayon would build its global pool.
    #[must_use]
    pub fn tasks(self) -> usize {
        match self {
            Self::ONE => 1,
            Self::All => rayon::current_num_threads(),
            Self::Count(n) => n.get().min(rayon::current_num_threads()),
        }
    }

    /// Items per task, at least `grain`, in at most [`tasks`](Self::tasks) tasks under a count;
    /// `None` when one task takes all `len` or one thread runs, which leaves them on the calling
    /// thread.
    #[must_use]
    pub fn per_task(self, len: usize, grain: usize) -> Option<usize> {
        let tasks = self.tasks();
        let per = match self {
            Self::All => grain.max(1),
            Self::Count(_) => bounded_per(len, grain, tasks),
        };
        (per < len && tasks > 1).then_some(per)
    }

    /// The budget of work nested in each of `tasks` tasks of a split loop: an equal share of a
    /// count, at least one.
    pub(crate) fn within_task(self, tasks: usize) -> Self {
        match self {
            Self::All => Self::All,
            Self::Count(n) => {
                NonZeroUsize::new(n.get() / tasks.max(1)).map_or(Self::ONE, Self::Count)
            }
        }
    }

    /// The budgets of a split's two tasks: a count's halves, the larger second, so an odd count
    /// keeps every thread. A budget of one halves into two of one: fork only when
    /// [`tasks`](Self::tasks) is above one.
    pub(crate) fn halves(self) -> (Self, Self) {
        let larger = match self {
            Self::All => Self::All,
            Self::Count(n) => NonZeroUsize::new(n.get().div_ceil(2)).map_or(Self::ONE, Self::Count),
        };
        (self.within_task(2), larger)
    }

    /// How a loop over items splits under this budget, at least `grain` items a task.
    #[must_use]
    pub const fn schedule(self, grain: usize) -> Schedule {
        Schedule {
            threads: self,
            grain,
        }
    }
}

/// Items per piece when `len` items split into at most `tasks` pieces of at least `grain`, and at
/// least one.
pub(crate) fn bounded_per(len: usize, grain: usize, tasks: usize) -> usize {
    grain.max(len.div_ceil(tasks)).max(1)
}

/// How a loop over items splits under a budget: into tasks of at least a grain of items, each item
/// under the budget its task leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    threads: Threads,
    grain: usize,
}

impl Schedule {
    /// `f` of each item and its position, in item order, each under the budget its task leaves it.
    pub fn map<X: Send, R: Send, P>(
        &self,
        items: P,
        f: impl Fn(usize, X, Threads) -> R + Sync,
    ) -> Vec<R>
    where
        P: IntoParallelIterator<Item = X> + IntoIterator<Item = X>,
        <P as IntoParallelIterator>::Iter: IndexedParallelIterator,
    {
        self.map_init(|| (), items, |(), k, x, threads| f(k, x, threads))
    }

    /// As [`map`](Self::map), each task reusing one `init()` across its items.
    pub fn map_init<S, X: Send, R: Send, P>(
        &self,
        init: impl Fn() -> S + Sync + Send,
        items: P,
        f: impl Fn(&mut S, usize, X, Threads) -> R + Sync,
    ) -> Vec<R>
    where
        P: IntoParallelIterator<Item = X> + IntoIterator<Item = X>,
        <P as IntoParallelIterator>::Iter: IndexedParallelIterator,
    {
        if self.threads.tasks() <= 1 {
            let mut s = init();
            return (IntoIterator::into_iter(items).enumerate())
                .map(|(k, x)| f(&mut s, k, x, self.threads))
                .collect();
        }
        let items = IntoParallelIterator::into_par_iter(items);
        let count = items.len();
        // Unsplit, one task runs every item under the whole budget.
        let (per_task, within_task) = match self.threads.per_task(count, self.grain) {
            Some(per) => (per, self.threads.within_task(count.div_ceil(per))),
            None => (count.max(1), self.threads),
        };
        // Exactly sized: collect's reserve rounds a few items up to 4.
        let mut out = Vec::with_capacity(count);
        items
            .enumerate()
            .with_min_len(per_task)
            .map_init(&init, |s, (k, x)| f(s, k, x, within_task))
            .collect_into_vec(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }

    fn count(n: usize) -> Threads {
        Threads::Count(NonZeroUsize::new(n).unwrap())
    }

    #[test]
    fn all_splits_by_the_grain() {
        pool(4).install(|| {
            for len in [0, 1, 2, 7, 100, 4097] {
                for grain in [0, 1, 625] {
                    let per = grain.max(1);
                    let split = Threads::All.per_task(len, grain);
                    assert_eq!(split, (per < len).then_some(per));
                }
            }
        });
    }

    #[test]
    fn a_count_splits_into_at_most_its_tasks() {
        pool(9).install(|| {
            for n in 1..=9 {
                for len in [0, 1, 2, 7, 8, 9, 100, 4097, 100_003] {
                    for grain in [0, 1, 625] {
                        let Some(per) = count(n).per_task(len, grain) else {
                            assert!(
                                n == 1 || len <= grain.max(1),
                                "{len} rows unsplit under {n}"
                            );
                            continue;
                        };
                        let tasks = len.div_ceil(per);
                        assert!(
                            per >= grain && (2..=n).contains(&tasks),
                            "{len} in {tasks} under {n}"
                        );
                        if grain <= 1 && len >= n * n {
                            assert_eq!(tasks, n, "{len} rows under {n}");
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn a_count_splits_into_at_most_the_pools_threads() {
        pool(3).install(|| {
            assert_eq!(count(8).per_task(100, 1), Some(34));
        });
        pool(1).install(|| {
            for threads in [Threads::All, count(8)] {
                assert_eq!(threads.per_task(100, 1), None, "{threads:?}");
            }
        });
    }

    #[test]
    fn nested_work_shares_a_count() {
        assert_eq!(Threads::All.within_task(5), Threads::All);
        assert_eq!(count(7).within_task(2), count(3));
        assert_eq!(count(3).within_task(3), Threads::ONE);
        assert_eq!(count(2).within_task(3), Threads::ONE);
        assert_eq!(count(7).halves(), (count(3), count(4)));
        assert_eq!(count(1).halves(), (Threads::ONE, Threads::ONE));
        assert_eq!(count(4).within_task(0), count(4));
    }

    #[test]
    fn a_schedule_shares_the_budget_among_its_tasks() {
        let budgets = pool(8).install(|| count(4).schedule(1).map(0..2, |k, _, t| (k, t)));
        assert_eq!(budgets, [(0, count(2)), (1, count(2))]);
        let serial = pool(8).install(|| count(4).schedule(5).map(0..2, |_, _, t| t));
        assert_eq!(serial, [count(4), count(4)]);
    }

    #[test]
    fn chunks_cover_the_slice_once_with_their_starts() {
        let mut x = vec![usize::MAX; 1000];
        pool(4).install(|| {
            Threads::All.for_each_chunk(&mut x[..], 7, |start, chunk| {
                (start..).zip(chunk).for_each(|(i, x)| *x = i);
            });
        });
        assert_eq!(x, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn a_count_runs_at_most_its_threads_in_chunks() {
        use crate::parallel::count::{counted, peak};
        for (budget, seen) in [(Threads::ONE, 1..=1), (count(3), 2..=3)] {
            let peak = peak(8, || {
                let mut x = vec![0u64; 1 << 16];
                budget.for_each_chunk(&mut x[..], 1, |_, chunk| {
                    counted(|| std::thread::sleep(std::time::Duration::from_millis(20)));
                    chunk.fill(1);
                });
            });
            assert!(seen.contains(&peak), "{peak} threads under {budget:?}");
        }
    }
}
