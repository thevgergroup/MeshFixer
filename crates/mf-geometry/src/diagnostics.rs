//! `mesh.inspect` diagnostics (MF-GEO-004).

use std::collections::HashMap;

use serde::Serialize;

use crate::mesh::{Aabb, Mesh};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoopSummary {
    pub id: u32,
    pub edge_count: usize,
    /// `None` if it overflowed (see `non_finite`).
    pub perimeter: Option<f64>,
    pub centroid: [f64; 3],
}

/// Why `signed_volume` is or isn't available. Each case maps to a different
/// repair: fill holes, fix non-manifold geometry, or reorient faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeStatus {
    Ok,
    Empty,
    /// Coordinates so large that area or volume overflowed.
    NonFinite,
    NonManifold,
    Open,
    InconsistentOrientation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Diagnostics {
    pub triangle_count: usize,
    pub vertex_count: usize,
    pub components: usize,
    pub boundary_edges: usize,
    pub boundary_loops: Vec<LoopSummary>,
    pub untraced_boundary_edges: usize,
    pub non_manifold_edges: usize,
    pub non_manifold_vertices: usize,
    /// Triangles with area below `1e-12 * bbox_diagonal²`, or a repeated index.
    pub degenerate_triangles: usize,
    pub duplicate_vertices_exact: usize,
    /// Vertices within `1e-6 × bbox diagonal` of a lower-ID vertex.
    pub duplicate_vertices_tolerance: usize,
    /// Faces with the same vertex set as a lower-ID face.
    pub duplicate_faces: usize,
    pub inconsistent_orientation_edges: usize,
    pub bounding_box: Option<Aabb>,
    /// `None` if it overflowed (see `non_finite`).
    pub surface_area: Option<f64>,
    /// True if area, volume or a perimeter overflowed to infinity.
    pub non_finite: bool,
    /// Only reported for closed, manifold, consistently oriented meshes.
    pub signed_volume: Option<f64>,
    pub volume_status: VolumeStatus,
    pub is_closed: bool,
    pub is_manifold: bool,
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

pub fn inspect(mesh: &Mesh) -> Diagnostics {
    let topo = mesh.topology();
    let pos = |v: u32| mesh.corner_position(v);
    let bbox = mesh.bounding_box();
    let diag = bbox.map(|b| b.diagonal()).unwrap_or(0.0);
    let degenerate_area = 1e-12 * diag * diag;

    let mut surface_area = 0.0;
    let mut volume6 = 0.0;
    let mut degenerate = 0;
    for (_, t) in mesh.faces() {
        let (a, b, c) = (pos(t[0]), pos(t[1]), pos(t[2]));
        let area = 0.5 * norm(cross(sub(b, a), sub(c, a)));
        surface_area += area;
        volume6 += dot(a, cross(b, c));
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || area < degenerate_area {
            degenerate += 1;
        }
    }

    let mut sorted: Vec<[u64; 3]> = mesh.vertices().map(|(_, p)| p.map(f64::to_bits)).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let duplicate_vertices_exact = mesh.vertex_count() - sorted.len();

    let boundary_loops: Vec<LoopSummary> = topo
        .boundary_loops
        .iter()
        .map(|l| {
            let n = l.vertices.len();
            let mut perimeter = 0.0;
            let mut sum = [0.0; 3];
            for k in 0..n {
                let p = pos(l.vertices[k]);
                perimeter += norm(sub(pos(l.vertices[(k + 1) % n]), p));
                for i in 0..3 {
                    sum[i] += p[i];
                }
            }
            LoopSummary {
                id: l.id,
                edge_count: l.edge_count,
                perimeter: perimeter.is_finite().then_some(perimeter),
                centroid: sum.map(|s| s / n as f64),
            }
        })
        .collect();

    let duplicate_vertices_tolerance = near_duplicates(mesh, 1e-6 * diag);
    let duplicate_faces = {
        let mut sorted: Vec<[u32; 3]> = mesh
            .faces()
            .map(|(_, mut t)| {
                t.sort_unstable();
                t
            })
            .filter(|t| t[0] != t[1] && t[1] != t[2])
            .collect();
        let n = sorted.len();
        sorted.sort_unstable();
        sorted.dedup();
        n - sorted.len()
    };
    let non_finite = !surface_area.is_finite()
        || !volume6.is_finite()
        || boundary_loops
            .iter()
            .any(|l: &LoopSummary| l.perimeter.is_none());

    let is_closed = !mesh.is_empty() && topo.boundary_edges == 0;
    let is_manifold = topo.non_manifold_edges.is_empty() && topo.non_manifold_vertices == 0;
    let volume_status = if mesh.is_empty() {
        VolumeStatus::Empty
    } else if non_finite {
        VolumeStatus::NonFinite
    } else if !is_manifold {
        VolumeStatus::NonManifold
    } else if !is_closed {
        VolumeStatus::Open
    } else if topo.inconsistent_edges > 0 {
        VolumeStatus::InconsistentOrientation
    } else {
        VolumeStatus::Ok
    };
    let signed_volume = (volume_status == VolumeStatus::Ok).then_some(volume6 / 6.0);

    Diagnostics {
        triangle_count: mesh.triangle_count(),
        vertex_count: mesh.vertex_count(),
        components: topo.components.len(),
        boundary_edges: topo.boundary_edges,
        boundary_loops,
        untraced_boundary_edges: topo.untraced_boundary_edges,
        non_manifold_edges: topo.non_manifold_edges.len(),
        non_manifold_vertices: topo.non_manifold_vertices,
        degenerate_triangles: degenerate,
        duplicate_vertices_exact,
        duplicate_vertices_tolerance,
        duplicate_faces,
        inconsistent_orientation_edges: topo.inconsistent_edges,
        bounding_box: bbox,
        surface_area: surface_area.is_finite().then_some(surface_area),
        non_finite,
        signed_volume,
        volume_status,
        is_closed,
        is_manifold,
    }
}

/// Count live vertices lying within `tolerance` of a lower-ID live vertex,
/// using a uniform grid (cell = tolerance). The count doesn't depend on hash
/// iteration order (MF-ISSUE-012).
fn near_duplicates(mesh: &Mesh, tolerance: f64) -> usize {
    if !(tolerance > 0.0 && tolerance.is_finite()) {
        return 0;
    }
    let cell = |p: [f64; 3]| p.map(|c| (c / tolerance).floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<[f64; 3]>> = HashMap::new();
    let mut count = 0;
    for (_, p) in mesh.vertices() {
        let c = cell(p);
        let mut dup = false;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(pts) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz])
                        && pts.iter().any(|q| norm(sub(p, *q)) <= tolerance)
                    {
                        dup = true;
                        break 'search;
                    }
                }
            }
        }
        if dup {
            count += 1;
        }
        grid.entry(c).or_default().push(p);
    }
    count
}
