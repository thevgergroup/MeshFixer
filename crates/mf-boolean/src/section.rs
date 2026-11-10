//! Section triangulation (D3): `spade`'s constrained Delaunay
//! triangulation (exact predicates), with regions chosen by winding number.
//!
//! Loops are oriented: an outer boundary runs counter-clockwise and a hole
//! clockwise, so a point's winding number (the loops' signed turns around
//! it) is 1 inside the section and 0 in a hole or outside. The winding of
//! every triangle is found by walking the triangulation from the outer face
//! (winding 0), adding the signed count of constraint edges crossed. A
//! vertex is never added (no Steiner points): every output triangle joins
//! three input points.

use std::collections::HashMap;

use spade::handles::FixedVertexHandle;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

/// Why a section can't be triangulated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SectionError {
    /// Loops cross or touch each other or themselves (two edges meet other
    /// than at a shared end, a point repeats, or a point lies on an edge).
    #[error("the section's outline crosses or touches itself")]
    SelfIntersects,
    /// A coordinate spade can't take (not finite, or out of its range), or
    /// fewer than three points.
    #[error("the section has no area or an unusable coordinate")]
    Degenerate,
}

/// Triangulates the region the oriented `loops` enclose (outer loops
/// counter-clockwise, holes clockwise). Triangles are counter-clockwise and
/// index the loops' points in order (loop 0's first, then loop 1's...).
pub fn triangulate_section(loops: &[Vec<[f64; 2]>]) -> Result<Vec<[u32; 3]>, SectionError> {
    triangulate_section_at(loops).map_err(|(e, _)| e)
}

/// [`triangulate_section`], with the index of a point near the fault (in
/// the loops' concatenated points) on failure. Used by the plane cut to
/// say where to move the plane.
pub(crate) fn triangulate_section_at(
    loops: &[Vec<[f64; 2]>],
) -> Result<Vec<[u32; 3]>, (SectionError, usize)> {
    let mut points = Vec::new();
    let mut edges = Vec::new();
    for l in loops {
        if l.len() < 3 {
            return Err((SectionError::Degenerate, points.len()));
        }
        let base = points.len();
        points.extend_from_slice(l);
        for i in 0..l.len() {
            edges.push((base + i, base + (i + 1) % l.len()));
        }
    }
    let mut out = Vec::new();
    for (t, w) in winding_faces(&points, &edges)? {
        match w {
            0 => {}
            1 => out.push(t.map(|i| i as u32)),
            // Nested loops of one orientation, or a hole outside every
            // outer loop: the outline overlaps itself.
            _ => return Err((SectionError::SelfIntersects, t[0])),
        }
    }
    if out.is_empty() {
        return Err((SectionError::Degenerate, 0));
    }
    Ok(out)
}

/// A section error and the index of a point near it.
pub(crate) type Fault = (SectionError, usize);

/// The constrained Delaunay triangulation of `points` with the directed
/// constraint `edges`, and each triangle's winding number: the signed count
/// of constraint edges crossed on the way in from outside (an edge `a → b`
/// counts +1 for the triangles to its left). Triangles are
/// counter-clockwise, as indices into `points`. An edge listed twice in
/// the same direction counts twice; in opposite directions, zero.
pub(crate) fn winding_faces(
    points: &[[f64; 2]],
    edges: &[(usize, usize)],
) -> Result<Vec<([usize; 3], i32)>, Fault> {
    let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
    let mut handles: Vec<FixedVertexHandle> = Vec::with_capacity(points.len());
    let mut owner: HashMap<usize, usize> = HashMap::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let h = cdt
            .insert(Point2::new(p[0], p[1]))
            .map_err(|_| (SectionError::Degenerate, i))?;
        if owner.insert(h.index(), i).is_some() {
            // Two points at one place.
            return Err((SectionError::SelfIntersects, i));
        }
        handles.push(h);
    }
    // Net directed count per undirected edge (low handle → high handle).
    let mut net: HashMap<(usize, usize), i32> = HashMap::new();
    for &(a, b) in edges {
        let (ha, hb) = (handles[a], handles[b]);
        if ha == hb {
            return Err((SectionError::SelfIntersects, a));
        }
        let key = (ha.index().min(hb.index()), ha.index().max(hb.index()));
        if !net.contains_key(&key) {
            if !cdt.can_add_constraint(ha, hb) {
                return Err((SectionError::SelfIntersects, a));
            }
            cdt.add_constraint(ha, hb);
            // A point exactly on the edge splits it: the outline touches
            // itself there.
            if cdt.get_edge_from_neighbors(ha, hb).is_none() {
                return Err((SectionError::SelfIntersects, a));
            }
        }
        *net.entry(key).or_insert(0) += if ha.index() < hb.index() { 1 } else { -1 };
    }
    // Signed count for crossing directed edge from → to, left to right
    // (positive counts are for edges whose left is inside).
    let along = |from: usize, to: usize| -> i32 {
        let key = (from.min(to), from.max(to));
        let n = net.get(&key).copied().unwrap_or(0); // allow-swallow: not a constraint edge, so it counts zero
        if from < to { n } else { -n }
    };
    let mut winding: HashMap<usize, i32> = HashMap::new();
    let mut queue: Vec<usize> = Vec::new();
    // Faces on the hull: entered from the outer face (winding 0).
    for face in cdt.inner_faces() {
        for e in face.adjacent_edges() {
            if e.rev().face().as_inner().is_none() {
                let w = along(e.from().fix().index(), e.to().fix().index());
                let id = face.fix().index();
                match winding.insert(id, w) {
                    Some(old) if old != w => {
                        return Err((SectionError::SelfIntersects, owner[&e.from().fix().index()]));
                    }
                    Some(_) => {}
                    None => queue.push(id),
                }
            }
        }
    }
    let faces: HashMap<usize, _> = cdt.inner_faces().map(|f| (f.fix().index(), f)).collect();
    while let Some(id) = queue.pop() {
        let w = winding[&id];
        let face = faces[&id];
        for e in face.adjacent_edges() {
            let Some(other) = e.rev().face().as_inner() else {
                continue;
            };
            // Crossing e (this face on its left) to its right.
            let wo = w - along(e.from().fix().index(), e.to().fix().index());
            let oid = other.fix().index();
            match winding.insert(oid, wo) {
                Some(old) if old != wo => {
                    return Err((SectionError::SelfIntersects, owner[&e.from().fix().index()]));
                }
                Some(_) => {}
                None => queue.push(oid),
            }
        }
    }
    let mut out = Vec::with_capacity(faces.len());
    for face in cdt.inner_faces() {
        let Some(&w) = winding.get(&face.fix().index()) else {
            return Err((SectionError::Degenerate, 0));
        };
        let t = face.vertices().map(|v| owner[&v.fix().index()]);
        out.push((t, w));
    }
    // Deterministic order (spade's face order already is, but sort so it
    // never depends on hash iteration).
    out.sort_unstable();
    Ok(out)
}
