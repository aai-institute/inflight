//! Inputs cut at an index, sums over them in a tree fixed by their length, and loops over items
//! that each own their part of a buffer.

use std::convert::Infallible;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use rayon::iter::ParallelIterator;

use super::{instrument_task, Threads};

/// An ordered input cut at an index into a prefix and a suffix, as rayon's producers are.
///
/// The loops on [`Threads`] cut only strictly inside, at `0 < mid < n_items`; [`Pieces::new`]
/// cuts its input at the ends of its span too.
pub trait Split: Sized + Send {
    /// Number of items.
    fn n_items(&self) -> usize;
    /// The work the items stand for, which decides whether [`Threads::fold_tree`] forks: the item
    /// count unless an item stands for many, such as a run of rows.
    fn weight(&self) -> usize {
        self.n_items()
    }
    /// The first `mid` items and the rest.
    fn split_at(self, mid: usize) -> (Self, Self);
}

impl<T: Sync> Split for &[T] {
    fn n_items(&self) -> usize {
        <[T]>::len(self)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        <[T]>::split_at(self, mid)
    }
}

impl<T: Send> Split for &mut [T] {
    fn n_items(&self) -> usize {
        <[T]>::len(self)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        <[T]>::split_at_mut(self, mid)
    }
}

impl Split for Range<usize> {
    fn n_items(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let cut = self.start + mid.min(self.n_items());
        (self.start..cut, cut..self.end)
    }
}

/// Inputs of one length, cut together; the heaviest one's work.
macro_rules! zip {
    ($first:ident $(, $rest:ident)+) => {
        #[allow(non_snake_case)]
        impl<$first: Split, $($rest: Split),+> Split for ($first, $($rest),+) {
            fn n_items(&self) -> usize {
                let ($first, $($rest),+) = self;
                $(debug_assert_eq!($first.n_items(), $rest.n_items());)+
                $first.n_items()
            }
            fn weight(&self) -> usize {
                let ($first, $($rest),+) = self;
                $first.weight()$(.max($rest.weight()))+
            }
            fn split_at(self, mid: usize) -> (Self, Self) {
                let ($first, $($rest),+) = self;
                let $first = $first.split_at(mid);
                $(let $rest = $rest.split_at(mid);)+
                (($first.0, $($rest.0),+), ($first.1, $($rest.1),+))
            }
        }
    };
}

zip!(A, B);
zip!(A, B, C);
zip!(A, B, C, D);

/// The leaves [`Threads::fold_tree`] cuts `n_items` items into, `block_len` each: at least one,
/// as an empty input is one leaf. Panics if `block_len` is 0.
pub fn fold_leaves(n_items: usize, block_len: usize) -> usize {
    assert!(block_len > 0, "blocks of at least one item");
    n_items.div_ceil(block_len).max(1)
}

impl Threads {
    /// `leaf(start, piece)` over pieces of `block_len` items of `input`, `start` the piece's first
    /// item, combined in a binary tree fixed by the length alone, so the result's bits do not
    /// depend on the threads; a fork splits the budget in halves. An empty input is one leaf.
    /// Panics if `block_len` is 0.
    pub fn fold_tree<P: Split, T: Send>(
        self,
        input: P,
        block_len: usize,
        leaf: impl Fn(usize, P) -> T + Sync,
        combine: impl Fn(T, T) -> T + Sync,
    ) -> T {
        let leaves = fold_leaves(input.n_items(), block_len);
        let leaf = |start, piece, _| leaf(start, piece);
        tree(0, input, 0..leaves, block_len, self, &leaf, &combine)
    }

    /// [`fold_tree`](Self::fold_tree), each leaf also owning its item of `state`, which has
    /// [`fold_leaves`] items.
    pub fn fold_tree_with<P: Split, S: Split, T: Send>(
        self,
        input: P,
        state: S,
        block_len: usize,
        leaf: impl Fn(usize, P, S) -> T + Sync,
        combine: impl Fn(T, T) -> T + Sync,
    ) -> T {
        let leaves = fold_leaves(input.n_items(), block_len);
        assert_eq!(state.n_items(), leaves, "one state per leaf");
        tree(0, input, state, block_len, self, &leaf, &combine)
    }
}

fn tree<P: Split, S: Split, T: Send>(
    start: usize,
    input: P,
    state: S,
    block_len: usize,
    threads: Threads,
    leaf: &(impl Fn(usize, P, S) -> T + Sync),
    combine: &(impl Fn(T, T) -> T + Sync),
) -> T {
    let (pieces, weight) = (input.n_items().div_ceil(block_len), input.weight());
    if pieces <= 1 {
        return instrument_task(|| leaf(start, input, state));
    }
    let half = pieces / 2;
    let (left, right) = input.split_at(half * block_len);
    let (left_state, right_state) = state.split_at(half);
    let mid = start + half * block_len;
    let (l, r) = if weight > SEQUENTIAL_WEIGHT && threads.tasks() > 1 {
        let (first, second) = threads.halves();
        rayon::join(
            || tree(start, left, left_state, block_len, first, leaf, combine),
            || tree(mid, right, right_state, block_len, second, leaf, combine),
        )
    } else {
        (
            tree(start, left, left_state, block_len, threads, leaf, combine),
            tree(mid, right, right_state, block_len, threads, leaf, combine),
        )
    };
    combine(l, r)
}

/// Subtrees this light run on the calling thread: a `join` per piece costs more than it spreads.
const SEQUENTIAL_WEIGHT: usize = 1 << 16;

impl Threads {
    /// `f(start, chunk)` over `input` cut into chunks as [`per_task`](Self::per_task) says for
    /// `grain`, `start` the chunk's first item; unsplit, `f(0, input)` on the calling thread. The
    /// cuts follow the pool's size, so each item's result must not depend on where they fall.
    pub fn for_each_chunk<P: Split>(self, input: P, grain: usize, f: impl Fn(usize, P) + Sync) {
        match self.per_task(input.n_items(), grain) {
            Some(per) => chunks(0, input, per, &f),
            None => f(0, input),
        }
    }

    /// `f(i, part)` for each item `i` of `input`, `part` holding item `i` alone, at most
    /// [`tasks`](Self::tasks) at once; the pool decides which run at once.
    pub fn for_each_item<P: Split>(self, input: P, f: impl Fn(usize, P) + Sync) {
        let Ok(()) = self.try_for_each_item(input, |i, part| {
            f(i, part);
            Ok::<(), Infallible>(())
        });
    }

    /// [`for_each_item`](Self::for_each_item) for an `f` that may fail. Once an item fails, the
    /// others stop starting items as they see it: some may still start, running ones finish, and
    /// which of several errors returns is unspecified.
    pub fn try_for_each_item<P: Split, E: Send>(
        self,
        input: P,
        f: impl Fn(usize, P) -> Result<(), E> + Sync,
    ) -> Result<(), E> {
        let (tasks, n) = (self.tasks(), input.n_items());
        let failed = AtomicBool::new(false);
        if tasks <= 1 || n <= 1 {
            return items(0, input, &failed, &f);
        }
        let run = |i, part| {
            instrument_task(|| f(i, part)).inspect_err(|_| failed.store(true, Ordering::Release))
        };
        if self == Self::All {
            return par_split(input)
                .try_for_each(|(start, part)| items(start, part, &failed, &run));
        }
        // Under a count, workers claim the next item in turn, so no more than it run at once.
        let next = Mutex::new((0, Some(input)));
        let claim = || {
            let mut next = next.lock().unwrap_or_else(PoisonError::into_inner);
            let (start, rest) = (next.0, next.1.take()?);
            match rest.n_items() {
                0 => None,
                1 => Some((start, rest)),
                _ => {
                    let (part, tail) = rest.split_at(1);
                    *next = (start + 1, Some(tail));
                    Some((start, part))
                }
            }
        };
        let error = Mutex::new(None);
        workers(tasks.min(n), &|| {
            while !failed.load(Ordering::Acquire) {
                let Some((i, part)) = claim() else { break };
                if let Err(e) = run(i, part) {
                    error
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get_or_insert(e);
                }
            }
        });
        match error.into_inner().unwrap_or_else(PoisonError::into_inner) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// `f` over `input` cut at whole multiples of `per`, at most one task per chunk.
fn chunks<P: Split>(start: usize, input: P, per: usize, f: &(impl Fn(usize, P) + Sync)) {
    let pieces = input.n_items().div_ceil(per);
    if pieces <= 1 {
        return instrument_task(|| f(start, input));
    }
    let half = pieces / 2;
    let (left, right) = input.split_at(half * per);
    rayon::join(
        || chunks(start, left, per, f),
        || chunks(start + half * per, right, per, f),
    );
}

/// `f` of each item of `rest` in order, the first item `start`, until one fails or `failed` is
/// raised.
fn items<P: Split, E>(
    start: usize,
    mut rest: P,
    failed: &AtomicBool,
    f: &impl Fn(usize, P) -> Result<(), E>,
) -> Result<(), E> {
    for i in start.. {
        if rest.n_items() == 0 || failed.load(Ordering::Acquire) {
            break;
        }
        // The last item goes whole: an input is cut strictly inside.
        if rest.n_items() == 1 {
            return f(i, rest);
        }
        let (part, tail) = rest.split_at(1);
        f(i, part)?;
        rest = tail;
    }
    Ok(())
}

/// `work` on each of `n` tasks at once.
fn workers(n: usize, work: &(impl Fn() + Sync)) {
    if n <= 1 {
        return work();
    }
    let left = n / 2;
    rayon::join(|| workers(left, work), || workers(n - left, work));
}

/// An input cut into consecutive pieces: piece `i` holds items `offsets[i]..offsets[i + 1]`.
pub struct Pieces<'o, P> {
    inner: P,
    offsets: &'o [usize],
}

impl<'o, P: Split> Pieces<'o, P> {
    /// `inner` cut at `offsets`, which are non-empty, non-decreasing and inside `inner`; items
    /// outside their span are left out.
    pub fn new(inner: P, offsets: &'o [usize]) -> Self {
        let inside = offsets.last().is_some_and(|&end| end <= inner.n_items());
        assert!(
            inside && offsets.is_sorted(),
            "non-decreasing offsets inside the input"
        );
        let (head, _) = inner.split_at(offsets[offsets.len() - 1]);
        let (_, inner) = head.split_at(offsets[0]);
        Self { inner, offsets }
    }

    /// The items of every piece, in order.
    pub fn into_inner(self) -> P {
        self.inner
    }
}

impl<P: Split> Split for Pieces<'_, P> {
    fn n_items(&self) -> usize {
        self.offsets.len() - 1
    }
    /// The work of the items the pieces hold.
    fn weight(&self) -> usize {
        self.inner.weight()
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let (left, right) = self.inner.split_at(self.offsets[mid] - self.offsets[0]);
        (
            Self {
                inner: left,
                offsets: &self.offsets[..=mid],
            },
            Self {
                inner: right,
                offsets: &self.offsets[mid..],
            },
        )
    }
}

/// A buffer cut into a count of equal chunks, one per item; rayon's `par_chunks_mut` takes their
/// width instead.
pub struct Chunks<'a, T> {
    values: &'a mut [T],
    width: usize,
    n: usize,
}

impl<'a, T> Chunks<'a, T> {
    /// `values` cut into `n` chunks of equal length.
    pub fn equal(values: &'a mut [T], n: usize) -> Self {
        let width = values.len().checked_div(n).unwrap_or(0);
        assert_eq!(width * n, values.len(), "chunks of equal length");
        Self { values, width, n }
    }

    /// The values of every chunk, in order.
    pub fn into_values(self) -> &'a mut [T] {
        self.values
    }
}

impl<T: Send> Split for Chunks<'_, T> {
    fn n_items(&self) -> usize {
        self.n
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let (left, right) = self.values.split_at_mut(mid * self.width);
        let side = |values, n| Self {
            values,
            width: self.width,
            n,
        };
        (side(left, mid), side(right, self.n - mid))
    }
}

/// One accumulator per item, the first given apart, such as the output of a sum, and the others
/// after it in one buffer; cut strictly inside, as each side keeps a first.
pub struct Accumulators<'a, T> {
    first: &'a mut [T],
    rest: &'a mut [T],
    n: usize,
}

impl<'a, T> Accumulators<'a, T> {
    /// `n` accumulators: `first`, then `rest` cut into `n - 1` of its length.
    pub fn new(first: &'a mut [T], rest: &'a mut [T], n: usize) -> Self {
        let equal = n.checked_sub(1).and_then(|n| first.len().checked_mul(n)) == Some(rest.len());
        assert!(equal, "at least one accumulator, all of equal length");
        Self { first, rest, n }
    }

    /// The first item's accumulator.
    pub fn into_first(self) -> &'a mut [T] {
        self.first
    }
}

impl<T: Send> Split for Accumulators<'_, T> {
    fn n_items(&self) -> usize {
        self.n
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let width = self.first.len();
        let (left, right) = self.rest.split_at_mut((mid - 1) * width);
        let (right_first, right_rest) = right.split_at_mut(width);
        (
            Self {
                first: self.first,
                rest: left,
                n: mid,
            },
            Self {
                first: right_first,
                rest: right_rest,
                n: self.n - mid,
            },
        )
    }
}

/// `input` cut adaptively by rayon's `split`, each part with the index of its first item, on the
/// whole pool: how many parts there are follows the pool, so a result must not depend on it.
pub fn par_split<P: Split>(input: P) -> impl ParallelIterator<Item = (usize, P)> {
    rayon::iter::split((0, input), |(start, input): (usize, P)| {
        let n = input.n_items();
        if n < 2 {
            return ((start, input), None);
        }
        let (left, right) = input.split_at(n / 2);
        ((start, left), Some((start + n / 2, right)))
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use rstest::rstest;

    use super::*;
    use crate::parallel::testing::{in_pool, peak};

    fn count(n: usize) -> Threads {
        Threads::Count(NonZeroUsize::new(n).unwrap())
    }

    #[rstest]
    #[case(0)]
    #[case(1)]
    #[case(7)]
    #[case(8)]
    #[case(9)]
    #[case(43)]
    #[case(1_000)]
    fn leaves_cover_the_input_once_in_order(#[case] n: usize) {
        let leaves = Threads::All.fold_tree(
            0..n,
            8,
            |start, r: Range<usize>| {
                assert_eq!(start, r.start);
                vec![r]
            },
            |mut a, b| {
                a.extend(b);
                a
            },
        );
        assert_eq!(leaves.len(), fold_leaves(n, 8));
        assert!(leaves
            .iter()
            .all(|r| r.len() <= 8 && (n == 0 || !r.is_empty())));
        let covered: Vec<usize> = leaves.into_iter().flatten().collect();
        assert_eq!(covered, (0..n).collect::<Vec<_>>());
    }

    #[test]
    fn each_piece_gets_its_own_items_and_the_rest_stay() {
        let offsets = [2, 2, 5, 6, 10];
        let mut items = [usize::MAX; 12];
        in_pool(3, || {
            Threads::All.for_each_item(Pieces::new(&mut items[..], &offsets), |i, part| {
                part.into_inner().fill(i);
            });
        });
        let max = usize::MAX;
        assert_eq!(items, [max, max, 1, 1, 1, 2, 3, 3, 3, 3, max, max]);
    }

    #[rstest]
    #[should_panic(expected = "non-decreasing offsets inside the input")]
    #[case(&[3, 2])]
    #[should_panic(expected = "non-decreasing offsets inside the input")]
    #[case(&[0, 11])]
    #[should_panic(expected = "non-decreasing offsets inside the input")]
    #[case(&[])]
    fn pieces_take_offsets_inside_in_order(#[case] offsets: &[usize]) {
        Pieces::new(0..10, offsets);
    }

    #[test]
    fn the_sum_does_not_depend_on_the_pool() {
        // Magnitudes over 30 decades, so any change of summation order changes the bits.
        let x: Vec<f64> = (0..100_003u64)
            .map(|i| ((i * 2_654_435_761) % 997) as f64 * 10f64.powi((i % 31) as i32 - 15))
            .collect();
        let sum = |threads: Threads| {
            let leaf = |_, c: &[f64]| c.iter().sum::<f64>();
            threads
                .fold_tree(x.as_slice(), 64, leaf, |a, b| a + b)
                .to_bits()
        };
        let reference = in_pool(1, || sum(Threads::All));
        for threads in [2, 3, 8] {
            for budget in [Threads::All, Threads::ONE, count(3)] {
                assert_eq!(
                    in_pool(threads, || sum(budget)),
                    reference,
                    "{threads} threads"
                );
            }
        }
    }

    #[test]
    fn leaves_own_their_accumulators_and_the_first_is_the_output() {
        let (n, block_len, width) = (300_000, 1 << 15, 3);
        let n_blocks = fold_leaves(n, block_len);
        let mut out = vec![0usize; width];
        let mut rest = vec![0usize; (n_blocks - 1) * width];
        let state = Accumulators::new(&mut out, &mut rest, n_blocks);
        fn leaf<'a>(
            start: usize,
            rows: Range<usize>,
            acc: Accumulators<'a, usize>,
        ) -> &'a mut [usize] {
            assert_eq!(start, rows.start);
            let acc = acc.into_first();
            acc.iter_mut().for_each(|a| *a = rows.len());
            acc
        }
        let total = in_pool(4, || {
            (Threads::All)
                .fold_tree_with(0..n, state, block_len, leaf, |left, right| {
                    left.iter_mut().zip(&*right).for_each(|(l, r)| *l += r);
                    left
                })
                .to_vec()
        });
        assert_eq!(total, vec![n; width]);
        assert_eq!(out, total);
    }

    #[test]
    #[should_panic(expected = "at least one accumulator, all of equal length")]
    fn accumulators_are_at_least_one() {
        Accumulators::new(&mut [0u8; 2], &mut [], 0);
    }

    #[test]
    fn chunks_go_with_their_pieces() {
        let offsets = [0, 3, 3, 7];
        let (mut rows, mut scratch) = ([0usize; 7], [0usize; 6]);
        let parts = (
            Pieces::new(&mut rows[..], &offsets),
            Chunks::equal(&mut scratch, 3),
        );
        Threads::All.for_each_item(parts, |i, (rows, scratch)| {
            rows.into_inner().fill(i);
            scratch.into_values().fill(i + 10);
        });
        assert_eq!(rows, [0, 0, 0, 2, 2, 2, 2]);
        assert_eq!(scratch, [10, 10, 11, 11, 12, 12]);
    }

    #[test]
    fn a_lone_item_takes_its_input_whole() {
        let (mut out, mut rest) = ([0usize; 2], [0usize; 0]);
        let accumulators = Accumulators::new(&mut out, &mut rest, 1);
        Threads::All.for_each_item(accumulators, |i, acc| acc.into_first().fill(i + 1));
        assert_eq!(out, [1, 1]);
        #[allow(clippy::reversed_empty_ranges)]
        let empty = 5..3;
        let len = Threads::All.fold_tree(empty, 2, |_, r: Range<usize>| r.len(), |a, b| a + b);
        assert_eq!(len, 0);
    }

    #[test]
    fn chunks_of_zipped_inputs_keep_their_starts() {
        let (mut y, mut z) = (vec![0usize; 1000], vec![0usize; 1000]);
        let x: Vec<usize> = (0..1000).collect();
        in_pool(4, || {
            let zipped = (&mut y[..], &x[..], &mut z[..]);
            Threads::All.for_each_chunk(zipped, 7, |start, (y, x, z)| {
                for (k, ((y, &x), z)) in y.iter_mut().zip(x).zip(z).enumerate() {
                    assert_eq!(start + k, x);
                    (*y, *z) = (x + 1, x + 2);
                }
            });
        });
        assert_eq!(y, (1..=1000).collect::<Vec<_>>());
        assert_eq!(z, (2..=1001).collect::<Vec<_>>());
    }

    #[test]
    fn parts_cover_the_input_once() {
        let mut x = vec![0usize; 1000];
        in_pool(4, || {
            par_split(&mut x[..]).for_each(|(start, part)| {
                (start..).zip(part).for_each(|(i, x)| *x = i);
            });
        });
        assert_eq!(x, (0..1000).collect::<Vec<_>>());
    }

    #[rstest]
    fn items_cover_the_input_once_under_any_budget(
        #[values(Threads::All, Threads::ONE, count(3))] budget: Threads,
    ) {
        let mut x = vec![usize::MAX; 100];
        in_pool(4, || {
            budget.for_each_item(&mut x[..], |i, part| part[0] = i)
        });
        assert_eq!(x, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn a_count_runs_at_most_its_threads_in_items() {
        // On the calling thread alone, the items run outside any task.
        for (budget, seen) in [(Threads::ONE, 0..=0), (count(3), 2..=3)] {
            let peak = peak(8, || {
                budget.for_each_item(0..16, |_, _| std::thread::sleep(Duration::from_millis(20)));
            });
            assert!(seen.contains(&peak), "{peak} threads under {budget:?}");
        }
    }

    #[rstest]
    fn a_failed_item_stops_the_loop(
        #[values(Threads::All, Threads::ONE, count(3))] budget: Threads,
    ) {
        let started = std::sync::atomic::AtomicUsize::new(0);
        let result = in_pool(4, || {
            budget.try_for_each_item(0..10_000, |i, _| {
                started.fetch_add(1, Ordering::Relaxed);
                if i == 0 {
                    return Err(i);
                }
                std::thread::sleep(Duration::from_micros(100));
                Ok(())
            })
        });
        assert_eq!(result, Err(0));
        assert!(started.into_inner() < 10_000, "{budget:?} ran every item");
    }
}
