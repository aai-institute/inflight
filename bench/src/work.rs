// The loops on one version of the library, `krate`: included once per version timed.
use std::hint::black_box;
use std::num::NonZeroUsize;

use self::krate::parallel::{Blocks, Threads};

use inflight_bench::{Data, GRAIN, N_BUCKETS};

#[must_use]
fn budget(name: &str) -> Threads {
    match name {
        "All" => Threads::All,
        "Count3" => Threads::Count(NonZeroUsize::new(3).unwrap()),
        _ => panic!("unknown budget {name}"),
    }
}

/// One call of `lp` under the budget named `b`, returning a checksum of its result.
pub fn run(lp: &str, b: &str, d: &mut Data) -> u64 {
    let t = budget(b);
    match lp {
        "for_each_chunk" => {
            t.for_each_chunk(&mut d.buf[..], GRAIN, |start, chunk: &mut [u32]| {
                for (i, x) in chunk.iter_mut().enumerate() {
                    *x = (start + i) as u32 ^ 0x5bd1_e995;
                }
            });
            u64::from(black_box(d.buf[d.buf.len() / 2]))
        }
        "map_chunks" => {
            let v = t.map_chunks(&d.keys[..], GRAIN, |_, c: &[u32]| {
                c.iter().map(|&k| u64::from(k)).sum::<u64>()
            });
            black_box(v.iter().sum())
        }
        "fold_tree" => black_box(t.fold_tree(
            Blocks::new(&d.keys[..], GRAIN),
            |(_, c): (usize, &[u32])| c.iter().map(|&k| u64::from(k)).sum::<u64>(),
            |a, b| a + b,
        )),
        "places" => {
            let mask = (N_BUCKETS - 1) as u32;
            let p = t.places(&d.keys[..], GRAIN, N_BUCKETS, |_, c: &[u32], counts| {
                for &k in c {
                    counts[(k & mask) as usize] += 1;
                }
            });
            p.place(&d.keys[..], &mut d.out[..], |start, c: &[u32], slots| {
                for (i, &k) in c.iter().enumerate() {
                    slots.push((k & mask) as usize, (start + i) as u32);
                }
            });
            u64::from(black_box(d.out[d.out.len() / 3]))
        }
        _ => panic!("unknown loop {lp}"),
    }
}
