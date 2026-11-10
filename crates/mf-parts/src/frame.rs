//! Where a part goes on a surface (MF-MODEL-004): a [`SurfaceFrame`] at a
//! picked point and [`place`], which turns a part's [`Attach`] into the
//! transform that puts it there. Connectors (MF-MODEL-005) reuse both.

use mf_geometry::{Bvh, FaceId, Mesh};

use crate::manifest::Attach;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalized(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 0.0 && l.is_finite()).then(|| a.map(|c| c / l))
}
fn world(t: &[f64; 12], p: V3) -> V3 {
    std::array::from_fn(|c| t[c] * p[0] + t[3 + c] * p[1] + t[6 + c] * p[2] + t[9 + c])
}

/// A point on a surface with its outward normal and a tangent, in world
/// space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceFrame {
    pub point: V3,
    pub normal: V3,
    pub tangent: V3,
}

impl SurfaceFrame {
    /// The frame at `bary` on `face` of `mesh` placed by `transform`. The
    /// normal is area-weighted over the faces with a corner within
    /// `radius` (world) of the point, so it's stable on noisy scans. The
    /// tangent is world +y projected onto the surface (or +z where the
    /// surface faces ±y); [`SurfaceFrame::with_up`] uses the camera's up.
    /// Scans every face: see [`SurfaceFrame::at_indexed`] to use a BVH.
    pub fn at(
        mesh: &Mesh,
        transform: &[f64; 12],
        face: FaceId,
        bary: V3,
        radius: f64,
    ) -> SurfaceFrame {
        Self::build(mesh, transform, face, bary, radius, |_, f| {
            mesh.faces().for_each(|(id, _)| f(id))
        })
    }

    /// As [`SurfaceFrame::at`], finding the nearby faces with the mesh's
    /// BVH (object space; only for transforms that don't shear much).
    pub fn at_indexed(
        mesh: &Mesh,
        bvh: &Bvh,
        transform: &[f64; 12],
        face: FaceId,
        bary: V3,
        radius: f64,
    ) -> SurfaceFrame {
        let det = transform[0] * (transform[4] * transform[8] - transform[5] * transform[7])
            - transform[1] * (transform[3] * transform[8] - transform[5] * transform[6])
            + transform[2] * (transform[3] * transform[7] - transform[4] * transform[6]);
        // The smallest scale is at least |det| / (largest scale)², and the
        // largest is at most the Frobenius norm: a safe object-space radius.
        let frob = transform[..9].iter().map(|x| x * x).sum::<f64>().sqrt();
        let object_radius = radius * frob * frob / det.abs().max(f64::MIN_POSITIVE);
        Self::build(mesh, transform, face, bary, radius, |p, f| {
            bvh.for_each_overlapping(
                std::array::from_fn(|k| p[k] - object_radius),
                std::array::from_fn(|k| p[k] + object_radius),
                f,
            )
        })
    }

    fn build(
        mesh: &Mesh,
        transform: &[f64; 12],
        face: FaceId,
        bary: V3,
        radius: f64,
        candidates: impl Fn(V3, &mut dyn FnMut(FaceId)),
    ) -> SurfaceFrame {
        let t = mesh.face(face).expect("the picked face is live");
        let corners = t.map(|v| mesh.corner_position(v));
        let object_point: V3 =
            std::array::from_fn(|k| (0..3).map(|i| bary[i] * corners[i][k]).sum());
        let point = world(transform, object_point);
        let det_sign = {
            let m = transform;
            let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
                + m[2] * (m[3] * m[7] - m[4] * m[6]);
            if det < 0.0 { -1.0 } else { 1.0 }
        };
        let face_normal = |t: [u32; 3]| {
            let [a, b, c] = t.map(|v| world(transform, mesh.corner_position(v)));
            cross(sub(b, a), sub(c, a)).map(|x| x * det_sign)
        };
        let mut sum = [0.0; 3];
        let mut ids = Vec::new();
        candidates(object_point, &mut |f| ids.push(f));
        ids.sort_unstable();
        ids.dedup();
        for f in ids {
            let Some(t) = mesh.face(f) else { continue };
            let near = t.iter().any(|&v| {
                let d = sub(world(transform, mesh.corner_position(v)), point);
                dot(d, d) <= radius * radius
            });
            if near {
                let n = face_normal(t);
                sum = [sum[0] + n[0], sum[1] + n[1], sum[2] + n[2]];
            }
        }
        let normal = normalized(sum)
            .or_else(|| normalized(face_normal(t)))
            .unwrap_or([0.0, 0.0, 1.0]);
        let mut frame = SurfaceFrame {
            point,
            normal,
            tangent: [0.0; 3],
        };
        frame.tangent = frame.tangent_from([0.0, 1.0, 0.0]);
        frame
    }

    /// The frame with its tangent from `up` (the camera's up vector),
    /// projected onto the surface.
    pub fn with_up(mut self, up: V3) -> SurfaceFrame {
        self.tangent = self.tangent_from(up);
        self
    }

    fn tangent_from(&self, up: V3) -> V3 {
        let n = self.normal;
        let project = |u: V3| normalized(sub(u, n.map(|x| x * dot(u, n))));
        project(up)
            .filter(|_| dot(up, n).abs() < 0.999)
            .or_else(|| project([0.0, 0.0, 1.0]).filter(|_| n[2].abs() < 0.999))
            .or_else(|| project([1.0, 0.0, 0.0]))
            .unwrap_or([1.0, 0.0, 0.0])
    }
}

/// The transform (3MF row-major, D7) that puts a part's attach point on
/// `frame.point`, its attach normal along `frame.normal`, and its up along
/// the frame's tangent turned `spin_deg` about the normal, scaled by
/// `scale`.
pub fn place(attach: &Attach, frame: &SurfaceFrame, scale: f64, spin_deg: f64) -> [f64; 12] {
    let basis = |n: V3, up: V3| -> [V3; 3] {
        let n = normalized(n).unwrap_or([0.0, 0.0, 1.0]);
        let up = normalized(sub(up, n.map(|x| x * dot(up, n)))).unwrap_or_else(|| {
            if n[0].abs() < 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            }
        });
        [up, cross(n, up), n]
    };
    let p = basis(attach.normal, attach.up);
    let (s, c) = spin_deg.to_radians().sin_cos();
    let (t, b) = (frame.tangent, cross(frame.normal, frame.tangent));
    let turned: V3 = std::array::from_fn(|k| c * t[k] + s * b[k]);
    let w = basis(frame.normal, turned);
    // R = Σ w_k p_kᵀ; 3MF rows are p's coordinates: M[r][c] = s · R[c][r].
    let r = |c: usize, rr: usize| (0..3).map(|k| w[k][c] * p[k][rr]).sum::<f64>();
    let mut m = [0.0; 12];
    for row in 0..3 {
        for col in 0..3 {
            m[3 * row + col] = scale * r(col, row);
        }
    }
    let moved = world(&m, attach.point);
    for k in 0..3 {
        m[9 + k] = frame.point[k] - moved[k];
    }
    m
}
