//! Output whose size shows only once computed, counted first and then placed: each chunk of an
//! input tallies its entries per bucket, a prefix sum gives each (bucket, chunk) its run of the
//! output, and each chunk pushes its entries into its runs. Buckets come out in order, a bucket's
//! entries in input order, so the output does not depend on where the chunks were cut.
#![allow(
    unsafe_code,
    reason = "chunks write one output at once, each push checked inside its own run"
)]

use std::marker::PhantomData;

use super::budget::bounded_per;
use super::{fold_leaves, Blocks, Chunks, Split, Threads};

/// Where each chunk of an input places its entries of each bucket, from a tally of them.
#[derive(Debug)]
pub struct Places {
    threads: Threads,
    /// Items per chunk, the last chunk shorter.
    per: usize,
    /// Items tallied.
    len: usize,
    n_buckets: usize,
    /// Where chunk `c`'s run of bucket `b` starts, at `starts[c * n_buckets + b]`: the runs follow
    /// each other a bucket at a time, its chunks in input order.
    starts: Vec<usize>,
    total: usize,
}

impl Threads {
    /// The places of `input`'s entries in `n_buckets` buckets, from `tally(start, chunk, counts)`
    /// of each chunk, `start` its first item and `counts` its entries per bucket, zeroed. The
    /// chunks are at least `grain` items and, as each holds a row of counts, at most
    /// [`tasks`](Self::tasks) of them, under `All` too.
    ///
    /// # Panics
    /// If the counts, or the chunks times the buckets, add up past `usize::MAX`.
    pub fn places<P: Split>(
        self,
        input: P,
        grain: usize,
        n_buckets: usize,
        tally: impl Fn(usize, P, &mut [usize]) + Sync,
    ) -> Places {
        let len = input.n_items();
        let per = bounded_per(len, grain, self.tasks());
        let blocks = Blocks::new(input, per);
        let n_chunks = blocks.n_items();
        let cells = n_chunks.checked_mul(n_buckets);
        let mut starts = vec![0; cells.expect("a table that fits usize")];
        let rows = Chunks::equal(&mut starts, n_chunks);
        self.for_each_item((blocks, rows), |_, ((start, chunk), row)| {
            tally(start, chunk, row);
        });
        // Each count becomes where its run starts: a bucket at a time, its chunks in input order.
        let mut total: usize = 0;
        for b in 0..n_buckets {
            for c in 0..n_chunks {
                let count = std::mem::replace(&mut starts[c * n_buckets + b], total);
                total = total.checked_add(count).expect("a total that fits usize");
            }
        }
        Places {
            threads: self,
            per,
            len,
            n_buckets,
            starts,
            total,
        }
    }
}

impl Places {
    /// Entries tallied, the length of the output.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Where each bucket's entries start in the output, then the total: bucket `b`'s entries at
    /// `offsets[b]..offsets[b + 1]`.
    pub fn offsets(&self) -> impl Iterator<Item = usize> + '_ {
        let first_chunk = &self.starts[..self.n_buckets];
        first_chunk.iter().copied().chain([self.total])
    }

    /// `fill(start, chunk, slots)` for each chunk of `input`, cut as the tally cut it, each
    /// pushing into `out` exactly the entries it tallied. The places stay for another fill.
    ///
    /// # Panics
    /// If `input` is not as long as the tallied one or `out` as [`total`](Self::total), or if a
    /// chunk pushes into a bucket more entries than it tallied, or fewer, or into a bucket not
    /// below the bucket count.
    pub fn place<P: Split, O: Send>(
        &self,
        input: P,
        out: &mut [O],
        fill: impl Fn(usize, P, &mut Slots<'_, O>) + Sync,
    ) {
        assert_eq!(input.n_items(), self.len, "the tallied input");
        assert_eq!(out.len(), self.total, "an output as long as the tally");
        let out = Output {
            ptr: out.as_mut_ptr(),
            _out: PhantomData,
        };
        let (nb, n_chunks) = (self.n_buckets, fold_leaves(self.len, self.per));
        // Each chunk's cursors, one per bucket, from where its runs start: a copy, so a fill that
        // panics leaves the starts as they were.
        let mut cursors = self.starts.clone();
        // The chunk index and its cursors come from splits of our own, never from the input's.
        let chunks = (
            0..n_chunks,
            Chunks::equal(&mut cursors, n_chunks),
            Blocks::new(input, self.per),
        );
        self.threads
            .for_each_item(chunks, |_, (c, next, (start, chunk))| {
                assert_eq!(next.len(), nb, "one cursor a bucket");
                // Each run ends where the next starts: the next chunk's of its bucket, or after
                // the last chunk, the first chunk's of the next bucket.
                let following = match c + 1 < n_chunks {
                    true => &self.starts[(c + 1) * nb..(c + 2) * nb],
                    false => &self.starts[nb.min(1)..nb],
                };
                let mut slots = Slots {
                    out: &out,
                    next,
                    following,
                    total: self.total,
                };
                fill(start, chunk, &mut slots);
                let pushed = (0..nb).all(|b| slots.next[b] == slots.end(b));
                assert!(pushed, "every tallied entry pushed");
            });
    }
}

/// The output that chunks place into at once.
struct Output<'o, O> {
    ptr: *mut O,
    _out: PhantomData<&'o mut [O]>,
}

// SAFETY: each slot is written by the one chunk whose run holds it (see `Slots::push`).
unsafe impl<O: Send> Sync for Output<'_, O> {}

/// One chunk's runs of the output, by bucket, each filled from its first slot.
pub struct Slots<'s, O> {
    out: &'s Output<'s, O>,
    /// The slot each bucket's next entry lands in.
    next: &'s mut [usize],
    /// Where each bucket's run ends, but the last bucket's run of the last chunk.
    following: &'s [usize],
    total: usize,
}

impl<O> Slots<'_, O> {
    /// The slot of the output the next entry of `bucket` lands in.
    #[must_use]
    pub fn next(&self, bucket: usize) -> usize {
        self.next[bucket]
    }

    /// `value` as the next entry of `bucket`.
    ///
    /// # Panics
    /// If `bucket` is not below the bucket count, or the chunk tallied no more entries of it.
    #[inline]
    pub fn push(&mut self, bucket: usize, value: O) {
        let k = self.next[bucket];
        assert!(k < self.end(bucket), "no more pushes than tallied");
        // SAFETY: `k` is inside this chunk's run of `bucket`, which no other chunk writes: the runs
        // are disjoint, each inside the output (`end` at most the total, the output's length),
        // and each chunk's slots go to one task. The slot holds a value, which the write drops.
        unsafe { *self.out.ptr.add(k) = value };
        self.next[bucket] = k + 1;
    }

    /// Where this chunk's run of `bucket` ends: at most the total, the output's length.
    #[inline]
    fn end(&self, bucket: usize) -> usize {
        self.following.get(bucket).copied().unwrap_or(self.total)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rstest::rstest;

    use super::*;
    use crate::parallel::testing::in_pool;

    fn count(n: usize) -> Threads {
        Threads::Count(NonZeroUsize::new(n).unwrap())
    }

    /// Each key's entries counted in its bucket.
    fn by_key(threads: Threads, keys: &[u32], n_keys: usize) -> Places {
        threads.places(keys, 4, n_keys, |_, chunk, counts| {
            chunk.iter().for_each(|&k| counts[k as usize] += 1);
        })
    }

    /// Each item's index in its key's bucket.
    fn fill_rows(places: &Places, keys: &[u32]) -> Vec<u32> {
        let mut rows = vec![u32::MAX; places.total()];
        places.place(keys, &mut rows, |start, chunk, slots| {
            for (i, &k) in (start..).zip(chunk) {
                slots.push(k as usize, i as u32);
            }
        });
        rows
    }

    #[rstest]
    fn a_sort_by_key_is_stable_under_every_budget(
        #[values(Threads::All, Threads::ONE, count(2), count(3), count(7))] threads: Threads,
        #[values(1, 4)] pool: usize,
    ) {
        let keys: Vec<u32> = (0..1000u32).map(|i| (i * 7919) % 13).collect();
        let mut serial: Vec<u32> = (0..1000).collect();
        serial.sort_by_key(|&i| keys[i as usize]);
        let (offsets, rows) = in_pool(pool, || {
            let places = by_key(threads, &keys, 13);
            (
                places.offsets().collect::<Vec<_>>(),
                fill_rows(&places, &keys),
            )
        });
        assert_eq!(rows, serial);
        assert_eq!(offsets.len(), 14);
        for (b, w) in offsets.windows(2).enumerate() {
            assert!(rows[w[0]..w[1]]
                .iter()
                .all(|&i| keys[i as usize] == b as u32));
        }
    }

    /// Rows of computed length: row `i` holds the multiples of `i % 5` up to `i`, without zero,
    /// and `ptr[i]` where it starts.
    fn rows(threads: Threads, n: usize) -> (Vec<usize>, Vec<u32>) {
        let row = |i: usize| (1..=i).filter(move |j| i % 5 != 0 && j % (i % 5) == 0);
        let places = threads.places(0..n, 4, 1, |_, chunk, total| {
            total[0] = chunk.map(|i| row(i).count()).sum();
        });
        let (mut ptr, mut entries) = (vec![0; n + 1], vec![0; places.total()]);
        ptr[n] = places.total();
        places.place(
            (0..n, &mut ptr[..n]),
            &mut entries,
            |_, (items, ptr), slots| {
                for (i, ptr) in items.zip(ptr) {
                    *ptr = slots.next(0);
                    row(i).for_each(|j| slots.push(0, j as u32));
                }
            },
        );
        (ptr, entries)
    }

    #[rstest]
    fn rows_of_computed_length_are_the_serial_rows_under_every_budget(
        #[values(Threads::All, count(2), count(3), count(7))] threads: Threads,
        #[values(1, 4)] pool: usize,
    ) {
        let serial = rows(Threads::ONE, 300);
        assert_eq!(in_pool(pool, || rows(threads, 300)), serial);
        let (ptr, entries) = serial;
        assert_eq!(&entries[ptr[7]..ptr[8]], &[2, 4, 6]);
    }

    /// One cut whatever the loop: a count's chunk loops and the tally cut at the same items.
    #[rstest]
    #[case(Threads::ONE, vec![0])]
    #[case(count(3), vec![0, 334, 668])]
    #[case(Threads::All, vec![0, 250, 500, 750])]
    fn chunks_start_where_a_counts_loops_cut(#[case] threads: Threads, #[case] starts: Vec<usize>) {
        let seen = std::sync::Mutex::new(Vec::new());
        in_pool(4, || {
            threads.places(0..1_000, 7, 1, |start, _, _| {
                seen.lock().unwrap().push(start)
            })
        });
        let mut seen = seen.into_inner().unwrap();
        seen.sort_unstable();
        assert_eq!(seen, starts);
        if threads != Threads::All {
            let mapped = in_pool(4, || threads.map_chunks(0..1_000, 7, |start, _| start));
            assert_eq!(mapped, starts);
        }
    }

    #[test]
    #[should_panic(expected = "a total that fits usize")]
    fn counts_past_usize_panic() {
        Threads::ONE.places(0..1, 1, 2, |_, _, counts| {
            counts.copy_from_slice(&[usize::MAX, 2]);
        });
    }

    #[rstest]
    #[should_panic(expected = "no more pushes than tallied")]
    #[case(3)]
    #[should_panic(expected = "every tallied entry pushed")]
    #[case(1)]
    fn a_miscount_panics(#[case] pushes: u32) {
        let keys = [0u32, 1, 0, 1];
        let places = by_key(Threads::ONE, &keys, 2);
        let mut out = vec![0u32; 4];
        places.place(&keys[..], &mut out, |_, _, slots| {
            (0..pushes).for_each(|i| slots.push(0, i));
            (0..2).for_each(|i| slots.push(1, i));
        });
    }
}
