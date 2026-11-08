//! Picking (M3): boundary loops in screen space, faces by CPU ray cast.
//! The ray cast is brute force over live faces; M4 adds acceleration for
//! the < 16 ms budget at 5M triangles (MF-RENDER-004).

use glam::{DVec3, DVec4};
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

fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// The traced boundary loop with an edge nearest to `px` on screen, if
/// within `tolerance` pixels. Ties go to the lower loop ID. Uses the mesh's
/// topology: call when it is cached, as it is O(n) to compute.
pub fn pick_loop(
    mesh: &Mesh,
    cam: &OrbitCamera,
    origin: DVec3,
    size: (f64, f64),
    px: (f64, f64),
    tolerance: f64,
) -> Option<u32> {
    let topo = mesh.topology();
    let mut best: Option<(f64, u32)> = None;
    for l in &topo.boundary_loops {
        let n = l.vertices.len();
        for k in 0..n {
            let a = screen_position(cam, origin, size, mesh.corner_position(l.vertices[k]));
            let b = screen_position(
                cam,
                origin,
                size,
                mesh.corner_position(l.vertices[(k + 1) % n]),
            );
            let (Some(a), Some(b)) = (a, b) else { continue };
            let d = point_segment_distance(px, a, b);
            if d <= tolerance && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, l.id));
            }
        }
    }
    best.map(|(_, id)| id)
}

/// Nearest live face hit by the ray (Möller–Trumbore, both sides), with its
/// distance along the ray. Ties go to the lower face ID.
pub fn ray_pick(mesh: &Mesh, origin: [f64; 3], dir: [f64; 3]) -> Option<(FaceId, f64)> {
    let o = DVec3::from(origin);
    let d = DVec3::from(dir);
    let mut best: Option<(FaceId, f64)> = None;
    for (id, t) in mesh.faces() {
        let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
        let (e1, e2) = (b - a, c - a);
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-18 {
            continue;
        }
        let inv = 1.0 / det;
        let s = o - a;
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = d.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let dist = e2.dot(q) * inv;
        if dist > 1e-12 && best.is_none_or(|(_, bt)| dist < bt) {
            best = Some((id, dist));
        }
    }
    best
}
