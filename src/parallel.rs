//! Parallel work on a rayon pool whose results do not depend on the thread count.
//!
//! A thread budget, sums in a tree fixed by the input length, loops over items that each own their
//! part of a buffer, and room on a stack for tasks that run at once. Behind the `rayon` feature.

mod budget;
mod disjoint;
mod isolated;
mod split;
mod stack;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use budget::{instrument_task, Schedule, Threads};
pub use disjoint::{
    Claim, Disjoint, ListedIndex, Members, Partition, PartitionError, Scattered, Set,
};
pub use isolated::isolated;
pub use split::{fold_leaves, par_split, Accumulators, Chunks, Pieces, Split};
pub use stack::{Regions, Workspaces};
