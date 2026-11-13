//! The hinge swing check (parametric-shapes brief, "Swing preview"):
//! turn the moving side (object B and its leaf) about the pin axis from
//! 0 up to the open limit, and at each step find where it crosses the
//! fixed side (object A, its leaf, the pin, anything attached to either)
//! with the exact triangle test (`mf_geometry::intersections_between`).
//! Reports the first angle where they meet and every contact on the way,
//! in world space, so the desktop can say "first touches at 12°" and
//! highlight where.

use glam::{DMat4, DVec3};
use mf_geometry::{Bvh, FaceId, Mesh};
use mf_runtime::{Cancelled, RunContext};

/// One body of the swing: a mesh and where it sits (object to world).
#[derive(Clone)]
pub struct SwingBody {
    pub mesh: Mesh,
    pub to_world: DMat4,
}

/// Where a moving body crossed a fixed one at one angle.
#[derive(Debug, Clone, PartialEq)]
pub struct SwingContact {
    pub angle_deg: f64,
    /// Indexes into the `fixed` and `moving` lists given to [`swing`].
    pub fixed: usize,
    pub moving: usize,
    /// The contact region's centre, world space.
    pub centroid: [f64; 3],
    pub faces_fixed: Vec<FaceId>,
    pub faces_moving: Vec<FaceId>,
    /// Where the faces cross, world space (for drawing).
    pub segments: Vec<[[f64; 3]; 2]>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SwingReport {
    /// The first sampled angle where the two sides cross, if any.
    pub first_contact_deg: Option<f64>,
    /// Every contact, in angle order.
    pub contacts: Vec<SwingContact>,
    /// The angles sampled, from 0 to the limit.
    pub angles: Vec<f64>,
}

impl SwingReport {
    /// The contacts at the sampled angle nearest `angle_deg`, at or below
    /// it (what the preview highlights while its slider sits there).
    pub fn contacts_at(&self, angle_deg: f64) -> Vec<&SwingContact> {
        let Some(at) = self
            .angles
            .iter()
            .copied()
            .filter(|a| *a <= angle_deg + 1e-9)
            .fold(None, |m: Option<f64>, a| Some(m.map_or(a, |m| m.max(a))))
        else {
            return Vec::new();
        };
        self.contacts
            .iter()
            .filter(|c| (c.angle_deg - at).abs() < 1e-9)
            .collect()
    }
}

/// The world move that turns by `angle_deg` about the axis through
/// `origin` along `dir` (right hand).
pub fn rotation_about(origin: DVec3, dir: DVec3, angle_deg: f64) -> DMat4 {
    let r = DMat4::from_axis_angle(dir.normalize_or(DVec3::X), angle_deg.to_radians());
    DMat4::from_translation(origin) * r * DMat4::from_translation(-origin)
}

/// Turns `moving` about the axis (`origin`, `dir`) from 0 to `limit_deg`
/// in `step_deg` steps (and the limit itself), finding where it crosses
/// `fixed` at each. Cancellable; deterministic.
pub fn swing(
    fixed: &[SwingBody],
    moving: &[SwingBody],
    origin: DVec3,
    dir: DVec3,
    limit_deg: f64,
    step_deg: f64,
    ctx: &RunContext,
) -> Result<SwingReport, Cancelled> {
    let fixed_bvh: Vec<Bvh> = fixed
        .iter()
        .map(|b| Bvh::build(&b.mesh, ctx))
        .collect::<Result<_, _>>()?;
    let moving_bvh: Vec<Bvh> = moving
        .iter()
        .map(|b| Bvh::build(&b.mesh, ctx))
        .collect::<Result<_, _>>()?;
    let step = step_deg.max(0.1);
    let mut angles = Vec::new();
    let mut a = 0.0;
    while a < limit_deg - 1e-9 {
        angles.push(a);
        a += step;
    }
    angles.push(limit_deg.max(0.0));
    let mut report = SwingReport {
        angles: angles.clone(),
        ..SwingReport::default()
    };
    for &angle in &angles {
        ctx.checkpoint()?;
        let turn = rotation_about(origin, dir, angle);
        for (fi, f) in fixed.iter().enumerate() {
            let world_to_f = f.to_world.inverse();
            for (mi, m) in moving.iter().enumerate() {
                let m_to_f = world_to_f * turn * m.to_world;
                let r = mf_geometry::intersections_between(
                    &f.mesh,
                    &fixed_bvh[fi],
                    &m.mesh,
                    &moving_bvh[mi],
                    &m_to_f,
                    ctx,
                )?;
                for g in r.regions {
                    let w = |p: [f64; 3]| f.to_world.transform_point3(DVec3::from(p)).to_array();
                    report.contacts.push(SwingContact {
                        angle_deg: angle,
                        fixed: fi,
                        moving: mi,
                        centroid: w(g.centroid),
                        faces_fixed: g.faces_a,
                        faces_moving: g.faces_b,
                        segments: g.segments.iter().map(|s| [w(s[0]), w(s[1])]).collect(),
                    });
                }
            }
        }
        if report.first_contact_deg.is_none() && !report.contacts.is_empty() {
            report.first_contact_deg = Some(angle);
        }
    }
    Ok(report)
}
