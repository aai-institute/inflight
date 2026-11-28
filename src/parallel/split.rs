//! Inputs cut at an index, sums over them in a tree fixed by their length, and loops over items
//! that each own their part of a buffer.

use std::convert::Infallible;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use rayon::iter::ParallelIterator;

use super::Threads;

/// An ordered input cut at an index into a prefix and a suffix, as rayon's producers are.
///
/// The loops on [`Threads`] cut only strictly inside, at `0 < mid < n_items`; [`Pieces::new`]
/// cuts its input at the ends of its span too.
pub trait Split: Sized + Send {
    /// One item, as the loops over items hand it.
    type Item;
    /// Number of items.
    fn n_items(&self) -> usize;
    /// The first `mid` items and the rest.
    fn split_at(self, mid: usize) -> (Self, Self);
    /// The item of an input of one.
    fn into_item(self) -> Self::Item;
}

impl<'a, T: Sync> Split for &'a [T] {
    type Item = &'a T;
    fn n_items(&self) -> usize {
        <[T]>::len(self)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        <[T]>::split_at(self, mid)
    }
    fn into_item(self) -> &'a T {
        &self[0]
    }
}

impl<'a, T: Send> Split for &'a mut [T] {
    type Item = &'a mut T;
    fn n_items(&self) -> usize {
        <[T]>::len(self)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        <[T]>::split_at_mut(self, mid)
    }
    fn into_item(self) -> &'a mut T {
        &mut self[0]
    }
}

impl Split for Range<usize> {
    type Item = usize;
    fn n_items(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let cut = self.start + mid.min(self.n_items());
        (self.start..cut, cut..self.end)
    }
    fn into_item(self) -> usize {
        self.start
    }
}

/// Inputs of one length, cut together.
macro_rules! zip {
    ($first:ident $(, $rest:ident)+) => {
        #[allow(non_snake_case, reason = "each binding takes its type parameter's name")]
        impl<$first: Split, $($rest: Split),+> Split for ($first, $($rest),+) {
            type Item = ($first::Item, $($rest::Item),+);
            fn n_items(&self) -> usize {
                let ($first, $($rest),+) = self;
                $(debug_assert_eq!($first.n_items(), $rest.n_items());)+
                $first.n_items()
            }
            fn split_at(self, mid: usize) -> (Self, Self) {
                let ($first, $($rest),+) = self;
                let $first = $first.split_at(mid);
                $(let $rest = $rest.split_at(mid);)+
                (($first.0, $($rest.0),+), ($first.1, $($rest.1),+))
            }
            fn into_item(self) -> Self::Item {
                let ($first, $($rest),+) = self;
                ($first.into_item(), $($rest.into_item()),+)
            }
        }
    };
}

zip!(A, B);
zip!(A, B, C);
zip!(A, B, C, D);

/// The blocks [`Blocks::new`] cuts `n_items` items into, `block_len` each: at least one, as an
/// empty input is one block.
///
/// # Panics
/// If `block_len` is 0.
#[must_use]
pub fn fold_leaves(n_items: usize, block_len: usize) -> usize {
    assert!(block_len > 0, "blocks of at least one item");
    n_items.div_ceil(block_len).max(1)
}

impl Threads {
    /// `leaf` of each item of `input`, combined in a binary tree fixed by the item count alone, so
    /// the result's bits do not depend on the threads. Subtrees fork as rayon's adaptive splitter
    /// would, each fork halving the budget, down to one item a task: [`Blocks`] makes an item of
    /// several, and work too small to spread belongs under [`Threads::ONE`].
    ///
    /// # Panics
    /// If `input` has no item.
    pub fn fold_tree<P: Split, T: Send>(
        self,
        input: P,
        leaf: impl Fn(P::Item) -> T + Sync,
        combine: impl Fn(T, T) -> T + Sync,
    ) -> T {
        assert!(input.n_items() > 0, "a fold of at least one item");
        tree(input, Forks::new(self), &Fold { leaf, combine })
    }
}

/// What every node of a fold shares.
struct Fold<L, C> {
    leaf: L,
    combine: C,
}

fn tree<P: Split, T: Send>(
    input: P,
    forks: Forks,
    fold: &Fold<impl Fn(P::Item) -> T + Sync, impl Fn(T, T) -> T + Sync>,
) -> T {
    let n = input.n_items();
    if n <= 1 {
        return (fold.leaf)(input.into_item());
    }
    let (left, right) = input.split_at(n / 2);
    let (l, r) = if forks.fork() {
        let (first, second) = forks.threads.halves();
        rayon::join_context(
            |c| tree(left, forks.side(first, c), fold),
            |c| tree(right, forks.side(second, c), fold),
        )
    } else {
        let forks = forks.serial();
        (tree(left, forks, fold), tree(right, forks, fold))
    };
    (fold.combine)(l, r)
}

/// Whether a subtree forks, which leaves its bits alone: under its budget, while it has splits
/// left, as rayon's adaptive splitter does. A fork halves the splits; a side another thread stole
/// starts again from its budget's tasks.
#[derive(Clone, Copy)]
struct Forks {
    threads: Threads,
    splits: usize,
}

impl Forks {
    fn new(threads: Threads) -> Self {
        let splits = threads.tasks();
        Self { threads, splits }
    }

    fn fork(self) -> bool {
        self.splits > 0 && self.threads.tasks() > 1
    }

    /// One side of a fork, under `threads`, as its join `c` finds it.
    fn side(self, threads: Threads, c: rayon::FnContext) -> Self {
        let splits = match c.migrated() {
            true => (self.splits / 2).max(threads.tasks()),
            false => self.splits / 2,
        };
        Self { threads, splits }
    }

    /// Both sides on this thread: no split left to them.
    const fn serial(self) -> Self {
        Self { splits: 0, ..self }
    }
}

impl Threads {
    /// `f(start, chunk)` over `input` cut into chunks as [`per_task`](Self::per_task) says for
    /// `grain`, `start` the chunk's first item; unsplit, `f(0, input)` on the calling thread. The
    /// cuts follow the pool's size, so each item's result must not depend on where they fall.
    pub fn for_each_chunk<P: Split>(self, input: P, grain: usize, f: impl Fn(usize, P) + Sync) {
        match self.per_task(input.n_items(), grain) {
            Some(per) => {
                chunks(0, input, per, &f);
            }
            None => f(0, input),
        }
    }

    /// [`for_each_chunk`](Self::for_each_chunk) returning `f`'s results in input order; under
    /// `All`, cut adaptively, into as many chunks as rayon's stealing makes.
    #[must_use]
    pub fn map_chunks<P: Split, R: Send>(
        self,
        input: P,
        grain: usize,
        f: impl Fn(usize, P) -> R + Sync,
    ) -> Vec<R> {
        match (self, self.per_task(input.n_items(), grain)) {
            (_, None) => vec![f(0, input)],
            (Self::Count(_), Some(per)) => chunks(0, input, per, &f),
            (Self::All, Some(per)) => (split_at_least(input, per))
                .map(|(start, chunk)| f(start, chunk))
                .collect(),
        }
    }

    /// `f(i, item)` for each item `i` of `input`, at most [`tasks`](Self::tasks) at once; the
    /// pool decides which run at once.
    pub fn for_each_item<P: Split>(self, input: P, f: impl Fn(usize, P::Item) + Sync) {
        let Ok(()) = self.try_for_each_item(input, |i, item| {
            f(i, item);
            Ok::<(), Infallible>(())
        });
    }

    /// [`for_each_item`](Self::for_each_item) for an `f` that may fail. Once an item fails, the
    /// others stop starting items as they see it: some may still start, running ones finish, and
    /// which of several errors returns is unspecified.
    ///
    /// # Errors
    /// An error of `f`.
    pub fn try_for_each_item<P: Split, E: Send>(
        self,
        input: P,
        f: impl Fn(usize, P::Item) -> Result<(), E> + Sync,
    ) -> Result<(), E> {
        let (tasks, n) = (self.tasks(), input.n_items());
        let failed = AtomicBool::new(false);
        if tasks <= 1 || n <= 1 {
            return items(0, input, &failed, &f);
        }
        let run = |i, item| f(i, item).inspect_err(|_| failed.store(true, Ordering::Release));
        match self {
            Self::All => {
                par_split(input).try_for_each(|(start, part)| items(start, part, &failed, &run))
            }
            Self::Count(_) => claimed(tasks.min(n), input, &failed, &run),
        }
    }
}

/// `f` of `input` cut at whole multiples of `per`, one task per chunk, the results in order.
fn chunks<P: Split, R: Send>(
    start: usize,
    input: P,
    per: usize,
    f: &(impl Fn(usize, P) -> R + Sync),
) -> Vec<R> {
    let pieces = input.n_items().div_ceil(per);
    if pieces <= 1 {
        return vec![f(start, input)];
    }
    let half = pieces / 2;
    let (left, right) = input.split_at(half * per);
    let (mut left, mut right) = rayon::join(
        || chunks(start, left, per, f),
        || chunks(start + half * per, right, per, f),
    );
    left.append(&mut right);
    left
}

/// `f` of each item of `rest` in order, the first item `start`, until one fails or `failed` is
/// raised.
fn items<P: Split, E>(
    mut start: usize,
    mut rest: P,
    failed: &AtomicBool,
    f: &impl Fn(usize, P::Item) -> Result<(), E>,
) -> Result<(), E> {
    while rest.n_items() > 0 && !failed.load(Ordering::Acquire) {
        // The last item goes whole: an input is cut strictly inside.
        if rest.n_items() == 1 {
            return f(start, rest.into_item());
        }
        let (item, tail) = rest.split_at(1);
        f(start, item.into_item())?;
        (start, rest) = (start + 1, tail);
    }
    Ok(())
}

/// `f` of each item of `input` on `tasks` tasks, each claiming the next item in turn, so no more
/// than `tasks` run at once; stops claiming once `failed` is raised.
fn claimed<P: Split, E: Send>(
    tasks: usize,
    input: P,
    failed: &AtomicBool,
    f: &(impl Fn(usize, P::Item) -> Result<(), E> + Sync),
) -> Result<(), E> {
    let next = Mutex::new((0, Some(input)));
    let claim = || {
        let mut next = next.lock().unwrap_or_else(PoisonError::into_inner);
        let (start, rest) = (next.0, next.1.take()?);
        match rest.n_items() {
            0 => None,
            1 => Some((start, rest.into_item())),
            _ => {
                let (item, tail) = rest.split_at(1);
                *next = (start + 1, Some(tail));
                Some((start, item.into_item()))
            }
        }
    };
    let error = Mutex::new(None);
    workers(tasks, &|| {
        while !failed.load(Ordering::Acquire) {
            let Some((i, item)) = claim() else { break };
            if let Err(e) = f(i, item) {
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
    /// `inner` cut at `offsets`; items outside their span are left out.
    ///
    /// # Panics
    /// Unless `offsets` are non-empty, non-decreasing and inside `inner`.
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
    /// A piece's items.
    type Item = P;
    fn n_items(&self) -> usize {
        self.offsets.len() - 1
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
    fn into_item(self) -> P {
        self.inner
    }
}

/// An input cut into blocks of a length, the last one shorter, one per item: [`fold_leaves`] of
/// them, so an empty input is one empty block.
pub struct Blocks<P> {
    inner: P,
    len: usize,
    start: usize,
}

impl<P: Split> Blocks<P> {
    /// `inner` cut into blocks of `len` items.
    ///
    /// # Panics
    /// If `len` is 0.
    pub fn new(inner: P, len: usize) -> Self {
        assert!(len > 0, "blocks of at least one item");
        Self {
            inner,
            len,
            start: 0,
        }
    }
}

impl<P: Split> Split for Blocks<P> {
    /// A block's first item, by its index in the whole input, and its items.
    type Item = (usize, P);
    fn n_items(&self) -> usize {
        fold_leaves(self.inner.n_items(), self.len)
    }
    fn split_at(self, mid: usize) -> (Self, Self) {
        let (len, cut) = (self.len, mid * self.len);
        let (left, right) = self.inner.split_at(cut);
        let block = |inner, start| Self { inner, len, start };
        (block(left, self.start), block(right, self.start + cut))
    }
    fn into_item(self) -> (usize, P) {
        (self.start, self.inner)
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
    ///
    /// # Panics
    /// If `values` does not cut into `n` chunks of equal length.
    pub fn equal(values: &'a mut [T], n: usize) -> Self {
        let width = values.len().checked_div(n).unwrap_or(0);
        assert_eq!(width * n, values.len(), "chunks of equal length");
        Self { values, width, n }
    }

    /// The values of every chunk, in order.
    #[must_use]
    pub const fn into_values(self) -> &'a mut [T] {
        self.values
    }
}

impl<'a, T: Send> Split for Chunks<'a, T> {
    /// A chunk's values.
    type Item = &'a mut [T];
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
    fn into_item(self) -> &'a mut [T] {
        self.values
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
    ///
    /// # Panics
    /// If `n` is 0, or `rest` does not hold `n - 1` accumulators of `first`'s length.
    pub fn new(first: &'a mut [T], rest: &'a mut [T], n: usize) -> Self {
        let equal = n.checked_sub(1).and_then(|n| first.len().checked_mul(n)) == Some(rest.len());
        assert!(equal, "at least one accumulator, all of equal length");
        Self { first, rest, n }
    }
}

impl<'a, T: Send> Split for Accumulators<'a, T> {
    /// The first item's accumulator.
    type Item = &'a mut [T];
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
    fn into_item(self) -> &'a mut [T] {
        self.first
    }
}

/// `input` cut adaptively by rayon's `split`, each part with the index of its first item, on the
/// whole pool: how many parts there are follows the pool, so a result must not depend on it.
pub fn par_split<P: Split>(input: P) -> impl ParallelIterator<Item = (usize, P)> {
    split_at_least(input, 1)
}

/// [`par_split`] into parts of at least `per` items.
fn split_at_least<P: Split>(input: P, per: usize) -> impl ParallelIterator<Item = (usize, P)> {
    rayon::iter::split((0, input), move |(start, input): (usize, P)| {
        let n = input.n_items();
        if n / 2 < per {
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
    use crate::parallel::count::{counted, peak};
    use crate::parallel::testing::{assert_pool_independent, in_pool};

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
            Blocks::new(0..n, 8),
            |(start, r): (usize, Range<usize>)| {
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
            Threads::All.for_each_item(Pieces::new(&mut items[..], &offsets), |i, piece| {
                piece.fill(i);
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
        let sum = |budget: Threads| {
            let leaf = |(_, c): (usize, &[f64])| c.iter().sum::<f64>();
            budget
                .fold_tree(Blocks::new(x.as_slice(), 64), leaf, |a, b| a + b)
                .to_bits()
        };
        // Every budget's sum in every pool equals its one-thread sum, and those equal each other.
        let [all, one, three] =
            assert_pool_independent(|| [Threads::All, Threads::ONE, count(3)].map(&sum));
        assert_eq!([one, three], [all, all]);
    }

    #[test]
    fn leaves_own_their_accumulators_and_the_first_is_the_output() {
        let (n, block_len, width) = (300_000, 1 << 15, 3);
        let n_blocks = fold_leaves(n, block_len);
        let mut out = vec![0usize; width];
        let mut rest = vec![0usize; (n_blocks - 1) * width];
        let leaves = (
            Blocks::new(0..n, block_len),
            Accumulators::new(&mut out, &mut rest, n_blocks),
        );
        let total = in_pool(4, || {
            (Threads::All)
                .fold_tree(
                    leaves,
                    |((start, rows), acc)| {
                        assert_eq!(start, rows.start);
                        acc.fill(rows.len());
                        acc
                    },
                    |left: &mut [usize], right| {
                        left.iter_mut().zip(&*right).for_each(|(l, r)| *l += r);
                        left
                    },
                )
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
            rows.fill(i);
            scratch.fill(i + 10);
        });
        assert_eq!(rows, [0, 0, 0, 2, 2, 2, 2]);
        assert_eq!(scratch, [10, 10, 11, 11, 12, 12]);
    }

    #[test]
    fn a_lone_item_takes_its_input_whole() {
        let (mut out, mut rest) = ([0usize; 2], [0usize; 0]);
        let accumulators = Accumulators::new(&mut out, &mut rest, 1);
        Threads::All.for_each_item(accumulators, |i, acc| acc.fill(i + 1));
        assert_eq!(out, [1, 1]);
        #[allow(
            clippy::reversed_empty_ranges,
            reason = "an end before its start is empty"
        )]
        let empty = 5..3;
        let blocks = Blocks::new(empty, 2);
        let len = Threads::All.fold_tree(
            blocks,
            |(_, r): (usize, Range<usize>)| r.len(),
            |a, b| a + b,
        );
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

    #[test]
    fn a_fold_runs_at_most_its_threads() {
        for (budget, seen) in [
            (Threads::ONE, 1..=1),
            (count(3), 2..=3),
            (Threads::All, 4..=8),
        ] {
            let peak = peak(8, || {
                let leaf = |_| counted(|| std::thread::sleep(Duration::from_millis(5)));
                budget.fold_tree(0..64, leaf, |(), ()| ());
            });
            assert!(seen.contains(&peak), "{peak} threads under {budget:?}");
        }
    }

    #[rstest]
    fn mapped_chunks_cover_the_input_once_in_order(
        #[values(Threads::All, Threads::ONE, count(3))] budget: Threads,
        #[values(0, 7, 15, 1_000)] n: usize,
    ) {
        let (chunks, tasks) = in_pool(4, || {
            let chunks = budget.map_chunks(5..5 + n, 7, |start, chunk| {
                assert_eq!(5 + start, chunk.start);
                chunk.collect::<Vec<_>>()
            });
            (chunks, budget.tasks())
        });
        assert_eq!(chunks.concat(), (5..5 + n).collect::<Vec<_>>());
        let (last, rest) = chunks.split_last().unwrap();
        let whole = chunks.len() == 1 || budget != Threads::All || last.len() >= 7;
        assert!(whole && rest.iter().all(|c| c.len() >= 7), "{chunks:?}");
        assert!(
            budget == Threads::All || chunks.len() <= tasks,
            "{chunks:?}"
        );
    }

    #[test]
    fn a_count_maps_one_chunk_a_thread() {
        let starts = in_pool(4, || count(3).map_chunks(0..1_000, 7, |start, _| start));
        assert_eq!(starts, [0, 334, 668]);
        let starts = in_pool(4, || count(4).map_chunks(0..10, 1, |start, _| start));
        assert_eq!(starts, [0, 3, 6, 9]);
    }

    #[test]
    fn a_count_runs_at_most_its_threads_in_mapped_chunks() {
        for (budget, seen) in [(Threads::ONE, 1..=1), (count(3), 2..=3)] {
            let peak = peak(8, || {
                let chunk = |_, _| counted(|| std::thread::sleep(Duration::from_millis(20)));
                let _ = budget.map_chunks(0..8, 1, chunk);
            });
            assert!(seen.contains(&peak), "{peak} threads under {budget:?}");
        }
    }

    #[rstest]
    fn items_cover_the_input_once_under_any_budget(
        #[values(Threads::All, Threads::ONE, count(3))] budget: Threads,
    ) {
        let mut x = vec![usize::MAX; 100];
        in_pool(4, || budget.for_each_item(&mut x[..], |i, x| *x = i));
        assert_eq!(x, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn a_count_runs_at_most_its_threads_in_items() {
        for (budget, seen) in [(Threads::ONE, 1..=1), (count(3), 2..=3)] {
            let peak = peak(8, || {
                let item = |_, _| counted(|| std::thread::sleep(Duration::from_millis(20)));
                budget.for_each_item(0..16, item);
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
