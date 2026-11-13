//! A parallel, cancellable unstable sort.
//!
//! `rayon`'s `par_sort_unstable` can't be interrupted: on tens of millions
//! of elements it runs for hundreds of milliseconds with no chance to
//! notice a cancel (MF-RT-002). This is a parallel quicksort whose
//! partition passes checkpoint every [`TICK_INTERVAL`] elements and whose
//! leaves (at most [`LEAF`] elements) use the standard library's pdqsort.
//!
//! The result is the sorted order, so it is deterministic whenever the key
//! is a total order on the elements (equal keys only between equal
//! elements), however the work is split across threads.

use crate::{Cancelled, RunContext, TICK_INTERVAL};

/// Slices at most this long are sorted in one go. Small, because a key can
/// be expensive (binary STL welding decodes it from a random spot in the
/// file buffer, a cache miss each time): pdqsort computes two keys per
/// comparison, the partition passes above the leaves one per element.
const LEAF: usize = 1 << 11;

/// Sorts `v` by `key`, in parallel on the current `rayon` pool, returning
/// [`Cancelled`] (with `v` in some permutation of its input) if `ctx` is
/// cancelled first.
pub fn par_sort_unstable_by_key<T, K, F>(
    v: &mut [T],
    ctx: &RunContext,
    key: F,
) -> Result<(), Cancelled>
where
    T: Send,
    K: Ord,
    F: Fn(&T) -> K + Sync,
{
    ctx.checkpoint()?;
    // Pivots are medians of nine, so a bad split is rare; past this depth
    // the remaining slice is heapsorted (still O(n log n), still
    // cancellable).
    let depth = 2 * (usize::BITS - v.len().leading_zeros());
    sort(v, ctx, &key, depth)
}

fn sort<T, K, F>(v: &mut [T], ctx: &RunContext, key: &F, depth: u32) -> Result<(), Cancelled>
where
    T: Send,
    K: Ord,
    F: Fn(&T) -> K + Sync,
{
    if v.len() <= LEAF {
        ctx.checkpoint()?;
        v.sort_unstable_by_key(key);
        return Ok(());
    }
    if depth == 0 {
        return heapsort(v, ctx, key);
    }
    let (lt, gt) = partition(v, ctx, key)?;
    let (low, rest) = v.split_at_mut(lt);
    let high = &mut rest[gt - lt..];
    let (a, b) = rayon::join(
        || sort(low, ctx, key, depth - 1),
        || sort(high, ctx, key, depth - 1),
    );
    a.and(b)
}

/// Heapsort, checkpointing every [`TICK_INTERVAL`] sift-downs: the
/// fallback for slices whose pivots kept splitting badly. O(n log n) with
/// no recursion, and cancellable throughout.
fn heapsort<T, K, F>(v: &mut [T], ctx: &RunContext, key: &F) -> Result<(), Cancelled>
where
    K: Ord,
    F: Fn(&T) -> K,
{
    fn sift_down<T, K: Ord>(v: &mut [T], key: &impl Fn(&T) -> K, mut root: usize) {
        loop {
            let mut child = 2 * root + 1;
            if child >= v.len() {
                return;
            }
            if child + 1 < v.len() && key(&v[child]) < key(&v[child + 1]) {
                child += 1;
            }
            if key(&v[root]) >= key(&v[child]) {
                return;
            }
            v.swap(root, child);
            root = child;
        }
    }
    let mut since = 0u32;
    let mut tick = || {
        since += 1;
        if since == TICK_INTERVAL {
            since = 0;
            ctx.checkpoint()
        } else {
            Ok(())
        }
    };
    ctx.checkpoint()?;
    let n = v.len();
    for root in (0..n / 2).rev() {
        tick()?;
        sift_down(v, key, root);
    }
    for end in (1..n).rev() {
        tick()?;
        v.swap(0, end);
        sift_down(&mut v[..end], key, 0);
    }
    Ok(())
}

/// Index of the median of `v[a]`, `v[b]`, `v[c]` by key.
fn median3<T, K: Ord>(v: &[T], key: &impl Fn(&T) -> K, a: usize, b: usize, c: usize) -> usize {
    let (ka, kb, kc) = (key(&v[a]), key(&v[b]), key(&v[c]));
    if (ka <= kb) == (kb <= kc) {
        b
    } else if (kb <= ka) == (ka <= kc) {
        a
    } else {
        c
    }
}

/// Three-way partition around a median-of-nine pivot: afterwards
/// `v[..lt] < pivot`, `v[lt..gt] == pivot` and `v[gt..] > pivot` (by key).
/// The pivot band is never empty, so both sides shrink.
fn partition<T, K, F>(v: &mut [T], ctx: &RunContext, key: &F) -> Result<(usize, usize), Cancelled>
where
    K: Ord,
    F: Fn(&T) -> K,
{
    let n = v.len();
    let s = n / 8;
    let m1 = median3(v, key, 0, s, 2 * s);
    let m2 = median3(v, key, 3 * s, 4 * s, 5 * s);
    let m3 = median3(v, key, 6 * s, 7 * s, n - 1);
    let p = median3(v, key, m1, m2, m3);
    v.swap(0, p);
    let pivot = key(&v[0]);
    // Dijkstra's three-way partition.
    let (mut lt, mut i, mut gt) = (0, 1, n);
    let mut since = 0u32;
    while i < gt {
        since += 1;
        if since == TICK_INTERVAL {
            since = 0;
            ctx.checkpoint()?;
        }
        match key(&v[i]).cmp(&pivot) {
            std::cmp::Ordering::Less => {
                v.swap(lt, i);
                lt += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                gt -= 1;
                v.swap(i, gt);
            }
            std::cmp::Ordering::Equal => i += 1,
        }
    }
    Ok((lt, gt))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// The depth-limit fallback (reached only by inputs that defeat the
    /// median-of-nine pivots) must stay cancellable, and still sort.
    #[test]
    fn the_depth_limit_fallback_is_cancellable() {
        let n = 4_000_000u64;
        let input: Vec<u64> = (0..n)
            .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .collect();

        let mut v = input.clone();
        let mut expected = input.clone();
        expected.sort_unstable();
        sort(&mut v, &RunContext::none(), &|x: &u64| *x, 0).unwrap();
        assert_eq!(v, expected);

        let ctx = RunContext::none();
        let canceller = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(5));
                ctx.cancel.cancel();
                Instant::now()
            })
        };
        let mut v = input;
        let result = sort(&mut v, &ctx, &|x: &u64| *x, 0);
        let returned = Instant::now();
        let cancelled_at = canceller.join().unwrap();
        assert!(
            result.is_err(),
            "the fallback ran to completion uncancelled"
        );
        assert!(
            returned.saturating_duration_since(cancelled_at) < Duration::from_millis(50),
            "{:?}",
            returned.saturating_duration_since(cancelled_at)
        );
    }
}
