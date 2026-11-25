//! Parallel work on a rayon pool whose results do not depend on the thread count.
//!
//! A thread budget, sums in a tree fixed by the input length, loops over items that each own their
//! part of a buffer, and room on a stack for tasks that run at once. Behind the `rayon` feature.

mod budget;
#[cfg(test)]
mod count;
mod disjoint;
mod isolated;
mod split;
mod stack;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use budget::{Schedule, Threads};
pub use disjoint::{
    Claim, Disjoint, ListedIndex, Members, Partition, PartitionError, Scattered, Set,
};
pub use isolated::isolated;
pub use split::{fold_leaves, par_split, Accumulators, Blocks, Chunks, Pieces, Split};
pub use stack::{Regions, Workspaces};
