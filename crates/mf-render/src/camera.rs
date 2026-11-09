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

    /// The camera that shows `target` (world bounds) from direction `from`
    /// (target → eye, any length), with its bounding sphere filling
    /// `fill` of the view height, and everything in `scene` (world
    /// bounds) still inside the clipping planes: a speck framed close up
    /// doesn't clip away the model around it. Field of view kept.
    pub fn framing(&self, target: &Aabb, from: DVec3, fill: f64, scene: &Aabb) -> OrbitCamera {
        let centre = DVec3::from(target.center());
        let radius = (target.diagonal() * 0.5).max(1e-9);
        let mut out = OrbitCamera {
            target: centre,
            distance: self.distance_to_fill(radius, fill),
            ..*self
        };
        out.look_from(from);
        out.radius = clip_radius(centre, radius, scene);
        out
    }

    /// The eye distance at which a sphere of `radius` fills `fill` (0–1)
    /// of the view height.
    pub fn distance_to_fill(&self, radius: f64, fill: f64) -> f64 {
        radius / (fill.clamp(1e-3, 1.0) * (self.fov_y * 0.5).tan())
    }

    /// Unit direction from the target to the eye.
    pub fn view_dir(&self) -> DVec3 {
        self.forward_offset()
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

    /// This camera part of the way (`t`, 0–1) to `to`: the target moves in
    /// a straight line, the distance and clipping radius geometrically
    /// (so zooming feels even), yaw the short way round. `t` = 1 is `to`
    /// exactly.
    pub fn towards(&self, to: &OrbitCamera, t: f64) -> OrbitCamera {
        if t >= 1.0 {
            return *to;
        }
        if t <= 0.0 {
            return *self;
        }
        let geometric = |a: f64, b: f64| {
            if a > 0.0 && b > 0.0 {
                a * (b / a).powf(t)
            } else {
                a + (b - a) * t
            }
        };
        let tau = std::f64::consts::TAU;
        let turn = (to.yaw - self.yaw).rem_euclid(tau);
        let turn = if turn > std::f64::consts::PI {
            turn - tau
        } else {
            turn
        };
        OrbitCamera {
            target: self.target.lerp(to.target, t),
            distance: geometric(self.distance, to.distance),
            yaw: self.yaw + turn * t,
            pitch: self.pitch + (to.pitch - self.pitch) * t,
            fov_y: self.fov_y + (to.fov_y - self.fov_y) * t,
            // Never tighter than either end, so nothing is clipped on the
            // way.
            radius: self.radius.max(to.radius),
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

/// A clipping radius about `centre` that keeps a target of `radius` and
/// every corner of `scene` between the near and far planes.
pub fn clip_radius(centre: DVec3, radius: f64, scene: &Aabb) -> f64 {
    let far = (0..8)
        .map(|i| {
            let pick = |k: usize| {
                if i & (1 << k) == 0 {
                    scene.min[k]
                } else {
                    scene.max[k]
                }
            };
            (DVec3::new(pick(0), pick(1), pick(2)) - centre).length()
        })
        .fold(0.0, f64::max);
    radius.max(far)
}

/// An eased camera move (camera interpolation only): `from` to `to` over
/// `duration` seconds from `start` (seconds on the caller's clock), easing
/// out (fast, then settling).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraMove {
    pub from: OrbitCamera,
    pub to: OrbitCamera,
    pub start: f64,
    pub duration: f64,
}

impl CameraMove {
    /// The camera at time `now`.
    pub fn at(&self, now: f64) -> OrbitCamera {
        self.from.towards(&self.to, ease_out(self.progress(now)))
    }

    /// True once `now` is past the end: [`CameraMove::at`] is `to`.
    pub fn done(&self, now: f64) -> bool {
        self.progress(now) >= 1.0
    }

    fn progress(&self, now: f64) -> f64 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        ((now - self.start) / self.duration).clamp(0.0, 1.0)
    }
}

/// Cubic ease-out: 0 → 0, 1 → 1, fastest at the start.
pub fn ease_out(t: f64) -> f64 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}
