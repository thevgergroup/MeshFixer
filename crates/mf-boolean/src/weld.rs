//! The exact weld (Task 8 amendment): vertices at exactly the same
//! position become one vertex before an operand is checked and sent to the
//! kernel, so a scan's unwelded duplicates (cracks along coincident
//! vertices, or sheets touching at a shared corner) are not mistaken for
//! openings or self-intersections. Positions compare bit for bit (with
//! `-0.0` the same as `0.0`); nothing is merged by distance.

use std::collections::HashMap;

/// Welds `positions` exactly. Returns the welded positions (first
/// occurrence order) and, for each input vertex, its welded index.
pub fn weld_exact(positions: &[[f64; 3]]) -> (Vec<[f64; 3]>, Vec<u32>) {
    let mut seen: HashMap<[u64; 3], u32> = HashMap::with_capacity(positions.len());
    let mut out = Vec::with_capacity(positions.len());
    let remap = positions
        .iter()
        .map(|p| {
            *seen.entry(key(*p)).or_insert_with(|| {
                out.push(*p);
                (out.len() - 1) as u32
            })
        })
        .collect();
    (out, remap)
}

/// A position's bits, with `-0.0` as `0.0`.
pub(crate) fn key(p: [f64; 3]) -> [u64; 3] {
    p.map(|c| (c + 0.0).to_bits())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_identical_positions_merge() {
        let (p, r) = weld_exact(&[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [-0.0, 0.0, 0.0],
            [1.0, 0.0, 1e-300],
        ]);
        assert_eq!(p.len(), 3);
        assert_eq!(r, vec![0, 1, 0, 2]);
    }
}
