//! Output whose size shows only once computed, counted first and then placed: each chunk of an
//! input counts its entries per bucket, a prefix sum gives each (bucket, chunk) its place, and each
//! chunk writes its entries there. Buckets come out in order, a bucket's entries in input order,
//! so the output does not depend on where the chunks were cut.
#![allow(
    unsafe_code,
    reason = "chunks write one slice at once, each at the places its count gave it"
)]

use std::marker::PhantomData;

use super::{Chunks, Threads};

/// Where each chunk of an input writes its entries of each bucket, from a count of them.
#[derive(Clone, Debug)]
pub struct Places {
    n_buckets: usize,
    /// Chunk `c` holds input items `cuts[c]..cuts[c + 1]`.
    cuts: Vec<usize>,
    /// Chunk `c`'s first slot of bucket `b` at `starts[c * n_buckets + b]`.
    starts: Vec<usize>,
    total: usize,
}

impl Threads {
    /// `count(start, chunk, row)` for each chunk of `input`, `row` its `n_buckets` counts, zeroed;
    /// the chunks at least `grain` items, at most [`tasks`](Self::tasks) of them.
    pub fn count<T: Sync>(
        self,
        input: &[T],
        grain: usize,
        n_buckets: usize,
        count: impl Fn(usize, &[T], &mut [usize]) + Sync,
    ) -> Places {
        let n = input.len();
        let n_chunks = self.tasks().min(n.div_ceil(grain.max(1))).max(1);
        let cuts: Vec<usize> = (0..=n_chunks).map(|c| c * n / n_chunks).collect();
        let mut starts = vec![0usize; n_chunks * n_buckets];
        self.for_each_item(Chunks::equal(&mut starts, n_chunks), |c, row| {
            count(cuts[c], &input[cuts[c]..cuts[c + 1]], row);
        });
        // Buckets in order, a bucket's chunks in input order.
        let mut total = 0;
        for b in 0..n_buckets {
            for c in 0..n_chunks {
                let k = c * n_buckets + b;
                let n = starts[k];
                starts[k] = total;
                total += n;
            }
        }
        Places {
            n_buckets,
            cuts,
            starts,
            total,
        }
    }
}

impl Places {
    /// Entries counted, the length of the output.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Bucket `b`'s entries at `offsets[b]..offsets[b + 1]`.
    #[must_use]
    pub fn offsets(&self) -> Vec<usize> {
        let mut offsets = self.starts[..self.n_buckets].to_vec();
        offsets.push(self.total);
        offsets
    }

    /// `fill(start, chunk, slots)` for each chunk of the counted input, each pushing exactly the
    /// entries it counted into `out`; the places stay for another fill of the same structure.
    ///
    /// # Panics
    /// If `out` is shorter than [`total`](Self::total), `input` is not the counted one's length,
    /// or a push lands outside `out`; in debug builds, also if a chunk pushes into a bucket more
    /// than it counted, or fewer.
    pub fn place<T: Sync, O: Copy + Send>(
        &mut self,
        threads: Threads,
        input: &[T],
        out: &mut [O],
        fill: impl Fn(usize, &[T], &mut Slots<'_, O>) + Sync,
    ) {
        assert!(out.len() >= self.total, "an output as long as the count");
        assert_eq!(
            input.len(),
            self.cuts[self.cuts.len() - 1],
            "the counted input"
        );
        let n_chunks = self.cuts.len() - 1;
        let ends = cfg!(debug_assertions).then(|| self.ends());
        let shared = Shared {
            ptr: out.as_mut_ptr(),
            len: out.len(),
            _out: PhantomData,
        };
        let (cuts, nb) = (&self.cuts, self.n_buckets);
        threads.for_each_item(Chunks::equal(&mut self.starts, n_chunks), |c, cursors| {
            let ends = ends.as_ref().map(|e| &e[c * nb..(c + 1) * nb]);
            let mut slots = Slots {
                out: &shared,
                cursors,
                ends,
            };
            fill(cuts[c], &input[cuts[c]..cuts[c + 1]], &mut slots);
            if let Some(ends) = ends {
                assert_eq!(slots.cursors, ends, "every counted entry pushed");
            }
        });
        // Each cursor ends where the next (bucket, chunk) starts: shift them back.
        let mut previous = 0;
        for b in 0..nb {
            for c in 0..n_chunks {
                let k = c * nb + b;
                (self.starts[k], previous) = (previous, self.starts[k]);
            }
        }
    }

    /// Where each (bucket, chunk)'s slots end, chunk-major as the starts.
    fn ends(&self) -> Vec<usize> {
        let (nb, n_chunks) = (self.n_buckets, self.cuts.len() - 1);
        let mut ends = vec![self.total; self.starts.len()];
        for b in 0..nb {
            for c in 0..n_chunks {
                let next = match (c + 1 < n_chunks, b + 1 < nb) {
                    (true, _) => self.starts[(c + 1) * nb + b],
                    (false, true) => self.starts[b + 1],
                    (false, false) => self.total,
                };
                ends[c * nb + b] = next;
            }
        }
        ends
    }
}

/// The output, shared by the chunks that place into it.
struct Shared<'o, O> {
    ptr: *mut O,
    len: usize,
    _out: PhantomData<&'o mut [O]>,
}

// SAFETY: each chunk writes only the slots its count gave it, which no other chunk holds.
unsafe impl<O: Send> Sync for Shared<'_, O> {}

impl<O> Shared<'_, O> {
    /// # Safety
    /// `k` is inside the output, and no other thread reads or writes slot `k` meanwhile.
    unsafe fn write(&self, k: usize, value: O) {
        debug_assert!(k < self.len);
        // SAFETY: as the caller promises.
        unsafe { self.ptr.add(k).write(value) };
    }
}

/// One chunk's slots in the output, by bucket.
pub struct Slots<'s, O> {
    out: &'s Shared<'s, O>,
    cursors: &'s mut [usize],
    ends: Option<&'s [usize]>,
}

impl<O: Copy> Slots<'_, O> {
    /// The slot the next entry of `bucket` lands in.
    #[must_use]
    pub fn next(&self, bucket: usize) -> usize {
        self.cursors[bucket]
    }

    /// `value` as the next entry of `bucket`.
    #[inline]
    pub fn push(&mut self, bucket: usize, value: O) {
        let k = self.cursors[bucket];
        assert!(k < self.out.len, "a push inside the output");
        if let Some(ends) = self.ends {
            assert!(k < ends[bucket], "no more pushes than counted");
        }
        // SAFETY: inside the output; the slot is this chunk's while its count holds, and the
        // prototype trusts the count in release builds.
        unsafe { self.out.write(k, value) };
        self.cursors[bucket] = k + 1;
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;
    use crate::parallel::testing::in_pool;

    fn budgets() -> Vec<Threads> {
        let mut b = vec![Threads::All, Threads::ONE];
        b.extend([2, 3, 7].map(|n| Threads::Count(NonZeroUsize::new(n).unwrap())));
        b
    }

    /// A stable sort by key: each item's index in its key's bucket.
    fn argsort(threads: Threads, keys: &[u32], n_keys: usize) -> (Vec<usize>, Vec<u32>) {
        let mut places = threads.count(keys, 4, n_keys, |_, chunk, row| {
            for &k in chunk {
                row[k as usize] += 1;
            }
        });
        let mut rows = vec![0u32; places.total()];
        places.place(threads, keys, &mut rows, |start, chunk, slots| {
            for (i, &k) in chunk.iter().enumerate() {
                slots.push(k as usize, (start + i) as u32);
            }
        });
        (places.offsets(), rows)
    }

    #[test]
    fn a_sort_by_key_is_stable_under_every_budget() {
        let keys: Vec<u32> = (0..1000u32).map(|i| (i * 7919) % 13).collect();
        let mut serial: Vec<u32> = (0..1000).collect();
        serial.sort_by_key(|&i| keys[i as usize]);
        for pool in [1, 4] {
            for threads in budgets() {
                let (offsets, rows) = in_pool(pool, || argsort(threads, &keys, 13));
                assert_eq!(rows, serial, "{threads:?} on {pool}");
                assert_eq!(offsets[13], 1000);
                for (b, w) in offsets.windows(2).enumerate() {
                    assert!(rows[w[0]..w[1]]
                        .iter()
                        .all(|&i| keys[i as usize] == b as u32));
                }
            }
        }
    }

    /// Rows of computed length: row `i` holds the multiples of `i % 5` up to `i`, without zero.
    fn rows(threads: Threads, n: usize) -> (Vec<usize>, Vec<u32>) {
        let row = |i: usize| (1..=i).filter(move |j| i % 5 != 0 && j % (i % 5) == 0);
        let items: Vec<usize> = (0..n).collect();
        let mut places = threads.count(&items, 4, 1, |_, chunk, total| {
            total[0] = chunk.iter().map(|&i| row(i).count()).sum();
        });
        let (mut ptr, mut entries) = (vec![0usize; n + 1], vec![0u32; places.total()]);
        // Row pointers are owned by their item: a second, plain loop over the items' places.
        let ptr_out = Shared {
            ptr: ptr.as_mut_ptr(),
            len: n + 1,
            _out: PhantomData,
        };
        places.place(threads, &items, &mut entries, |_, chunk, slots| {
            for &i in chunk {
                // SAFETY: item `i` is this chunk's.
                unsafe { ptr_out.write(i, slots.next(0)) };
                for j in row(i) {
                    slots.push(0, j as u32);
                }
            }
        });
        ptr[n] = places.total();
        (ptr, entries)
    }

    #[test]
    fn rows_of_computed_length_are_the_serial_rows_under_every_budget() {
        let serial = rows(Threads::ONE, 300);
        for pool in [1, 4] {
            for threads in budgets() {
                assert_eq!(in_pool(pool, || rows(threads, 300)), serial, "{threads:?}");
            }
        }
        let (ptr, entries) = serial;
        assert_eq!(&entries[ptr[7]..ptr[8]], &[2, 4, 6]);
    }

    #[test]
    fn places_stay_for_a_second_fill() {
        let keys: Vec<u32> = (0..100u32).map(|i| i % 3).collect();
        let threads = Threads::Count(NonZeroUsize::new(3).unwrap());
        let mut places = threads.count(&keys, 4, 3, |_, chunk, row| {
            for &k in chunk {
                row[k as usize] += 1;
            }
        });
        let fill = |places: &mut Places| {
            let mut out = vec![0u32; places.total()];
            places.place(threads, &keys, &mut out, |s, chunk, slots| {
                for (i, &k) in chunk.iter().enumerate() {
                    slots.push(k as usize, (s + i) as u32);
                }
            });
            out
        };
        let first = fill(&mut places);
        assert_eq!(fill(&mut places), first);
    }

    #[test]
    #[should_panic(expected = "no more pushes than counted")]
    #[cfg(debug_assertions)]
    fn a_push_past_the_count_panics_in_debug() {
        let keys = [0u32, 1, 0, 1];
        let mut places = Threads::ONE.count(&keys, 1, 2, |_, chunk, row| {
            row[0] += chunk.iter().filter(|&&k| k == 0).count();
            row[1] += chunk.iter().filter(|&&k| k == 1).count();
        });
        let mut out = vec![0u32; 4];
        places.place(Threads::ONE, &keys, &mut out, |_, _, slots| {
            for i in 0..3 {
                slots.push(0, i);
            }
        });
    }
}
