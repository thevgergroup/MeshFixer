//! `mesh.hollow` (MF-ISSUE-112): Meshmixer's Hollow. Turns a closed solid
//! into a shell with walls `wall_thickness` thick: for a vase (Hollow,
//! then Split with Cap off), or to save material and print time.
//!
//! The outer surface is kept bit-identical; only the inner surface is
//! made, as the level set at depth `wall_thickness` of the solid's
//! distance field, with Make solid's machinery (`make_solid.rs`):
//!
//! 1. **Measuring.** The input must be closed, edge-manifold and
//!    consistently oriented, with positive volume (otherwise refused by
//!    name, with Repair or Make solid as the next action). A grid of cell
//!    `inner_detail` (default: 1/256 of the diagonal, at most a quarter of
//!    the wall; never more than half the wall) over the bounds. Only a
//!    band of it is ever stored (a sparse grid), so memory follows the
//!    surface's area, not the bounding box's volume; a band over the
//!    budget is refused by name (`grid_too_large`, "use a larger cell
//!    size").
//! 2. **Finding the inside.** Make solid's rays: along each axis, the
//!    surface's crossings and their signs give every grid point a winding
//!    number; a point is inside when two of the three axes say so.
//! 3. **Measuring the wall.** Every grid point within a cell of the
//!    surface gets its exact distance and nearest surface point (each
//!    triangle measured against the points around it). From there the
//!    nearest points spread inward one layer of neighbours at a time, so
//!    every inside point down to `wall_thickness` plus two cells gets its
//!    distance (closest-point propagation: off by a small fraction of a
//!    cell at most).
//! 4. **Building the inner surface.** Marching tetrahedra (Make solid's)
//!    on the band around depth `wall_thickness`, then a few Taubin
//!    passes within each vertex's own tangent plane to settle the
//!    voxel stairstep marching tetrahedra leaves behind (sideways only,
//!    so depth order between neighbours never changes), held to at
//!    least `wall_thickness` deep afterwards, then every triangle
//!    reversed so it faces into the cavity. Where the model is thinner
//!    than twice the wall no point is that deep, so no cavity is made
//!    there: those parts stay solid and the result is still valid.
//! 5. **Checking the result.** D4's `validate_solid` around every inner
//!    face against the whole result (closed, manifold, oriented, no
//!    self-intersection, so the inner and outer surfaces don't touch),
//!    and the result's volume is positive. Then the outer faces left
//!    solid because the model is thinner than twice the wall there are
//!    found (a ray from each, straight in, meets the far side within two
//!    walls without passing through the cavity) and reported.
//! 6. **Cutting drain holes** (optional): each a `shape.teardrop_cylinder`
//!    (Hole role) from the cavity to the outside, cut through
//!    [`crate::apply_parts`] like every other placed part. `auto` puts
//!    them at the cavity's lowest points in the object's plate
//!    orientation, straight down; `placed` at points on the surface,
//!    straight in. A hole whose axis is within 45 degrees of horizontal
//!    gets the teardrop profile when its profile is `auto`.
//!
//! Deterministic at any thread count (parallel work is merged in a fixed
//! order), so replay is Exact; cancellable at every stage.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use mf_geometry::{Bvh, FaceId, Mesh, Provenance, Sources, VertexId};
use mf_runtime::Cancelled;
use mf_spec::{ParamValue, Params};
use rayon::prelude::*;

use crate::make_solid::{Grid, corner, crossings, march_cells, nearest_face, taubin};
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    affine, needs_document,
};

pub struct Hollow;

/// Most grid points the band may hold (each about 40 bytes with its
/// index: about 650 MB at the limit).
pub const MAX_POINTS: usize = 16_000_000;

/// Stage words, in order.
pub const STAGES: [&str; 6] = [
    "measuring",
    "finding the inside",
    "measuring the wall",
    "building the inner surface",
    "checking the result",
    "cutting drain holes",
];

/// The plain-language trade-off the UI states.
pub const TRADE_OFF: &str = "Thinner walls save more material but are weaker; parts thinner than twice the wall stay solid.";

/// The drain-hole modes, as the spec's `drain_holes` choice.
pub const DRAIN_MODES: [&str; 3] = ["none", "auto", "placed"];

fn failed(code: &'static str, message: String) -> OpError {
    OpError::Failed { code, message }
}

// ---------------------------------------------------------------------
// A fast, deterministic hash for grid keys (the maps are only ever
// looked up, never iterated for a result).
// ---------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = mix(self.0 ^ u64::from(b));
        }
    }
    fn write_u64(&mut self, k: u64) {
        self.0 = mix(k);
    }
}

/// SplitMix64's finaliser.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

type KeyMap<V> = HashMap<u64, V, BuildHasherDefault<KeyHasher>>;

fn key_map<V>(capacity: usize) -> KeyMap<V> {
    KeyMap::with_capacity_and_hasher(capacity, BuildHasherDefault::default())
}

// ---------------------------------------------------------------------
// Small vector helpers.
// ---------------------------------------------------------------------

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = norm(a);
    (l > 0.0 && l.is_finite()).then(|| a.map(|x| x / l))
}

/// The point of triangle `t` closest to `p` (Ericson, *Real-Time
/// Collision Detection*, 5.1.5).
fn closest_on_triangle(p: [f64; 3], t: [[f64; 3]; 3]) -> [f64; 3] {
    let [a, b, c] = t;
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let at = |u: f64, v: f64| -> [f64; 3] { std::array::from_fn(|k| a[k] + u * ab[k] + v * ac[k]) };
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return at(d1 / (d1 - d3), 0.0);
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return at(0.0, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return std::array::from_fn(|k| b[k] + w * (c[k] - b[k]));
    }
    let denom = 1.0 / (va + vb + vc);
    at(vb * denom, vc * denom)
}

// ---------------------------------------------------------------------
// The input's own measures.
// ---------------------------------------------------------------------

/// The surface's signed volume and area, over every live face (in a fixed
/// order at any thread count).
fn volume_and_area(mesh: &Mesh, ctx: &RunContext) -> Result<(f64, f64), Cancelled> {
    const CHUNK: usize = 1 << 15;
    let slots = mesh.face_slots() as usize;
    let parts: Vec<(f64, f64)> = (0..slots.div_ceil(CHUNK))
        .into_par_iter()
        .map(|k| -> Result<(f64, f64), Cancelled> {
            ctx.checkpoint()?;
            let (mut v, mut a) = (0.0, 0.0);
            for f in k * CHUNK..((k + 1) * CHUNK).min(slots) {
                let Some(t) = mesh.face(FaceId(f as u32)) else {
                    continue;
                };
                let [p, q, r] = t.map(|i| mesh.corner_position(i));
                v += dot(p, cross(q, r));
                a += norm(cross(sub(q, p), sub(r, p))) * 0.5;
            }
            Ok((v / 6.0, a))
        })
        .collect::<Result<_, _>>()?;
    Ok(parts
        .into_iter()
        .fold((0.0, 0.0), |(v, a), (pv, pa)| (v + pv, a + pa)))
}

/// Refuses an input Hollow can't work on: open, non-manifold, wound
/// inconsistently, or inside out.
fn check_input(mesh: &Mesh, ctx: &RunContext) -> Result<(f64, f64), OpError> {
    let topo = mesh.topology_with(ctx)?;
    if topo.boundary_edges > 0 {
        return Err(failed(
            "open_input",
            format!(
                "Hollow needs a closed surface, and this one has {} open edges. Fill its holes (Repair) or run Make solid first.",
                topo.boundary_edges
            ),
        ));
    }
    if !topo.non_manifold_edges.is_empty() || topo.non_manifold_vertices > 0 {
        return Err(failed(
            "non_manifold_input",
            "Hollow needs a clean surface, and this one has tangled (non-manifold) edges. Run Make solid first.".into(),
        ));
    }
    if topo.inconsistent_edges > 0 {
        return Err(failed(
            "inconsistent_input",
            "Hollow needs every face turned the same way, and some of this surface's faces are flipped. Run Make solid first.".into(),
        ));
    }
    let (volume, area) = volume_and_area(mesh, ctx)?;
    if volume.is_nan() || volume <= 0.0 {
        return Err(failed(
            "inside_out_input",
            "This surface is inside out (it encloses no volume), so it has no inside to hollow. Run Make solid first.".into(),
        ));
    }
    Ok((volume, area))
}

// ---------------------------------------------------------------------
// The sparse field.
// ---------------------------------------------------------------------

/// The 26 neighbours of a grid point.
fn neighbours() -> [[i64; 3]; 26] {
    let mut out = [[0i64; 3]; 26];
    let mut k = 0;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                if (dx, dy, dz) != (0, 0, 0) {
                    out[k] = [dx, dy, dz];
                    k += 1;
                }
            }
        }
    }
    out
}

/// Make solid's rays, kept per column so any grid point can ask whether
/// it's inside (two of the three axes' winding numbers nonzero).
struct Rays {
    axes: Vec<AxisRays>,
}

struct AxisRays {
    /// Column ids, sorted, and where each one's crossings start.
    columns: Vec<u32>,
    start: Vec<usize>,
    /// Each crossing's coordinate along the axis, and the winding number
    /// just past it.
    along: Vec<f64>,
    winding: Vec<i32>,
}

impl Rays {
    fn build(grid: &Grid, mesh: &Mesh, ctx: &RunContext) -> Result<Rays, Cancelled> {
        let tri = |i: usize| {
            mesh.face(FaceId(i as u32))
                .map(|t| t.map(|v| mesh.corner_position(v)))
        };
        let mut axes = Vec::with_capacity(3);
        for a in 0..3 {
            ctx.progress.report(STAGES[1], a as f64 / 3.0);
            let hits = crossings(grid, mesh.face_slots() as usize, &tri, a, ctx)?;
            ctx.checkpoint()?;
            let mut columns = Vec::new();
            let mut start = Vec::new();
            let mut along = Vec::with_capacity(hits.len());
            let mut winding = Vec::with_capacity(hits.len());
            let mut w = 0i32;
            for (i, &(col, x, s)) in hits.iter().enumerate() {
                if columns.last() != Some(&col) {
                    columns.push(col);
                    start.push(i);
                    w = 0;
                }
                w += i32::from(s);
                along.push(x);
                winding.push(w);
            }
            start.push(hits.len());
            axes.push(AxisRays {
                columns,
                start,
                along,
                winding,
            });
        }
        Ok(Rays { axes })
    }

    fn inside(&self, grid: &Grid, i: [usize; 3]) -> bool {
        let mut votes = 0;
        for (a, ax) in self.axes.iter().enumerate() {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let col = (i[b] + grid.n[b] * i[c]) as u32;
            let Ok(k) = ax.columns.binary_search(&col) else {
                continue;
            };
            let (s, e) = (ax.start[k], ax.start[k + 1]);
            let x = grid.origin[a] + i[a] as f64 * grid.h;
            let m = ax.along[s..e].partition_point(|&v| v < x);
            if m > 0 && ax.winding[s + m - 1] != 0 {
                votes += 1;
            }
        }
        votes >= 2
    }
}

/// The band of the distance field Hollow keeps: every grid point within
/// a cell of the surface, and every inside point down to the wall plus
/// two cells.
struct Band {
    index: KeyMap<u32>,
    /// Distance to the surface.
    dist: Vec<f32>,
    inside: Vec<bool>,
}

impl Band {
    fn key(grid: &Grid, i: [usize; 3]) -> u64 {
        i[0] as u64 + grid.n[0] as u64 * (i[1] as u64 + grid.n[1] as u64 * i[2] as u64)
    }

    fn unkey(grid: &Grid, k: u64) -> [usize; 3] {
        let (n0, n1) = (grid.n[0] as u64, grid.n[1] as u64);
        [
            (k % n0) as usize,
            ((k / n0) % n1) as usize,
            (k / (n0 * n1)) as usize,
        ]
    }

    fn step(grid: &Grid, i: [usize; 3], d: [i64; 3]) -> Option<[usize; 3]> {
        let mut out = [0usize; 3];
        for a in 0..3 {
            let v = i[a] as i64 + d[a];
            if v < 0 || v >= grid.n[a] as i64 {
                return None;
            }
            out[a] = v as usize;
        }
        Some(out)
    }
}

fn too_large(h: f64, points: u64, wall: f64, needed: f64) -> OpError {
    let thicker = if needed > wall / 2.0 {
        format!(" or a thicker wall (at least {:.3})", 2.0 * needed)
    } else {
        String::new()
    };
    failed(
        "grid_too_large",
        format!(
            "A cell size of {h} needs about {points} grid points around the surface, more than the {MAX_POINTS} this can hold. Use a larger cell size (at least {needed:.3}){thicker}."
        ),
    )
}

/// About how many band points a cell of `h` needs over `area` with walls
/// of `wall` (both sides of the surface within a cell, and the inside
/// down to the wall plus two cells), with some room.
fn estimate_points(area: f64, h: f64, wall: f64) -> f64 {
    area / (h * h) * ((wall + 2.0 * h) / h + 3.0) * 1.15
}

/// Steps 3: the band's distances (see the module docs).
fn measure(
    grid: &Grid,
    mesh: &Mesh,
    rays: &Rays,
    wall: f64,
    area: f64,
    ctx: &RunContext,
) -> Result<Band, OpError> {
    let h = grid.h;
    let needed = || {
        // The smallest cell whose estimate fits, by bisection.
        let (mut lo, mut hi) = (h, h * 1e4);
        for _ in 0..60 {
            let mid = (lo * hi).sqrt();
            if estimate_points(area, mid, wall) > MAX_POINTS as f64 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi * 1.01
    };
    // Near: each triangle against the grid points within a cell of its
    // bounds, merged per chunk, then over all chunks (nearest wins, ties
    // to the lower point: the same at any thread count and chunking).
    const CHUNK: usize = 1 << 15;
    let slots = mesh.face_slots() as usize;
    let chunks = slots.div_ceil(CHUNK);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let parts: Vec<Vec<(u64, f32, [f32; 3])>> = (0..chunks)
        .into_par_iter()
        .map(|k| -> Result<Vec<(u64, f32, [f32; 3])>, Cancelled> {
            ctx.checkpoint()?;
            let mut local: KeyMap<(f64, [f64; 3])> = key_map(1024);
            for f in k * CHUNK..((k + 1) * CHUNK).min(slots) {
                let Some(t) = mesh.face(FaceId(f as u32)) else {
                    continue;
                };
                // Relative to the grid's origin.
                let p = t.map(|v| sub(mesh.corner_position(v), grid.origin));
                let mut range = [(0usize, 0usize); 3];
                for a in 0..3 {
                    let lo = p.iter().map(|q| q[a]).fold(f64::INFINITY, f64::min);
                    let hi = p.iter().map(|q| q[a]).fold(f64::NEG_INFINITY, f64::max);
                    let i0 = ((lo - h) / h).ceil().max(0.0) as usize;
                    let i1 = (((hi + h) / h).floor().max(0.0) as usize).min(grid.n[a] - 1);
                    range[a] = (i0, i1);
                }
                for z in range[2].0..=range[2].1 {
                    for y in range[1].0..=range[1].1 {
                        for x in range[0].0..=range[0].1 {
                            let q = [x as f64 * h, y as f64 * h, z as f64 * h];
                            let c = closest_on_triangle(q, p);
                            let d = sub(q, c);
                            let d2 = dot(d, d);
                            if d2 > h * h {
                                continue;
                            }
                            let key = Band::key(grid, [x, y, z]);
                            let e = local.entry(key).or_insert((f64::INFINITY, c));
                            if d2 < e.0 || (d2 == e.0 && c < e.1) {
                                *e = (d2, c);
                            }
                        }
                    }
                }
            }
            let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            ctx.progress
                .report(STAGES[2], 0.5 * finished as f64 / chunks.max(1) as f64);
            Ok(local
                .into_iter()
                .map(|(k, (d2, c))| (k, d2.sqrt() as f32, c.map(|x| x as f32)))
                .collect())
        })
        .collect::<Result<_, _>>()?;
    let total: usize = parts.iter().map(Vec::len).sum();
    if total > 4 * MAX_POINTS {
        return Err(too_large(h, total as u64, wall, needed()));
    }
    let mut near: Vec<(u64, f32, [f32; 3])> = Vec::with_capacity(total);
    for p in parts {
        near.extend(p);
    }
    ctx.checkpoint()?;
    near.par_sort_unstable_by(|p, q| {
        p.0.cmp(&q.0)
            .then(p.1.total_cmp(&q.1))
            .then(p.2[0].total_cmp(&q.2[0]))
            .then(p.2[1].total_cmp(&q.2[1]))
            .then(p.2[2].total_cmp(&q.2[2]))
    });
    near.dedup_by_key(|p| p.0);
    ctx.checkpoint()?;
    if near.len() > MAX_POINTS {
        return Err(too_large(h, near.len() as u64, wall, needed()));
    }
    let inside: Vec<bool> = near
        .par_iter()
        .map(|p| rays.inside(grid, Band::unkey(grid, p.0)))
        .collect();
    let mut band = Band {
        index: key_map(near.len() * 3),
        dist: Vec::with_capacity(near.len() * 3),
        inside,
    };
    let mut frontier: Vec<(u64, [f32; 3])> = Vec::with_capacity(near.len());
    for (i, &(k, d, c)) in near.iter().enumerate() {
        band.index.insert(k, i as u32);
        band.dist.push(d);
        frontier.push((k, c));
    }
    drop(near);
    // Inward, one layer of neighbours at a time: each new point takes the
    // nearest of its neighbours' nearest surface points. Points deeper
    // than the wall plus two cells don't spread further.
    let reach = (wall + 2.0 * h) as f32;
    let steps = neighbours();
    let layers = ((wall / h).ceil() as usize + 4).max(1);
    let mut layer = 0usize;
    while !frontier.is_empty() {
        ctx.checkpoint()?;
        layer += 1;
        ctx.progress.report(
            STAGES[2],
            0.5 + 0.5 * (layer as f64 / layers as f64).min(1.0),
        );
        let mut candidates: Vec<u64> = frontier
            .par_chunks(1 << 12)
            .map(|chunk| -> Result<Vec<u64>, Cancelled> {
                ctx.checkpoint()?;
                let mut out = Vec::new();
                for &(k, _) in chunk {
                    let i = band.index[&k] as usize;
                    if band.dist[i] > reach {
                        continue;
                    }
                    let p = Band::unkey(grid, k);
                    for s in &steps {
                        if let Some(q) = Band::step(grid, p, *s) {
                            let qk = Band::key(grid, q);
                            if !band.index.contains_key(&qk) {
                                out.push(qk);
                            }
                        }
                    }
                }
                Ok(out)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        candidates.par_sort_unstable();
        candidates.dedup();
        if band.index.len() + candidates.len() > MAX_POINTS {
            return Err(too_large(
                h,
                (band.index.len() + candidates.len()) as u64,
                wall,
                needed(),
            ));
        }
        let previous: KeyMap<[f32; 3]> = {
            let mut m = key_map(frontier.len());
            m.extend(frontier.iter().copied());
            m
        };
        // Each inside candidate's distance and nearest point.
        let next: Vec<Option<(u64, f32, [f32; 3])>> = candidates
            .par_chunks(1 << 12)
            .map(|chunk| -> Result<Vec<_>, Cancelled> {
                ctx.checkpoint()?;
                Ok(chunk
                    .iter()
                    .map(|&k| {
                        let p = Band::unkey(grid, k);
                        if !rays.inside(grid, p) {
                            return None;
                        }
                        let q = [0, 1, 2].map(|a| (p[a] as f64 * h) as f32);
                        let mut best: Option<(f32, [f32; 3])> = None;
                        for s in &steps {
                            let Some(n) = Band::step(grid, p, *s) else {
                                continue;
                            };
                            let Some(&c) = previous.get(&Band::key(grid, n)) else {
                                continue;
                            };
                            let d = [q[0] - c[0], q[1] - c[1], q[2] - c[2]];
                            let d = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                            if best.is_none_or(|(bd, bc)| d < bd || (d == bd && c < bc)) {
                                best = Some((d, c));
                            }
                        }
                        best.map(|(d, c)| (k, d, c))
                    })
                    .collect())
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        frontier = Vec::with_capacity(next.len());
        for (k, d, c) in next.into_iter().flatten() {
            band.index.insert(k, band.dist.len() as u32);
            band.dist.push(d);
            band.inside.push(true);
            frontier.push((k, c));
        }
    }
    Ok(band)
}

/// The inner surface's field at a band point: below zero in the cavity
/// (inside, deeper than the wall). A point not in the band is deep inside
/// (the cavity) or outside.
fn wall_field(grid: &Grid, band: &Band, rays: &Rays, wall: f64, i: [usize; 3]) -> f32 {
    let w = wall as f32;
    match band.index.get(&Band::key(grid, i)) {
        Some(&j) => {
            let d = band.dist[j as usize];
            if band.inside[j as usize] {
                w - d
            } else {
                w + d
            }
        }
        None if rays.inside(grid, i) => -(grid.h as f32),
        None => w,
    }
}

/// An inner surface: its vertices and triangles, and the grid cells it
/// was built in (each triangle lies in one of them), sorted.
type Inner = (Vec<[f64; 3]>, Vec<[u32; 3]>, Vec<u64>);

/// How many extra Taubin passes settle the marching tetrahedra's voxel
/// steps (the owner's own report, 2026-11-14: visible ripples), beyond
/// the single pass `hollow_shell` always makes. 0 for now: a free
/// (not edge-confined) pass behind a self-intersection check, below,
/// does settle the ripple substantially on its own — but was found to
/// also widen a thin part's cavity reach by a fraction of a cell on at
/// least one shape (a mushroom's cap, where a placed drain hole's own
/// ray test, already close to the cavity boundary by design, started
/// reaching it when it shouldn't have). Several ways to pin the
/// vertices nearest a cavity's own natural edge were tried and didn't
/// reliably catch that specific case; shipping a smoothing pass that
/// can silently widen where a cavity reaches is a worse trade than no
/// extra smoothing, so it stays off pending a safer way to localise
/// it. The machinery (`taubin_free`, `self_intersects`) stays in place,
/// tested, so turning it back on is one constant once that's found.
const RIPPLE_SMOOTHING_PASSES: usize = 0;

/// Step 4: the inner surface (facing into the cavity), or `None` when
/// nothing is deep enough to hollow. `extra_passes` is always
/// [`RIPPLE_SMOOTHING_PASSES`] outside tests; a test calls this with a
/// non-zero count to measure what the extra smoothing would do, against
/// the real, shipped (currently unsmoothed beyond the one plain pass)
/// result, on identical input data.
fn inner_surface(
    grid: &Grid,
    band: &Band,
    rays: &Rays,
    wall: f64,
    extra_passes: usize,
    ctx: &RunContext,
) -> Result<Option<Inner>, Cancelled> {
    let w = wall as f32;
    // Every cell with a corner inside and no deeper than the wall (the
    // field there is zero or more: outside the cavity): the only cells
    // the cavity's surface can cross (a cell is too small to reach from
    // outside the solid to the cavity).
    let keys: Vec<u64> = band.index.keys().copied().collect();
    let mut cells: Vec<u64> = keys
        .par_chunks(1 << 14)
        .map(|chunk| -> Result<Vec<u64>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for &k in chunk {
                let j = band.index[&k] as usize;
                if !(band.inside[j] && band.dist[j] <= w) {
                    continue;
                }
                let p = Band::unkey(grid, k);
                for c in 0..8u8 {
                    let off = [
                        -((c & 1) as i64),
                        -(((c >> 1) & 1) as i64),
                        -(((c >> 2) & 1) as i64),
                    ];
                    if let Some(m) = Band::step(grid, p, off)
                        && (0..3).all(|a| m[a] + 1 < grid.n[a])
                    {
                        out.push(Band::key(grid, m));
                    }
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    drop(keys);
    cells.par_sort_unstable();
    cells.dedup();
    ctx.checkpoint()?;
    // The cells with corners on both sides, with their corners' values.
    let crossing: Vec<(u64, [f32; 8])> = cells
        .par_chunks(1 << 12)
        .map(|chunk| -> Result<Vec<(u64, [f32; 8])>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for &k in chunk {
                let m = Band::unkey(grid, k);
                let v: [f32; 8] =
                    std::array::from_fn(|c| wall_field(grid, band, rays, wall, corner(m, c as u8)));
                let neg = v.iter().filter(|x| **x < 0.0).count();
                if neg > 0 && neg < 8 {
                    out.push((k, v));
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    drop(cells);
    if crossing.is_empty() {
        return Ok(None);
    }
    let mut values: KeyMap<f32> = key_map(crossing.len() * 3);
    for (k, v) in &crossing {
        let m = Band::unkey(grid, *k);
        for (c, x) in v.iter().enumerate() {
            values.insert(Band::key(grid, corner(m, c as u8)), *x);
        }
    }
    let list: Vec<[usize; 3]> = crossing
        .iter()
        .map(|(k, _)| Band::unkey(grid, *k))
        .collect();
    let cells: Vec<u64> = crossing.iter().map(|(k, _)| *k).collect();
    drop(crossing);
    let value = |g: usize| values.get(&(g as u64)).copied().unwrap_or(w);
    let (verts, edges, tris) = march_cells(grid, &list, &value, ctx)?;
    let Some((mut verts, edges, mut tris)) = drop_specks(verts, edges, tris, grid.h, ctx)? else {
        return Ok(None);
    };
    // The one Taubin pass `hollow_shell` always made, confined to the
    // grid edge each vertex was born on: safe (two triangles sharing an
    // edge can't cross when their shared vertices only ever slide
    // towards each other along a fixed line), but a voxel stairstep is
    // a *lateral* artifact this has no room to settle (confirmed: more
    // edge-confined passes measurably make it worse, not better).
    taubin(&mut verts, &edges, &tris, ctx)?;
    // A free (not edge-confined) pass settles it sideways instead, the
    // owner's own report (2026-11-14: visible ripples) — but a vertex
    // free to move in 3-D can, on some shapes, move far enough to cross
    // a neighbour and fold the surface, or (a thin part's own cavity
    // boundary, found by a placed drain hole's own ray test unexpectedly
    // reaching across it) drift past where the field said to stop.
    // `max_step` keeps every move within a cell, well past what any
    // real stairstep needs to settle but short enough to stay inside
    // the band this surface was built in. Rather than risk the rest
    // silently, try it and check too: `self_intersects` rebuilds this
    // candidate surface's own tiny BVH and runs the same exact crossing
    // test D4 trusts. Any extra pass that would have introduced one is
    // skipped — the surface stays at the last known-good (still at
    // least as smooth as the single safe pass above), never corrupted.
    // Vertices born near the cavity's own natural edge (where the model
    // gets thin and the cavity simply stops, not a mesh boundary) stay
    // put: moving them, even a little, was enough to let a placed
    // drain hole's own ray graze across into a thin part that should
    // have refused it. Pinned: a vertex whose crossing edge's shallow
    // end is within half a wall of the wall setting itself (a field of
    // `w/2` there is `depth = w/2`, half as deep as the wall — well
    // outside the ordinary crossing band, which only ever reaches a
    // cell or so past zero).
    let to_index = |p: [f64; 3]| -> [usize; 3] {
        std::array::from_fn(|k| ((p[k] - grid.origin[k]) / grid.h).round() as usize)
    };
    let mut pinned: Vec<bool> = edges
        .iter()
        .map(|&[a, b]| {
            let (fa, fb) = (
                value(grid.index(to_index(a))),
                value(grid.index(to_index(b))),
            );
            fa.max(fb) > 0.5 * w
        })
        .collect();
    // Dilated a few rings out, so the whole neighbourhood a pinned
    // vertex's own average could reach stays still too — not just the
    // exact edge, which a placed drain hole's own ray test found could
    // still graze across after only the exact edge was held.
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); verts.len()];
    for t in &tris {
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            neighbours[u as usize].push(v);
            neighbours[v as usize].push(u);
        }
    }
    for _ in 0..3 {
        let next: Vec<bool> = (0..verts.len())
            .map(|v| pinned[v] || neighbours[v].iter().any(|&u| pinned[u as usize]))
            .collect();
        pinned = next;
    }
    let max_step = 0.25 * grid.h;
    for _ in 0..extra_passes {
        ctx.checkpoint()?;
        let mut candidate = verts.clone();
        taubin_free(&mut candidate, &tris, max_step, &pinned, ctx)?;
        if self_intersects(&candidate, &tris, ctx)? {
            break;
        }
        verts = candidate;
    }
    // Facing into the cavity: away from the material.
    for t in &mut tris {
        t.swap(1, 2);
    }
    Ok(Some((verts, tris, cells)))
}

/// One Taubin pass (λ = 0.5, μ = -0.53), each vertex free to move in
/// any direction towards its neighbours' mean (not confined to its
/// birth grid edge): settles a voxel stairstep, a lateral artifact an
/// edge-confined pass has no room to move sideways and smooth away.
/// Only used behind [`self_intersects`]'s own check, since freedom to
/// move in 3-D can fold the surface on some shapes.
fn taubin_free(
    verts: &mut [[f64; 3]],
    tris: &[[u32; 3]],
    max_step: f64,
    pinned: &[bool],
    ctx: &RunContext,
) -> Result<(), Cancelled> {
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); verts.len()];
    for t in tris {
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            neighbours[u as usize].push(v);
            neighbours[v as usize].push(u);
        }
    }
    for list in &mut neighbours {
        list.sort_unstable();
        list.dedup();
    }
    for factor in [0.5, -0.53] {
        ctx.checkpoint()?;
        let next: Vec<[f64; 3]> = (0..verts.len())
            .into_par_iter()
            .map(|v| {
                if pinned[v] {
                    return verts[v];
                }
                let ns = &neighbours[v];
                if ns.is_empty() {
                    return verts[v];
                }
                let m = ns.iter().fold([0.0; 3], |acc, &u| {
                    let q = verts[u as usize];
                    [acc[0] + q[0], acc[1] + q[1], acc[2] + q[2]]
                });
                let m = m.map(|x| x / ns.len() as f64);
                let want = [0, 1, 2].map(|k| factor * (m[k] - verts[v][k]));
                let len = norm(want);
                let want = if len > max_step {
                    want.map(|x| x * (max_step / len))
                } else {
                    want
                };
                [0, 1, 2].map(|k| verts[v][k] + want[k])
            })
            .collect();
        verts.copy_from_slice(&next);
    }
    Ok(())
}

/// Whether this candidate surface (not yet a [`Mesh`]; built fresh each
/// call since the positions change every pass) has any pair of faces
/// that cross: the same exact test D4's own `validate_solid` trusts,
/// through a throwaway `Mesh` and `Bvh` built just for this check.
/// Deterministic; cancellable. A surface marching tetrahedra built is
/// already closed and manifold (its own triangulation guarantees that),
/// so a fresh `Mesh` never fails to build here.
fn self_intersects(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    ctx: &RunContext,
) -> Result<bool, Cancelled> {
    let Ok(mesh) = Mesh::new(verts.to_vec(), tris.to_vec()) else {
        // Not a reason to trust this candidate: treat an unbuildable
        // mesh as unsafe, the same as a detected crossing.
        return Ok(true);
    };
    let bvh = Bvh::build(&mesh, ctx)?;
    let report = mf_geometry::self_intersections(&mesh, &bvh, ctx)?;
    Ok(report.pair_count > 0)
}

/// The surface without its specks: closed pieces enclosing less than
/// eight cells (a pocket a couple of cells across, where the field only
/// just dips past the wall, too small to print or drain; left solid).
/// `None` if nothing is left. Vertices are renumbered in order.
#[allow(clippy::type_complexity)]
fn drop_specks(
    verts: Vec<[f64; 3]>,
    edges: Vec<[[f64; 3]; 2]>,
    tris: Vec<[u32; 3]>,
    h: f64,
    ctx: &RunContext,
) -> Result<Option<crate::make_solid::Marched>, Cancelled> {
    ctx.checkpoint()?;
    // Pieces: triangles joined through shared vertices (the surface is
    // manifold, so these are its closed pieces).
    let mut parent: Vec<u32> = (0..verts.len() as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            let p = parent[x as usize];
            parent[x as usize] = parent[p as usize];
            x = p;
        }
        x
    }
    for t in &tris {
        for k in 1..3 {
            let (a, b) = (find(&mut parent, t[0]), find(&mut parent, t[k]));
            if a != b {
                let (lo, hi) = (a.min(b), a.max(b));
                parent[hi as usize] = lo;
            }
        }
    }
    let mut volume: HashMap<u32, f64> = HashMap::new();
    for t in &tris {
        let [p, q, r] = t.map(|v| verts[v as usize]);
        *volume.entry(find(&mut parent, t[0])).or_default() += dot(p, cross(q, r)) / 6.0;
    }
    let speck = 8.0 * h * h * h;
    let keep: Vec<bool> = tris
        .iter()
        .map(|t| volume[&find(&mut parent, t[0])].abs() >= speck)
        .collect();
    if keep.iter().all(|k| *k) {
        return Ok(Some((verts, edges, tris)));
    }
    let mut renumber = vec![u32::MAX; verts.len()];
    let (mut nv, mut ne, mut nt) = (Vec::new(), Vec::new(), Vec::new());
    for (t, k) in tris.iter().zip(&keep) {
        if !k {
            continue;
        }
        nt.push(t.map(|v| {
            if renumber[v as usize] == u32::MAX {
                renumber[v as usize] = nv.len() as u32;
                nv.push(verts[v as usize]);
                ne.push(edges[v as usize]);
            }
            renumber[v as usize]
        }));
    }
    Ok((!nt.is_empty()).then_some((nv, ne, nt)))
}

// ---------------------------------------------------------------------
// The shell.
// ---------------------------------------------------------------------

/// A hollowed object, before drain holes: what the operation builds and
/// the desktop previews.
pub struct HollowShell {
    /// The outer surface unchanged (same face and vertex ids) plus the
    /// inner one.
    pub mesh: Mesh,
    pub provenance: Provenance,
    /// The inner surface's faces.
    pub inner: Vec<FaceId>,
    /// Outer faces left solid because the model is thinner than twice
    /// the wall there, sorted.
    pub thin: Vec<FaceId>,
    pub thin_area: f64,
    pub outer_area: f64,
    pub cell: f64,
    pub grid: [usize; 3],
    pub band_points: usize,
}

/// The cell size `inner_detail` asks for (0: automatic), for an object
/// with this bounding-box diagonal.
pub fn cell_size(detail: f64, wall: f64, diagonal: f64) -> f64 {
    if detail > 0.0 {
        detail
    } else {
        (diagonal / 256.0).min(wall / 4.0)
    }
}

/// Steps 1–5 (see the module docs): `input` with an inner surface
/// `wall` in, every face of it checked.
pub fn hollow_shell(
    input: &Mesh,
    wall: f64,
    detail: f64,
    ctx: &RunContext,
) -> Result<HollowShell, OpError> {
    hollow_shell_passes(input, wall, detail, RIPPLE_SMOOTHING_PASSES, ctx)
}

/// `hollow_shell`, with the number of extra ripple-smoothing passes
/// exposed: a test calls this with `0` to measure the surface as it
/// shipped before the 2026-11-14 fix, against [`hollow_shell`]'s own
/// real result, on the same input.
fn hollow_shell_passes(
    input: &Mesh,
    wall: f64,
    detail: f64,
    extra_passes: usize,
    ctx: &RunContext,
) -> Result<HollowShell, OpError> {
    // 1. Measuring.
    ctx.progress.report(STAGES[0], 0.0);
    ctx.checkpoint()?;
    if !(wall > 0.0 && wall.is_finite()) {
        return Err(failed(
            "invalid_parameter",
            format!("The wall thickness must be more than 0 (it's {wall})."),
        ));
    }
    let Some(bb) = input.bounding_box_with(ctx)? else {
        return Err(failed("empty", "The object has no faces to hollow.".into()));
    };
    let (_, outer_area) = check_input(input, ctx)?;
    let h = cell_size(detail, wall, bb.diagonal());
    if !(h > 0.0 && h.is_finite()) {
        return Err(failed(
            "invalid_parameter",
            format!("The cell size {h} isn't usable."),
        ));
    }
    if h > wall / 2.0 {
        return Err(failed(
            "cell_too_coarse",
            format!(
                "A cell size of {h} is too coarse for a {wall} wall: it can be at most half the wall ({}). Use a smaller cell size or a thicker wall.",
                wall / 2.0
            ),
        ));
    }
    let estimate = estimate_points(outer_area, h, wall);
    if estimate > MAX_POINTS as f64 {
        let (mut lo, mut hi) = (h, h * 1e4);
        for _ in 0..60 {
            let mid = (lo * hi).sqrt();
            if estimate_points(outer_area, mid, wall) > MAX_POINTS as f64 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return Err(too_large(h, estimate as u64, wall, hi * 1.01));
    }
    let pad = 2.0 * h;
    let origin = bb.min.map(|x| x - pad);
    let n = [0, 1, 2].map(|a| ((bb.max[a] - bb.min[a] + 2.0 * pad) / h).ceil() as u64 + 1);
    // Column ids are u32 (`crossings`).
    if n[0].saturating_mul(n[1]) > u64::from(u32::MAX) / 2
        || n[1].saturating_mul(n[2]) > u64::from(u32::MAX) / 2
        || n[2].saturating_mul(n[0]) > u64::from(u32::MAX) / 2
    {
        return Err(too_large(h, estimate as u64, wall, h * 4.0));
    }
    let grid = Grid {
        origin,
        h,
        n: n.map(|x| x as usize),
    };

    // 2. Finding the inside.
    ctx.progress.report(STAGES[1], 0.0);
    let rays = Rays::build(&grid, input, ctx)?;

    // 3. Measuring the wall.
    ctx.progress.report(STAGES[2], 0.0);
    let band = measure(&grid, input, &rays, wall, outer_area, ctx)?;
    let band_points = band.dist.len();

    // 4. Building the inner surface.
    ctx.progress.report(STAGES[3], 0.0);
    let Some((verts, tris, cells)) = inner_surface(&grid, &band, &rays, wall, extra_passes, ctx)?
    else {
        return Err(failed(
            "too_thin",
            format!(
                "Every part of the object is thinner than {} (twice the wall), so there's nothing to hollow. Use a thinner wall.",
                2.0 * wall
            ),
        ));
    };
    let mut edit = input.edit();
    let ids: Vec<VertexId> = verts.iter().map(|&p| edit.add_vertex(p)).collect();
    // With materials (MF-ISSUE-091), each inner face names the nearest
    // outer face as its source, so it prints in that face's material.
    let nearest = if input.has_materials() {
        Some(Bvh::build(input, ctx)?)
    } else {
        None
    };
    let mut inner = Vec::with_capacity(tris.len());
    let mut ticker = ctx.ticker();
    for t in &tris {
        ticker.tick()?;
        let sources = match &nearest {
            None => Sources::default(),
            Some(bvh) => {
                let c: [f64; 3] = std::array::from_fn(|k| {
                    t.iter().map(|&v| verts[v as usize][k]).sum::<f64>() / 3.0
                });
                Sources {
                    faces: nearest_face(input, bvh, c, h).into_iter().collect(),
                    ..Sources::default()
                }
            }
        };
        inner.push(
            edit.add_face(t.map(|v| ids[v as usize].0), sources)
                .map_err(geometry)?,
        );
    }
    drop(nearest);
    let (mesh, provenance) = edit.finish().map_err(geometry)?;

    // 5. Checking the result.
    ctx.progress.report(STAGES[4], 0.0);
    // Only an outer face whose bounds reach into a cell the inner surface
    // was built in can touch it: the check's BVH is those and the inner
    // faces (not the whole object).
    let mut near = outer_near_cells(input, &grid, &cells, ctx)?;
    drop(cells);
    near.extend_from_slice(&inner);
    let bvh = Bvh::build_subset(&mesh, &near, ctx)?;
    drop(near);
    ctx.progress.report(STAGES[4], 0.3);
    if let Err(d) = mf_geometry::validate_solid(&mesh, &inner, &bvh, ctx)? {
        return Err(failed(
            "result_not_valid",
            format!(
                "The hollowed object didn't pass the solid check ({d}). Try a different cell size."
            ),
        ));
    }
    let (volume, _) = volume_and_area(&mesh, ctx)?;
    if volume.is_nan() || volume <= 0.0 {
        return Err(failed(
            "result_not_valid",
            "The hollowed object encloses no volume. Try a thinner wall or a different cell size."
                .into(),
        ));
    }
    ctx.progress.report(STAGES[4], 0.6);
    let (thin, thin_area) = thin_faces(input, &mesh, &inner, &grid, &band, &rays, wall, ctx)?;
    ctx.progress.report(STAGES[4], 1.0);
    Ok(HollowShell {
        mesh,
        provenance,
        inner,
        thin,
        thin_area,
        outer_area,
        cell: h,
        grid: grid.n,
        band_points,
    })
}

/// The outer faces whose bounds (a hair wider) reach into one of
/// `cells` (sorted grid cell keys), sorted. A face whose bounds span more
/// than 4096 cells is included without looking (a mesh with faces that
/// big has few faces).
fn outer_near_cells(
    input: &Mesh,
    grid: &Grid,
    cells: &[u64],
    ctx: &RunContext,
) -> Result<Vec<FaceId>, Cancelled> {
    const CHUNK: usize = 1 << 15;
    let h = grid.h;
    let mut set: KeyMap<()> = key_map(cells.len());
    set.extend(cells.iter().map(|&k| (k, ())));
    let slots = input.face_slots() as usize;
    let parts: Vec<Vec<FaceId>> = (0..slots.div_ceil(CHUNK))
        .into_par_iter()
        .map(|k| -> Result<Vec<FaceId>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for f in k * CHUNK..((k + 1) * CHUNK).min(slots) {
                let id = FaceId(f as u32);
                let Some(t) = input.face(id) else { continue };
                let p = t.map(|v| input.corner_position(v));
                let mut range = [(0usize, 0usize); 3];
                let mut outside = false;
                for a in 0..3 {
                    let lo = p.iter().map(|q| q[a]).fold(f64::INFINITY, f64::min);
                    let hi = p.iter().map(|q| q[a]).fold(f64::NEG_INFINITY, f64::max);
                    let c0 = ((lo - grid.origin[a]) / h - 1e-3).floor();
                    let c1 = ((hi - grid.origin[a]) / h + 1e-3).floor();
                    let last = (grid.n[a] - 2) as f64;
                    if c1 < 0.0 || c0 > last {
                        outside = true;
                        break;
                    }
                    range[a] = (c0.max(0.0) as usize, c1.min(last) as usize);
                }
                if outside {
                    continue;
                }
                let span: usize = range.iter().map(|(a, b)| b - a + 1).product();
                if span > 4096 {
                    out.push(id);
                    continue;
                }
                'cells: for z in range[2].0..=range[2].1 {
                    for y in range[1].0..=range[1].1 {
                        for x in range[0].0..=range[0].1 {
                            if set.contains_key(&Band::key(grid, [x, y, z])) {
                                out.push(id);
                                break 'cells;
                            }
                        }
                    }
                }
            }
            Ok(out)
        })
        .collect::<Result<_, _>>()?;
    Ok(parts.into_iter().flatten().collect())
}

/// The outer faces left solid because the model is thinner than twice
/// the wall there (step 5), sorted, and their area: no part of the cavity
/// is within two walls (and a cell) of the face's centre. (The cavity is a
/// wall in from every face where the model is thick enough, and a little
/// over 1.7 walls from a convex corner's faces.) Most faces have cavity
/// straight under them, which the grid answers at once; the rest search
/// the inner surface around them.
#[allow(clippy::too_many_arguments)]
fn thin_faces(
    input: &Mesh,
    result: &Mesh,
    inner: &[FaceId],
    grid: &Grid,
    band: &Band,
    rays: &Rays,
    wall: f64,
    ctx: &RunContext,
) -> Result<(Vec<FaceId>, f64), Cancelled> {
    const CHUNK: usize = 1 << 14;
    let h = grid.h;
    let reach = 2.0 * wall + h;
    let near_inner = Bvh::build_subset(result, inner, ctx)?;
    let slots = input.face_slots() as usize;
    let parts: Vec<(Vec<FaceId>, f64)> = (0..slots.div_ceil(CHUNK))
        .into_par_iter()
        .map(|k| -> Result<(Vec<FaceId>, f64), Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            let mut area = 0.0;
            for f in k * CHUNK..((k + 1) * CHUNK).min(slots) {
                let id = FaceId(f as u32);
                let Some(t) = input.face(id) else { continue };
                let [a, b, c] = t.map(|v| input.corner_position(v));
                let n = cross(sub(b, a), sub(c, a));
                let Some(nu) = unit(n) else { continue };
                let centre: [f64; 3] = std::array::from_fn(|i| (a[i] + b[i] + c[i]) / 3.0);
                // Cavity straight under it?
                let q: [f64; 3] = std::array::from_fn(|i| centre[i] - nu[i] * (wall + 1.5 * h));
                let cell: [f64; 3] = std::array::from_fn(|i| ((q[i] - grid.origin[i]) / h).floor());
                if (0..3).all(|i| cell[i] >= 0.0 && cell[i] + 1.0 < grid.n[i] as f64) {
                    let m = cell.map(|x| x as usize);
                    if (0..8u8).any(|c| wall_field(grid, band, rays, wall, corner(m, c)) < 0.0) {
                        continue;
                    }
                }
                // Any of the inner surface within reach?
                let mut near = false;
                near_inner.for_each_overlapping(
                    std::array::from_fn(|i| centre[i] - reach),
                    std::array::from_fn(|i| centre[i] + reach),
                    |g| {
                        if near {
                            return;
                        }
                        if let Some(s) = result.face(g) {
                            let p =
                                closest_on_triangle(centre, s.map(|v| result.corner_position(v)));
                            near = norm(sub(p, centre)) <= reach;
                        }
                    },
                );
                if !near {
                    out.push(id);
                    area += norm(n) * 0.5;
                }
            }
            Ok((out, area))
        })
        .collect::<Result<_, _>>()?;
    let mut thin = Vec::new();
    let mut area = 0.0;
    for (f, a) in parts {
        thin.extend(f);
        area += a;
    }
    Ok((thin, area))
}

fn geometry(e: mf_geometry::GeometryError) -> OpError {
    failed(
        "result_not_valid",
        format!("The hollowed object is invalid ({e})."),
    )
}

// ---------------------------------------------------------------------
// Opening through the picked surface.
// ---------------------------------------------------------------------

/// How far past the inner surface the plug reaches, so the subtraction
/// clears the whole wall under the picked region with room to spare,
/// however the inner surface curves there.
fn plug_margin(wall: f64, cell: f64) -> f64 {
    wall + 8.0 * cell
}

/// A closed solid over the cavity's own footprint at `faces` (a region
/// of `input`'s surface): each boundary loop's outline, projected to
/// 2D and shrunk inward by `wall` (the cavity's own outline at the cap
/// plane, since the hollow shell's inner wall is uniformly `wall` in
/// from the outer surface everywhere) — never the picked region's own,
/// unshrunk boundary, which would plug the whole wall ring under it,
/// not just the cavity (owner 2026-11-14 follow-up: a hemisphere's rim
/// came out with its own wall thickness gone, 2.94 mm below the cut
/// plane, because the previous plug's top cap was the full cut face).
/// The shrunk outline, ear-clipped, is the top cap (pushed out by
/// `margin`, clear of the surface itself so the boolean kernel never
/// sees an exactly coincident face) and — the same 2D shape, rigidly
/// translated `depth` further along the region's own average normal,
/// never per-vertex (so it can't fold) — the bottom cap; a ruled wall
/// joins the two rings. `None` if `faces` names no live face, or they
/// enclose no boundary (the whole closed surface was picked).
fn through_plug(
    input: &Mesh,
    faces: &[FaceId],
    wall: f64,
    margin: f64,
    depth: f64,
) -> Result<Option<Mesh>, OpError> {
    let mut positions: Vec<[f64; 3]> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut normal_sum = [0.0; 3];
    for &f in faces {
        let Some(t) = input.face(f) else { continue };
        let p = t.map(|v| input.corner_position(v));
        let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
        normal_sum = [
            normal_sum[0] + n[0],
            normal_sum[1] + n[1],
            normal_sum[2] + n[2],
        ];
        let tri = t.map(|v| {
            *remap.entry(v).or_insert_with(|| {
                positions.push(input.corner_position(v));
                positions.len() as u32 - 1
            })
        });
        triangles.push(tri);
    }
    if triangles.is_empty() {
        return Ok(None);
    }
    let Some(normal) = unit(normal_sum) else {
        return Err(failed(
            "open_through_region_degenerate",
            "The picked surface has no well-defined outward direction (its faces cancel out). Pick a different region.".into(),
        ));
    };
    let region = Mesh::new(positions.clone(), triangles).map_err(geometry)?;
    let topo = region.topology();
    if topo.boundary_loops.is_empty() {
        // The picked region is the whole closed surface: nothing left
        // outside it to keep bit-identical, so there's no rim to cut.
        return Ok(None);
    }
    let (u, v) = orthonormal_basis(normal);
    let mut out_positions: Vec<[f64; 3]> = Vec::new();
    let mut out_triangles: Vec<[u32; 3]> = Vec::new();
    for loop_ in &topo.boundary_loops {
        // Consecutive boundary vertices this close together are merged
        // into one before the inset: a plane cut's own near-plane
        // vertex, now snapped exactly onto its plane, and a freshly cut
        // point an edge away from it can end up a single ulp apart
        // (never farther) rather than exactly coincident, which would
        // otherwise give the inset a zero-length edge to turn at
        // (owner 2026-11-13 follow-up).
        let loop_positions: Vec<[f64; 3]> = loop_
            .vertices
            .iter()
            .map(|&vid| positions[vid as usize])
            .collect();
        let scale = loop_positions
            .iter()
            .copied()
            .fold(1.0_f64, |m, p| m.max(norm(p)));
        let merge_tol = scale * 1e-9;
        let mut loop_3d: Vec<[f64; 3]> = Vec::with_capacity(loop_positions.len());
        for &p in &loop_positions {
            let dup = loop_3d
                .last()
                .is_some_and(|&last| norm(sub(p, last)) <= merge_tol);
            if !dup {
                loop_3d.push(p);
            }
        }
        if let (Some(&first), Some(&last)) = (loop_3d.first(), loop_3d.last())
            && loop_3d.len() > 1
            && norm(sub(first, last)) <= merge_tol
        {
            loop_3d.pop();
        }
        if loop_3d.len() < 3 {
            continue;
        }
        // Projected to 2D (the loop's own direction runs opposite to
        // how it bounds the patch it closes off —
        // `fill_holes::triangulate`'s own convention — so reversed
        // here to wind the same way the region's triangles do, CCW
        // seen from `normal`).
        let polygon: Vec<[f64; 2]> = loop_3d
            .iter()
            .rev()
            .map(|p| [dot(*p, u), dot(*p, v)])
            .collect();
        let mean_z = loop_3d.iter().map(|p| dot(*p, normal)).sum::<f64>() / loop_3d.len() as f64;
        let inset = inset_polygon(&polygon, wall);
        let ears = ear_clip(&inset).ok_or_else(|| {
            failed(
                "open_through_region_too_curved",
                "The picked surface curves too much, relative to the wall thickness, for a straight opening: shrinking it inward by the wall crossed itself. Pick a flatter or larger region, or use a thinner wall.".into(),
            )
        })?;
        let to_3d = |p: [f64; 2], along_normal: f64| -> [f64; 3] {
            std::array::from_fn(|k| u[k] * p[0] + v[k] * p[1] + normal[k] * along_normal)
        };
        let base = out_positions.len() as u32;
        let n = inset.len() as u32;
        let top_z = mean_z + margin;
        let bottom_z = mean_z - depth;
        out_positions.extend(inset.iter().map(|&p| to_3d(p, top_z)));
        out_positions.extend(inset.iter().map(|&p| to_3d(p, bottom_z)));
        let (top_ids, bottom_ids): (Vec<u32>, Vec<u32>) = (
            (base..base + n).collect(),
            (base + n..base + 2 * n).collect(),
        );
        // The top cap, facing outward (+normal): the ears as found (the
        // inset polygon is CCW seen from `normal`, same as the region).
        for &[a, b, c] in &ears {
            out_triangles.push([top_ids[a], top_ids[b], top_ids[c]]);
        }
        // The bottom cap, facing inward (-normal): the same ears,
        // reversed.
        for &[a, b, c] in &ears {
            out_triangles.push([bottom_ids[a], bottom_ids[c], bottom_ids[b]]);
        }
        // The side wall: a ruled strip between the two rings, facing
        // outward.
        for i in 0..n as usize {
            let j = (i + 1) % n as usize;
            let (a, b) = (top_ids[i], top_ids[j]);
            let (a2, b2) = (bottom_ids[i], bottom_ids[j]);
            out_triangles.push([a, b, b2]);
            out_triangles.push([a, b2, a2]);
        }
    }
    if out_triangles.is_empty() {
        return Ok(None);
    }
    let plug = Mesh::new(out_positions, out_triangles).map_err(geometry)?;
    Ok(Some(plug))
}

/// Any unit `x`, `y` with `x`, `y`, `normal` an orthonormal basis.
fn orthonormal_basis(normal: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let z = glam::DVec3::from(normal);
    let x = z.any_orthonormal_vector();
    let y = z.cross(x);
    (x.to_array(), y.to_array())
}

/// `polygon` (2D, simple, CCW or CW, `n >= 3`) moved inward by `dist`
/// (perpendicular to each edge): at vertex `i`, the mitered offset of
/// its two adjacent edges' own inward normals — `offset / sin(half the
/// turn angle)` along their bisector, which is exactly `dist`
/// perpendicular to each edge meeting there (standard polygon-offset
/// construction; a vertex sharper than about 1 degree, where the
/// miter would run away to a spike, is pinned to its own inward normal
/// bisector direction scaled to `dist` instead, never materially wrong
/// for a cap boundary this is ever run on). The cavity's own outline
/// at the cap plane is exactly this, `dist = wall_thickness`, in from
/// the picked region's own boundary (owner 2026-11-14 follow-up: the
/// previous plug used the picked region's own, unshrunk boundary,
/// removing the whole wall ring under it, not just the cavity).
fn inset_polygon(polygon: &[[f64; 2]], dist: f64) -> Vec<[f64; 2]> {
    let n = polygon.len();
    let signed_area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (polygon[i], polygon[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        * 0.5;
    // Inward is to the left of each directed edge for a CCW polygon
    // (rotating the edge direction 90 degrees CCW: (dx,dy) -> (-dy,dx)),
    // to the right for a CW one.
    let sign = if signed_area >= 0.0 { 1.0 } else { -1.0 };
    let edge_inward_normal = |a: [f64; 2], b: [f64; 2]| -> [f64; 2] {
        let e = [b[0] - a[0], b[1] - a[1]];
        let len = (e[0] * e[0] + e[1] * e[1]).sqrt().max(1e-300);
        [-sign * e[1] / len, sign * e[0] / len]
    };
    (0..n)
        .map(|i| {
            let (prev, cur, next) = (polygon[(i + n - 1) % n], polygon[i], polygon[(i + 1) % n]);
            let n0 = edge_inward_normal(prev, cur);
            let n1 = edge_inward_normal(cur, next);
            let bisector = [n0[0] + n1[0], n0[1] + n1[1]];
            let blen = (bisector[0] * bisector[0] + bisector[1] * bisector[1]).sqrt();
            // sin(half the turn angle) = blen / 2 for two unit normals'
            // sum (half-angle formula); below about 1 degree's worth
            // the miter is pinned, not let run to a spike.
            let half_sin = (blen / 2.0).max(0.0087);
            let scale = dist / half_sin;
            if blen < 1e-12 {
                // Normals cancel (a 180 degree fold back): offset along
                // either one, both the same distance either way.
                [cur[0] + n0[0] * dist, cur[1] + n0[1] * dist]
            } else {
                let u = [bisector[0] / blen, bisector[1] / blen];
                [cur[0] + u[0] * scale, cur[1] + u[1] * scale]
            }
        })
        .collect()
}

/// A simple polygon's ears (index triples into `p`, winding the same way
/// as `p`), by repeatedly clipping a convex vertex whose ear contains no
/// other vertex (O(n²), fine for a picked region's own boundary). `None`
/// if `p` has fewer than 3 vertices or no valid ear is ever found (a
/// self-crossing outline).
fn ear_clip(p: &[[f64; 2]]) -> Option<Vec<[usize; 3]>> {
    let n = p.len();
    if n < 3 {
        return None;
    }
    let cross2 = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let signed_area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        * 0.5;
    if signed_area.abs() < 1e-300 {
        return None;
    }
    let ccw = signed_area > 0.0;
    let inside = |a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]| {
        let (d1, d2, d3) = (cross2(a, b, p), cross2(b, c, p), cross2(c, a, p));
        (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
    };
    let mut ring: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let mut guard = 0usize;
    while ring.len() > 3 {
        guard += 1;
        if guard > n * n + 8 {
            return None;
        }
        let m = ring.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (ring[(k + m - 1) % m], ring[k], ring[(k + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            let turn = cross2(a, b, c);
            let convex = if ccw { turn > 0.0 } else { turn < 0.0 };
            if !convex {
                continue;
            }
            if ring
                .iter()
                .any(|&iv| iv != ia && iv != ib && iv != ic && inside(a, b, c, p[iv]))
            {
                continue;
            }
            out.push(if ccw { [ia, ib, ic] } else { [ic, ib, ia] });
            ring.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            return None;
        }
    }
    let [ia, ib, ic] = [ring[0], ring[1], ring[2]];
    out.push(if ccw { [ia, ib, ic] } else { [ic, ib, ia] });
    Some(out)
}

/// Cuts the shell open through `faces` (input-face seeds, `open_through`):
/// extrudes the picked region inward by the wall plus a margin and
/// subtracts that plug through [`crate::apply_parts`], the same boolean
/// kernel and D4 check every other cut goes through. `None` faces (an
/// empty list) is a configuration error the caller already refused.
fn open_through(
    cell: f64,
    target: &OperandInput,
    input: &Mesh,
    faces: &[FaceId],
    wall: f64,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance, serde_json::Value), OpError> {
    // Clear of the surface itself (never exactly coincident with the
    // boolean tool), and deep enough past the inner surface to clear
    // the whole wall under the region with room to spare.
    let outward_margin = 4.0 * cell;
    let depth = plug_margin(wall, cell);
    let Some(plug) = through_plug(input, faces, wall, outward_margin, depth)? else {
        return Err(failed(
            "open_through_region_invalid",
            "The picked surface region isn't a patch with a boundary on this object (or no listed face is still live). Pick the surface again.".into(),
        ));
    };
    let part = crate::PartIn {
        name: "Open through picked surface".into(),
        mesh: std::borrow::Cow::Owned(plug),
        transform: target.transform,
        role: mf_parts::ShapeRole::Hole,
        host: 0,
        operand: None,
    };
    let host = OperandInput {
        slot: target.slot,
        object: target.object,
        name: target.name,
        mesh: target.mesh,
        transform: target.transform,
        target: Target::WholeObject,
    };
    let mut applied = crate::apply_parts(
        [Some((0, &host)), None],
        &[part],
        crate::ApplyOrder::Sequential,
        ctx,
    )?;
    let a = applied.pop().ok_or_else(|| {
        failed(
            "result_not_valid",
            "Opening the picked surface through produced nothing.".into(),
        )
    })?;
    // A live original (pre-hollow) face outside the picked region may
    // legitimately gain a new id (the boolean kernel's own local window
    // around the cut, same as any other cut), but its geometry must
    // never move. A picked region curved too much, relative to the wall
    // plus margin, for a straight-sided opening to stay inside the
    // solid would move one; caught here rather than returned as a
    // silently wrong outer surface.
    let picked: std::collections::HashSet<FaceId> = faces.iter().copied().collect();
    let moved = input.faces().any(|(f, t)| {
        if picked.contains(&f) || !a.mesh.is_face_live(f) {
            return false;
        }
        let before: [[f64; 3]; 3] = t.map(|v| input.corner_position(v));
        match a.mesh.face(f) {
            Some(after_t) => {
                let after: [[f64; 3]; 3] = after_t.map(|v| a.mesh.corner_position(v));
                before != after
            }
            None => false,
        }
    });
    if moved {
        return Err(failed(
            "open_through_region_too_curved",
            "The picked surface curves too much, relative to the wall thickness, for a straight opening: it would move part of the surface outside the picked region. Pick a flatter or smaller region, or use a thinner wall.".into(),
        ));
    }
    // The whole result must be one connected surface: the opening's
    // whole point is to join the hollow inside to the outside (a bowl,
    // not a solid with a sealed cavity sitting inside it). A plug whose
    // depth outruns the surface's own curvature can slice clean
    // through the shell at a smaller cross-section elsewhere, cutting
    // the inner wall off from the rim without moving or removing any
    // single face by itself — D4-valid as two separate closed shells,
    // the cavity never actually opened (a hemisphere's rim losing its
    // own wall thickness was exactly this; owner 2026-11-14 follow-up).
    let components = a.mesh.topology_with(ctx)?.components.len();
    if components > 1 {
        return Err(failed(
            "open_through_region_too_curved",
            format!(
                "The picked surface curves too much, relative to the wall thickness, for a straight opening: the result would be {components} separate shells, not one open bowl. Pick a flatter or smaller region, or use a thinner wall."
            ),
        ));
    }
    let row = serde_json::json!({
        "faces": faces.len(),
        "depth": depth,
    });
    Ok((a.mesh, a.provenance, row))
}

// ---------------------------------------------------------------------
// Drain holes.
// ---------------------------------------------------------------------

/// One drain hole: where it meets the outside (object space), its axis
/// (outward, unit), and how deep it reaches in from there.
#[derive(Debug, Clone, Copy)]
struct Hole {
    outside: [f64; 3],
    axis: [f64; 3],
    depth: f64,
}

/// Where the ray from `from` along unit `dir` first meets `mesh`, as
/// (face, distance), within `reach`: through a BVH over just the faces
/// whose bounds meet the ray's box (`radius` wider), found in one pass.
fn first_hit(
    mesh: &Mesh,
    from: [f64; 3],
    dir: [f64; 3],
    reach: f64,
    radius: f64,
    ctx: &RunContext,
) -> Result<Option<(FaceId, f64)>, Cancelled> {
    const CHUNK: usize = 1 << 15;
    let to: [f64; 3] = std::array::from_fn(|i| from[i] + dir[i] * reach);
    let lo: [f64; 3] = std::array::from_fn(|i| from[i].min(to[i]) - radius);
    let hi: [f64; 3] = std::array::from_fn(|i| from[i].max(to[i]) + radius);
    let slots = mesh.face_slots() as usize;
    let faces: Vec<FaceId> = (0..slots.div_ceil(CHUNK))
        .into_par_iter()
        .map(|k| -> Result<Vec<FaceId>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for f in k * CHUNK..((k + 1) * CHUNK).min(slots) {
                let id = FaceId(f as u32);
                let Some(t) = mesh.face(id) else { continue };
                let p = t.map(|v| mesh.corner_position(v));
                if (0..3).all(|a| {
                    let flo = p.iter().map(|q| q[a]).fold(f64::INFINITY, f64::min);
                    let fhi = p.iter().map(|q| q[a]).fold(f64::NEG_INFINITY, f64::max);
                    flo <= hi[a] && lo[a] <= fhi
                }) {
                    out.push(id);
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let near = Bvh::build_subset(mesh, &faces, ctx)?;
    Ok(near.ray_pick(mesh, from, dir, &glam::DMat4::IDENTITY))
}

/// The holes `auto` places: the cavity's lowest points in plate
/// orientation (world −z), at least `spacing` apart, straight down.
fn auto_holes(
    shell: &HollowShell,
    transform: &[f64; 12],
    count: usize,
    diameter: f64,
    ctx: &RunContext,
) -> Result<Vec<Hole>, OpError> {
    let mesh = &shell.mesh;
    let inv = affine::inverse(transform).ok_or_else(|| {
        failed(
            "singular_transform",
            "The object's transform can't be inverted; reset it first.".into(),
        )
    })?;
    // World down, in object space.
    let Some(down) = unit(affine::apply_linear(&inv, [0.0, 0.0, -1.0])) else {
        return Err(failed(
            "singular_transform",
            "The object's transform can't be inverted; reset it first.".into(),
        ));
    };
    let mut verts: Vec<u32> = shell
        .inner
        .iter()
        .filter_map(|&f| mesh.face(f))
        .flatten()
        .collect();
    verts.sort_unstable();
    verts.dedup();
    let world_z = |v: u32| affine::apply(transform, mesh.corner_position(v))[2];
    verts.sort_by(|&a, &b| world_z(a).total_cmp(&world_z(b)).then(a.cmp(&b)));
    let diag = mesh.bounding_box().map_or(1.0, |b| b.diagonal());
    let spacing = (4.0 * diameter).max(diag / 8.0);
    let mut picked: Vec<[f64; 3]> = Vec::new();
    for v in verts {
        if picked.len() == count {
            break;
        }
        let p = mesh.corner_position(v);
        if picked.iter().all(|q| norm(sub(p, *q)) >= spacing) {
            picked.push(p);
        }
    }
    let mut out = Vec::new();
    for (i, p) in picked.into_iter().enumerate() {
        ctx.checkpoint()?;
        // Straight down from just below the cavity's lowest point: the
        // first surface met is the outside.
        let from: [f64; 3] = std::array::from_fn(|k| p[k] + down[k] * 1e-7 * shell.cell);
        let Some((_, t)) = first_hit(mesh, from, down, diag * 2.0, diameter, ctx)? else {
            return Err(failed(
                "hole_misses_outside",
                format!(
                    "Drain hole {} found no outside below the cavity's lowest point. Place the holes yourself (Placed).",
                    i + 1
                ),
            ));
        };
        let outside: [f64; 3] = std::array::from_fn(|k| from[k] + down[k] * t);
        out.push(Hole {
            outside,
            axis: down,
            depth: t + reach_in(shell, diameter),
        });
    }
    Ok(out)
}

/// How far past the inner surface a hole reaches into the cavity, so it
/// opens cleanly however the cavity's floor curves.
fn reach_in(shell: &HollowShell, diameter: f64) -> f64 {
    diameter.max(2.0 * shell.cell)
}

/// The holes `placed` puts at `samples` (`[face, b0, b1, b2, _]` on the
/// input's outer faces), straight in.
fn placed_holes(
    shell: &HollowShell,
    samples: &[[f64; 5]],
    diameter: f64,
    ctx: &RunContext,
) -> Result<Vec<Hole>, OpError> {
    let mesh = &shell.mesh;
    let first_inner = shell.inner.first().map_or(u32::MAX, |f| f.0);
    let diag = mesh.bounding_box().map_or(1.0, |b| b.diagonal());
    let mut out = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        ctx.checkpoint()?;
        let face = FaceId(s[0] as u32);
        let bad = || {
            failed(
                "hole_point_invalid",
                format!(
                    "Drain hole {} isn't on the object's surface. Place it again.",
                    i + 1
                ),
            )
        };
        if s[0] < 0.0 || s[0].fract() != 0.0 || face.0 >= first_inner {
            return Err(bad());
        }
        let Some(t) = mesh.face(face) else {
            return Err(bad());
        };
        let [a, b, c] = t.map(|v| mesh.corner_position(v));
        let Some(n) = unit(cross(sub(b, a), sub(c, a))) else {
            return Err(bad());
        };
        let p: [f64; 3] = std::array::from_fn(|k| s[1] * a[k] + s[2] * b[k] + s[3] * c[k]);
        let inward = n.map(|x| -x);
        let from: [f64; 3] = std::array::from_fn(|k| p[k] + inward[k] * 1e-7 * shell.cell);
        let hit = first_hit(mesh, from, inward, diag * 2.0, diameter, ctx)?;
        match hit {
            Some((g, t)) if g.0 >= first_inner => out.push(Hole {
                outside: p,
                axis: n,
                depth: t + reach_in(shell, diameter),
            }),
            _ => {
                return Err(failed(
                    "hole_misses_cavity",
                    format!(
                        "Drain hole {} at ({:.2}, {:.2}, {:.2}) doesn't reach the hollow inside: the object is solid there. Move it to a thicker part.",
                        i + 1,
                        p[0],
                        p[1],
                        p[2]
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// A frame whose z is `axis` (unit), as columns x, y, z.
fn frame(axis: [f64; 3]) -> [[f64; 3]; 3] {
    let z = glam::DVec3::from(axis);
    let x = z.any_orthonormal_vector();
    let y = z.cross(x);
    [x.to_array(), y.to_array(), z.to_array()]
}

/// Cuts `holes` into the shell through [`crate::apply_parts`], each a
/// `shape.teardrop_cylinder` (Hole role). Returns the result, its
/// provenance relative to the shell, and a report row per hole.
fn cut_holes(
    shell: &HollowShell,
    target: &OperandInput,
    holes: &[Hole],
    diameter: f64,
    profile: &str,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance, Vec<serde_json::Value>), OpError> {
    use mf_parts::GenContext;
    let g = mf_parts::find_generator("shape.teardrop_cylinder").ok_or_else(|| {
        failed(
            "missing_generator",
            "This build has no hole shape (shape.teardrop_cylinder).".into(),
        )
    })?;
    let mut parts = Vec::with_capacity(holes.len());
    let mut rows = Vec::with_capacity(holes.len());
    for (i, hole) in holes.iter().enumerate() {
        ctx.checkpoint()?;
        let raw = std::collections::BTreeMap::from([
            ("diameter".to_owned(), diameter.to_string()),
            ("depth".to_owned(), hole.depth.to_string()),
            ("hole_profile".to_owned(), profile.to_owned()),
        ]);
        let params = mf_parts::generator::validate_raw(g, raw).map_err(part_error)?;
        let f = frame(hole.axis);
        // The hole's local axes in the world, for its profile and the
        // way its teardrop points.
        let world = |d: [f64; 3]| glam::DVec3::from(affine::apply_linear(&target.transform, d));
        let local_to_world = glam::DMat3::from_cols(world(f[0]), world(f[1]), world(f[2]));
        let params = mf_parts::shape::orient_params(&params, local_to_world);
        // What the generator builds: the choice, or what `auto` resolved
        // to for this axis.
        let resolved = match params.get_choice("hole_profile")? {
            "auto" => {
                use mf_parts::connectors::hole_profile::HoleProfile;
                match HoleProfile::from_code(params.get_float("profile")?) {
                    HoleProfile::Round => "round",
                    HoleProfile::Teardrop => "teardrop",
                    HoleProfile::FlatTop => "flat_top",
                }
            }
            other => other,
        }
        .to_owned();
        let generated = g
            .generate(&params, &GenContext::new(ctx))
            .map_err(part_error)?;
        for part in generated {
            // Local (x, y, z) to object space at the hole's outside point.
            let (positions, triangles) = part.mesh.to_arrays();
            let placed: Vec<[f64; 3]> = positions
                .iter()
                .map(|p| {
                    std::array::from_fn(|k| {
                        hole.outside[k] + p[0] * f[0][k] + p[1] * f[1][k] + p[2] * f[2][k]
                    })
                })
                .collect();
            let mesh = Mesh::new(placed, triangles).map_err(geometry)?;
            parts.push(crate::PartIn {
                name: format!("Drain hole {}", i + 1),
                mesh: std::borrow::Cow::Owned(mesh),
                transform: target.transform,
                role: mf_parts::ShapeRole::Hole,
                host: 0,
                operand: None,
            });
        }
        rows.push(serde_json::json!({
            "at": hole.outside,
            "axis": hole.axis,
            "depth": hole.depth,
            "diameter": diameter,
            "profile": resolved,
        }));
    }
    let host = OperandInput {
        slot: target.slot,
        object: target.object,
        name: target.name,
        mesh: &shell.mesh,
        transform: target.transform,
        target: Target::WholeObject,
    };
    let mut applied = crate::apply_parts(
        [Some((0, &host)), None],
        &parts,
        crate::ApplyOrder::Sequential,
        ctx,
    )?;
    let a = applied.pop().ok_or_else(|| {
        failed(
            "result_not_valid",
            "Cutting the drain holes produced nothing.".into(),
        )
    })?;
    Ok((a.mesh, a.provenance, rows))
}

fn part_error(e: mf_parts::ConnectorError) -> OpError {
    let mf_parts::ConnectorError::Failed { code, message } = e;
    OpError::Failed { code, message }
}

// ---------------------------------------------------------------------
// The operation.
// ---------------------------------------------------------------------

/// The report's sentence about thin parts, in document units.
pub fn thin_message(wall: f64) -> String {
    format!("Parts thinner than {} were left solid", short(2.0 * wall))
}

/// World (document) units per object unit under `transform`: the cube
/// root of its linear part's determinant (exact for a uniform scale, a
/// rotation or a mirror).
pub fn world_per_object(transform: &[f64; 12]) -> f64 {
    affine::determinant(transform).abs().cbrt()
}

/// A length as people write it: up to three decimals, no trailing zeros.
pub fn short(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_owned()
}

impl Operation for Hollow {
    fn id(&self) -> &'static str {
        "mesh.hollow"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = inputs.first().ok_or_else(|| OpError::Failed {
            code: "missing_target",
            message: format!("{} was run with no target", self.id()),
        })?;
        if target.target != Target::WholeObject {
            return Err(failed(
                "region_not_supported",
                "Hollow works on a whole object. Select the whole object (click Model), then run it again.".into(),
            ));
        }
        let wall = params.get_float("wall_thickness")?;
        let detail = params.get_float("inner_detail")?;
        let mode = params.get_choice("drain_holes")?;
        let diameter = params.get_float("hole_diameter")?;
        let count = params.get_int("hole_count")?.max(1) as usize;
        let profile = params.get_choice("hole_profile")?;
        let samples = params.get_samples("hole_points")?;
        let open_through_on = params.get_bool("open_through")?;
        let open_through_face_ids = params.get_faces("open_through_faces")?;
        if mode != "none" && !(diameter > 0.0 && diameter.is_finite()) {
            return Err(failed(
                "invalid_parameter",
                format!("The drain hole diameter must be more than 0 (it's {diameter})."),
            ));
        }
        if mode == "placed" && samples.is_empty() {
            return Err(failed(
                "no_hole_points",
                "Placed drain holes need at least one point. Click the surface where each hole goes, or pick Auto.".into(),
            ));
        }
        if open_through_on && open_through_face_ids.is_empty() {
            return Err(failed(
                "open_through_needs_faces",
                "Open through needs a picked surface. Click a surface on the object, or turn it off.".into(),
            ));
        }
        let open_through_faces: Vec<FaceId> =
            open_through_face_ids.iter().map(|&f| FaceId(f)).collect();
        if open_through_on
            && !open_through_faces
                .iter()
                .all(|&f| target.mesh.is_face_live(f))
        {
            return Err(failed(
                "open_through_needs_faces",
                "The picked surface no longer matches this object (it may have changed). Pick the surface again.".into(),
            ));
        }

        // The settings are in document (world) units; the mesh is in its
        // object's own.
        let scale = world_per_object(&target.transform);
        if !(scale > 0.0 && scale.is_finite()) {
            return Err(failed(
                "singular_transform",
                "The object's transform can't be inverted; reset it first.".into(),
            ));
        }
        let shell = hollow_shell(target.mesh, wall / scale, detail / scale, ctx)?;
        let diameter = diameter / scale;

        ctx.progress.report(STAGES[5], 0.0);
        let holes = match mode {
            "auto" => auto_holes(&shell, &target.transform, count, diameter, ctx)?,
            "placed" => placed_holes(&shell, &samples, diameter, ctx)?,
            _ => Vec::new(),
        };
        let thin = serde_json::json!({
            "faces": shell.thin.len(),
            "area": shell.thin_area * scale * scale,
            "fraction": if shell.outer_area > 0.0 { shell.thin_area / shell.outer_area } else { 0.0 },
            "below": 2.0 * wall,
            "message": if shell.thin.is_empty() { serde_json::Value::Null } else { thin_message(wall).into() },
        });
        let inner_faces = shell.inner.len();
        let object_cell = shell.cell;
        let (cell, grid, points) = (shell.cell * scale, shell.grid, shell.band_points);
        let (mesh, provenance, hole_rows) = if holes.is_empty() {
            (shell.mesh, shell.provenance, Vec::new())
        } else {
            let (mesh, step, mut rows) = cut_holes(&shell, target, &holes, diameter, profile, ctx)?;
            // Sizes in document units, like the settings ("at" and "axis"
            // stay in the object's own space).
            for r in &mut rows {
                for k in ["depth", "diameter"] {
                    if let Some(v) = r[k].as_f64() {
                        r[k] = (v * scale).into();
                    }
                }
            }
            let provenance = crate::blend::compose(&shell.provenance, &step);
            (mesh, provenance, rows)
        };
        let (mesh, provenance, open_through_row) = if open_through_on {
            // A fresh shell to open through: the plug is built from the
            // input's own picked faces, whose ids and positions it keeps
            // whole across both the hollow and any drain holes.
            let through_target = OperandInput {
                slot: target.slot,
                object: target.object,
                name: target.name,
                mesh: &mesh,
                transform: target.transform,
                target: target.target.clone(),
            };
            let (mesh, step, mut row) = open_through(
                object_cell,
                &through_target,
                target.mesh,
                &open_through_faces,
                wall / scale,
                ctx,
            )?;
            if let Some(v) = row["depth"].as_f64() {
                row["depth"] = (v * scale).into();
            }
            let provenance = crate::blend::compose(&provenance, &step);
            (mesh, provenance, Some(row))
        } else {
            (mesh, provenance, None)
        };
        ctx.progress.report(STAGES[5], 1.0);
        Ok(DocOutput {
            report: serde_json::json!({
                "wall_thickness": wall,
                "cell_size": cell,
                "grid": grid,
                "band_points": points,
                "inner_faces": inner_faces,
                "faces": mesh.face_count(),
                "vertices": mesh.vertex_count(),
                "thin": thin,
                "holes": hole_rows,
                "open_through": open_through_row,
                "trade_off": TRADE_OFF,
            }),
            changes: vec![ObjectChange::Geometry {
                operand: 0,
                mesh,
                provenance,
            }],
        })
    }
}

/// `ParamValue` for a placed hole at barycentric `b` on `face`.
pub fn hole_sample(face: FaceId, b: [f64; 3]) -> [f64; 5] {
    [f64::from(face.0), b[0], b[1], b[2], 0.0]
}

/// The `hole_points` value for `samples`.
pub fn hole_points(samples: Vec<[f64; 5]>) -> ParamValue {
    ParamValue::Samples(samples)
}

#[cfg(test)]
mod through_plug_tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    /// A flat disk of `faces`, radius `r`, centred at the origin, facing
    /// +Z (`segments` wedges).
    fn flat_cap(r: f64, segments: u32) -> (Mesh, Vec<FaceId>) {
        let mut positions = vec![[0.0, 0.0, 0.0]];
        for s in 0..segments {
            let a = std::f64::consts::TAU * f64::from(s) / f64::from(segments);
            positions.push([r * a.cos(), r * a.sin(), 0.0]);
        }
        let mut triangles = Vec::new();
        for s in 0..segments {
            triangles.push([0, 1 + s, 1 + (s + 1) % segments]);
        }
        let mesh = Mesh::new(positions, triangles).unwrap();
        let faces: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        (mesh, faces)
    }

    #[test]
    fn a_flat_disk_s_plug_is_a_valid_solid() {
        let (disk, faces) = flat_cap(17.3, 96);
        let plug = through_plug(&disk, &faces, 2.0, 1.0, 4.0).unwrap().unwrap();
        let bvh = Bvh::build(&plug, &RunContext::none()).unwrap();
        let all: Vec<FaceId> = plug.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(&plug, &all, &bvh, &RunContext::none())
            .unwrap()
            .unwrap_or_else(|e| panic!("not a valid solid: {e}"));
        let d = mf_geometry::inspect(&plug);
        assert!(d.is_closed && d.is_manifold, "{d:?}");
    }

    /// A flat-topped cylinder (zero curvature anywhere near the cap):
    /// isolates whether `through_plug` can ever go wrong with no
    /// curvature at all to contend with, before blaming curvature for
    /// a sphere's own result.
    /// A flat-topped cylinder of outer radius `r`, its top a disk of
    /// radius `r2` (< `r`) at the centre surrounded by an annulus out to
    /// `r`: a cap region (the fan from vertex 1) whose boundary sits
    /// inside the top's own silhouette, not coincident with the side
    /// wall — like a cap on a sphere, not the cylinder's whole top.
    fn cylinder(r: f64, r2: f64, h: f64, segments: u32) -> Mesh {
        // 0: bottom centre, 1: top centre; per segment: bottom rim,
        // top inner ring, top outer rim.
        let mut positions = vec![[0.0, 0.0, 0.0], [0.0, 0.0, h]];
        for s in 0..segments {
            let a = std::f64::consts::TAU * f64::from(s) / f64::from(segments);
            positions.push([r * a.cos(), r * a.sin(), 0.0]);
            positions.push([r2 * a.cos(), r2 * a.sin(), h]);
            positions.push([r * a.cos(), r * a.sin(), h]);
        }
        let at = |s: u32, k: u32| 2 + 3 * (s % segments) + k;
        let mut triangles = Vec::new();
        for s in 0..segments {
            let (bo0, bo1) = (at(s, 0), at(s + 1, 0));
            let (ti0, ti1) = (at(s, 1), at(s + 1, 1));
            let (to0, to1) = (at(s, 2), at(s + 1, 2));
            triangles.push([0, bo1, bo0]);
            triangles.push([1, ti0, ti1]);
            triangles.push([ti0, to0, to1]);
            triangles.push([ti0, to1, ti1]);
            triangles.push([bo0, bo1, to1]);
            triangles.push([bo0, to1, to0]);
        }
        Mesh::new(positions, triangles).unwrap()
    }

    #[test]
    fn a_cylinder_s_flat_top_plug_only_removes_the_top() {
        let solid = cylinder(30.0, 17.3, 40.0, 96);
        {
            let bvh = Bvh::build(&solid, &RunContext::none()).unwrap();
            let all: Vec<FaceId> = solid.faces().map(|(f, _)| f).collect();
            mf_geometry::validate_solid(&solid, &all, &bvh, &RunContext::none())
                .unwrap()
                .unwrap_or_else(|e| panic!("cylinder itself is not valid: {e}"));
        }
        // A patch of the flat top smaller than the cylinder's own radius
        // (its boundary loop strictly inside the top's own silhouette,
        // like a cap on a sphere, not coincident with the side wall):
        // the fan of triangles around the centre point.
        let top: Vec<FaceId> = solid
            .faces()
            .filter(|(_, t)| t.contains(&1))
            .map(|(f, _)| f)
            .collect();
        assert!(!top.is_empty());
        let plug = through_plug(&solid, &top, 2.0, 0.0, 4.0).unwrap().unwrap();
        let bvh = Bvh::build(&plug, &RunContext::none()).unwrap();
        let all: Vec<FaceId> = plug.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(&plug, &all, &bvh, &RunContext::none())
            .unwrap()
            .unwrap_or_else(|e| panic!("plug is not a valid solid: {e}"));
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let host = crate::OperandInput {
            slot: "target",
            object: mf_document::ObjectId(1),
            name: "scan",
            mesh: &solid,
            transform: identity,
            target: Target::WholeObject,
        };
        let part = crate::PartIn {
            name: "Open through picked surface".into(),
            mesh: std::borrow::Cow::Owned(plug),
            transform: identity,
            role: mf_parts::ShapeRole::Hole,
            host: 0,
            operand: None,
        };
        let applied = crate::apply_parts(
            [Some((0, &host)), None],
            &[part],
            crate::ApplyOrder::Sequential,
            &RunContext::none(),
        )
        .unwrap_or_else(|e| panic!("subtracting the plug failed: {e:?}"));
        let a = &applied[0];
        // Far from the cut (outside the boolean window around the
        // plug), the surface is bit-identical, same ids. Right at the
        // cut's own boundary a boolean kernel re-triangulates its local
        // window, same as any other cut (split, a drain hole): a face
        // there may get a new id, but never moves (checked below by
        // position, not id).
        let far: Vec<(FaceId, [u32; 3])> = solid
            .faces()
            .filter(|(f, _)| !top.contains(f))
            .filter(|(_, t)| {
                t.iter().all(|&v| {
                    let p = solid.corner_position(v);
                    (p[0] * p[0] + p[1] * p[1]).sqrt() > 20.0
                })
            })
            .collect();
        assert!(!far.is_empty());
        for (f, t) in far {
            assert_eq!(a.mesh.face(f), Some(t), "face {f:?} far from the cut moved");
        }
        // No face anywhere outside the picked region actually moved
        // (only the bit-identical outer surface matters, whatever id a
        // face near the cut's own boundary ends up with).
        let mut moved = 0;
        for (f, t) in solid.faces() {
            if top.contains(&f) || !a.mesh.is_face_live(f) {
                continue;
            }
            let before: Vec<[f64; 3]> = t.iter().map(|&v| solid.corner_position(v)).collect();
            let after: Vec<[f64; 3]> = a
                .mesh
                .face(f)
                .unwrap()
                .iter()
                .map(|&v| a.mesh.corner_position(v))
                .collect();
            if before != after {
                moved += 1;
            }
        }
        assert_eq!(moved, 0, "no face outside the picked region should move");
    }

    /// A flat disk whose boundary has a deliberate near-duplicate pair
    /// (two consecutive loop vertices a single ulp apart, never farther
    /// — the shape a plane cut's own near-plane vertex and a freshly
    /// cut point next to it can leave after the vertex is snapped onto
    /// the cut plane exactly, owner 2026-11-13 follow-up): the plug is
    /// still a valid, watertight solid (the near-duplicate is merged,
    /// and the top cap's own triangles that referenced it are
    /// remapped, not left dangling).
    #[test]
    fn a_boundary_with_a_near_duplicate_vertex_still_gives_a_watertight_plug() {
        let (mut disk, faces) = flat_cap(17.3, 96);
        // Nudge one boundary vertex's neighbour a single ulp away from
        // it (never farther), as if it were two independently computed
        // near-coincident cut points.
        let mut edit = disk.edit();
        let p = disk.position(mf_geometry::VertexId(1)).unwrap();
        let nudged = p.map(|c| c + f64::EPSILON.max(c.abs() * f64::EPSILON));
        edit.move_vertex(mf_geometry::VertexId(1), nudged).unwrap();
        disk = edit.finish().unwrap().0;
        let plug = through_plug(&disk, &faces, 2.0, 1.0, 4.0).unwrap().unwrap();
        let bvh = Bvh::build(&plug, &RunContext::none()).unwrap();
        let all: Vec<FaceId> = plug.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(&plug, &all, &bvh, &RunContext::none())
            .unwrap()
            .unwrap_or_else(|e| panic!("not a valid solid: {e}"));
        let topo = plug.topology();
        assert_eq!(topo.boundary_edges, 0, "the plug must be watertight");
    }

    /// A real split-sphere cap (`mesh.split_plane`, `keep: above`),
    /// through `open_through` itself (not just `through_plug`): the
    /// plug is a valid solid, and the dome's own surface away from the
    /// cap never moves (the safety check this function makes).
    #[test]
    fn open_through_a_real_split_cap_moves_nothing_outside_it() {
        let sphere = mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 20.0);
        let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
        let raw: std::collections::BTreeMap<String, String> = [
            ("plane_point", "0,0,1.3".to_string()),
            ("plane_normal", "0,0,1".to_string()),
            ("keep", "above".to_string()),
            ("cap", "true".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let params = spec.validate(&raw).unwrap();
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let out = crate::find("mesh.split_plane")
            .unwrap()
            .execute_doc(
                &[crate::OperandInput {
                    slot: "target",
                    object: mf_document::ObjectId(1),
                    name: "scan",
                    mesh: &sphere,
                    transform: identity,
                    target: Target::WholeObject,
                }],
                &params,
                &RunContext::none(),
            )
            .unwrap();
        let crate::ObjectChange::Geometry {
            mesh: dome,
            provenance,
            ..
        } = out.changes.into_iter().next().unwrap()
        else {
            panic!("expected geometry");
        };
        let cap: Vec<FaceId> = provenance.created_faces.iter().map(|(f, _)| *f).collect();
        assert!(!cap.is_empty());
        let target = crate::OperandInput {
            slot: "target",
            object: mf_document::ObjectId(1),
            name: "scan",
            mesh: &dome,
            transform: identity,
            target: Target::WholeObject,
        };
        let (result, _, _) =
            open_through(0.5, &target, &dome, &cap, 2.0, &RunContext::none()).unwrap();
        let moved = dome
            .faces()
            .filter(|(f, _)| !cap.contains(f))
            .filter(|(f, _)| result.is_face_live(*f))
            .filter(|(f, t)| {
                let before: [[f64; 3]; 3] = t.map(|v| dome.corner_position(v));
                let after: [[f64; 3]; 3] =
                    result.face(*f).unwrap().map(|v| result.corner_position(v));
                before != after
            })
            .count();
        assert_eq!(moved, 0, "the dome's own surface must not move");
    }
}

/// The inner surface's ripple amplitude (owner 2026-11-14): the extra
/// Taubin + `hold_wall_depth` passes in [`inner_surface`] against the
/// surface marching tetrahedra alone leaves behind, on the same grid
/// and band, and the wall-thickness guarantee that smoothing must not
/// break.
#[cfg(test)]
mod ripple_tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    /// Each inner vertex's own distance from the plane fit through its
    /// immediate neighbours' mean radius (the sphere's own curvature,
    /// the sagitta over one cell, is the same at every vertex and
    /// cancels out of this; what's left is the voxel stairstep a
    /// marching-tetrahedra surface leaves behind, which the owner
    /// reported as "ripples"). Returns (mean, max), in the object's
    /// own units.
    fn ripple_amplitude(mesh: &Mesh, inner: &[FaceId]) -> (f64, f64) {
        let mut neighbours: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::new();
        for &f in inner {
            let Some(t) = mesh.face(f) else { continue };
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                neighbours.entry(a).or_default().push(b);
                neighbours.entry(b).or_default().push(a);
            }
        }
        let mut devs = Vec::new();
        for (&v, ns) in &neighbours {
            if ns.len() < 3 {
                continue;
            }
            let r = norm(mesh.position(VertexId(v)).unwrap());
            let mr = ns
                .iter()
                .map(|&u| norm(mesh.position(VertexId(u)).unwrap()))
                .sum::<f64>()
                / ns.len() as f64;
            devs.push((r - mr).abs());
        }
        devs.sort_unstable_by(f64::total_cmp);
        let mean = devs.iter().sum::<f64>() / devs.len().max(1) as f64;
        let max = devs.last().copied().unwrap_or(0.0);
        (mean, max)
    }

    /// A sphere's own shell, with and without 4 ripple-smoothing passes
    /// (not [`RIPPLE_SMOOTHING_PASSES`] itself, which is 0 for now — see
    /// its own doc comment — so this exercises the machinery directly,
    /// on the same grid and band): smoothing cuts the ripple amplitude
    /// substantially, and never leaves the wall thinner than asked (the
    /// owner's own report, 2026-11-14). Each extra pass is a free
    /// Taubin pass tried behind its own self-intersection check (an
    /// edge-confined pass, tried first, was always safe but never
    /// actually reduced this lateral artifact — confirmed: more of it
    /// measurably made the ripple worse, not better).
    #[test]
    fn the_smoothing_pass_cuts_ripple_amplitude_and_holds_the_wall() {
        let input = mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 20.0);
        let (wall, detail) = (2.0, 0.5);
        let ctx = RunContext::none();
        let before = hollow_shell_passes(&input, wall, detail, 0, &ctx).unwrap();
        let after = hollow_shell_passes(&input, wall, detail, 4, &ctx).unwrap();
        let (mean_before, max_before) = ripple_amplitude(&before.mesh, &before.inner);
        let (mean_after, max_after) = ripple_amplitude(&after.mesh, &after.inner);
        // Reported for the record (owner 2026-11-14's own ask): before
        // and after ripple amplitude, in the object's own units.
        println!(
            "ripple amplitude before: mean {mean_before:.5}, max {max_before:.5}; after 4 passes: mean {mean_after:.5}, max {max_after:.5}"
        );
        assert!(
            mean_after < mean_before * 0.6,
            "mean ripple should drop substantially: before {mean_before}, after {mean_after}"
        );
        assert!(
            max_after < max_before * 0.8,
            "max ripple should drop too: before {max_before}, after {max_after}"
        );
        // The wall thickness guarantee smoothing must not break: every
        // inner vertex at least `wall` deep (within float slop).
        let outer_r = input
            .vertices()
            .map(|(_, p)| norm(p))
            .fold(f64::INFINITY, f64::min);
        let inner_positions: Vec<[f64; 3]> = after
            .mesh
            .vertices()
            .filter(|(v, _)| input.position(*v).is_none())
            .map(|(_, p)| p)
            .collect();
        assert!(!inner_positions.is_empty());
        for p in &inner_positions {
            let depth = outer_r - norm(*p);
            assert!(
                depth >= wall - after.cell,
                "a smoothed vertex at depth {depth} is thinner than the wall {wall} (cell {})",
                after.cell
            );
        }
    }

    /// The mushroom's own cap stays thin at the setting this actually
    /// ships with ([`RIPPLE_SMOOTHING_PASSES`], 0 for now): a placed
    /// drain hole at the thin cap's rim must stay refused. At 4 passes
    /// (the extra free smoothing this module's own doc comment
    /// explains is held back) the same hole stops being refused — a
    /// stem-cavity vertex near its own top moves just enough for the
    /// ray this checks to start reaching it. That's the regression
    /// that kept this off; asserted here too (as a known failure, not a
    /// silent one) so fixing it is checked against the real case, not
    /// just the sphere the ripple-amplitude test above uses.
    #[test]
    fn smoothing_does_not_widen_a_thin_part_s_own_cavity_reach() {
        let profile = [(15.0, 0.0), (15.0, 20.0), (30.0, 20.0), (30.0, 23.0)];
        let segments = 96u32;
        let mut positions = vec![[0.0, 0.0, 0.0], [0.0, 0.0, 23.0]];
        for k in 0..segments {
            let a = std::f64::consts::TAU * f64::from(k) / f64::from(segments);
            for &(r, z) in &profile {
                positions.push([r * a.cos(), r * a.sin(), z]);
            }
        }
        let p = profile.len() as u32;
        let at = |k: u32, i: u32| 2 + (k % segments) * p + i;
        let mut tris = Vec::new();
        for k in 0..segments {
            tris.push([0, at(k + 1, 0), at(k, 0)]);
            tris.push([1, at(k, p - 1), at(k + 1, p - 1)]);
            for i in 0..p - 1 {
                let (a, b, c, d) = (at(k, i), at(k + 1, i), at(k + 1, i + 1), at(k, i + 1));
                tris.push([a, b, c]);
                tris.push([a, c, d]);
            }
        }
        let input = Mesh::new(positions, tris).unwrap();
        let ctx = RunContext::none();
        let (face, _) = input
            .faces()
            .find(|(_, t)| {
                t.iter().all(|&v| {
                    let p = input.corner_position(v);
                    (norm([p[0], p[1], 0.0]) - 30.0).abs() < 1e-9
                })
            })
            .expect("the cap has a rim face at radius 30");
        let sample = [
            f64::from(face.0),
            1.0 / 3.0,
            1.0 / 3.0,
            1.0 / 3.0 + 1e-10,
            0.0,
        ];
        let refused = |extra_passes: usize| -> bool {
            let shell = hollow_shell_passes(&input, 2.0, 0.5, extra_passes, &ctx).unwrap();
            let result = placed_holes(&shell, &[sample], 3.0, &ctx);
            matches!(&result, Err(OpError::Failed { code, .. }) if *code == "hole_misses_cavity")
        };
        assert!(
            refused(RIPPLE_SMOOTHING_PASSES),
            "a drain hole on the thin cap's rim should stay refused at what this ships with"
        );
        assert!(
            !refused(4),
            "this is the known regression (see this test's own doc comment): if it's now \
             refused too, RIPPLE_SMOOTHING_PASSES can go back to 4 and this assert can flip"
        );
    }
}
