//! Room on a stack for tasks that run at once, each in a part of its own.

use std::mem::MaybeUninit;
use std::sync::Arc;

use dyn_stack::{MemStack, StackReq};

use super::Pieces;

/// One workspace per piece, side by side on a stack, each on one alignment: a cache line, or the
/// widest a piece asks.
#[derive(Clone, Debug)]
pub struct Workspaces {
    /// Piece `i` owns bytes `offsets[i]..offsets[i + 1]`.
    offsets: Arc<[usize]>,
    align: usize,
}

impl Workspaces {
    /// A workspace for each of `each`, in order.
    ///
    /// # Panics
    /// If together they overflow the address space.
    pub fn new(each: impl IntoIterator<Item = StackReq>) -> Self {
        let each: Vec<StackReq> = each.into_iter().collect();
        assert!(
            each.iter().all(|w| w.align_bytes() > 0),
            "a workspace overflows"
        );
        let align = each.iter().map(StackReq::align_bytes).fold(64, usize::max);
        let offsets = std::iter::once(0)
            .chain(each.iter().scan(0usize, |end, w| {
                *end = w
                    .size_bytes()
                    .checked_next_multiple_of(align)
                    .and_then(|size| end.checked_add(size))
                    .expect("a workspace overflows");
                Some(*end)
            }))
            .collect();
        Self { offsets, align }
    }

    /// What [`carve`](Self::carve) takes from a stack.
    #[must_use]
    pub fn scratch(&self) -> StackReq {
        StackReq::new_aligned::<u8>(self.len(), self.align)
    }

    /// Every piece's workspace, from `stack`.
    pub fn carve<'s>(&'s self, stack: &'s mut MemStack) -> Pieces<'s, &'s mut [MaybeUninit<u8>]> {
        let (bytes, _) = stack.make_aligned_uninit::<u8>(self.len(), self.align);
        Pieces::new(bytes, &self.offsets)
    }

    fn len(&self) -> usize {
        self.offsets[self.offsets.len() - 1]
    }
}

/// Equal regions of a stack, one per worker; nothing is allocated to cut them.
#[derive(Clone, Copy, Debug)]
pub struct Regions {
    region: StackReq,
    count: usize,
}

impl Regions {
    /// As many regions of `region` as `stack` holds, at most `at_most`, at least one: a stack
    /// that holds none is the caller's to have checked. Each region is rounded up to its
    /// alignment, so the next starts aligned.
    ///
    /// # Panics
    /// If `region` rounded up to its alignment overflows.
    #[must_use]
    pub fn fitting(stack: &MemStack, region: StackReq, at_most: usize) -> Self {
        let align = region.align_bytes();
        let size = (region.size_bytes().checked_next_multiple_of(align)).expect("a region fits");
        let region = StackReq::new_aligned::<u8>(size, align);
        let fit = match region.size_bytes() {
            0 => at_most,
            size => (stack.len_bytes() / size).min(at_most),
        };
        let count = (1..=fit)
            .rev()
            .find(|&count| stack.can_hold(region.array(count)))
            .unwrap_or(1);
        Self { region, count }
    }

    /// Regions cut.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// `work` on each region carved from `stack`, each on a task of its own.
    pub fn run(&self, stack: &mut MemStack, work: impl Fn(&mut MemStack) + Sync) {
        split(stack, self.region, self.count, &work);
    }
}

fn split(
    stack: &mut MemStack,
    region: StackReq,
    workers: usize,
    work: &(impl Fn(&mut MemStack) + Sync),
) {
    if workers == 1 {
        return work(stack);
    }
    let left = workers / 2;
    let carved = region.array(left);
    let (mine, rest) = stack.make_aligned_uninit::<u8>(carved.size_bytes(), carved.align_bytes());
    rayon::join(
        || split(MemStack::new(mine), region, left, work),
        || split(rest, region, workers - left, work),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dyn_stack::MemBuffer;

    use super::*;

    #[test]
    #[should_panic(expected = "a workspace overflows")]
    fn workspaces_past_the_address_space_panic() {
        Workspaces::new([StackReq::new::<u8>(1 << 62); 4]);
    }

    #[test]
    fn regions_fit_the_stack_and_each_holds_one() {
        let region = StackReq::new::<f64>(100);
        let mut buffer = MemBuffer::new(region.array(3));
        let stack = MemStack::new(&mut buffer);
        let regions = Regions::fitting(stack, region, 8);
        assert_eq!(regions.count(), 3);
        let ran = AtomicUsize::new(0);
        regions.run(stack, |stack| {
            assert!(stack.can_hold(region));
            ran.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(ran.into_inner(), 3);
        assert_eq!(Regions::fitting(stack, region, 2).count(), 2);
    }

    #[test]
    fn a_region_of_padded_size_keeps_the_next_aligned() {
        let region = StackReq::new_aligned::<u8>(100, 64);
        let mut buffer = MemBuffer::new(region.array(3));
        let stack = MemStack::new(&mut buffer);
        let regions = Regions::fitting(stack, region, 8);
        let ran = AtomicUsize::new(0);
        regions.run(stack, |stack| {
            assert!(stack.can_hold(region));
            ran.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(ran.into_inner(), regions.count());
    }
}
