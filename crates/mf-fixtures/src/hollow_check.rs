//! Checks for a `mesh.hollow` result, shared by the command-layer and
//! GUI acceptance tests (owner 2026-11-14): closed and manifold; the
//! wall, measured, is the setting within a cell; the outer surface is
//! unchanged apart from the openings; an open-through rim stays at the
//! cut's own height. Plus the Stanford bunny, stood up at 100 mm.
#![allow(clippy::unwrap_used, clippy::panic)] // test support: a failed check panics

use mf_geometry::{Bvh, FaceId, Mesh, validate_solid};
use mf_runtime::RunContext;

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Distance from `p` to triangle `t` (Ericson's closest-point regions).
pub fn to_triangle(p: [f64; 3], t: [[f64; 3]; 3]) -> f64 {
    let [a, b, c] = t;
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    let q = if d1 <= 0.0 && d2 <= 0.0 {
        a
    } else {
        let bp = sub(p, b);
        let (d3, d4) = (dot(ab, bp), dot(ac, bp));
        let cp = sub(p, c);
        let (d5, d6) = (dot(ab, cp), dot(ac, cp));
        let (vc, vb, va) = (d1 * d4 - d3 * d2, d5 * d2 - d1 * d6, d3 * d6 - d5 * d4);
        if d3 >= 0.0 && d4 <= d3 {
            b
        } else if d6 >= 0.0 && d5 <= d6 {
            c
        } else if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            let v = d1 / (d1 - d3);
            std::array::from_fn(|k| a[k] + v * ab[k])
        } else if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            let w = d2 / (d2 - d6);
            std::array::from_fn(|k| a[k] + w * ac[k])
        } else if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
            std::array::from_fn(|k| b[k] + w * (c[k] - b[k]))
        } else {
            let denom = 1.0 / (va + vb + vc);
            let (v, w) = (vb * denom, vc * denom);
            std::array::from_fn(|k| a[k] + ab[k] * v + ac[k] * w)
        }
    };
    norm(sub(p, q))
}

/// Distance from points to a mesh's surface, within a reach.
pub struct Surface<'a> {
    mesh: &'a Mesh,
    bvh: Bvh,
}

impl<'a> Surface<'a> {
    pub fn new(mesh: &'a Mesh) -> Self {
        Surface {
            mesh,
            bvh: Bvh::build(mesh, &RunContext::none()).unwrap(),
        }
    }

    /// The distance from `p` to the surface, or `reach` if it's farther.
    pub fn distance(&self, p: [f64; 3], reach: f64) -> f64 {
        let mut best = reach;
        self.bvh.for_each_overlapping(
            std::array::from_fn(|k| p[k] - reach),
            std::array::from_fn(|k| p[k] + reach),
            |f| {
                let t = self
                    .mesh
                    .face(f)
                    .unwrap()
                    .map(|v| self.mesh.corner_position(v));
                best = best.min(to_triangle(p, t));
            },
        );
        best
    }
}

/// What was opened, to tell the openings apart from the rest of the
/// outer surface.
pub enum Opening<'a> {
    None,
    /// Drain holes: their outside point and axis (object space), and
    /// their diameter.
    Holes(Vec<([f64; 3], [f64; 3])>, f64),
    /// Open through a flat face lying at `z`, facing `side` (+1 up, -1
    /// down); its faces in the input.
    Through {
        z: f64,
        side: f64,
        faces: &'a [u32],
    },
}

impl Opening<'_> {
    /// Whether `p` (object space) lies in an opening's own zone: a
    /// drain hole's cylinder (with a cell's grace), or the open-through
    /// rim's band under the cut, `reach` deep.
    pub fn near(&self, p: [f64; 3], wall: f64, reach: f64) -> bool {
        match self {
            Opening::None => false,
            Opening::Holes(holes, diameter) => holes.iter().any(|&(at, axis)| {
                let d = sub(p, at);
                let along = dot(d, axis);
                let off = norm(sub(d, axis.map(|c| c * along)));
                off <= diameter / 2.0 + reach
            }),
            Opening::Through { z, side, .. } => side * (z - p[2]) < wall + reach,
        }
    }
}

/// Checks a Hollow result `out` against its input `before` (both object
/// space), `cell` the hollow's own cell size: closed and manifold; the
/// wall, measured at every inner vertex away from the openings, equal to
/// `wall` within a cell; every outer face away from the openings still
/// there, unmoved; an open-through rim at the cut's own height.
pub fn check(what: &str, before: &Mesh, out: &Mesh, wall: f64, cell: f64, opening: &Opening) {
    let all: Vec<FaceId> = out.faces().map(|(f, _)| f).collect();
    let bvh = Bvh::build(out, &RunContext::none()).unwrap();
    validate_solid(out, &all, &bvh, &RunContext::none())
        .unwrap()
        .unwrap_or_else(|e| panic!("{what}: not a closed, manifold solid: {e}"));
    let topo = out.topology_with(&RunContext::none()).unwrap();
    assert!(
        topo.boundary_edges == 0 && topo.non_manifold_edges.is_empty(),
        "{what}: {topo:?}"
    );

    // The wall: every new vertex (not on the input's own surface), away
    // from the openings, is the wall's own depth in.
    let outer = Surface::new(before);
    let reach = wall + 4.0 * cell;
    let (mut measured, mut lo, mut hi) = (0usize, f64::INFINITY, 0.0_f64);
    let kept: std::collections::HashSet<[u64; 3]> = before
        .vertices()
        .map(|(_, p)| p.map(f64::to_bits))
        .collect();
    for (_, q) in out.vertices() {
        if kept.contains(&q.map(f64::to_bits)) {
            continue;
        }
        let d = outer.distance(q, reach);
        if d < 1e-6 || opening.near(q, wall, 2.0 * cell) {
            continue;
        }
        measured += 1;
        lo = lo.min(d);
        hi = hi.max(d);
    }
    assert!(measured > 0, "{what}: no inner surface to measure");
    assert!(
        lo >= wall - cell && hi <= wall + cell,
        "{what}: the wall measures {lo}..{hi}, not {wall} within a cell ({cell})"
    );

    // The outer surface: every input face away from the openings still
    // lies on the result's surface, where it was.
    let result = Surface::new(out);
    let picked: std::collections::HashSet<u32> = match opening {
        Opening::Through { faces, .. } => faces.iter().copied().collect(),
        _ => Default::default(),
    };
    for (f, tri) in before.faces() {
        let corners = tri.map(|v| before.corner_position(v));
        let c: [f64; 3] =
            std::array::from_fn(|k| (corners[0][k] + corners[1][k] + corners[2][k]) / 3.0);
        if picked.contains(&f.0) || opening.near(c, wall, cell) {
            continue;
        }
        if out.face(f) == Some(tri)
            && out.face(f).map(|t| t.map(|v| out.corner_position(v))) == Some(corners)
        {
            continue;
        }
        let d = result.distance(c, 1.0);
        assert!(
            d < 1e-9,
            "{what}: outer face {f:?} away from the openings moved or went ({d} off)"
        );
    }

    if let Opening::Through { z, side, .. } = opening {
        let rim = out
            .vertices()
            .map(|(_, q)| side * q[2])
            .fold(f64::NEG_INFINITY, f64::max);
        assert_eq!(
            rim,
            side * z,
            "{what}: the rim isn't at the cut's own height"
        );
        assert_eq!(
            topo.components.len(),
            1,
            "{what}: the opening should join the inside to the outside"
        );
    }
}

/// The faces of `mesh` lying flat at `z` (object space), facing `side`.
pub fn flat_at(mesh: &Mesh, z: f64, side: f64) -> Vec<u32> {
    mesh.faces()
        .filter(|(_, t)| {
            let p = t.map(|v| mesh.corner_position(v));
            let up = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            up * side > 0.0 && p.iter().all(|q| q[2] == z)
        })
        .map(|(f, _)| f.0)
        .collect()
}

/// The Stanford bunny's scan (y-up, in metres) stood up z-up and scaled
/// to 100 mm across, as a fresh mesh.
pub fn stood_bunny(mesh: &Mesh) -> Mesh {
    let bb = mesh.bounding_box().unwrap();
    let size = (0..3).map(|k| bb.max[k] - bb.min[k]).fold(0.0, f64::max);
    let k = 100.0 / size;
    let mut remap = vec![0u32; mesh.vertex_slots() as usize];
    let mut positions = Vec::new();
    for (i, (v, p)) in mesh.vertices().enumerate() {
        remap[v.0 as usize] = i as u32;
        positions.push([p[0] * k, -p[2] * k, p[1] * k]);
    }
    let triangles: Vec<[u32; 3]> = mesh
        .faces()
        .map(|(_, t)| t.map(|v| remap[v as usize]))
        .collect();
    Mesh::new(positions, triangles).unwrap()
}

/// The drain hole a Hollow with Auto holes (one, the default count) cut,
/// found from the result alone (for a GUI flow, which has no report to
/// read): the middle of the new vertices lying on the input's own
/// surface (the hole's rim, where it meets the outside), along `axis`
/// (Auto's own: straight down, the plate's -z, in object space for an
/// unrotated object). Empty if there's no such rim.
pub fn holes_found(before: &Mesh, out: &Mesh, axis: [f64; 3]) -> Vec<([f64; 3], [f64; 3])> {
    let outer = Surface::new(before);
    let kept: std::collections::HashSet<[u64; 3]> = before
        .vertices()
        .map(|(_, p)| p.map(f64::to_bits))
        .collect();
    let rim: Vec<[f64; 3]> = out
        .vertices()
        .map(|(_, p)| p)
        .filter(|p| !kept.contains(&p.map(f64::to_bits)) && outer.distance(*p, 1.0) < 1e-6)
        .collect();
    if rim.is_empty() {
        return Vec::new();
    }
    let n = rim.len() as f64;
    let at: [f64; 3] = std::array::from_fn(|k| rim.iter().map(|c| c[k]).sum::<f64>() / n);
    vec![(at, axis)]
}
