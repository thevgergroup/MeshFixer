//! `mesh.make_solid` (MF-MODEL-001, Phase 1 Task 8a): Meshmixer's Make
//! Solid. Rebuilds the surface as one closed solid; fine detail smaller
//! than the cell size is lost. Never applied silently: it is the next
//! action of the boolean's `operand_overlaps_itself` refusal.
//!
//! 1. **Measuring.** A grid of `cell_size` over the bounds (default 1/256
//!    of the diagonal), padded so its border is outside. A grid over the
//!    budget is refused by name (`grid_too_large`, "use a larger cell
//!    size"); it is never coarsened silently.
//! 2. **Building the field.** Along each axis, a ray through every grid
//!    column collects the surface's crossings and their signs, so each grid
//!    point gets a winding number (nonzero = inside: self-intersecting and
//!    overlapping shells become their union) and its distance to the
//!    nearest crossing along that axis. A point is inside when two of the
//!    three axes say so (so a small opening misleads one ray at most), and
//!    its value is its distance (negative inside), less `offset`.
//! 3. **Extracting the surface.** Marching tetrahedra on the grid split
//!    into six tetrahedra per cell around the main diagonal (the same split
//!    in every cell, so neighbours agree): the surface of a
//!    piecewise-linear field with no value exactly at the level, which is
//!    closed, 2-manifold and doesn't cross itself. With `preserve_sharp`
//!    off, one Taubin pass then smooths it, each vertex sliding only along
//!    its own grid edge (so every triangle stays inside its tetrahedron and
//!    the surface still can't cross itself).
//! 4. **Checking the result.** D4's `validate_solid` over every face; a
//!    failure is refused by name.
//!
//! Deterministic at any thread count (parallel work is merged in a fixed
//! order), cancellable at every stage.

use std::collections::HashMap;

use mf_geometry::{Bvh, FaceId, Mesh, Provenance, Sources, VertexId};
use mf_runtime::Cancelled;
use mf_spec::Params;
use rayon::prelude::*;

use crate::{OpError, OpOutput, Operation, RunContext, Target};

pub struct MakeSolid;

/// Most grid points: two f32-sized arrays of this many (about 300 MB).
pub const MAX_GRID_POINTS: u64 = 40_000_000;

/// Stage words, in order.
pub const STAGES: [&str; 4] = [
    "measuring",
    "building the field",
    "extracting the surface",
    "checking the result",
];

/// The plain-language trade-off the UI states.
pub const TRADE_OFF: &str =
    "Rebuilds the surface as one closed solid; fine detail smaller than the cell size is lost.";

fn failed(code: &'static str, message: String) -> OpError {
    OpError::Failed { code, message }
}

struct Grid {
    origin: [f64; 3],
    h: f64,
    n: [usize; 3],
}

impl Grid {
    fn index(&self, i: [usize; 3]) -> usize {
        i[0] + self.n[0] * (i[1] + self.n[1] * i[2])
    }

    fn point(&self, i: [usize; 3]) -> [f64; 3] {
        [0, 1, 2].map(|a| self.origin[a] + i[a] as f64 * self.h)
    }

    fn count(&self) -> usize {
        self.n[0] * self.n[1] * self.n[2]
    }
}

impl Operation for MakeSolid {
    fn id(&self) -> &'static str {
        "mesh.make_solid"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        params: &Params,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        if *target != Target::WholeObject {
            return Err(failed(
                "region_not_supported",
                "Make solid works on a whole object in this version. Select the whole object (click Model), then run it again.".into(),
            ));
        }
        let cell = params.get_float("cell_size")?;
        let offset = params.get_float("offset")?;
        let sharp = params.get_bool("preserve_sharp")?;

        // 1. Measuring.
        ctx.progress.report(STAGES[0], 0.0);
        ctx.checkpoint()?;
        let Some(bb) = input.bounding_box_with(ctx)? else {
            return Err(failed(
                "empty",
                "The object has no faces to rebuild.".into(),
            ));
        };
        let h = if cell > 0.0 {
            cell
        } else {
            bb.diagonal() / 256.0
        };
        if !(h > 0.0 && h.is_finite()) {
            return Err(failed(
                "invalid_parameter",
                format!("cell size {h} isn't usable"),
            ));
        }
        let pad = 2.0 * h + offset.max(0.0);
        let origin = bb.min.map(|x| x - pad);
        let n = [0, 1, 2].map(|a| ((bb.max[a] - bb.min[a] + 2.0 * pad) / h).ceil() as u64 + 1);
        let total = n[0].saturating_mul(n[1]).saturating_mul(n[2]);
        if total > MAX_GRID_POINTS {
            return Err(failed(
                "grid_too_large",
                format!(
                    "A cell size of {h} needs {total} grid points, more than the {MAX_GRID_POINTS} this can hold. Use a larger cell size (at least {:.3}).",
                    h * (total as f64 / MAX_GRID_POINTS as f64).cbrt() * 1.01
                ),
            ));
        }
        let grid = Grid {
            origin,
            h,
            n: n.map(|x| x as usize),
        };
        let (positions, triangles) = input.to_arrays();

        // 2. Building the field.
        ctx.progress.report(STAGES[1], 0.0);
        let band = (3.0 * h + offset.abs()) as f32;
        let field = field(&grid, &positions, &triangles, band, ctx)?;
        let field: Vec<f32> = field.into_iter().map(|v| v - offset as f32).collect();

        // 3. Extracting the surface.
        ctx.progress.report(STAGES[2], 0.0);
        let (mut verts, edges, tris) = march(&grid, &field, ctx)?;
        drop(field);
        if tris.is_empty() {
            return Err(failed(
                "result_empty",
                "Nothing would be left at this cell size and offset. Use a smaller cell size or a smaller negative offset.".into(),
            ));
        }
        if !sharp {
            taubin(&mut verts, &edges, &tris, ctx)?;
        }

        // 4. Checking the result.
        ctx.progress.report(STAGES[3], 0.0);
        ctx.checkpoint()?;
        let solid = Mesh::new(verts.clone(), tris.clone()).map_err(|e| {
            failed(
                "result_not_valid",
                format!("The rebuilt surface is invalid ({e})."),
            )
        })?;
        let bvh = Bvh::build(&solid, ctx)?;
        let all: Vec<FaceId> = solid.faces().map(|(f, _)| f).collect();
        if let Err(d) = mf_geometry::validate_solid(&solid, &all, &bvh, ctx)? {
            return Err(failed(
                "result_not_valid",
                format!(
                    "The rebuilt surface didn't pass the solid check ({d}). Try a different cell size."
                ),
            ));
        }
        drop(solid);

        // The new surface replaces the object's: every face is created.
        let mut edit = input.edit();
        let mut ticker = ctx.ticker();
        for (f, _) in input.faces() {
            ticker.tick()?;
            edit.remove_face(f).map_err(geometry)?;
        }
        for (v, _) in input.vertices() {
            ticker.tick()?;
            edit.remove_vertex(v).map_err(geometry)?;
        }
        let ids: Vec<VertexId> = verts.iter().map(|&p| edit.add_vertex(p)).collect();
        for t in &tris {
            ticker.tick()?;
            edit.add_face(t.map(|v| ids[v as usize].0), Sources::default())
                .map_err(geometry)?;
        }
        let (mesh, provenance): (Mesh, Provenance) = edit.finish().map_err(geometry)?;
        ctx.progress.report(STAGES[3], 1.0);
        Ok(OpOutput {
            report: serde_json::json!({
                "cell_size": h,
                "grid": grid.n,
                "faces": mesh.face_count(),
                "vertices": mesh.vertex_count(),
                "trade_off": TRADE_OFF,
            }),
            change: Some((mesh, provenance)),
        })
    }
}

fn geometry(e: mf_geometry::GeometryError) -> OpError {
    failed(
        "result_not_valid",
        format!("The rebuilt surface is invalid ({e})."),
    )
}

/// The signed field (see the module docs), `band` far at most.
fn field(
    grid: &Grid,
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    band: f32,
    ctx: &RunContext,
) -> Result<Vec<f32>, Cancelled> {
    let count = grid.count();
    let mut votes = vec![0u8; count];
    let mut dist = vec![band; count];
    // Rays run a tiny, irregular fraction of a cell off the grid lines, so
    // they don't pass exactly through the mesh's edges and vertices.
    let jitter = [0.000_123_457, 0.000_271_828, 0.000_314_159];
    for (step, a) in [0usize, 1, 2].into_iter().enumerate() {
        ctx.progress.report(STAGES[1], step as f64 / 3.0);
        let (b, c) = ((a + 1) % 3, (a + 2) % 3);
        let (nb, nc) = (grid.n[b], grid.n[c]);
        let lb = |i: usize| grid.origin[b] + (i as f64 + jitter[b]) * grid.h;
        let lc = |i: usize| grid.origin[c] + (i as f64 + jitter[c]) * grid.h;
        // (column, coordinate along a, sign) for every crossing.
        let mut hits: Vec<(u32, f64, i8)> = triangles
            .par_chunks(1 << 14)
            .map(|chunk| -> Result<Vec<(u32, f64, i8)>, Cancelled> {
                ctx.checkpoint()?;
                let mut out = Vec::new();
                for t in chunk {
                    let p = t.map(|v| positions[v as usize]);
                    let (pb, pc) = (p.map(|q| q[b]), p.map(|q| q[c]));
                    let area =
                        (pb[1] - pb[0]) * (pc[2] - pc[0]) - (pb[2] - pb[0]) * (pc[1] - pc[0]);
                    if area == 0.0 {
                        continue;
                    }
                    let col = |x: f64, o: f64| ((x - o) / grid.h - 1e-9).floor();
                    let lo_b = col(
                        pb.iter().copied().fold(f64::INFINITY, f64::min),
                        grid.origin[b],
                    )
                    .max(0.0) as usize;
                    let hi_b = (col(
                        pb.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                        grid.origin[b],
                    ) + 1.0)
                        .min(nb as f64 - 1.0) as usize;
                    let lo_c = col(
                        pc.iter().copied().fold(f64::INFINITY, f64::min),
                        grid.origin[c],
                    )
                    .max(0.0) as usize;
                    let hi_c = (col(
                        pc.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                        grid.origin[c],
                    ) + 1.0)
                        .min(nc as f64 - 1.0) as usize;
                    for ib in lo_b..=hi_b {
                        let y = lb(ib);
                        for ic in lo_c..=hi_c {
                            let z = lc(ic);
                            // Barycentric coordinates of (y, z) in the projection.
                            let w0 = ((pb[1] - y) * (pc[2] - z) - (pb[2] - y) * (pc[1] - z)) / area;
                            let w1 = ((pb[2] - y) * (pc[0] - z) - (pb[0] - y) * (pc[2] - z)) / area;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let x = w0 * p[0][a] + w1 * p[1][a] + w2 * p[2][a];
                            out.push(((ib + nb * ic) as u32, x, if area > 0.0 { 1 } else { -1 }));
                        }
                    }
                }
                Ok(out)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        hits.par_sort_unstable_by(|p, q| {
            p.0.cmp(&q.0).then(p.1.total_cmp(&q.1)).then(p.2.cmp(&q.2))
        });
        ctx.checkpoint()?;
        let columns: Vec<&[(u32, f64, i8)]> = hits.chunk_by(|p, q| p.0 == q.0).collect();
        // Per column: (column, per point along a: inside, distance).
        let per_column: Vec<(u32, Vec<(bool, f32)>)> = columns
            .par_iter()
            .map(|col| -> Result<_, Cancelled> {
                ctx.checkpoint()?;
                let mut out = Vec::with_capacity(grid.n[a]);
                let mut k = 0;
                let mut winding: i32 = 0;
                for i in 0..grid.n[a] {
                    let x = grid.origin[a] + i as f64 * grid.h;
                    while k < col.len() && col[k].1 < x {
                        winding += i32::from(col[k].2);
                        k += 1;
                    }
                    let mut d = f64::from(band);
                    if k < col.len() {
                        d = d.min(col[k].1 - x);
                    }
                    if k > 0 {
                        d = d.min(x - col[k - 1].1);
                    }
                    out.push((winding != 0, d as f32));
                }
                Ok((col[0].0, out))
            })
            .collect::<Result<_, _>>()?;
        for (column, along) in per_column {
            let (ib, ic) = (column as usize % nb, column as usize / nb);
            for (i, (inside, d)) in along.into_iter().enumerate() {
                let mut idx = [0; 3];
                idx[a] = i;
                idx[b] = ib;
                idx[c] = ic;
                let g = grid.index(idx);
                votes[g] += u8::from(inside);
                dist[g] = dist[g].min(d);
            }
        }
        ctx.checkpoint()?;
    }
    Ok(votes
        .into_iter()
        .zip(dist)
        .map(|(v, d)| if v >= 2 { -d } else { d })
        .collect())
}

/// The six tetrahedra of a cell around its main diagonal, as corner
/// offsets (bit 0 = +x, bit 1 = +y, bit 2 = +z).
const TETS: [[u8; 4]; 6] = [
    [0, 1, 3, 7],
    [0, 1, 5, 7],
    [0, 2, 3, 7],
    [0, 2, 6, 7],
    [0, 4, 5, 7],
    [0, 4, 6, 7],
];

/// Marching tetrahedra (see the module docs): vertices on grid edges
/// (shared through their edge), triangles facing outward.
#[allow(clippy::type_complexity)]
fn march(
    grid: &Grid,
    field: &[f32],
    ctx: &RunContext,
) -> Result<(Vec<[f64; 3]>, Vec<[[f64; 3]; 2]>, Vec<[u32; 3]>), Cancelled> {
    let [n0, n1, n2] = grid.n;
    let inside = |g: usize| field[g] < 0.0;
    let corner = |i: [usize; 3], c: u8| {
        [
            i[0] + (c & 1) as usize,
            i[1] + ((c >> 1) & 1) as usize,
            i[2] + ((c >> 2) & 1) as usize,
        ]
    };
    // Cells with corners on both sides, in order, found per z-slab in
    // parallel.
    let cells: Vec<[usize; 3]> = (0..n2 - 1)
        .into_par_iter()
        .map(|z| -> Result<Vec<[usize; 3]>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for y in 0..n1 - 1 {
                for x in 0..n0 - 1 {
                    let i = [x, y, z];
                    let first = inside(grid.index(i));
                    if (1..8u8).any(|c| inside(grid.index(corner(i, c))) != first) {
                        out.push(i);
                    }
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut edges: Vec<[[f64; 3]; 2]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut on_edge: HashMap<(usize, usize), u32> = HashMap::new();
    let mut ticker = ctx.ticker();
    for &cell in &cells {
        ticker.tick()?;
        for tet in TETS {
            let g = tet.map(|c| grid.index(corner(cell, c)));
            let p = tet.map(|c| grid.point(corner(cell, c)));
            let ins: Vec<usize> = (0..4).filter(|&k| inside(g[k])).collect();
            let outs: Vec<usize> = (0..4).filter(|&k| !inside(g[k])).collect();
            if ins.is_empty() || outs.is_empty() {
                continue;
            }
            let mut vertex = |a: usize, b: usize| -> u32 {
                let key = (g[a].min(g[b]), g[a].max(g[b]));
                *on_edge.entry(key).or_insert_with(|| {
                    // From the lower grid index, so both cells sharing the
                    // edge compute the same point.
                    let (i, j) = if g[a] < g[b] { (a, b) } else { (b, a) };
                    let (fi, fj) = (f64::from(field[g[i]]), f64::from(field[g[j]]));
                    let t = (fi / (fi - fj)).clamp(1e-3, 1.0 - 1e-3);
                    verts.push([0, 1, 2].map(|k| p[i][k] + t * (p[j][k] - p[i][k])));
                    edges.push([p[i], p[j]]);
                    (verts.len() - 1) as u32
                })
            };
            let mut faces: Vec<[u32; 3]> = Vec::new();
            match (ins.len(), outs.len()) {
                (1, 3) => faces.push([
                    vertex(ins[0], outs[0]),
                    vertex(ins[0], outs[1]),
                    vertex(ins[0], outs[2]),
                ]),
                (3, 1) => faces.push([
                    vertex(outs[0], ins[0]),
                    vertex(outs[0], ins[1]),
                    vertex(outs[0], ins[2]),
                ]),
                _ => {
                    let (a, b, c, d) = (ins[0], ins[1], outs[0], outs[1]);
                    let (ac, ad, bd, bc) = (vertex(a, c), vertex(a, d), vertex(b, d), vertex(b, c));
                    faces.push([ac, ad, bd]);
                    faces.push([ac, bd, bc]);
                }
            }
            // Outward: from the inside corners towards the outside ones.
            let mean = |ks: &[usize]| {
                let s = ks.iter().fold([0.0; 3], |acc, &k| {
                    [acc[0] + p[k][0], acc[1] + p[k][1], acc[2] + p[k][2]]
                });
                s.map(|x| x / ks.len() as f64)
            };
            let (mi, mo) = (mean(&ins), mean(&outs));
            let dir = [mo[0] - mi[0], mo[1] - mi[1], mo[2] - mi[2]];
            for mut f in faces {
                let [a, b, c] = f.map(|v| verts[v as usize]);
                let (u, w) = (
                    [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                );
                let n = [
                    u[1] * w[2] - u[2] * w[1],
                    u[2] * w[0] - u[0] * w[2],
                    u[0] * w[1] - u[1] * w[0],
                ];
                if n[0] * dir[0] + n[1] * dir[1] + n[2] * dir[2] < 0.0 {
                    f.swap(1, 2);
                }
                tris.push(f);
            }
        }
    }
    Ok((verts, edges, tris))
}

/// One Taubin pass (λ = 0.5, μ = -0.53): smooths without shrinking. Each
/// vertex moves only along its grid edge `edges[v]` (projected, kept off
/// the edge's ends).
fn taubin(
    verts: &mut [[f64; 3]],
    edges: &[[[f64; 3]; 2]],
    tris: &[[u32; 3]],
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
                let ns = &neighbours[v];
                if ns.is_empty() {
                    return verts[v];
                }
                let m = ns.iter().fold([0.0; 3], |acc, &u| {
                    let q = verts[u as usize];
                    [acc[0] + q[0], acc[1] + q[1], acc[2] + q[2]]
                });
                let m = m.map(|x| x / ns.len() as f64);
                let want: [f64; 3] = [0, 1, 2].map(|k| verts[v][k] + factor * (m[k] - verts[v][k]));
                let [a, b] = edges[v];
                let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                let t =
                    ((want[0] - a[0]) * d[0] + (want[1] - a[1]) * d[1] + (want[2] - a[2]) * d[2])
                        / len2;
                let t = t.clamp(1e-3, 1.0 - 1e-3);
                [0, 1, 2].map(|k| a[k] + t * d[k])
            })
            .collect();
        verts.copy_from_slice(&next);
    }
    Ok(())
}
