//! Geometric equivalence, as opposed to content identity (MF-DOC-003).
//!
//! Two meshes are equivalent at tolerance `t` when their topological
//! invariants match and every live vertex of each lies within `t` of some
//! vertex of the other (symmetric nearest-vertex distance).

use serde::Serialize;

use crate::grid::PointGrid;
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

/// Topological invariants, from topology alone (no full diagnostics pass).
fn invariants(m: &Mesh) -> (usize, usize, usize, i64, bool, usize) {
    let t = m.topology();
    let euler = m.vertex_count() as i64 - t.edge_count as i64 + m.face_count() as i64;
    (
        m.face_count(),
        t.components.len(),
        t.boundary_loops.len(),
        euler,
        t.non_manifold_edges.is_empty() && t.non_manifold_vertices == 0,
        t.inconsistent_edges,
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
    // Cells at least `tolerance` wide: any point outside the 3×3×3
    // neighbourhood of a cell is farther than the tolerance.
    let grid = PointGrid::new(|| b.vertices().map(|(id, p)| (id.0, p)), tolerance);
    let mut worst = 0.0f64;
    for (_, p) in a.vertices() {
        let best = grid
            .near(p)
            .map(|j| {
                let q = b.corner_position(j);
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
            })
            .fold(f64::INFINITY, f64::min);
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
