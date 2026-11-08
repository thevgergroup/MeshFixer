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

    pub fn view_proj(&self, aspect: f32, origin: DVec3) -> Mat4 {
        let view = DMat4::look_at_rh(self.eye() - origin, self.target - origin, DVec3::Z);
        let near = (self.distance - 2.0 * self.radius).max(self.distance * 1e-3);
        let far = self.distance + 2.0 * self.radius;
        let proj = DMat4::perspective_rh(self.fov_y, aspect.max(1e-3) as f64, near, far);
        (proj * view).as_mat4()
    }
}
