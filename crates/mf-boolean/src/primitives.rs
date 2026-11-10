//! Closed, outward-facing primitive solids for alignment holes, dowels and
//! joints (Phase 1 Task 13, D12). Round shapes are polygons with their
//! corners on the true circle, with enough sides that the chordal error
//! (the sagitta) is at most `chord_tol`.
//!
//! Every prism is built along a [`Axis`]: a base point, two unit
//! directions `u`, `v` across the profile and `w = u × v` along it. The
//! profile (2D, in `u`, `v`) must be convex and counter-clockwise.

use mf_geometry::Mesh;

/// Where a prism stands: `base + x·u + y·v + z·w`, with `w = u × v`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub base: [f64; 3],
    pub u: [f64; 3],
    pub v: [f64; 3],
}

impl Axis {
    /// The world axes at `base`: profiles in x, y; length along z.
    pub fn z_up(base: [f64; 3]) -> Axis {
        Axis {
            base,
            u: [1.0, 0.0, 0.0],
            v: [0.0, 1.0, 0.0],
        }
    }

    fn w(&self) -> [f64; 3] {
        let (u, v) = (self.u, self.v);
        [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ]
    }

    fn at(&self, x: f64, y: f64, z: f64) -> [f64; 3] {
        let w = self.w();
        std::array::from_fn(|i| self.base[i] + x * self.u[i] + y * self.v[i] + z * w[i])
    }
}

/// Sides for a circle of radius `r` with chordal error at most `chord_tol`
/// (at least 8).
pub fn sides_for(r: f64, chord_tol: f64) -> usize {
    if !(r > 0.0 && chord_tol > 0.0) || chord_tol >= r {
        return 8;
    }
    let n = (std::f64::consts::PI / (1.0 - chord_tol / r).acos()).ceil() as usize;
    n.clamp(8, 4096)
}

/// A regular polygon of radius `r` (corners on the circle), counter-clockwise.
pub fn circle(r: f64, chord_tol: f64) -> Vec<[f64; 2]> {
    let n = sides_for(r, chord_tol);
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A stack of profiles: `rings[k]` is drawn at height `z[k]`, all with the
/// same number of corners; the ends are closed by fans to their centres.
fn lofted(axis: &Axis, rings: &[(f64, Vec<[f64; 2]>)]) -> Mesh {
    let n = rings[0].1.len();
    let mut positions = Vec::with_capacity(rings.len() * n + 2);
    for (z, ring) in rings {
        positions.extend(ring.iter().map(|p| axis.at(p[0], p[1], *z)));
    }
    let centre = |ring: &[[f64; 2]]| {
        let s = ring
            .iter()
            .fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
        [s[0] / n as f64, s[1] / n as f64]
    };
    let (z0, r0) = (&rings[0].0, &rings[0].1);
    let (z1, r1) = (&rings[rings.len() - 1].0, &rings[rings.len() - 1].1);
    let c0 = centre(r0);
    let c1 = centre(r1);
    let bottom = positions.len() as u32;
    positions.push(axis.at(c0[0], c0[1], *z0));
    let top = positions.len() as u32;
    positions.push(axis.at(c1[0], c1[1], *z1));
    let n32 = n as u32;
    let mut triangles = Vec::new();
    for i in 0..n32 {
        let j = (i + 1) % n32;
        // Bottom faces down (−w): clockwise seen from +w.
        triangles.push([bottom, j, i]);
        let t = (rings.len() as u32 - 1) * n32;
        triangles.push([top, t + i, t + j]);
        for k in 0..rings.len() as u32 - 1 {
            let (a, b) = (k * n32, (k + 1) * n32);
            triangles.push([a + i, a + j, b + j]);
            triangles.push([a + i, b + j, b + i]);
        }
    }
    Mesh::new(positions, triangles).expect("a lofted prism is a valid mesh")
}

/// A straight prism of `profile` from `z0` to `z1` along the axis.
pub fn prism(axis: &Axis, profile: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    lofted(axis, &[(z0, profile.to_vec()), (z1, profile.to_vec())])
}

/// A round hole or peg: radius `r`, from `z0` to `z1`.
pub fn cylinder(axis: &Axis, r: f64, z0: f64, z1: f64, chord_tol: f64) -> Mesh {
    prism(axis, &circle(r, chord_tol), z0, z1)
}

/// A square of side `side` (centred).
pub fn square(side: f64) -> Vec<[f64; 2]> {
    let h = side / 2.0;
    vec![[-h, -h], [h, -h], [h, h], [-h, h]]
}

/// A square prism of side `side`.
pub fn box_prism(axis: &Axis, side: f64, z0: f64, z1: f64) -> Mesh {
    prism(axis, &square(side), z0, z1)
}

/// A "D" profile: a circle of radius `r` with the part beyond `flat` (along
/// +u) cut off, so a keyed peg can't turn. `flat` in `(0, r)`.
pub fn keyed(r: f64, flat: f64, chord_tol: f64) -> Vec<[f64; 2]> {
    let n = sides_for(r, chord_tol);
    let half = (flat / r).acos();
    // Arc from +half to 2π − half, then the flat edge closes it.
    let mut out: Vec<[f64; 2]> = (0..=n)
        .map(|i| {
            let a = half + (std::f64::consts::TAU - 2.0 * half) * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect();
    out.dedup();
    out
}

/// A keyed ("D") prism.
pub fn keyed_prism(axis: &Axis, r: f64, flat: f64, z0: f64, z1: f64, chord_tol: f64) -> Mesh {
    prism(axis, &keyed(r, flat, chord_tol), z0, z1)
}

/// A dovetail prism: a trapezoid `narrow` wide at y = 0 and `wide` wide at
/// y = `depth`, from `z0` to `z1`.
pub fn dovetail(axis: &Axis, narrow: f64, wide: f64, depth: f64, z0: f64, z1: f64) -> Mesh {
    let profile = vec![
        [-narrow / 2.0, 0.0],
        [narrow / 2.0, 0.0],
        [wide / 2.0, depth],
        [-wide / 2.0, depth],
    ];
    prism(axis, &profile, z0, z1)
}

/// A dowel standing on `axis.base`: `profile` (its full cross-section)
/// from 0 to `length`, chamfered by `chamfer` at both ends (the ends are
/// the profile scaled to leave `chamfer` all round, approximately).
pub fn chamfered_dowel(axis: &Axis, profile: &[[f64; 2]], length: f64, chamfer: f64) -> Mesh {
    let reach = profile
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
        .fold(0.0, f64::max);
    let c = chamfer.min(length / 4.0).min(reach / 4.0).max(0.0);
    if c == 0.0 {
        return prism(axis, profile, 0.0, length);
    }
    let s = (reach - c) / reach;
    let small: Vec<[f64; 2]> = profile.iter().map(|p| [p[0] * s, p[1] * s]).collect();
    lofted(
        axis,
        &[
            (0.0, small.clone()),
            (c, profile.to_vec()),
            (length - c, profile.to_vec()),
            (length, small),
        ],
    )
}
