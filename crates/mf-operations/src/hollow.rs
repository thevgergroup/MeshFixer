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
//!    on the band around depth `wall_thickness`, one Taubin pass sliding
//!    vertices along their grid edges, then every triangle reversed so it
//!    faces into the cavity. Where the model is thinner than twice the
//!    wall no point is that deep, so no cavity is made there: those parts
//!    stay solid and the result is still valid.
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

/// Step 4: the inner surface (facing into the cavity), or `None` when
/// nothing is deep enough to hollow.
fn inner_surface(
    grid: &Grid,
    band: &Band,
    rays: &Rays,
    wall: f64,
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
    taubin(&mut verts, &edges, &tris, ctx)?;
    // Facing into the cavity: away from the material.
    for t in &mut tris {
        t.swap(1, 2);
    }
    Ok(Some((verts, tris, cells)))
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
    let Some((verts, tris, cells)) = inner_surface(&grid, &band, &rays, wall, ctx)? else {
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

        let shell = hollow_shell(target.mesh, wall, detail, ctx)?;

        ctx.progress.report(STAGES[5], 0.0);
        let holes = match mode {
            "auto" => auto_holes(&shell, &target.transform, count, diameter, ctx)?,
            "placed" => placed_holes(&shell, &samples, diameter, ctx)?,
            _ => Vec::new(),
        };
        let thin = serde_json::json!({
            "faces": shell.thin.len(),
            "area": shell.thin_area,
            "fraction": if shell.outer_area > 0.0 { shell.thin_area / shell.outer_area } else { 0.0 },
            "below": 2.0 * wall,
            "message": if shell.thin.is_empty() { serde_json::Value::Null } else { thin_message(wall).into() },
        });
        let inner_faces = shell.inner.len();
        let (cell, grid, points) = (shell.cell, shell.grid, shell.band_points);
        let (mesh, provenance, hole_rows) = if holes.is_empty() {
            (shell.mesh, shell.provenance, Vec::new())
        } else {
            let (mesh, step, rows) = cut_holes(&shell, target, &holes, diameter, profile, ctx)?;
            let provenance = crate::blend::compose(&shell.provenance, &step);
            (mesh, provenance, rows)
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
