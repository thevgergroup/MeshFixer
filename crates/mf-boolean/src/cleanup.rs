//! The degenerate post-pass (Phase 1 Task 7b, ADR-0002): removes the
//! zero-area triangles a kernel *created* (on input faces that have area),
//! by exact edits that leave the surface's point set unchanged.
//!
//! A zero-area triangle (decided exactly, as [`crate::validate_solid`]
//! does) is one of:
//! - **a needle**: two corners at the same position (distinct vertices).
//!   The edge between them is **collapsed**: the later vertex is replaced by
//!   the earlier everywhere, and the two triangles on that edge (both zero
//!   area) are removed. Every other triangle keeps its corners' positions
//!   exactly. Done only when the link condition holds (the two vertices'
//!   common neighbours are exactly the two opposite corners), so the
//!   surface stays a closed 2-manifold.
//! - **a cap**: three distinct, collinear corners, one strictly between the
//!   other two. The cap's long edge is **flipped** against its neighbour
//!   across it: the cap `(p, q, m)` and neighbour `(q, p, d)` become
//!   `(m, p, d)` and `(m, d, q)`. Because `m` lies exactly on `pq`, the two
//!   new triangles tile the neighbour exactly, so both take the neighbour's
//!   provenance. Done only when the edge `m–d` doesn't exist yet.
//!
//! The pass repeats until nothing changes. A zero-area triangle it can't
//! remove (a link-condition or existing-edge conflict, or one lying on an
//! input face that was already degenerate, which is the input's defect) is
//! left in place, counted in [`DegenerateCleanup::remaining`], and the
//! validator refuses the result by name (D4). Nothing is decided with a
//! tolerance, and nothing is hidden.

use std::collections::{HashMap, HashSet};

use mf_runtime::RunContext;
use robust::{Coord, orient2d};

use crate::kernel::{KernelMesh, KernelResult};
use crate::validate::SolidError;

/// What [`remove_created_degenerates`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DegenerateCleanup {
    /// Caps removed by an edge flip.
    pub flipped: usize,
    /// Needle edges collapsed.
    pub collapsed: usize,
    /// Zero-area triangles left (the validator refuses them).
    pub remaining: usize,
}

/// Passes before giving up: each pass fixes every independent degenerate
/// triangle, so chains need only a few.
const MAX_PASSES: usize = 32;

fn proj(p: [f64; 3], k: usize) -> Coord<f64> {
    let (x, y) = match k {
        0 => (p[1], p[2]),
        1 => (p[2], p[0]),
        _ => (p[0], p[1]),
    };
    Coord { x, y }
}

/// Exactly zero area: every axis projection has zero orientation.
fn zero_area(c: [[f64; 3]; 3]) -> bool {
    (0..3).all(|k| orient2d(proj(c[0], k), proj(c[1], k), proj(c[2], k)) == 0.0)
}

/// Removes zero-area triangles the kernel created in `result` (see the
/// module docs), keeping `origin` for every triangle. `inputs` are the
/// operands whose `face_ids` the origins name. Unreferenced vertices are
/// removed afterwards (order kept), so vertex indices may change.
pub fn remove_created_degenerates(
    result: &mut KernelResult,
    inputs: [&KernelMesh; 2],
    ctx: &RunContext,
) -> Result<DegenerateCleanup, SolidError> {
    ctx.checkpoint()?;
    let mut stats = DegenerateCleanup::default();
    let label_degenerate: HashMap<(u8, u32), bool> = inputs
        .iter()
        .flat_map(|m| {
            m.face_ids.iter().zip(&m.triangles).map(|(label, t)| {
                let c = t.map(|v| m.positions[v as usize]);
                (*label, zero_area(c))
            })
        })
        .collect();
    let p = &result.positions;
    let degenerate = |t: [u32; 3]| zero_area(t.map(|v| p[v as usize]));
    // The kernel's own: on an input face with area (an unknown label is
    // the kernel's too: it names no input face).
    let created = |o: (u8, u32)| !label_degenerate.get(&o).copied().unwrap_or(false);

    let mut alive = vec![true; result.triangles.len()];
    let mut changed_any = false;
    for _ in 0..MAX_PASSES {
        ctx.checkpoint()?;
        let todo: Vec<usize> = (0..result.triangles.len())
            .filter(|&f| alive[f] && created(result.origin[f]) && degenerate(result.triangles[f]))
            .collect();
        if todo.is_empty() {
            break;
        }
        // Directed edge → face, and vertex → faces, for this pass.
        let mut edge: HashMap<(u32, u32), usize> = HashMap::new();
        let mut vfaces: HashMap<u32, Vec<usize>> = HashMap::new();
        let involved: HashSet<u32> = todo.iter().flat_map(|&f| result.triangles[f]).collect();
        for (f, t) in result.triangles.iter().enumerate() {
            if !alive[f] {
                continue;
            }
            for i in 0..3 {
                edge.insert((t[i], t[(i + 1) % 3]), f);
                if involved.contains(&t[i]) {
                    vfaces.entry(t[i]).or_default().push(f);
                }
            }
        }
        // Faces edited this pass; a degenerate face touching one waits.
        let mut dirty = vec![false; result.triangles.len()];
        let mut progress = false;
        for &f in &todo {
            if !alive[f] || dirty[f] {
                continue;
            }
            let t = result.triangles[f];
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue; // a repeated index: not ours to repair
            }
            let c = t.map(|v| result.positions[v as usize]);
            // A needle: corners i, i+1 coincide.
            if let Some(i) = (0..3).find(|&i| c[i] == c[(i + 1) % 3]) {
                let (u, v) = (t[i], t[(i + 1) % 3]);
                let Some(&g) = edge.get(&(v, u)) else {
                    continue;
                };
                if dirty[g] {
                    continue;
                }
                let (keep, gone) = (u.min(v), u.max(v));
                let opposite = |face: usize| {
                    let s = result.triangles[face];
                    s.into_iter().find(|&w| w != u && w != v)
                };
                let (Some(o1), Some(o2)) = (opposite(f), opposite(g)) else {
                    continue;
                };
                // The link condition below reads each endpoint's full one
                // ring from `vfaces`. That is complete only because `vfaces`
                // holds every live face of every vertex in `involved`, and
                // `u` and `v` are corners of a `todo` face, so they are in
                // it (Task 7b review I3). Checked, not assumed: a future
                // narrowing of `involved` must not let a collapse through
                // on a partial ring.
                if !(ring_is_complete(&vfaces, &involved, u, [f, g])
                    && ring_is_complete(&vfaces, &involved, v, [f, g]))
                {
                    debug_assert!(false, "one ring of {u} or {v} is incomplete");
                    continue;
                }
                let around = |x: u32| -> HashSet<u32> {
                    vfaces
                        .get(&x)
                        .into_iter()
                        .flatten()
                        .flat_map(|&h| result.triangles[h])
                        .filter(|&w| w != x)
                        .collect()
                };
                let (nu, nv) = (around(u), around(v));
                let common: HashSet<u32> = nu.intersection(&nv).copied().collect();
                if o1 == o2 || common != HashSet::from([o1, o2]) {
                    continue;
                }
                // Neighbourhoods read from this pass's maps must be current.
                // `gone` is a corner of face `f`, so it has faces; if the map
                // somehow had none, collapsing would orphan its faces: skip
                // the edge rather than collapse it on an empty list.
                let Some(faces) = vfaces.get(&gone).filter(|f| !f.is_empty()).cloned() else {
                    continue;
                };
                let near_keep = vfaces.get(&keep).into_iter().flatten();
                if faces.iter().chain(near_keep).any(|&h| dirty[h]) {
                    continue;
                }
                alive[f] = false;
                alive[g] = false;
                for h in faces {
                    if alive[h] {
                        for w in &mut result.triangles[h] {
                            if *w == gone {
                                *w = keep;
                            }
                        }
                    }
                    dirty[h] = true;
                }
                for h in vfaces.get(&keep).into_iter().flatten() {
                    dirty[*h] = true;
                }
                stats.collapsed += 1;
                progress = true;
                continue;
            }
            // A cap: the middle corner m, strictly between the other two
            // along the axis where they differ most (exact comparisons).
            let ext = |k: usize| {
                let lo = c.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min);
                let hi = c.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max);
                hi - lo
            };
            // The widest axis; on a tie the later one, as `max_by` would.
            let axis = (1..3).fold(0, |best, k| {
                if ext(k).total_cmp(&ext(best)).is_ge() {
                    k
                } else {
                    best
                }
            });
            let Some(mi) = (0..3).find(|&i| {
                let (x, y, z) = (c[i][axis], c[(i + 1) % 3][axis], c[(i + 2) % 3][axis]);
                (y < x && x < z) || (z < x && x < y)
            }) else {
                continue;
            };
            // Rotate so the cap is (p, q, m).
            let (pv, qv, mv) = (t[(mi + 1) % 3], t[(mi + 2) % 3], t[mi]);
            let Some(&g) = edge.get(&(qv, pv)) else {
                continue;
            };
            if dirty[g] || g == f {
                continue;
            }
            let s = result.triangles[g];
            let Some(dv) = s.into_iter().find(|&w| w != pv && w != qv) else {
                continue;
            };
            if dv == mv || edge.contains_key(&(mv, dv)) || edge.contains_key(&(dv, mv)) {
                continue;
            }
            let origin = result.origin[g];
            result.triangles[f] = [mv, pv, dv];
            result.origin[f] = origin;
            result.triangles[g] = [mv, dv, qv];
            dirty[f] = true;
            dirty[g] = true;
            stats.flipped += 1;
            progress = true;
        }
        if !progress {
            break;
        }
        changed_any = true;
    }
    stats.remaining = (0..result.triangles.len())
        .filter(|&f| alive[f] && degenerate(result.triangles[f]))
        .count();
    if changed_any {
        compact(result, &alive);
    }
    ctx.checkpoint()?;
    Ok(stats)
}

/// Whether `vfaces` holds vertex `x`'s one ring as the link condition
/// needs it: `x` was gathered (it is in `involved`), and its list holds the
/// two faces on the edge being collapsed.
fn ring_is_complete(
    vfaces: &HashMap<u32, Vec<usize>>,
    involved: &HashSet<u32>,
    x: u32,
    on_edge: [usize; 2],
) -> bool {
    involved.contains(&x)
        && vfaces
            .get(&x)
            .is_some_and(|faces| on_edge.iter().all(|f| faces.contains(f)))
}

/// Drops dead triangles and unreferenced vertices, keeping order.
fn compact(result: &mut KernelResult, alive: &[bool]) {
    let mut triangles = Vec::with_capacity(result.triangles.len());
    let mut origin = Vec::with_capacity(result.origin.len());
    for (f, &live) in alive.iter().enumerate() {
        if live {
            triangles.push(result.triangles[f]);
            origin.push(result.origin[f]);
        }
    }
    let mut used = vec![false; result.positions.len()];
    for t in &triangles {
        for &v in t {
            used[v as usize] = true;
        }
    }
    let mut remap = vec![u32::MAX; result.positions.len()];
    let mut positions = Vec::with_capacity(result.positions.len());
    for (v, &u) in used.iter().enumerate() {
        if u {
            remap[v] = positions.len() as u32;
            positions.push(result.positions[v]);
        }
    }
    for t in &mut triangles {
        *t = t.map(|v| remap[v as usize]);
    }
    *result = KernelResult {
        positions,
        triangles,
        origin,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_missing_a_vertex_or_a_face_is_incomplete() {
        let involved: HashSet<u32> = [1, 2].into();
        let vfaces: HashMap<u32, Vec<usize>> = [(1, vec![0, 1, 2]), (2, vec![0, 2])].into();
        assert!(ring_is_complete(&vfaces, &involved, 1, [0, 1]));
        // Face 1 isn't in vertex 2's list: its ring was gathered partially.
        assert!(!ring_is_complete(&vfaces, &involved, 2, [0, 1]));
        // Vertex 3 was never gathered.
        assert!(!ring_is_complete(&vfaces, &involved, 3, [0, 1]));
    }
}
