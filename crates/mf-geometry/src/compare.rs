//! Geometric equivalence, as opposed to content identity (MF-DOC-003).
//!
//! Two meshes are equivalent at tolerance `t` when their topological
//! invariants match and every live vertex of each lies within `t` of some
//! vertex of the other (symmetric nearest-vertex distance).

use std::collections::HashMap;

use serde::Serialize;

use crate::diagnostics::inspect;
use crate::mesh::Mesh;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Equivalence {
    /// Face count, components, boundary loops, Euler characteristic,
    /// manifoldness and orientation consistency all match.
    pub invariants_match: bool,
    /// Symmetric maximum nearest-vertex distance, when it is within the
    /// tolerance. `None` means some vertex has no counterpart within it.
    pub max_vertex_distance: Option<f64>,
    pub within: bool,
}

fn invariants(m: &Mesh) -> (usize, usize, usize, i64, bool, usize) {
    let d = inspect(m);
    let euler = m.vertex_count() as i64 - m.topology().edge_count as i64 + m.face_count() as i64;
    (
        d.triangle_count,
        d.components,
        d.boundary_loops.len(),
        euler,
        d.is_manifold,
        d.inconsistent_orientation_edges,
    )
}

/// Largest distance from a vertex of `a` to its nearest vertex of `b`, if
/// every vertex of `a` has a neighbour within `tolerance`.
fn directed(a: &Mesh, b: &Mesh, tolerance: f64) -> Option<f64> {
    if tolerance == 0.0 {
        let mut theirs: Vec<[u64; 3]> = b.vertices().map(|(_, p)| p.map(f64::to_bits)).collect();
        theirs.sort_unstable();
        let all = a
            .vertices()
            .all(|(_, p)| theirs.binary_search(&p.map(f64::to_bits)).is_ok());
        return all.then_some(0.0);
    }
    // Uniform grid with cell size = tolerance: any point outside the 3×3×3
    // neighbourhood of a cell is farther than the tolerance.
    let cell = |p: [f64; 3]| p.map(|c| (c / tolerance).floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<[f64; 3]>> = HashMap::new();
    for (_, p) in b.vertices() {
        grid.entry(cell(p)).or_default().push(p);
    }
    let mut worst = 0.0f64;
    for (_, p) in a.vertices() {
        let c = cell(p);
        let mut best = f64::INFINITY;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(pts) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) {
                        for q in pts {
                            let d = ((p[0] - q[0]).powi(2)
                                + (p[1] - q[1]).powi(2)
                                + (p[2] - q[2]).powi(2))
                            .sqrt();
                            best = best.min(d);
                        }
                    }
                }
            }
        }
        if best > tolerance {
            return None;
        }
        worst = worst.max(best);
    }
    Some(worst)
}

pub fn equivalent(a: &Mesh, b: &Mesh, tolerance: f64) -> Equivalence {
    let invariants_match = invariants(a) == invariants(b);
    let max_vertex_distance = match (directed(a, b, tolerance), directed(b, a, tolerance)) {
        (Some(x), Some(y)) => Some(x.max(y)),
        _ => None,
    };
    Equivalence {
        invariants_match,
        within: invariants_match && max_vertex_distance.is_some(),
        max_vertex_distance,
    }
}
