//! `mesh.fill_holes` (MF-OP-001/002/003).
//!
//! Each target loop is closed by a minimum-weight triangulation of its
//! boundary polygon, refined to the density around it and (fill styles
//! `smooth` and `curvature`) faired to the surface around it. Everything
//! goes through `MeshEdit`, so kept geometry is untouched and provenance
//! names the loop of every new face.

pub(crate) mod fairing;
mod refine;
mod triangulate;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use fairing::Style;
use mf_geometry::{BoundaryLoop, Mesh, Sources};
use mf_runtime::Cancelled;
use mf_spec::{ParamValue, Params};
use rayon::prelude::*;
use serde_json::json;

use crate::{
    DocContext, OpError, OpOutput, Operation, RunContext, TRIANGULATION_BUDGET_BYTES, Target,
    unsupported_target,
};

pub struct FillHoles;

/// MF-ISSUE-055 (open, an algorithm limit): the largest hole `curvature`
/// fills to within 1% of the surface, measured on a radius-50 UV sphere
/// (192×96): 50–60-edge holes stay under 1%, a 64-edge hole reaches 1.3%,
/// 94 edges 2.6%, ~200 edges 12%, while `smooth` stays under 1%. Above it
/// the report says so and recommends smooth; the style is never switched.
pub const CURVATURE_ACCURATE_EDGES: usize = 60;

/// The plain-language warning for `count` curvature-filled holes over
/// [`CURVATURE_ACCURATE_EDGES`].
pub fn curvature_limit_warning(count: usize) -> String {
    let (holes, has) = if count == 1 {
        ("1 hole".to_owned(), "has")
    } else {
        (format!("{count} holes"), "have")
    };
    format!(
        "Curvature matching is less accurate on large holes: {holes} filled here {has} more \
         than {CURVATURE_ACCURATE_EDGES} edges, and the fill may bulge away from the surface. \
         Smooth fill is recommended for holes this size."
    )
}

fn failed(code: &'static str, message: String) -> OpError {
    OpError::Failed { code, message }
}

fn unique_vertices(l: &BoundaryLoop) -> bool {
    let set: BTreeSet<u32> = l.vertices.iter().copied().collect();
    set.len() == l.vertices.len()
}

/// Marks every vertex on one of `loops`, so whole-mesh passes can skip the
/// (vast majority of) faces that touch none.
fn on_loops(mesh: &Mesh, loops: &[&BoundaryLoop]) -> Vec<bool> {
    let mut on = vec![false; mesh.vertex_slots() as usize];
    for l in loops {
        for &v in &l.vertices {
            on[v as usize] = true;
        }
    }
    on
}

/// Unit normal of the existing face across each directed loop edge.
fn outer_normals(
    mesh: &Mesh,
    loops: &[&BoundaryLoop],
    on: &[bool],
    ctx: &RunContext,
) -> Result<Vec<Vec<[f64; 3]>>, Cancelled> {
    let mut want: BTreeMap<(u32, u32), (usize, usize)> = BTreeMap::new();
    for (li, l) in loops.iter().enumerate() {
        let n = l.vertices.len();
        for k in 0..n {
            want.insert((l.vertices[k], l.vertices[(k + 1) % n]), (li, k));
        }
    }
    let mut out: Vec<Vec<[f64; 3]>> = loops
        .iter()
        .map(|l| vec![[0.0; 3]; l.vertices.len()])
        .collect();
    let mut tick = ctx.ticker();
    for (_, t) in mesh.faces() {
        tick.tick()?;
        if !t.iter().any(|&v| on[v as usize]) {
            continue;
        }
        for c in 0..3 {
            if let Some(&(li, k)) = want.get(&(t[c], t[(c + 1) % 3])) {
                let [a, b, d] = t.map(|v| mesh.corner_position(v));
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
                let nrm = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
                if len > 0.0 {
                    out[li][k] = nrm.map(|x| x / len);
                }
            }
        }
    }
    Ok(out)
}

/// Per loop: the target edge length at each loop vertex (mean length of its
/// existing edges), and the existing edges between loop vertices in local
/// indices (so neither triangulation nor refinement duplicates one).
type LoopContext = (Vec<Vec<f64>>, Vec<BTreeSet<(usize, usize)>>);

fn loop_context(
    mesh: &Mesh,
    loops: &[&BoundaryLoop],
    on: &[bool],
    ctx: &RunContext,
) -> Result<LoopContext, Cancelled> {
    let mut local: BTreeMap<u32, Vec<(usize, usize)>> = BTreeMap::new();
    for (li, l) in loops.iter().enumerate() {
        for (k, &v) in l.vertices.iter().enumerate() {
            local.entry(v).or_default().push((li, k));
        }
    }
    let mut sums: Vec<Vec<(f64, u32)>> = loops
        .iter()
        .map(|l| vec![(0.0, 0); l.vertices.len()])
        .collect();
    let mut existing: Vec<BTreeSet<(usize, usize)>> = vec![BTreeSet::new(); loops.len()];
    let mut tick = ctx.ticker();
    for (_, t) in mesh.faces() {
        tick.tick()?;
        if !t.iter().any(|&v| on[v as usize]) {
            continue;
        }
        for c in 0..3 {
            let (a, b) = (t[c], t[(c + 1) % 3]);
            let (pa, pb) = (mesh.corner_position(a), mesh.corner_position(b));
            let length =
                ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2))
                    .sqrt();
            for v in [a, b] {
                if let Some(slots) = local.get(&v) {
                    for &(li, k) in slots {
                        sums[li][k].0 += length;
                        sums[li][k].1 += 1;
                    }
                }
            }
            if let (Some(sa), Some(sb)) = (local.get(&a), local.get(&b)) {
                for &(la, ka) in sa {
                    for &(lb, kb) in sb {
                        if la == lb {
                            existing[la].insert((ka.min(kb), ka.max(kb)));
                        }
                    }
                }
            }
        }
    }
    let sigma = sums
        .into_iter()
        .map(|v| {
            v.into_iter()
                .map(|(s, n)| if n > 0 { s / n as f64 } else { 0.0 })
                .collect()
        })
        .collect();
    Ok((sigma, existing))
}

/// Per loop, the existing faces within `rings` rings of its rim (those
/// touching a rim vertex, then those touching a vertex of those, ...), in
/// face order: what fairing reads around the patch.
fn ring_faces(
    mesh: &Mesh,
    loops: &[&BoundaryLoop],
    on: &[bool],
    rings: usize,
    ctx: &RunContext,
) -> Result<Vec<Vec<[u32; 3]>>, Cancelled> {
    let mut marked = on.to_vec();
    let mut members: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    let mut seen: Vec<BTreeSet<u32>> = Vec::with_capacity(loops.len());
    for (li, l) in loops.iter().enumerate() {
        for &v in &l.vertices {
            members.entry(v).or_default().push(li);
        }
        seen.push(l.vertices.iter().copied().collect());
    }
    let mut faces: Vec<BTreeMap<u32, [u32; 3]>> = vec![BTreeMap::new(); loops.len()];
    let mut tick = ctx.ticker();
    for _ in 0..rings {
        let mut grown: Vec<(u32, usize)> = Vec::new();
        for (id, t) in mesh.faces() {
            tick.tick()?;
            if !t.iter().any(|&v| marked[v as usize]) {
                continue;
            }
            let mut owners: Vec<usize> = t
                .iter()
                .filter_map(|v| members.get(v))
                .flatten()
                .copied()
                .collect();
            owners.sort_unstable();
            owners.dedup();
            for li in owners {
                if faces[li].insert(id.0, t).is_none() {
                    grown.extend(t.iter().map(|&v| (v, li)));
                }
            }
        }
        for (v, li) in grown {
            if seen[li].insert(v) {
                members.entry(v).or_default().push(li);
                marked[v as usize] = true;
            }
        }
    }
    Ok(faces
        .into_iter()
        .map(|f| f.into_values().collect())
        .collect())
}

/// Edges of `patch` between two of its loop's vertices (local indices,
/// `i < j`): the loop's own edges and the chords across it.
fn rim_edges(patch: &refine::Patch) -> impl Iterator<Item = (usize, usize)> + '_ {
    patch.tris.iter().flat_map(move |t| {
        (0..3).filter_map(move |k| {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            (a < patch.n_boundary && b < patch.n_boundary).then_some((a.min(b), a.max(b)))
        })
    })
}

/// Local edge (i, j) of loop `l` as a mesh edge (low vertex first).
fn mesh_edge(l: &BoundaryLoop, i: usize, j: usize) -> (u32, u32) {
    let (a, b) = (l.vertices[i], l.vertices[j]);
    (a.min(b), a.max(b))
}

impl Operation for FillHoles {
    fn id(&self) -> &'static str {
        "mesh.fill_holes"
    }

    fn version(&self) -> u32 {
        2
    }

    fn upgrade_params(&self, recorded: u32, params: &mut Params) -> bool {
        match recorded {
            // Version 1 had no fill styles: it triangulated and refined.
            1 => {
                params
                    .0
                    .insert("fill_style".to_owned(), ParamValue::Text("flat".into()));
                true
            }
            v => v == self.version(),
        }
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        params: &Params,
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        let max_edges = params.get_int("max_loop_edges")?.max(3) as usize;
        let refine = params.get_bool("refine")?;
        let word = params.get_choice("fill_style")?;
        let style = Style::parse(word).ok_or_else(|| {
            failed(
                "invalid_params",
                format!("fill_style '{word}' is not flat, smooth or curvature"),
            )
        })?;
        // Fairing moves the vertices refinement adds, so a non-flat style
        // always refines (asking for smooth without refinement would
        // otherwise be a flat patch in disguise).
        let fair = style != Style::Flat;
        let refine = refine || fair;
        let last_stage = if fair {
            "fairing"
        } else if refine {
            "refining"
        } else {
            "triangulating"
        };
        let topo = input.topology_with(ctx)?;
        let mut fill: Vec<&BoundaryLoop> = Vec::new();
        let mut skipped = Vec::new();
        match target {
            Target::WholeObject => {
                let mut tick = ctx.ticker();
                for l in &topo.boundary_loops {
                    tick.tick()?;
                    if l.edge_count > max_edges {
                        skipped.push(
                            json!({"id": l.id, "edges": l.edge_count, "reason": "too_large"}),
                        );
                    } else if !unique_vertices(l) {
                        skipped.push(
                            json!({"id": l.id, "edges": l.edge_count, "reason": "self_touching"}),
                        );
                    } else {
                        fill.push(l);
                    }
                }
            }
            Target::Loops(ids) => {
                // Validate every target before filling any (all-or-nothing).
                for &id in ids {
                    let l = topo
                        .boundary_loops
                        .iter()
                        .find(|l| l.id == id)
                        .ok_or_else(|| {
                            failed(
                                "invalid_target",
                                format!("loop {id} is not a traced boundary loop"),
                            )
                        })?;
                    if l.edge_count > max_edges {
                        return Err(failed(
                            "loop_too_large",
                            format!(
                                "loop {id} has {} edges, more than max_loop_edges {max_edges}",
                                l.edge_count
                            ),
                        ));
                    }
                    if !unique_vertices(l) {
                        return Err(failed(
                            "loop_self_touching",
                            format!(
                                "loop {id} passes through the same vertex twice, which fill_holes can't close as one patch"
                            ),
                        ));
                    }
                    if !fill.iter().any(|f| f.id == id) {
                        fill.push(l);
                    }
                }
            }
            other => return Err(unsupported_target(self.id(), other)),
        }

        let on = on_loops(input, &fill);
        let normals = outer_normals(input, &fill, &on, ctx)?;
        let (sigma, existing) = loop_context(input, &fill, &on, ctx)?;
        let rings = if fair {
            ring_faces(input, &fill, &on, style.rings(), ctx)?
        } else {
            Vec::new()
        };
        drop(on);
        // The same threshold `inspect` reports as degenerate.
        let min_area = input
            .bounding_box_with(ctx)?
            .map_or(0.0, |b| 1e-12 * b.diagonal() * b.diagonal());
        let points: Vec<Vec<[f64; 3]>> = fill
            .iter()
            .map(|l| {
                l.vertices
                    .iter()
                    .map(|&v| input.corner_position(v))
                    .collect()
            })
            .collect();

        // Loops are independent: triangulate them all in parallel, then
        // refine them all, each stage reporting loops done. The results
        // are applied below in loop order, so the output doesn't depend on
        // how the work was split (MF-CMD-005).
        let total = fill.len();
        let done = AtomicUsize::new(0);
        let loop_done = |stage: &'static str| {
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            ctx.progress.report(stage, n as f64 / total.max(1) as f64);
        };
        ctx.progress.report("triangulating", 0.0);
        // Each table is O(n²) (12 MB at 500 edges): run consecutive loops in
        // parallel only while their tables fit the budget together, so peak
        // memory doesn't grow with the thread count.
        let mut triangulations: Vec<Option<Vec<[usize; 3]>>> = Vec::with_capacity(total);
        let mut start = 0;
        while start < total {
            let mut end = start + 1;
            let mut bytes = triangulate::table_bytes(points[start].len());
            while end < total {
                let next = triangulate::table_bytes(points[end].len());
                if bytes + next > TRIANGULATION_BUDGET_BYTES {
                    break;
                }
                bytes += next;
                end += 1;
            }
            let batch: Vec<Option<Vec<[usize; 3]>>> = (start..end)
                .into_par_iter()
                .map(|li| {
                    let tris = triangulate::triangulate(
                        &points[li],
                        &normals[li],
                        &existing[li],
                        min_area,
                        ctx,
                    )?;
                    loop_done("triangulating");
                    Ok(tris)
                })
                .collect::<Result<_, Cancelled>>()?;
            triangulations.extend(batch);
            start = end;
        }
        ctx.progress.report("triangulating", 1.0);

        if refine {
            done.store(0, Ordering::Relaxed);
            ctx.progress.report("refining", 0.0);
        }
        let mut patches: Vec<Option<refine::Patch>> = triangulations
            .into_par_iter()
            .zip(points)
            .enumerate()
            .map(|(li, (tris, positions))| {
                let Some(tris) = tris else {
                    if refine {
                        loop_done("refining");
                    }
                    return Ok(None);
                };
                let mut patch = refine::Patch {
                    n_boundary: positions.len(),
                    sigma: Vec::new(),
                    tris,
                    positions,
                };
                if refine {
                    patch.sigma = sigma[li].clone();
                    patch.refine(&existing[li], ctx)?;
                    loop_done("refining");
                }
                Ok(Some(patch))
            })
            .collect::<Result<_, Cancelled>>()?;
        ctx.checkpoint()?;

        // Fairing: each patch solved on its own, single-threaded, so the
        // result doesn't depend on how loops were spread over threads.
        let mut fairing: Vec<Option<Result<fairing::Faired, fairing::Failed>>> =
            vec![None; patches.len()];
        if fair {
            done.store(0, Ordering::Relaxed);
            ctx.progress.report("fairing", 0.0);
            fairing = patches
                .par_iter_mut()
                .enumerate()
                .map(|(li, patch)| {
                    let solved = match patch {
                        Some(p) => Some(fairing::fair(
                            p,
                            &fill[li].vertices,
                            &rings[li],
                            input,
                            style,
                            ctx,
                        )?),
                        None => None,
                    };
                    loop_done("fairing");
                    Ok(solved)
                })
                .collect::<Result<_, Cancelled>>()?;
            ctx.checkpoint()?;
        }

        // MF-ISSUE-038: loops were patched independently, so two loops that
        // share two non-adjacent vertices could both have used the chord
        // between them (giving that edge four faces). Going in loop order,
        // a patch that uses a chord an earlier patch already used is built
        // again with those chords excluded, exactly as an existing mesh
        // edge is. Rare, so this runs one loop at a time; the result still
        // depends only on loop order.
        // Mesh edge → the loop whose patch used it.
        let mut used_chords: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        let mut edit = input.edit();
        let mut filled = Vec::new();
        let mut beyond_curvature_limit = 0;
        for (li, ((l, mut patch), mut faired)) in fill.iter().zip(patches).zip(fairing).enumerate()
        {
            // The first chord an earlier patch took from this loop, and
            // that patch's loop: named if the rebuilt loop can't close.
            let mut taken_by: Option<((u32, u32), u32)> = None;
            if let Some(p) = &patch {
                let taken: BTreeSet<(usize, usize)> = rim_edges(p)
                    .filter(|&(i, j)| used_chords.contains_key(&mesh_edge(l, i, j)))
                    .collect();
                taken_by = taken
                    .iter()
                    .map(|&(i, j)| mesh_edge(l, i, j))
                    .min()
                    .map(|e| (e, used_chords[&e]));
                if !taken.is_empty() {
                    let mut excluded = existing[li].clone();
                    excluded.extend(taken);
                    let positions: Vec<[f64; 3]> = l
                        .vertices
                        .iter()
                        .map(|&v| input.corner_position(v))
                        .collect();
                    patch = triangulate::triangulate(
                        &positions,
                        &normals[li],
                        &excluded,
                        min_area,
                        ctx,
                    )?
                    .map(|tris| refine::Patch {
                        n_boundary: positions.len(),
                        sigma: Vec::new(),
                        tris,
                        positions,
                    });
                    faired = None;
                    if let Some(p) = &mut patch {
                        if refine {
                            p.sigma = sigma[li].clone();
                            p.refine(&excluded, ctx)?;
                        }
                        if fair {
                            faired = Some(fairing::fair(
                                p,
                                &l.vertices,
                                &rings[li],
                                input,
                                style,
                                ctx,
                            )?);
                        }
                    }
                }
            }
            // A patch whose fairing didn't converge is never kept flat: an
            // error when selected, skipped and reported in a whole-object
            // run.
            if let Some(Err(e)) = &faired {
                if matches!(target, Target::Loops(_)) {
                    return Err(failed(
                        "fairing_did_not_converge",
                        format!(
                            "loop {} could not be shaped to the surface around it ({word} fill: {e}); try another fill style",
                            l.id
                        ),
                    ));
                }
                skipped.push(
                    json!({"id": l.id, "edges": l.edge_count, "reason": "fairing_did_not_converge"}),
                );
                continue;
            }
            // A loop with no valid patch: an error when it was selected,
            // skipped and reported in a whole-object run.
            let patch = match patch {
                Some(p) if p.min_area() < min_area => None,
                other => other,
            };
            let Some(patch) = patch else {
                // MF-ISSUE-038: not a slit; a neighbouring hole's fill took
                // the only way across.
                if let Some(((a, b), by)) = taken_by {
                    let message = format!(
                        "Hole {id} can't be closed: the only way across it uses the edge between vertices {a} and {b}, which hole {by}'s fill already uses. Fill hole {id} first, on its own, or leave it open.",
                        id = l.id
                    );
                    if matches!(target, Target::Loops(_)) {
                        return Err(failed("loop_chord_used_by_another_hole", message));
                    }
                    skipped.push(json!({
                        "id": l.id,
                        "edges": l.edge_count,
                        "reason": "chord_used_by_another_hole",
                        "by": by,
                        "edge": [a, b],
                        "message": message,
                    }));
                    continue;
                }
                if matches!(target, Target::Loops(_)) {
                    return Err(failed(
                        "loop_untriangulable",
                        format!(
                            "loop {} cannot be closed without a zero-area triangle or a face on an existing edge (it is flat, or folded onto the mesh)",
                            l.id
                        ),
                    ));
                }
                skipped
                    .push(json!({"id": l.id, "edges": l.edge_count, "reason": "untriangulable"}));
                continue;
            };
            // Local → mesh vertex IDs: boundary first, then new vertices.
            let mut ids: Vec<u32> = l.vertices.clone();
            for &p in &patch.positions[patch.n_boundary..] {
                ids.push(edit.add_vertex(p).0);
            }
            for t in &patch.tris {
                edit.add_face(
                    t.map(|k| ids[k]),
                    Sources {
                        faces: vec![],
                        loops: vec![l.id],
                        operands: vec![],
                    },
                )
                .map_err(|e| failed("invalid_edit", e.to_string()))?;
            }
            for (i, j) in rim_edges(&patch) {
                used_chords.entry(mesh_edge(l, i, j)).or_insert(l.id);
            }
            let mut entry = json!({"id": l.id, "edges": l.edge_count, "faces": patch.tris.len()});
            if let Some(Ok(f)) = faired {
                entry["faired_vertices"] = json!(f.vertices);
            }
            if style == Style::Curvature && l.edge_count > CURVATURE_ACCURATE_EDGES {
                entry["curvature_less_accurate"] = json!(true);
                beyond_curvature_limit += 1;
            }
            filled.push(entry);
        }
        let warnings: Vec<String> = if beyond_curvature_limit > 0 {
            vec![curvature_limit_warning(beyond_curvature_limit)]
        } else {
            Vec::new()
        };
        let report = json!({
            "filled_loops": filled,
            "skipped_loops": skipped,
            "refined": refine,
        });
        if filled.is_empty() {
            ctx.progress.report(last_stage, 1.0);
            return Ok(OpOutput {
                change: None,
                report,
            });
        }
        let (mesh, prov) = edit
            .finish()
            .map_err(|e| failed("invalid_edit", e.to_string()))?;
        let report = json!({
            "filled_loops": report["filled_loops"],
            "skipped_loops": report["skipped_loops"],
            "refined": refine,
            "created_faces": prov.created_faces.len(),
            "created_vertices": prov.created_vertices.len(),
        });
        let mut report = report;
        if !warnings.is_empty() {
            report["warnings"] = json!(warnings);
        }
        ctx.progress.report(last_stage, 1.0);
        Ok(OpOutput {
            change: Some((mesh, prov)),
            report,
        })
    }
}
