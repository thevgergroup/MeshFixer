use glam::{DMat4, DVec3, Mat4};
use mf_geometry::Aabb;

/// Z-up orbit camera (the 3D-printing convention). All maths is `f64`,
/// relative to the mesh's local origin, and only the final matrix is `f32`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitCamera {
    pub target: DVec3,
    pub distance: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub fov_y: f64,
    /// Radius of the framed content, used for clipping planes.
    pub radius: f64,
}

const MAX_PITCH: f64 = 1.55;

impl OrbitCamera {
    pub fn fit(aabb: &Aabb) -> OrbitCamera {
        let radius = (aabb.diagonal() * 0.5).max(1e-6);
        let fov_y = 40f64.to_radians();
        OrbitCamera {
            target: DVec3::from(aabb.center()),
            distance: radius / (fov_y * 0.5).sin() * 1.1,
            yaw: -60f64.to_radians(),
            pitch: 25f64.to_radians(),
            fov_y,
            radius,
        }
    }

    /// Re-centre on `aabb` and fit it, keeping the current view direction
    /// (instant; `F` frames the selection with this).
    pub fn frame(&mut self, aabb: &Aabb) {
        let fitted = OrbitCamera::fit(aabb);
        self.target = fitted.target;
        self.radius = fitted.radius;
        self.distance = fitted.radius / (self.fov_y * 0.5).sin() * 1.1;
    }

    /// Turn to look at the target from direction `from` (target → eye),
    /// keeping target and distance. Pitch stays short of the poles.
    pub fn look_from(&mut self, from: DVec3) {
        let d = from.normalize_or_zero();
        if d == DVec3::ZERO {
            return;
        }
        self.pitch = d.z.clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH);
        if d.x != 0.0 || d.y != 0.0 {
            self.yaw = d.y.atan2(d.x);
        }
    }

    fn forward_offset(&self) -> DVec3 {
        DVec3::new(
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        )
    }

    pub fn eye(&self) -> DVec3 {
        self.target + self.forward_offset() * self.distance
    }

    /// Rotate by `dx`, `dy` radians.
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        self.yaw -= dx;
        self.pitch = (self.pitch + dy).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Pan by fractions of the view height.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        let fwd = -self.forward_offset();
        let right = fwd.cross(DVec3::Z).normalize_or_zero();
        let up = right.cross(fwd);
        let scale = self.distance * (self.fov_y * 0.5).tan() * 2.0;
        self.target += (-right * dx + up * dy) * scale;
    }

    pub fn zoom(&mut self, factor: f64) {
        self.distance = (self.distance * factor).max(1e-9);
    }

    /// Zoom by `factor` towards the point under pixel `px` of a `size`
    /// viewport (on the plane through the target facing the camera), so
    /// that point stays under the cursor.
    pub fn zoom_at(&mut self, factor: f64, size: (f64, f64), px: (f64, f64)) {
        let (o, d) = self.ray(self.target, size, px);
        let (o, d) = (DVec3::from(o), DVec3::from(d));
        let normal = self.forward_offset();
        let denom = d.dot(normal);
        let before = self.distance;
        self.zoom(factor);
        if denom.abs() > 1e-12 {
            let under = o + d * ((self.target - o).dot(normal) / denom);
            let applied = self.distance / before;
            self.target = under + (self.target - under) * applied;
        }
    }

    pub fn view_proj(&self, aspect: f32, origin: DVec3) -> Mat4 {
        self.view_proj_f64(aspect as f64, origin).as_mat4()
    }

    /// View-projection in `f64`, relative to `origin` (picking needs the
    /// precision the GPU matrix doesn't have).
    pub fn view_proj_f64(&self, aspect: f64, origin: DVec3) -> DMat4 {
        let view = DMat4::look_at_rh(self.eye() - origin, self.target - origin, DVec3::Z);
        let near = (self.distance - 2.0 * self.radius).max(self.distance * 1e-3);
        let far = self.distance + 2.0 * self.radius;
        let proj = DMat4::perspective_rh(self.fov_y, aspect.max(1e-3), near, far);
        proj * view
    }

    /// World-space ray through pixel `px` of a `size` viewport (pixels, y down).
    /// Returns (origin, unit direction).
    pub fn ray(&self, origin: DVec3, size: (f64, f64), px: (f64, f64)) -> ([f64; 3], [f64; 3]) {
        let inv = self
            .view_proj_f64(size.0 / size.1.max(1.0), origin)
            .inverse();
        let ndc_x = 2.0 * px.0 / size.0 - 1.0;
        let ndc_y = 1.0 - 2.0 * px.1 / size.1;
        let unproject = |z: f64| {
            let p = inv * glam::DVec4::new(ndc_x, ndc_y, z, 1.0);
            p.truncate() / p.w + origin
        };
        let (near, far) = (unproject(0.0), unproject(1.0));
        (near.to_array(), (far - near).normalize_or_zero().to_array())
    }
}
