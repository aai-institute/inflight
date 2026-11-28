//! The loops timed, on simple work, and the grid of sizes, pools and budgets they run on.
extern crate self as inflight_bench;

pub const GRAIN: usize = 1 << 14;
pub const N_BUCKETS: usize = 1 << 10;
pub const LOOPS: [&str; 4] = ["for_each_chunk", "map_chunks", "fold_tree", "places"];
pub const SIZES: [usize; 2] = [1 << 20, 1 << 24];
pub const THREADS: [usize; 3] = [1, 4, 12];
pub const BUDGETS: [&str; 2] = ["All", "Count3"];

/// The inputs and outputs of every loop at one size.
pub struct Data {
    pub keys: Vec<u32>,
    pub buf: Vec<u32>,
    pub out: Vec<u32>,
}

impl Data {
    /// Pseudo-random keys fixed by `n` (xorshift).
    #[must_use]
    pub fn new(n: usize) -> Self {
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let keys = (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 32) as u32
            })
            .collect();
        Self {
            keys,
            buf: vec![0; n],
            out: vec![0; n],
        }
    }
}

pub mod pair;

/// The loops on the library this crate builds against.
pub mod lib {
    use ::inflight as krate;
    include!("work.rs");
}
