//! The object transform gizmo's maths (MF-OBJ-001): handle geometry,
//! hit-testing, drag maths, snapping, and decompose/compose. Pure `f64`,
//! no GPU, no egui (that layer draws [`HandleShape`] and feeds pixel
//! positions in).
//!
//! The 12-value transform is the 3MF row-major 3x4 affine [`crate::model_matrix`]
//! documents: rows `t[0..3]`, `t[3..6]`, `t[6..9]` are the linear part and
//! `t[9..12]` the translation, applied as a row vector times the matrix
//! (`p' = p * T`). This module keeps the linear part as a `DMat3` and the
//! translation as a `DVec3` (`linear_part`/`to_rows` below) rather than
//! round-tripping through `model_matrix`'s `DMat4` on every drag step, but
//! the column convention is identical: a `DMat3`/`DVec3` pair's columns are
//! exactly `model_matrix`'s first three columns and its fourth.
//!
//! Screen positions are plain pixel coordinates (y down), the same unit
//! [`OrbitCamera::ray`] and [`crate::screen_position`] already use; a point
//! in this module is that pixel unit, not a DPI-scaled logical point. The
//! gizmo always reads its own `pivot` as the local origin passed to the
//! camera's `f64` methods, for precision far from the world origin.

use glam::{DMat3, DVec3, EulerRot};

use crate::{OrbitCamera, screen_position};

/// Which operation the gizmo is set up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    Move,
    Rotate,
    Scale,
}

/// A gizmo handle. The axis/plane/ring index runs 0..3 in world X/Y/Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Axis(u8),
    Plane(u8),
    Ring(u8),
    ViewRing,
    ScaleAxis(u8),
    ScaleUniform,
    Centre,
}

/// The object transform gizmo: a pivot, a mode, and a constant on-screen size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gizmo {
    pub pivot: DVec3,
    pub mode: GizmoMode,
    pub size_pt: f64,
}

impl Default for Gizmo {
    fn default() -> Gizmo {
        Gizmo {
            pivot: DVec3::ZERO,
            mode: GizmoMode::Move,
            size_pt: 110.0,
        }
    }
}

/// A screen-space shape the renderer draws for one handle: either a line
/// (axes and planes, drawn as an arrow/quad along `a..b`) or a circle
/// (rings), both in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandleShape {
    pub handle: Handle,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// Set for ring/circle handles: the screen-space radius: `b` is then
    /// unused and `a` is the screen-space centre.
    pub radius: Option<f64>,
}

const AXES: [DVec3; 3] = [DVec3::X, DVec3::Y, DVec3::Z];

/// Screen-pixel distance from `world`'s own projection to the projection of
/// `world + dir * len`, or `None` if either point is behind the camera.
fn projected_length_px(
    cam: &OrbitCamera,
    origin: DVec3,
    view: (f64, f64),
    world: DVec3,
    dir: DVec3,
    len: f64,
) -> Option<f64> {
    let centre = screen_position(cam, origin, view, world.to_array())?;
    let tip = screen_position(cam, origin, view, (world + dir * len).to_array())?;
    let dx = tip.0 - centre.0;
    let dy = tip.1 - centre.1;
    Some((dx * dx + dy * dy).sqrt())
}

/// World-space length along `dir` from `world` whose projection is exactly
/// `size_pt` screen pixels from `world`'s own projection, as seen by `cam`
/// with `view` the viewport size in pixels. Solved by bisection (the
/// perspective projection isn't linear over a handle's full arm length, so
/// a single finite-difference probe over- or under-shoots by double-digit
/// percentages for axes with any depth component): each handle lands at
/// exactly the requested on-screen size regardless of camera distance or
/// viewing angle. Degenerates (behind the camera, zero viewport, `dir`
/// end-on to the view) fall back to a length derived from the camera's
/// distance, so handles never vanish.
fn handle_arm_length(
    cam: &OrbitCamera,
    origin: DVec3,
    view: (f64, f64),
    world: DVec3,
    dir: DVec3,
    size_pt: f64,
) -> f64 {
    let fallback = cam.distance.max(1e-9) * 1e-3;
    let dir = dir.normalize_or_zero();
    let dir = if dir.length_squared() < 1e-12 {
        DVec3::X
    } else {
        dir
    };
    // Bracket using a linear estimate from a small probe, then refine.
    let probe = 1e-4 * cam.distance.max(1e-9);
    let Some(probe_px) = projected_length_px(cam, origin, view, world, dir, probe) else {
        return fallback;
    };
    if probe_px < 1e-12 {
        return fallback;
    }
    let mut lo = 0.0_f64;
    let mut hi = (size_pt / probe_px) * probe * 4.0;
    // Grow the bracket until it overshoots `size_pt`, guarding against a
    // non-monotonic or still-undershooting projection (e.g. very wide FOV).
    for _ in 0..20 {
        match projected_length_px(cam, origin, view, world, dir, hi) {
            Some(px) if px >= size_pt => break,
            _ => hi *= 2.0,
        }
    }
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        match projected_length_px(cam, origin, view, world, dir, mid) {
            Some(px) if px < size_pt => lo = mid,
            Some(_) => hi = mid,
            None => hi = mid,
        }
    }
    0.5 * (lo + hi)
}

impl Gizmo {
    /// Screen-space shapes for the handles of the current mode, at a
    /// constant on-screen size regardless of camera distance.
    pub fn handles(&self, cam: &OrbitCamera, view: (f64, f64)) -> Vec<HandleShape> {
        let origin = self.pivot;
        // Each axis probes its own direction, so its arm lands at
        // `size_pt` screen pixels regardless of that axis's own
        // foreshortening under the current view.
        let arm_along =
            |dir: DVec3| handle_arm_length(cam, origin, view, self.pivot, dir, self.size_pt);
        let Some(centre_px) = screen_position(cam, origin, view, self.pivot.to_array()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match self.mode {
            GizmoMode::Move => {
                for (i, axis) in AXES.into_iter().enumerate() {
                    let arm = arm_along(axis);
                    if let Some(tip) =
                        screen_position(cam, origin, view, (self.pivot + axis * arm).to_array())
                    {
                        out.push(HandleShape {
                            handle: Handle::Axis(i as u8),
                            a: centre_px,
                            b: tip,
                            radius: None,
                        });
                    }
                }
                for i in 0..3u8 {
                    let (u, v) = plane_basis(i);
                    let arm_u = arm_along(u) * 0.3;
                    let arm_v = arm_along(v) * 0.3;
                    if let (Some(pu), Some(pv)) = (
                        screen_position(cam, origin, view, (self.pivot + u * arm_u).to_array()),
                        screen_position(cam, origin, view, (self.pivot + v * arm_v).to_array()),
                    ) {
                        out.push(HandleShape {
                            handle: Handle::Plane(i),
                            a: pu,
                            b: pv,
                            radius: None,
                        });
                    }
                }
                out.push(HandleShape {
                    handle: Handle::Centre,
                    a: centre_px,
                    b: centre_px,
                    radius: None,
                });
            }
            GizmoMode::Rotate => {
                for i in 0..3u8 {
                    out.push(HandleShape {
                        handle: Handle::Ring(i),
                        a: centre_px,
                        b: centre_px,
                        radius: Some(self.size_pt),
                    });
                }
                out.push(HandleShape {
                    handle: Handle::ViewRing,
                    a: centre_px,
                    b: centre_px,
                    radius: Some(self.size_pt * 1.15),
                });
            }
            GizmoMode::Scale => {
                for (i, axis) in AXES.into_iter().enumerate() {
                    let arm = arm_along(axis);
                    if let Some(tip) =
                        screen_position(cam, origin, view, (self.pivot + axis * arm).to_array())
                    {
                        out.push(HandleShape {
                            handle: Handle::ScaleAxis(i as u8),
                            a: centre_px,
                            b: tip,
                            radius: None,
                        });
                    }
                }
                out.push(HandleShape {
                    handle: Handle::ScaleUniform,
                    a: centre_px,
                    b: centre_px,
                    radius: None,
                });
            }
        }
        out
    }

    /// The nearest handle within 8pt of `px`, or `None`. Axes beat planes,
    /// which beat rings, which beat the centre. An axis within 10 degrees
    /// of end-on (the view direction nearly parallel to it) isn't offered.
    pub fn hit(&self, cam: &OrbitCamera, view: (f64, f64), px: (f64, f64)) -> Option<Handle> {
        const TOLERANCE: f64 = 8.0;
        let forward = (cam.target - cam.eye()).normalize_or_zero();

        let mut best: Option<(u8, f64, Handle)> = None; // (priority, dist, handle)
        let mut consider = |priority: u8, dist: f64, handle: Handle| {
            if dist <= TOLERANCE
                && best.is_none_or(|(bp, bd, _)| priority < bp || (priority == bp && dist < bd))
            {
                best = Some((priority, dist, handle));
            }
        };

        for shape in self.handles(cam, view) {
            match shape.handle {
                Handle::Axis(i) | Handle::ScaleAxis(i) => {
                    let axis = AXES[i as usize];
                    if is_end_on(forward, axis) {
                        continue;
                    }
                    let (d, _) = point_segment_distance(px, shape.a, shape.b);
                    consider(0, d, shape.handle);
                }
                Handle::Plane(_) => {
                    let (d, _) = point_segment_distance(px, shape.a, shape.b);
                    consider(1, d, shape.handle);
                }
                Handle::Ring(_) | Handle::ViewRing => {
                    let Some(r) = shape.radius else { continue };
                    let dx = px.0 - shape.a.0;
                    let dy = px.1 - shape.a.1;
                    let d = ((dx * dx + dy * dy).sqrt() - r).abs();
                    consider(2, d, shape.handle);
                }
                Handle::ScaleUniform | Handle::Centre => {
                    let dx = px.0 - shape.a.0;
                    let dy = px.1 - shape.a.1;
                    let d = (dx * dx + dy * dy).sqrt();
                    consider(3, d, shape.handle);
                }
            }
        }
        best.map(|(_, _, h)| h)
    }

    /// Begin a drag on `handle`, started at pixel `start_px`, from the
    /// absolute transform `start`.
    pub fn begin(
        &self,
        handle: Handle,
        start_px: (f64, f64),
        cam: &OrbitCamera,
        view: (f64, f64),
        start: [f64; 12],
    ) -> Drag {
        let (ro, rd) = cam.ray(self.pivot, view, start_px);
        let ro = DVec3::from(ro);
        let rd = DVec3::from(rd);
        let anchor = match handle {
            Handle::Axis(i) => {
                let axis = AXES[i as usize];
                DragAnchor::Axis {
                    axis,
                    start_param: closest_point_on_line_param(self.pivot, axis, ro, rd),
                }
            }
            Handle::Plane(i) => {
                let (u, v) = plane_basis(i);
                let normal = u.cross(v).normalize_or_zero();
                let start_hit = ray_plane_hit(ro, rd, self.pivot, normal).unwrap_or(self.pivot);
                DragAnchor::Plane {
                    normal,
                    start_offset: start_hit - self.pivot,
                }
            }
            Handle::Ring(i) => {
                let normal = AXES[i as usize];
                let start_hit = ray_plane_hit(ro, rd, self.pivot, normal)
                    .unwrap_or_else(|| self.pivot + plane_basis(i).0);
                DragAnchor::Ring {
                    normal,
                    start_vec: (start_hit - self.pivot).reject_from(normal),
                }
            }
            Handle::ViewRing => {
                let normal = (cam.target - cam.eye()).normalize_or_zero();
                let start_hit = ray_plane_hit(ro, rd, self.pivot, normal).unwrap_or(self.pivot);
                DragAnchor::Ring {
                    normal,
                    start_vec: start_hit - self.pivot,
                }
            }
            Handle::ScaleAxis(i) => {
                let axis = AXES[i as usize];
                DragAnchor::Axis {
                    axis,
                    start_param: closest_point_on_line_param(self.pivot, axis, ro, rd).max(1e-6),
                }
            }
            Handle::ScaleUniform => {
                // Distance from the pivot to the closest point on the ray,
                // well-defined for any drag direction.
                let t = (self.pivot - ro).dot(rd);
                let closest = ro + rd * t;
                let dist = (closest - self.pivot).length().max(1e-6);
                DragAnchor::Uniform { start_dist: dist }
            }
            Handle::Centre => DragAnchor::Plane {
                normal: (cam.target - cam.eye()).normalize_or_zero(),
                start_offset: DVec3::ZERO,
            },
        };
        Drag {
            handle,
            pivot: self.pivot,
            mode: self.mode,
            start,
            anchor,
            cam: *cam,
            view,
        }
    }
}

/// True when `forward` (the camera's view direction) is within 10 degrees
/// of parallel to `axis` (either sign), i.e. the axis is end-on.
fn is_end_on(forward: DVec3, axis: DVec3) -> bool {
    let cos10 = 10f64.to_radians().cos();
    forward
        .normalize_or_zero()
        .dot(axis.normalize_or_zero())
        .abs()
        >= cos10
}

/// The two other world axes, in a fixed order, for plane handle `i`
/// (0=X -> YZ, 1=Y -> ZX, 2=Z -> XY), matching `Handle::Plane`/`Ring`'s axis
/// index meaning "the plane/ring whose normal is that axis".
fn plane_basis(i: u8) -> (DVec3, DVec3) {
    match i {
        0 => (DVec3::Y, DVec3::Z),
        1 => (DVec3::Z, DVec3::X),
        _ => (DVec3::X, DVec3::Y),
    }
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

/// The parameter `s` such that `line_origin + s*axis` is the closest point
/// on the infinite line to the ray `(ro, rd)` (Ericson, *Real-Time
/// Collision Detection* section 5.1.9, specialised to two infinite lines).
/// Falls back to projecting `ro` onto the axis when the ray is parallel to
/// it, where any point on the axis is equally close.
fn closest_point_on_line_param(line_origin: DVec3, axis: DVec3, ro: DVec3, rd: DVec3) -> f64 {
    let r = line_origin - ro;
    let a = axis.dot(axis);
    let e = rd.dot(rd).max(1e-18);
    let b = axis.dot(rd);
    let c = axis.dot(r);
    let f = rd.dot(r);
    let denom = a * e - b * b;
    if denom.abs() < 1e-12 {
        // Ray parallel to the axis: any point projects the same; use the
        // ray origin's own projection.
        return -c / a.max(1e-18);
    }
    (b * f - c * e) / denom
}

/// World point where the ray `(ro, rd)` crosses the plane through `point`
/// with normal `normal`, or `None` if the ray is parallel to it.
fn ray_plane_hit(ro: DVec3, rd: DVec3, point: DVec3, normal: DVec3) -> Option<DVec3> {
    let denom = normal.dot(rd);
    if denom.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(point - ro) / denom;
    Some(ro + rd * t)
}

/// How a drag's pixel position maps back to the world, fixed at `begin`.
#[derive(Debug, Clone, Copy)]
enum DragAnchor {
    Axis { axis: DVec3, start_param: f64 },
    Plane { normal: DVec3, start_offset: DVec3 },
    Ring { normal: DVec3, start_vec: DVec3 },
    Uniform { start_dist: f64 },
}

/// An in-progress gizmo drag: computes the new absolute transform for a
/// pixel position, with the handle, pivot, mode, camera, viewport and start
/// fixed at `begin`.
#[derive(Debug, Clone, Copy)]
pub struct Drag {
    handle: Handle,
    pivot: DVec3,
    mode: GizmoMode,
    start: [f64; 12],
    anchor: DragAnchor,
    cam: OrbitCamera,
    view: (f64, f64),
}

impl Drag {
    /// The handle this drag was started on.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// The gizmo mode this drag was started under.
    pub fn mode(&self) -> GizmoMode {
        self.mode
    }
}

/// The 3x3 linear part of `t`, as the column-vector matrix `model_matrix`
/// uses: each column is one of `t`'s row-triples.
fn linear_part(t: &[f64; 12]) -> DMat3 {
    DMat3::from_cols(
        DVec3::new(t[0], t[1], t[2]),
        DVec3::new(t[3], t[4], t[5]),
        DVec3::new(t[6], t[7], t[8]),
    )
}

/// `t` as the 12-value row-major form, from its column-vector linear part
/// and translation.
fn to_rows(linear: DMat3, translate: DVec3) -> [f64; 12] {
    let c0 = linear.col(0);
    let c1 = linear.col(1);
    let c2 = linear.col(2);
    [
        c0.x,
        c0.y,
        c0.z,
        c1.x,
        c1.y,
        c1.z,
        c2.x,
        c2.y,
        c2.z,
        translate.x,
        translate.y,
        translate.z,
    ]
}

impl Drag {
    /// The absolute transform for pointer position `px`, with `snap` applied.
    ///
    /// `px` maps back to world space through the camera and viewport fixed
    /// at [`Gizmo::begin`] (stored on [`DragAnchor`] as the world-space
    /// line/plane each handle reduces to); callers must keep driving the
    /// same drag with pixel positions under that same camera and viewport.
    pub fn update(&self, px: (f64, f64), snap: &Snap) -> [f64; 12] {
        match self.anchor {
            DragAnchor::Axis { axis, start_param } => self.update_move(axis, start_param, px, snap),
            DragAnchor::Plane {
                normal,
                start_offset,
            } => self.update_plane(normal, start_offset, px, snap),
            DragAnchor::Ring { normal, start_vec } => {
                self.update_rotate(normal, start_vec, px, snap)
            }
            DragAnchor::Uniform { start_dist } => self.update_scale(start_dist, px, snap),
        }
    }

    fn ray(&self, px: (f64, f64)) -> (DVec3, DVec3) {
        let (ro, rd) = self.cam.ray(self.pivot, self.view, px);
        (DVec3::from(ro), DVec3::from(rd))
    }

    /// A translation along `axis` by the ray-axis closest point's motion
    /// from `start_param`, grid- and plate-snapped.
    fn update_move(&self, axis: DVec3, start_param: f64, px: (f64, f64), snap: &Snap) -> [f64; 12] {
        let (ro, rd) = self.ray(px);
        let param = closest_point_on_line_param(self.pivot, axis, ro, rd);
        let delta = (param - start_param) * axis;
        self.translate_start_by(delta, snap)
    }

    /// A translation in the handle's plane by the ray-plane hit's motion
    /// from the drag's start offset, grid- and plate-snapped.
    fn update_plane(
        &self,
        normal: DVec3,
        start_offset: DVec3,
        px: (f64, f64),
        snap: &Snap,
    ) -> [f64; 12] {
        let (ro, rd) = self.ray(px);
        let delta = match ray_plane_hit(ro, rd, self.pivot, normal) {
            Some(hit) => (hit - self.pivot) - start_offset,
            None => DVec3::ZERO,
        };
        self.translate_start_by(delta, snap)
    }

    /// Applies a translation delta to the start transform's translation
    /// column only, then grid- and plate-snaps it.
    fn translate_start_by(&self, delta: DVec3, snap: &Snap) -> [f64; 12] {
        let mut t = self.start;
        let mut translate = DVec3::new(t[9], t[10], t[11]) + delta;
        if let Some(grid) = snap.grid {
            translate = DVec3::new(
                round_to_step(translate.x, grid),
                round_to_step(translate.y, grid),
                round_to_step(translate.z, grid),
            );
        }
        if let Some(plate) = snap.plate {
            translate = apply_plate_snap(self, translate, plate);
        }
        t[9] = translate.x;
        t[10] = translate.y;
        t[11] = translate.z;
        t
    }

    /// A rotation about the pivot, by the signed angle between `start_vec`
    /// and the ray's hit on the ring's plane, applied as
    /// `T(pivot)*R*T(-pivot)*start` (column-vector form, matching
    /// `model_matrix`'s convention), snapped to `angle_deg`.
    fn update_rotate(
        &self,
        normal: DVec3,
        start_vec: DVec3,
        px: (f64, f64),
        snap: &Snap,
    ) -> [f64; 12] {
        let (ro, rd) = self.ray(px);
        let angle = match ray_plane_hit(ro, rd, self.pivot, normal) {
            Some(hit) => signed_angle((hit - self.pivot).reject_from(normal), start_vec, normal),
            None => 0.0,
        };
        let angle = if let Some(step) = snap.angle_deg {
            round_to_step(angle.to_degrees(), step).to_radians()
        } else {
            angle
        };
        let rot = DMat3::from_axis_angle(normal, angle);
        let linear = rot * linear_part(&self.start);
        let start_translate = DVec3::new(self.start[9], self.start[10], self.start[11]);
        // T(pivot)*R*T(-pivot)*start, applied to the start's translation
        // column (the image of the local origin): rotate it about the
        // pivot, then apply the (unrotated) linear part's pivot shift is
        // already folded into `start`, so only the translation moves.
        let translate = rot * (start_translate - self.pivot) + self.pivot;
        to_rows(linear, translate)
    }

    /// A uniform scale about the pivot, by the ratio of the current ray's
    /// closest distance from the pivot to the drag's start distance,
    /// snapped to `scale_step`.
    fn update_scale(&self, start_dist: f64, px: (f64, f64), snap: &Snap) -> [f64; 12] {
        let (ro, rd) = self.ray(px);
        let t = (self.pivot - ro).dot(rd);
        let closest = ro + rd * t;
        let dist = (closest - self.pivot).length();
        let mut factor = (dist / start_dist.max(1e-9)).max(1e-6);
        if let Some(step) = snap.scale_step {
            factor = round_to_step(factor, step).max(1e-6);
        }
        let linear = DMat3::from_diagonal(DVec3::splat(factor)) * linear_part(&self.start);
        let start_translate = DVec3::new(self.start[9], self.start[10], self.start[11]);
        let translate = factor * (start_translate - self.pivot) + self.pivot;
        to_rows(linear, translate)
    }
}

/// The angle (radians, signed by the right-hand rule about `normal`) to
/// rotate `from` onto `to`, both already (approximately) in the plane
/// perpendicular to `normal`.
fn signed_angle(to: DVec3, from: DVec3, normal: DVec3) -> f64 {
    let from = from.reject_from(normal);
    let to = to.reject_from(normal);
    if from.length_squared() < 1e-18 || to.length_squared() < 1e-18 {
        return 0.0;
    }
    let from_n = from.normalize();
    let to_n = to.normalize();
    let cos = from_n.dot(to_n).clamp(-1.0, 1.0);
    let sin = normal.normalize_or_zero().dot(from_n.cross(to_n));
    sin.atan2(cos)
}

/// Pulls `translate` so the transform's world min Z lands on the plate
/// (Z=0), if the screen distance between the dragged and snapped min-Z
/// positions is within `within_pt`. `plate.world_min_z` is the object's
/// own min Z in world space under `drag`'s `start` transform, used to
/// recover the (constant) object-space offset from the translation column
/// to the min Z.
fn apply_plate_snap(drag: &Drag, translate: DVec3, plate: PlateSnap) -> DVec3 {
    // The object's local min Z, recovered from `start`: world_min_z(start)
    // = start_translate.z + local_min_z, so local_min_z = world_min_z(start)
    // - start_translate.z. Re-apply with the live translate.
    let start_translate_z = drag.start[11];
    let local_min_z = plate.world_min_z - start_translate_z;
    let current_world_min_z = translate.z + local_min_z;
    let snapped_z = 0.0_f64;
    let dz = snapped_z - current_world_min_z;
    if dz == 0.0 {
        return translate;
    }
    let snapped = DVec3::new(translate.x, translate.y, translate.z + dz);
    // The exact screen distance a `dz` move along world Z covers, measured
    // at the object's own current min-Z depth (not the pivot's, which can
    // sit far from the plate) so foreshortening doesn't skew the check.
    let at = DVec3::new(drag.pivot.x, drag.pivot.y, current_world_min_z);
    let screen_dist = projected_length_px(&drag.cam, drag.pivot, drag.view, at, DVec3::Z, dz)
        .unwrap_or(f64::INFINITY);
    if screen_dist <= plate.within_pt {
        snapped
    } else {
        translate
    }
}

/// Snapping configuration for a drag. Defaults are a 1mm grid, 15 degrees,
/// steps of 0.1, and 8pt from the plate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub grid: Option<f64>,
    pub angle_deg: Option<f64>,
    pub scale_step: Option<f64>,
    pub plate: Option<PlateSnap>,
}

impl Default for Snap {
    fn default() -> Snap {
        Snap {
            grid: Some(1.0),
            angle_deg: Some(15.0),
            scale_step: Some(0.1),
            plate: Some(PlateSnap {
                world_min_z: 0.0,
                within_pt: 8.0,
            }),
        }
    }
}

/// Snaps the dragged object's lowest Z to the plate (Z=0) when the screen
/// distance between the dragged and snapped positions is within `within_pt`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateSnap {
    pub world_min_z: f64,
    pub within_pt: f64,
}

fn round_to_step(value: f64, step: f64) -> f64 {
    if step <= 0.0 {
        value
    } else {
        (value / step).round() * step
    }
}

/// The result of decomposing a 12-value transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decomposed {
    Trs {
        translate: DVec3,
        rotate_deg_xyz: DVec3,
        scale: DVec3,
    },
    Sheared,
}

/// Decomposes `t` into translate/rotate/scale, or reports `Sheared` when
/// the linear part isn't a rotation times an axis-aligned scale (within
/// tolerance). The rotation is extrinsic XYZ. Mirroring (a negative
/// determinant) is reported as a negative X scale.
pub fn decompose(t: &[f64; 12]) -> Decomposed {
    let linear = linear_part(t);
    let translate = DVec3::new(t[9], t[10], t[11]);
    let c0 = linear.col(0);
    let c1 = linear.col(1);
    let c2 = linear.col(2);
    let (mut sx, sy, sz) = (c0.length(), c1.length(), c2.length());
    const EPS: f64 = 1e-9;
    if sx < EPS || sy < EPS || sz < EPS {
        return Decomposed::Sheared;
    }
    if linear.determinant() < 0.0 {
        sx = -sx;
    }
    let r0 = c0 / sx;
    let r1 = c1 / sy;
    let r2 = c2 / sz;
    let rot = DMat3::from_cols(r0, r1, r2);
    // Shear check: the columns of a pure rotation are orthonormal.
    let ortho_tol = 1e-6;
    let dots = [r0.dot(r1), r1.dot(r2), r0.dot(r2)];
    if dots.iter().any(|d| d.abs() > ortho_tol) {
        return Decomposed::Sheared;
    }
    if (rot.determinant() - 1.0).abs() > 1e-6 {
        return Decomposed::Sheared;
    }
    let (rx, ry, rz) = rot.to_euler(EulerRot::XYZEx);
    Decomposed::Trs {
        translate,
        rotate_deg_xyz: DVec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees()),
        scale: DVec3::new(sx, sy, sz),
    }
}

/// Builds the absolute 12-value transform for `translate`/`rotate_deg_xyz`
/// (extrinsic XYZ, degrees)/`scale`, with rotation and scale centred on
/// `pivot`: the pivot's world position only moves by `translate`, not by
/// the rotation or scale themselves. `decompose(compose(t, r, s, ZERO))`
/// round-trips `(t, r, s)` exactly; a non-zero pivot anchors the rotation
/// and scale without changing that contract, since `decompose` has no
/// pivot of its own.
pub fn compose(translate: DVec3, rotate_deg_xyz: DVec3, scale: DVec3, pivot: DVec3) -> [f64; 12] {
    let rot = DMat3::from_euler(
        EulerRot::XYZEx,
        rotate_deg_xyz.x.to_radians(),
        rotate_deg_xyz.y.to_radians(),
        rotate_deg_xyz.z.to_radians(),
    );
    let scale_mat = DMat3::from_diagonal(scale);
    let linear = rot * scale_mat;
    let translation = translate + pivot - linear * pivot;
    to_rows(linear, translation)
}
