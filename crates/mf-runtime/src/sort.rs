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
    // the remaining slice is sorted directly (still O(n log n)).
    let depth = 2 * (usize::BITS - v.len().leading_zeros());
    sort(v, ctx, &key, depth)
}

fn sort<T, K, F>(v: &mut [T], ctx: &RunContext, key: &F, depth: u32) -> Result<(), Cancelled>
where
    T: Send,
    K: Ord,
    F: Fn(&T) -> K + Sync,
{
    if v.len() <= LEAF || depth == 0 {
        ctx.checkpoint()?;
        v.sort_unstable_by_key(key);
        return Ok(());
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
