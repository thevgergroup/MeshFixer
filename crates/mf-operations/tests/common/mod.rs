//! Helpers shared by the real-file tests (`real_scan.rs`, `messy_corpus.rs`).
//! Each test binary uses its own subset.
#![allow(dead_code)]

use std::collections::HashMap;

use mf_geometry::{Mesh, Provenance};

/// Faces on each undirected edge.
pub fn edge_faces(m: &Mesh) -> HashMap<(u32, u32), u32> {
    let mut count = HashMap::new();
    for (_, t) in m.faces() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    count
}

/// Magnitude of a loop's vector area: ~0 for a slit (collinear vertices),
/// which can't be closed without zero-area triangles.
pub fn vector_area(m: &Mesh, vertices: &[u32]) -> f64 {
    let p: Vec<[f64; 3]> = vertices.iter().map(|&v| m.corner_position(v)).collect();
    let mut s = [0.0; 3];
    for k in 0..p.len() {
        let (a, b) = (p[k], p[(k + 1) % p.len()]);
        s[0] += a[1] * b[2] - a[2] * b[1];
        s[1] += a[2] * b[0] - a[0] * b[2];
        s[2] += a[0] * b[1] - a[1] * b[0];
    }
    0.5 * (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt()
}

/// MF-OP-003 for an additive edit (a hole fill): nothing removed, modified
/// or moved; every input face keeps its ID and indices, every input vertex
/// its exact position; and each new face's edges end up with exactly two
/// faces, so the patch meets the input without adding a non-manifold edge.
/// The one exception is a hole whose rim runs along an edge that was
/// already non-manifold (a tangled edge, MF-ISSUE-047): the patch adds its
/// single face there like on any rim edge, and such edges are counted in
/// the `Ok` value. `before_edges` is [`edge_faces`] of `before`. The first
/// violation is returned by name.
pub fn untouched_outside(
    before: &Mesh,
    before_edges: &HashMap<(u32, u32), u32>,
    after: &Mesh,
    prov: &Provenance,
) -> Result<usize, String> {
    if !prov.removed_faces.is_empty() || !prov.removed_vertices.is_empty() {
        return Err(format!(
            "removed {} faces and {} vertices",
            prov.removed_faces.len(),
            prov.removed_vertices.len()
        ));
    }
    if !prov.modified_faces.is_empty() || !prov.moved_vertices.is_empty() {
        return Err(format!(
            "modified {} faces and moved {} vertices",
            prov.modified_faces.len(),
            prov.moved_vertices.len()
        ));
    }
    for (f, t) in before.faces() {
        if after.face(f) != Some(t) {
            return Err(format!("face {f:?} changed"));
        }
    }
    for (v, p) in before.vertices() {
        let q = after.position(v).ok_or(format!("vertex {v:?} is gone"))?;
        if p.map(f64::to_bits) != q.map(f64::to_bits) {
            return Err(format!("vertex {v:?} moved"));
        }
    }
    let mut added: HashMap<(u32, u32), u32> = HashMap::new();
    for (f, _) in &prov.created_faces {
        let t = after
            .face(*f)
            .ok_or(format!("created face {f:?} is missing"))?;
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *added.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut tangled_rim = 0;
    for (e, n) in &added {
        let before = before_edges.get(e).copied().unwrap_or(0);
        if before >= 2 && *n == 1 {
            tangled_rim += 1;
            continue;
        }
        let total = n + before;
        if total != 2 {
            return Err(format!(
                "edge {e:?} has {total} faces after the fill ({before} before)"
            ));
        }
    }
    Ok(tangled_rim)
}
