//! Alignment holes and dowels for `mesh.split_plane` (D12, Phase 1 Task 13).
//!
//! - **Placement** ([`place_holes`]) works on each section region in the
//!   plane's 2D frame: the pole of inaccessibility (the point furthest
//!   from the outline, polylabel) and the region's principal axis. Two
//!   holes go along the axis where both fit with `min_wall` around them;
//!   where only one fits it is keyed (a "D" that can't turn). A region too
//!   thin for any hole is refused with the widest hole that would fit.
//!   Regions under 1% of the largest region's area (slivers at the edge of
//!   a scan) get no hole and are listed in the report.
//! - **Sizes** ([`dowel_size`]) are exactly D12's: diameter = hole −
//!   clearance; length = depth above + depth below − length allowance.
//! - **Holes** are subtracted from each part with `mesh.boolean`'s engine
//!   (D5 window, D4 validation); attached pegs are unioned onto the top
//!   part. Round holes are polygons with corners on the circle and a
//!   chordal error of at most a tenth of the clearance.

use mf_boolean::primitives::{self, Axis};
use mf_boolean::{BoolOp, SectionRegion};
use mf_geometry::{FaceId, Mesh, Provenance, Sources, VertexId};

/// The shape of a hole (and its dowel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PegShape {
    Round,
    Square,
    Keyed,
}

/// One hole: which region, where (2D, in the region's frame), the
/// direction its key faces, and its shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HolePlacement {
    pub region: usize,
    pub center: [f64; 2],
    pub axis: [f64; 2],
    pub shape: PegShape,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PegError {
    /// Region `region` can't hold the hole: the widest that would fit (with
    /// the minimum wall) is `widest_fit_mm` across (document units).
    #[error("section region {region} is too thin: the widest hole that fits is {widest_fit_mm:.2}")]
    DoesNotFit { region: usize, widest_fit_mm: f64 },
}

/// D12's dowel: `(diameter, length)`.
pub fn dowel_size(
    hole_diameter: f64,
    depth_above: f64,
    depth_below: f64,
    peg_clearance: f64,
    length_allowance: f64,
) -> (f64, f64) {
    (
        hole_diameter - peg_clearance,
        depth_above + depth_below - length_allowance,
    )
}

fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
    (x * x + y * y).sqrt()
}

fn inside(l: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut c = false;
    let n = l.len();
    for i in 0..n {
        let (a, b) = (l[i], l[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
        {
            c = !c;
        }
    }
    c
}

/// Distance from `p` to the region's outline, negative outside it.
pub fn region_distance(region: &SectionRegion, p: [f64; 2]) -> f64 {
    let mut d = f64::INFINITY;
    for l in std::iter::once(&region.outer).chain(&region.holes) {
        for i in 0..l.len() {
            d = d.min(seg_dist(p, l[i], l[(i + 1) % l.len()]));
        }
    }
    let ins = inside(&region.outer, p) && !region.holes.iter().any(|h| inside(h, p));
    if ins { d } else { -d }
}

fn bounds(l: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in l {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// The pole of inaccessibility (polylabel): the point inside the region
/// furthest from its outline, to within `precision`, and that distance.
pub fn pole_of_inaccessibility(region: &SectionRegion, precision: f64) -> ([f64; 2], f64) {
    let (lo, hi) = bounds(&region.outer);
    let size = (hi[0] - lo[0]).min(hi[1] - lo[1]);
    if size <= 0.0 {
        return (lo, 0.0);
    }
    let precision = precision.max(size * 1e-6);
    // (potential, distance, centre, half-size), searched best-potential first.
    let cell = |c: [f64; 2], h: f64| {
        let d = region_distance(region, c);
        (d + h * std::f64::consts::SQRT_2, d, c, h)
    };
    let mut queue: Vec<(f64, f64, [f64; 2], f64)> = Vec::new();
    let h = size / 2.0;
    let mut y = lo[1];
    while y < hi[1] {
        let mut x = lo[0];
        while x < hi[0] {
            queue.push(cell([x + h, y + h], h));
            x += size;
        }
        y += size;
    }
    // The centroid of the outline's vertices is a good first guess.
    let n = region.outer.len() as f64;
    let mean = region
        .outer
        .iter()
        .fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
    let mut best = cell(mean, 0.0);
    let mut steps = 0;
    while let Some(i) = (0..queue.len()).max_by(|&a, &b| queue[a].0.total_cmp(&queue[b].0)) {
        let c = queue.swap_remove(i);
        if c.1 > best.1 {
            best = c;
        }
        steps += 1;
        if c.0 - best.1 <= precision || steps > 20_000 {
            continue;
        }
        let h = c.3 / 2.0;
        for (dx, dy) in [(-h, -h), (h, -h), (-h, h), (h, h)] {
            queue.push(cell([c.2[0] + dx, c.2[1] + dy], h));
        }
    }
    (best.2, best.1)
}

/// The region's principal axis (of its outline's vertices), unit length.
fn principal_axis(region: &SectionRegion) -> [f64; 2] {
    let n = region.outer.len() as f64;
    let m = region
        .outer
        .iter()
        .fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for p in &region.outer {
        let (x, y) = (p[0] - m[0], p[1] - m[1]);
        xx += x * x;
        xy += x * y;
        yy += y * y;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    [angle.cos(), angle.sin()]
}

fn area(l: &[[f64; 2]]) -> f64 {
    let n = l.len();
    (0..n)
        .map(|i| l[i][0] * l[(i + 1) % n][1] - l[(i + 1) % n][0] * l[i][1])
        .sum::<f64>()
        .abs()
        / 2.0
}

/// How far a hole's outline reaches from its centre.
fn reach(shape: PegShape, diameter: f64) -> f64 {
    match shape {
        PegShape::Square => diameter * std::f64::consts::FRAC_1_SQRT_2,
        PegShape::Round | PegShape::Keyed => diameter / 2.0,
    }
}

/// Where the holes go (see the module docs). `count` 0 is automatic.
/// Also returns the regions skipped as slivers.
pub fn place_holes(
    regions: &[SectionRegion],
    shape: PegShape,
    diameter: f64,
    count: u32,
    min_wall: f64,
) -> Result<(Vec<HolePlacement>, Vec<usize>), PegError> {
    let largest = regions.iter().map(|r| area(&r.outer)).fold(0.0, f64::max);
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for (k, region) in regions.iter().enumerate() {
        if area(&region.outer) < largest * 0.01 {
            skipped.push(k);
            continue;
        }
        let need = reach(shape, diameter) + min_wall;
        let (c, r) = pole_of_inaccessibility(region, diameter * 1e-3);
        if r < need {
            return Err(PegError::DoesNotFit {
                region: k,
                widest_fit_mm: (2.0 * (r - min_wall)).max(0.0),
            });
        }
        let axis = principal_axis(region);
        let (lo, hi) = bounds(&region.outer);
        let span = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2)).sqrt();
        let at = |t: f64| [c[0] + t * axis[0], c[1] + t * axis[1]];
        let fits = |p: [f64; 2]| region_distance(region, p) >= need;
        // The widest symmetric spacing `t` (holes at offsets `ts`·t) where
        // every hole fits and neighbours keep a wall between them.
        let spread = |offsets: &[f64]| -> Option<f64> {
            let gap = 2.0 * reach(shape, diameter) + min_wall;
            let ok = |t: f64| {
                let ok_gap = offsets.windows(2).all(|w| (w[1] - w[0]) * t >= gap);
                ok_gap && offsets.iter().all(|&o| fits(at(o * t)))
            };
            let widest = (0..=128)
                .map(|i| span * (1.0 - i as f64 / 128.0))
                .find(|&t| ok(t))?;
            // Halfway out, not at the edge: thick walls all round, as long
            // as the holes keep their wall between them.
            let min_gap = offsets
                .windows(2)
                .map(|w| gap / (w[1] - w[0]))
                .fold(0.0, f64::max);
            let t = (widest * 0.5).max(min_gap);
            Some(if ok(t) { t } else { widest })
        };
        let place = |p: [f64; 2], shape: PegShape| HolePlacement {
            region: k,
            center: p,
            axis,
            shape,
        };
        match count {
            0 => match spread(&[-0.5, 0.5]) {
                Some(t) => {
                    out.push(place(at(-0.5 * t), shape));
                    out.push(place(at(0.5 * t), shape));
                }
                None => {
                    let s = if shape == PegShape::Round {
                        PegShape::Keyed
                    } else {
                        shape
                    };
                    out.push(place(c, s));
                }
            },
            1 => out.push(place(c, shape)),
            n => {
                let offsets: Vec<f64> = (0..n).map(|i| i as f64 - (n - 1) as f64 / 2.0).collect();
                let t = spread(&offsets).ok_or(PegError::DoesNotFit {
                    region: k,
                    widest_fit_mm: (2.0 * (r - min_wall)).max(0.0),
                })?;
                for o in offsets {
                    out.push(place(at(o * t), shape));
                }
            }
        }
    }
    Ok((out, skipped))
}

/// A hole's (or dowel's) cross-section of full size `diameter`, its key
/// (if keyed) facing +u.
pub fn profile(shape: PegShape, diameter: f64, chord_tol: f64) -> Vec<[f64; 2]> {
    let r = diameter / 2.0;
    match shape {
        PegShape::Round => primitives::circle(r, chord_tol),
        PegShape::Square => primitives::square(diameter),
        PegShape::Keyed => primitives::keyed(r, 0.6 * r, chord_tol),
    }
}

/// The dowel's cross-section for a hole: `clearance` smaller across.
pub fn dowel_profile(shape: PegShape, hole: f64, clearance: f64, chord_tol: f64) -> Vec<[f64; 2]> {
    let (r, c) = (hole / 2.0, clearance / 2.0);
    match shape {
        PegShape::Round => primitives::circle(r - c, chord_tol),
        PegShape::Square => primitives::square(hole - clearance),
        PegShape::Keyed => primitives::keyed(r - c, 0.6 * r - c, chord_tol),
    }
}

/// A prism of `profile` placed at a hole: world `base`, unit world normal
/// `n` (towards the top part), and the key direction `key` (world).
pub fn prism_at(
    base: [f64; 3],
    n: [f64; 3],
    key: [f64; 3],
    profile: &[[f64; 2]],
    z0: f64,
    z1: f64,
) -> Mesh {
    let dot = key[0] * n[0] + key[1] * n[1] + key[2] * n[2];
    let mut u = [
        key[0] - dot * n[0],
        key[1] - dot * n[1],
        key[2] - dot * n[2],
    ];
    let len = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    if len < 1e-12 {
        // Key along the normal (can't happen for a key in the plane): any u.
        u = if n[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let d = u[0] * n[0] + u[1] * n[1] + u[2] * n[2];
        u = [u[0] - d * n[0], u[1] - d * n[1], u[2] - d * n[2]];
    }
    let l = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    let u = u.map(|x| x / l);
    let v = [
        n[1] * u[2] - n[2] * u[1],
        n[2] * u[0] - n[0] * u[2],
        n[0] * u[1] - n[1] * u[0],
    ];
    primitives::prism(&Axis { base, u, v }, profile, z0, z1)
}

/// Several closed meshes as one (disjoint components).
pub fn merged(meshes: &[Mesh]) -> Mesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for m in meshes {
        let (p, t) = m.to_arrays();
        let base = positions.len() as u32;
        positions.extend(p);
        triangles.extend(t.into_iter().map(|t| t.map(|v| v + base)));
    }
    Mesh::new(positions, triangles).expect("components of valid meshes")
}

/// `first` then `second`, as one provenance relative to `first`'s input:
/// `second` is an edit of the mesh `first` produced. Created faces of
/// `second` that came from `first`'s faces name those faces' own sources;
/// faces from a tool (operand 1 of the boolean) have none in this
/// command.
pub fn compose(first: &Provenance, second: &Provenance) -> Provenance {
    use std::collections::{BTreeMap, BTreeSet};
    let created_first: BTreeMap<FaceId, &Sources> =
        first.created_faces.iter().map(|(f, s)| (*f, s)).collect();
    let created_v_first: BTreeSet<VertexId> = first.created_vertices.iter().copied().collect();
    let removed2: BTreeSet<FaceId> = second.removed_faces.iter().copied().collect();
    let removed_v2: BTreeSet<VertexId> = second.removed_vertices.iter().copied().collect();
    let mut p = Provenance {
        removed_faces: first.removed_faces.clone(),
        ..Provenance::default()
    };
    p.removed_faces.extend(
        second
            .removed_faces
            .iter()
            .filter(|f| !created_first.contains_key(f)),
    );
    p.removed_faces.sort_unstable();
    p.removed_faces.dedup();
    for (f, s) in &first.created_faces {
        if !removed2.contains(f) {
            p.created_faces.push((*f, s.clone()));
        }
    }
    for (f, s) in &second.created_faces {
        let mut src = Sources::default();
        for g in &s.faces {
            match created_first.get(g) {
                Some(s1) => {
                    src.faces.extend(&s1.faces);
                    src.operands.extend(&s1.operands);
                }
                None => src.faces.push(*g),
            }
        }
        src.faces.sort_unstable();
        src.faces.dedup();
        p.created_faces.push((*f, src));
    }
    p.created_faces.sort_by_key(|(f, _)| *f);
    p.modified_faces = first
        .modified_faces
        .iter()
        .chain(
            second
                .modified_faces
                .iter()
                .filter(|f| !created_first.contains_key(f)),
        )
        .filter(|f| !removed2.contains(f))
        .copied()
        .collect();
    p.modified_faces.sort_unstable();
    p.modified_faces.dedup();
    p.removed_vertices = first.removed_vertices.clone();
    p.removed_vertices.extend(
        second
            .removed_vertices
            .iter()
            .filter(|v| !created_v_first.contains(v)),
    );
    p.removed_vertices.sort_unstable();
    p.removed_vertices.dedup();
    p.created_vertices = first
        .created_vertices
        .iter()
        .filter(|v| !removed_v2.contains(v))
        .chain(&second.created_vertices)
        .copied()
        .collect();
    p.created_vertices.sort_unstable();
    p.created_vertices.dedup();
    p.moved_vertices = first
        .moved_vertices
        .iter()
        .chain(
            second
                .moved_vertices
                .iter()
                .filter(|v| !created_v_first.contains(v)),
        )
        .filter(|v| !removed_v2.contains(v))
        .copied()
        .collect();
    p.moved_vertices.sort_unstable();
    p.moved_vertices.dedup();
    p
}

/// `mesh op tool` with the boolean engine, `tool` in `mesh`'s frame.
pub fn apply_tool(
    mesh: &Mesh,
    tool: &Mesh,
    op: BoolOp,
    ctx: &crate::RunContext,
) -> Result<(Mesh, Provenance), crate::OpError> {
    let out =
        mf_boolean::boolean(mesh, tool, op, crate::boolean::kernel(), ctx).map_err(
            |e| match e {
                mf_boolean::BooleanError::Cancelled => crate::OpError::Cancelled,
                e => crate::OpError::Failed {
                    code: e.code(),
                    message: format!(
                        "Making the alignment holes failed: {}. {}",
                        e,
                        crate::boolean::refusal_copy(e.code())
                            .map_or("Try other hole positions or sizes", |c| c.1)
                    ),
                },
            },
        )?;
    Ok((out.mesh, out.provenance))
}
