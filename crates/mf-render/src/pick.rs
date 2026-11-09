//! Picking: boundary loops in screen space, faces by CPU ray cast. Ray
//! casts and occlusion tests go through a [`Bvh`] (MF-RENDER-004: < 16 ms
//! at 5M triangles). The brute-force versions here ([`ray_pick`], and
//! [`edge_occluded`] without a BVH) walk every live face: they are the
//! oracle the BVH is tested against, and what a caller uses, explicitly,
//! while a new revision's BVH is still being built.

use glam::{DMat4, DVec3, DVec4};
use mf_geometry::{FaceId, Mesh};

use mf_runtime::{Cancelled, RunContext};

use crate::{Bvh, OrbitCamera};

/// Hits nearer than this (along the ray) don't count: the ray's own start.
pub(crate) const HIT_MIN: f64 = 1e-12;

/// Möller–Trumbore, both sides: the distance along `o + t·d` at which it
/// crosses triangle `abc` (in `d`'s units), if it does. The one triangle
/// test the brute-force and BVH casts share, so they agree exactly.
#[inline]
pub(crate) fn intersect(o: DVec3, d: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let (e1, e2) = (b - a, c - a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv_det = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(e2.dot(q) * inv_det)
}

/// The unit direction and length from `from` to `to`, unless they coincide.
pub(crate) fn segment(from: DVec3, to: DVec3) -> Option<(DVec3, f64)> {
    let v = to - from;
    let len = v.length();
    (len > 0.0).then(|| (v / len, len))
}

/// True if face `t` (unless `skip`ped) lies strictly between `from` and
/// the point `len` along unit direction `d`.
pub(crate) fn blocks(
    mesh: &Mesh,
    t: &[u32; 3],
    from: DVec3,
    d: DVec3,
    len: f64,
    skip: &impl Fn(&[u32; 3]) -> bool,
) -> bool {
    if skip(t) {
        return false;
    }
    let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
    intersect(from, d, a, b, c).is_some_and(|t| t > HIT_MIN && t < len * (1.0 - 1e-9))
}

/// Pixel position (y down) of a world point, or `None` if it is behind the camera.
pub fn screen_position(
    cam: &OrbitCamera,
    origin: DVec3,
    size: (f64, f64),
    world: [f64; 3],
) -> Option<(f64, f64)> {
    let vp = cam.view_proj_f64(size.0 / size.1.max(1.0), origin);
    let p = DVec3::from(world) - origin;
    let clip = vp * DVec4::new(p.x, p.y, p.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(((ndc.x + 1.0) * 0.5 * size.0, (1.0 - ndc.y) * 0.5 * size.1))
}

/// Distance from `p` to segment `ab`, and the parameter of the closest point.
fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    (((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt(), t)
}

/// A boundary-loop edge within `tolerance` screen pixels of `px`, sorted by
/// screen distance (ties by loop ID). `model` places the mesh in world
/// space; screen projection happens in world space (MF-ISSUE-042). Doesn't
/// check occlusion: callers either own the only mesh in the scene (then
/// [`edge_occluded`] against that same mesh is enough, as [`pick_loop`]
/// does) or must check every object a click could land on (multi-object
/// picking, done by the caller — see `mf-desktop::worker::pick`).
pub struct LoopEdgeCandidate {
    pub loop_id: u32,
    /// Screen-space distance from `px` (pixels).
    pub screen_distance: f64,
    /// World-space point on the edge nearest `px`.
    pub point: DVec3,
    /// The edge's two mesh vertices (for an own-mesh occlusion check that
    /// must not let the edge's own adjoining faces hide it).
    pub endpoints: [u32; 2],
}

/// Every boundary-loop edge of `mesh` within `tolerance` screen pixels of
/// `px`, nearest first. O(n) over the mesh's topology.
pub fn loop_edge_candidates(
    mesh: &Mesh,
    model: &DMat4,
    cam: &OrbitCamera,
    origin: DVec3,
    size: (f64, f64),
    px: (f64, f64),
    tolerance: f64,
) -> Vec<LoopEdgeCandidate> {
    let topo = mesh.topology();
    let world = |v: u32| {
        (*model * DVec4::from((DVec3::from(mesh.corner_position(v)), 1.0)))
            .truncate()
            .to_array()
    };
    // (screen distance, loop id, edge endpoints, edge parameter)
    let mut candidates: Vec<(f64, u32, [u32; 2], f64)> = Vec::new();
    for l in &topo.boundary_loops {
        let n = l.vertices.len();
        for k in 0..n {
            let (va, vb) = (l.vertices[k], l.vertices[(k + 1) % n]);
            let a = screen_position(cam, origin, size, world(va));
            let b = screen_position(cam, origin, size, world(vb));
            let (Some(a), Some(b)) = (a, b) else { continue };
            let (d, t) = point_segment_distance(px, a, b);
            if d <= tolerance {
                candidates.push((d, l.id, [va, vb], t));
            }
        }
    }
    candidates.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    candidates
        .into_iter()
        .map(|(d, loop_id, endpoints, t)| {
            let (a, b) = (
                DVec3::from(world(endpoints[0])),
                DVec3::from(world(endpoints[1])),
            );
            LoopEdgeCandidate {
                loop_id,
                screen_distance: d,
                point: a + (b - a) * t,
                endpoints,
            }
        })
        .collect()
}

/// One of the given edges near a click (see [`edge_candidates`]).
pub struct EdgeCandidate {
    /// Its index in the edges given.
    pub index: usize,
    /// Screen-space distance from `px` (pixels).
    pub screen_distance: f64,
    /// World-space point on the edge nearest `px`.
    pub point: DVec3,
    pub endpoints: [u32; 2],
}

/// Every edge of `edges` (vertex pairs of `mesh`, placed by `model`) within
/// `tolerance` screen pixels of `px`, nearest first (ties by index). For
/// clicking on edges the view draws, such as non-manifold ones. Doesn't
/// check occlusion (see [`edge_occluded`]). O(edges).
#[allow(clippy::too_many_arguments)]
pub fn edge_candidates(
    mesh: &Mesh,
    model: &DMat4,
    cam: &OrbitCamera,
    origin: DVec3,
    size: (f64, f64),
    px: (f64, f64),
    tolerance: f64,
    edges: &[[u32; 2]],
) -> Vec<EdgeCandidate> {
    let world =
        |v: u32| (*model * DVec4::from((DVec3::from(mesh.corner_position(v)), 1.0))).truncate();
    let mut out: Vec<EdgeCandidate> = edges
        .iter()
        .enumerate()
        .filter_map(|(index, &[va, vb])| {
            let (a, b) = (world(va), world(vb));
            let sa = screen_position(cam, origin, size, a.to_array())?;
            let sb = screen_position(cam, origin, size, b.to_array())?;
            let (d, t) = point_segment_distance(px, sa, sb);
            (d <= tolerance).then(|| EdgeCandidate {
                index,
                screen_distance: d,
                point: a + (b - a) * t,
                endpoints: [va, vb],
            })
        })
        .collect();
    out.sort_by(|x, y| {
        x.screen_distance
            .total_cmp(&y.screen_distance)
            .then(x.index.cmp(&y.index))
    });
    out
}

/// True if some face of `mesh` (other than one touching `skip_vertices`)
/// lies between `eye` and `point` (world space), hiding it. `model` places
/// `mesh` in world space; the test runs in object space. With a `bvh`
/// (built for this `mesh`) it visits only the faces near the segment;
/// without one it walks every face (the oracle, and the explicit path for
/// a revision whose BVH isn't built yet). Both give the same answer.
pub fn edge_occluded(
    mesh: &Mesh,
    bvh: Option<&Bvh>,
    model: &DMat4,
    eye: DVec3,
    point: DVec3,
    skip_vertices: [u32; 2],
) -> bool {
    edge_occluded_with(
        mesh,
        bvh,
        model,
        eye,
        point,
        skip_vertices,
        &RunContext::none(),
    )
    .expect("a context that is never cancelled can't be cancelled")
}

/// [`edge_occluded`], cancellable: without a BVH it walks every face
/// (O(n), ~10 ms at 5M), checking `ctx` every [`mf_runtime::TICK_INTERVAL`]
/// faces, so a newer click can supersede it.
pub fn edge_occluded_with(
    mesh: &Mesh,
    bvh: Option<&Bvh>,
    model: &DMat4,
    eye: DVec3,
    point: DVec3,
    skip_vertices: [u32; 2],
    ctx: &RunContext,
) -> Result<bool, Cancelled> {
    ctx.checkpoint()?;
    let inv = model.inverse();
    let from = (inv * DVec4::from((eye, 1.0))).truncate();
    let to = (inv * DVec4::from((point, 1.0))).truncate();
    let skip = |tri: &[u32; 3]| tri.contains(&skip_vertices[0]) || tri.contains(&skip_vertices[1]);
    match bvh {
        Some(bvh) => Ok(bvh.occluded(mesh, from, to, skip)),
        None => {
            let Some((d, len)) = segment(from, to) else {
                return Ok(false);
            };
            let mut ticker = ctx.ticker();
            for (_, t) in mesh.faces() {
                ticker.tick()?;
                if blocks(mesh, &t, from, d, len, &skip) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

/// The traced boundary loop with a *visible* edge nearest to `px` on screen,
/// if within `tolerance` pixels. Ties go to the lower loop ID. An edge is
/// hidden when a face that doesn't touch it lies between the eye and the
/// edge point under the cursor. `model` places the mesh (object space) in
/// world space (MF-ISSUE-042); screen projection and the occlusion ray cast
/// both happen in world space. For a scene with more than this one mesh,
/// use [`loop_edge_candidates`] and [`edge_occluded`] directly so every
/// object in the scene can occlude a candidate, not just this one. Uses the
/// mesh's topology (O(n) to compute, then cached) and a BVH occlusion test
/// per candidate (`bvh` is `mesh`'s): call it off the UI thread.
#[allow(clippy::too_many_arguments)]
pub fn pick_loop(
    mesh: &Mesh,
    bvh: &Bvh,
    model: &DMat4,
    cam: &OrbitCamera,
    origin: DVec3,
    size: (f64, f64),
    px: (f64, f64),
    tolerance: f64,
) -> Option<u32> {
    let eye = cam.eye();
    loop_edge_candidates(mesh, model, cam, origin, size, px, tolerance)
        .into_iter()
        .find(|c| !edge_occluded(mesh, Some(bvh), model, eye, c.point, c.endpoints))
        .map(|c| c.loop_id)
}

/// Nearest live face hit by the ray (Möller–Trumbore, both sides), with its
/// distance along the ray, by brute force over every live face: the
/// oracle for [`Bvh::ray_pick`] (which finds the same), and the explicit
/// path while a revision's BVH is still being built. `model` places the
/// mesh in world space; `origin` and `dir` are world-space, and the
/// reported distance is along the world-space ray (MF-ISSUE-042). Ties go
/// to the lower face ID.
pub fn ray_pick(
    mesh: &Mesh,
    model: &DMat4,
    origin: [f64; 3],
    dir: [f64; 3],
) -> Option<(FaceId, f64)> {
    ray_pick_with(mesh, model, origin, dir, &RunContext::none())
        .expect("a context that is never cancelled can't be cancelled")
}

/// [`ray_pick`], cancellable: it checks `ctx` every
/// [`mf_runtime::TICK_INTERVAL`] faces, so a newer click can supersede a
/// brute-force cast over a big mesh.
pub fn ray_pick_with(
    mesh: &Mesh,
    model: &DMat4,
    origin: [f64; 3],
    dir: [f64; 3],
    ctx: &RunContext,
) -> Result<Option<(FaceId, f64)>, Cancelled> {
    ctx.checkpoint()?;
    // The ray is cast in object space (so it walks the mesh's own
    // triangles); world-space `dir` is a unit vector, and object space may
    // rescale it, so distances are converted back by `scale`.
    let inv = model.inverse();
    let o = (inv * DVec4::from((DVec3::from(origin), 1.0))).truncate();
    let d_obj = (inv * DVec4::from((DVec3::from(dir), 0.0))).truncate();
    let scale = d_obj.length();
    if scale < 1e-18 {
        return Ok(None);
    }
    let d = d_obj / scale;
    let mut best: Option<(FaceId, f64)> = None;
    let mut ticker = ctx.ticker();
    for (id, t) in mesh.faces() {
        ticker.tick()?;
        let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
        let Some(raw) = intersect(o, d, a, b, c) else {
            continue;
        };
        let dist = raw / scale;
        if dist > HIT_MIN && best.is_none_or(|(_, bt)| dist < bt) {
            best = Some((id, dist));
        }
    }
    Ok(best)
}
