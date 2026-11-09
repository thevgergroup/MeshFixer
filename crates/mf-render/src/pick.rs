//! Picking (M3): boundary loops in screen space, faces by CPU ray cast.
//! The ray cast is brute force over live faces; M4 adds acceleration for
//! the < 16 ms budget at 5M triangles (MF-RENDER-004).

use glam::{DMat4, DVec3, DVec4};
use mf_geometry::{FaceId, Mesh};

use crate::OrbitCamera;

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
/// lies between `eye` and `point`, hiding it. `model` places `mesh` in
/// world space.
pub fn edge_occluded(
    mesh: &Mesh,
    model: &DMat4,
    eye: DVec3,
    point: DVec3,
    skip_vertices: [u32; 2],
) -> bool {
    let to = point - eye;
    let len = to.length();
    if len == 0.0 {
        return false;
    }
    let hit = first_hit(mesh, model, eye.to_array(), (to / len).to_array(), |tri| {
        tri.contains(&skip_vertices[0]) || tri.contains(&skip_vertices[1])
    });
    hit.is_some_and(|(_, dist)| dist < len * (1.0 - 1e-9))
}

/// The traced boundary loop with a *visible* edge nearest to `px` on screen,
/// if within `tolerance` pixels. Ties go to the lower loop ID. An edge is
/// hidden when a face that doesn't touch it lies between the eye and the
/// edge point under the cursor. `model` places the mesh (object space) in
/// world space (MF-ISSUE-042); screen projection and the occlusion ray cast
/// both happen in world space. For a scene with more than this one mesh,
/// use [`loop_edge_candidates`] and [`edge_occluded`] directly so every
/// object in the scene can occlude a candidate, not just this one. Uses the
/// mesh's topology (O(n) to compute) and a ray cast per candidate (O(faces)):
/// call it off the UI thread.
pub fn pick_loop(
    mesh: &Mesh,
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
        .find(|c| !edge_occluded(mesh, model, eye, c.point, c.endpoints))
        .map(|c| c.loop_id)
}

/// Nearest live face hit by the ray (Möller–Trumbore, both sides), with its
/// distance along the ray. `model` places the mesh in world space; `origin`
/// and `dir` are world-space, and the reported distance is along the
/// world-space ray (MF-ISSUE-042). Ties go to the lower face ID.
pub fn ray_pick(
    mesh: &Mesh,
    model: &DMat4,
    origin: [f64; 3],
    dir: [f64; 3],
) -> Option<(FaceId, f64)> {
    first_hit(mesh, model, origin, dir, |_| false)
}

/// Nearest hit among faces for which `skip` is false. The ray is transformed
/// into object space with `model`'s inverse (so the brute-force cast still
/// walks the mesh's own triangles); the returned distance is rescaled back
/// to the world-space ray `dir` gave.
fn first_hit(
    mesh: &Mesh,
    model: &DMat4,
    origin: [f64; 3],
    dir: [f64; 3],
    skip: impl Fn(&[u32; 3]) -> bool,
) -> Option<(FaceId, f64)> {
    let inv = model.inverse();
    let o = (inv * DVec4::from((DVec3::from(origin), 1.0))).truncate();
    let d_obj = (inv * DVec4::from((DVec3::from(dir), 0.0))).truncate();
    // World-space `dir` is a unit vector; object space may rescale it, so
    // distances along `d_obj` are converted back by this factor.
    let scale = d_obj.length();
    if scale < 1e-18 {
        return None;
    }
    let d = d_obj / scale;
    let mut best: Option<(FaceId, f64)> = None;
    for (id, t) in mesh.faces() {
        if skip(&t) {
            continue;
        }
        let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
        let (e1, e2) = (b - a, c - a);
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-18 {
            continue;
        }
        let inv_det = 1.0 / det;
        let s = o - a;
        let u = s.dot(p) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = d.dot(q) * inv_det;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let dist = e2.dot(q) * inv_det / scale;
        if dist > 1e-12 && best.is_none_or(|(_, bt)| dist < bt) {
            best = Some((id, dist));
        }
    }
    best
}
