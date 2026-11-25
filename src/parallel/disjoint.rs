//! One slice written by many tasks at once, each through a claim on a set of items no other set
//! holds: the sets are checked once, when built, and each is handed out once.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Sets of indices below `n`, no index in two: each a run of consecutive indices storing none, or
/// listed.
#[derive(Clone, Debug)]
pub struct Partition<I> {
    n: usize,
    listed: Box<[I]>,
    /// Each set, a listed one by where its indices start in `listed`.
    sets: Box<[Stored]>,
}

/// A set as [`Partition::new`] takes it: a listed one takes the next `len` listed indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    /// Indices `start..start + len`.
    Run {
        /// Its first index.
        start: usize,
        /// Its indices.
        len: usize,
    },
    /// The next `len` listed indices.
    Listed {
        /// Its indices.
        len: usize,
    },
}

/// A set's indices.
#[derive(Clone, Copy, Debug)]
pub enum Members<'p, I> {
    /// Indices `start..start + len`.
    Run {
        /// Its first index.
        start: usize,
        /// Its indices.
        len: usize,
    },
    /// The indices listed.
    Listed(&'p [I]),
}

#[derive(Clone, Copy, Debug)]
enum Stored {
    Run { start: usize, len: usize },
    Listed { start: usize, len: usize },
}

impl<I: ListedIndex> Partition<I> {
    /// `sets` of indices below `n`, the listed ones taking `listed` in turn; an error unless they
    /// take all of it, every index is below `n` and none is in two sets. Allocates `n` bits once
    /// the indices are known to be below `n`.
    pub fn new(
        n: usize,
        listed: Box<[I]>,
        sets: impl IntoIterator<Item = Set>,
    ) -> Result<Self, PartitionError> {
        let mut end = 0usize;
        let sets = (sets.into_iter())
            .map(|set| match set {
                Set::Run { start, len } => match start.checked_add(len) {
                    Some(run_end) if run_end <= n => Ok(Stored::Run { start, len }),
                    _ => Err(PartitionError::OutOfBounds),
                },
                Set::Listed { len } => {
                    let start = end;
                    end = end.checked_add(len).ok_or(PartitionError::ListedCount)?;
                    Ok(Stored::Listed { start, len })
                }
            })
            .collect::<Result<Box<[Stored]>, _>>()?;
        if end != listed.len() {
            return Err(PartitionError::ListedCount);
        }
        if listed.iter().any(|&i| i.to_u64() >= n as u64) {
            return Err(PartitionError::OutOfBounds);
        }
        let partition = Self { n, listed, sets };
        let mut seen = vec![0u64; n.div_ceil(64)];
        let mut once = |i: usize| {
            let (word, bit) = (&mut seen[i / 64], 1 << (i % 64));
            let fresh = *word & bit == 0;
            *word |= bit;
            fresh.then_some(()).ok_or(PartitionError::Overlap(i))
        };
        for k in 0..partition.len() {
            match partition.members(k) {
                Members::Run { start, len } => (start..start + len).try_for_each(&mut once)?,
                Members::Listed(listed) => listed.iter().try_for_each(|&i| once(index(i)))?,
            }
        }
        Ok(partition)
    }
}

/// Why [`Partition::new`] rejects its sets.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionError {
    /// An index is not below `n`, or a run's end overflows.
    OutOfBounds,
    /// The listed sets take fewer or more indices than are listed.
    ListedCount,
    /// The index is in two sets.
    Overlap(usize),
}

impl std::fmt::Display for PartitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds => f.write_str("an index is not below the bound"),
            Self::ListedCount => f.write_str("the listed sets do not take every listed index"),
            Self::Overlap(i) => write!(f, "index {i} is in two sets"),
        }
    }
}

impl std::error::Error for PartitionError {}

impl<I> Partition<I> {
    /// The bound every index is below.
    pub fn n(&self) -> usize {
        self.n
    }

    /// Its sets.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Whether it has no set.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// Set `k`'s indices.
    pub fn members(&self, k: usize) -> Members<'_, I> {
        match self.sets[k] {
            Stored::Run { start, len } => Members::Run { start, len },
            Stored::Listed { start, len } => Members::Listed(&self.listed[start..start + len]),
        }
    }

    /// The listed sets' indices, in turn.
    pub fn listed(&self) -> &[I] {
        &self.listed
    }
}

impl<I: ListedIndex> Partition<I> {
    /// Set `k`'s items of `x`, which no one else holds while the claim lives.
    pub fn claim<'a, V>(&'a self, x: &'a mut [V], k: usize) -> Claim<'a, I, V> {
        assert!(self.n <= x.len(), "every set's items inside the slice");
        // SAFETY: every member is below `n`, and `x` is borrowed for the claim alone.
        unsafe { claim(x.as_mut_ptr(), self.members(k)) }
    }
}

/// Members' items of the slice at `ptr`.
///
/// # Safety
/// The slice holds every member, and no other access touches them while the claim lives.
unsafe fn claim<'a, I, V>(ptr: *mut V, members: Members<'a, I>) -> Claim<'a, I, V> {
    match members {
        // SAFETY: inside the slice, and no other access touches these items.
        Members::Run { start, len } => {
            Claim::Run(unsafe { std::slice::from_raw_parts_mut(ptr.add(start), len) })
        }
        Members::Listed(members) => Claim::Scattered(Scattered {
            ptr,
            members,
            _x: PhantomData,
        }),
    }
}

/// An unsigned integer a [`Partition`] lists indices in. Sealed: a [`Disjoint`] trusts that an
/// index converts alike every time.
pub trait ListedIndex: sealed::Sealed + Copy {
    /// The index as a `u64`.
    fn to_u64(self) -> u64;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! index {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl ListedIndex for $t {
            #[inline(always)]
            fn to_u64(self) -> u64 {
                self as u64
            }
        }
    )*};
}

index!(u8, u16, u32, u64, usize);

/// An index already checked below `n`, which is a `usize`.
#[inline(always)]
fn index<I: ListedIndex>(i: I) -> usize {
    i.to_u64() as usize
}

/// A slice that tasks write at once, each through the [`Claim`]s of sets [`claims`](Self::claims)
/// hands it.
pub struct Disjoint<'x, I, V> {
    ptr: *mut V,
    partition: &'x Partition<I>,
    next: AtomicUsize,
    _x: PhantomData<&'x mut [V]>,
}

// SAFETY: a `&mut [V]` moves between threads when `V` does, a `&[I]` when `I` is `Sync`, and each
// set's items go to one claim.
unsafe impl<I: Sync, V: Send> Sync for Disjoint<'_, I, V> {}

impl<'x, I: ListedIndex, V> Disjoint<'x, I, V> {
    /// `x`, shared by `partition`'s sets until this drops.
    pub fn new(x: &'x mut [V], partition: &'x Partition<I>) -> Self {
        assert!(
            partition.n() <= x.len(),
            "every set's items inside the slice"
        );
        Self {
            ptr: x.as_mut_ptr(),
            partition,
            next: AtomicUsize::new(0),
            _x: PhantomData,
        }
    }

    /// Each set no one holds yet, by its index with its claim, claimed `batch` sets at a time (at
    /// least one) as the last batch runs out; across every call, each set is handed out once.
    pub fn claims(&self, batch: usize) -> impl Iterator<Item = (usize, Claim<'_, I, V>)> + '_ {
        let mut held = 0..0;
        std::iter::from_fn(move || {
            if held.is_empty() {
                held = self.next_batch(batch)?;
            }
            let k = held.next()?;
            Some((k, self.claim(k)))
        })
    }

    /// The next `batch` sets no one holds yet, at least one; `None` once every set is held.
    fn next_batch(&self, batch: usize) -> Option<std::ops::Range<usize>> {
        let len = self.partition.len();
        let take = |start: usize| (start < len).then(|| start + batch.max(1).min(len - start));
        let mut start = self.next.load(Ordering::Relaxed);
        loop {
            let end = take(start)?;
            match (self.next).compare_exchange_weak(
                start,
                end,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(start..end),
                Err(now) => start = now,
            }
        }
    }

    /// Set `k`'s claim; handed out once, as no other set holds its items.
    fn claim(&self, k: usize) -> Claim<'_, I, V> {
        // SAFETY: every member is below `n`, inside the slice, and set `k` goes to one claim.
        unsafe { claim(self.ptr, self.partition.members(k)) }
    }
}

/// One set's items of a [`Disjoint`] slice.
pub enum Claim<'s, I, V> {
    /// A run's items, in place.
    Run(&'s mut [V]),
    /// A listed set's items.
    Scattered(Scattered<'s, I, V>),
}

/// A listed set's items, by their place in the set.
pub struct Scattered<'s, I, V> {
    ptr: *mut V,
    members: &'s [I],
    _x: PhantomData<&'s mut V>,
}

impl<I: ListedIndex, V: Copy> Scattered<'_, I, V> {
    /// Its items.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether it has no item.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Its `k`th item.
    pub fn get(&self, k: usize) -> V {
        // SAFETY: a member is below `n`, inside the slice, and no other claim writes it.
        unsafe { self.ptr.add(index(self.members[k])).read() }
    }

    /// Sets its `k`th item.
    pub fn set(&mut self, k: usize, value: V) {
        // SAFETY: a member is below `n`, inside the slice, and no other claim reads or writes it.
        unsafe { self.ptr.add(index(self.members[k])).write(value) }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Runs `0..3` and `7..9` with listed `{3, 6}`, `{5}` and `{4}`, on 9 items.
    fn partition() -> Partition<u32> {
        let sets = [
            Set::Run { start: 0, len: 3 },
            Set::Listed { len: 2 },
            Set::Run { start: 7, len: 2 },
            Set::Listed { len: 1 },
            Set::Listed { len: 1 },
        ];
        Partition::new(9, Box::new([3, 6, 5, 4]), sets).expect("a partition")
    }

    #[test]
    fn sets_come_back_as_built() {
        let partition = partition();
        assert!(matches!(partition.members(1), Members::Listed([3, 6])));
        assert!(matches!(
            partition.members(2),
            Members::Run { start: 7, len: 2 }
        ));
        let mut x = [0usize; 9];
        if let Claim::Scattered(mut set) = partition.claim(&mut x, 1) {
            set.set(1, 7);
        }
        assert_eq!(x[6], 7);
    }

    #[rstest]
    #[case::runs_overlap(vec![Set::Run { start: 0, len: 4 }, Set::Run { start: 3, len: 1 }], vec![], PartitionError::Overlap(3))]
    #[case::listed_in_a_run(vec![Set::Run { start: 0, len: 4 }, Set::Listed { len: 1 }], vec![2], PartitionError::Overlap(2))]
    #[case::listed_twice(vec![Set::Listed { len: 1 }, Set::Listed { len: 1 }], vec![5, 5], PartitionError::Overlap(5))]
    #[case::listed_past_n(vec![Set::Listed { len: 1 }], vec![9], PartitionError::OutOfBounds)]
    #[case::run_past_n(vec![Set::Run { start: 8, len: 2 }], vec![], PartitionError::OutOfBounds)]
    #[case::run_end_overflows(vec![Set::Run { start: usize::MAX, len: 2 }], vec![], PartitionError::OutOfBounds)]
    #[case::listed_left_over(vec![Set::Listed { len: 1 }], vec![1, 2], PartitionError::ListedCount)]
    #[case::listed_past_the_list(vec![Set::Listed { len: 3 }], vec![1, 2], PartitionError::ListedCount)]
    fn overlap_or_bounds_fail(
        #[case] sets: Vec<Set>,
        #[case] listed: Vec<u32>,
        #[case] error: PartitionError,
    ) {
        let partition = Partition::new(9, listed.into_boxed_slice(), sets);
        assert_eq!(partition.err(), Some(error));
    }

    #[test]
    fn each_set_is_claimed_once_and_written() {
        let partition = partition();
        let mut x = vec![0usize; 10];
        let shared = Disjoint::new(&mut x, &partition);
        rayon::scope(|s| {
            for _ in 0..4 {
                s.spawn(|_| {
                    for (k, claim) in shared.claims(2) {
                        match claim {
                            Claim::Run(run) => run.iter_mut().for_each(|x| *x += k + 1),
                            Claim::Scattered(mut set) => {
                                (0..set.len()).for_each(|i| set.set(i, set.get(i) + k + 1));
                            }
                        }
                    }
                });
            }
        });
        assert_eq!(x, [1, 1, 1, 2, 5, 4, 2, 3, 3, 0]);
        assert_eq!(Disjoint::new(&mut x, &partition).claims(0).count(), 5);
    }

    #[test]
    #[should_panic(expected = "every set's items inside the slice")]
    fn a_slice_shorter_than_n_panics() {
        let mut x = [0.0; 8];
        Disjoint::new(&mut x, &partition());
    }
}
