//! `mesh.fill_holes` (MF-OP-001/002/003).
//!
//! Each target loop is closed by a minimum-weight triangulation of its
//! boundary polygon. Everything goes through `MeshEdit`, so kept geometry is
//! untouched and provenance names the loop of every new face.

mod refine;
mod triangulate;

use std::collections::{BTreeMap, BTreeSet};

use mf_geometry::{BoundaryLoop, Mesh, Sources};
use mf_spec::Params;
use serde_json::json;

use crate::{OpError, OpOutput, Operation, Target, unsupported_target};

pub struct FillHoles;

fn failed(code: &'static str, message: String) -> OpError {
    OpError::Failed { code, message }
}

fn unique_vertices(l: &BoundaryLoop) -> bool {
    let set: BTreeSet<u32> = l.vertices.iter().copied().collect();
    set.len() == l.vertices.len()
}

/// Unit normal of the existing face across each directed loop edge.
fn outer_normals(mesh: &Mesh, loops: &[&BoundaryLoop]) -> Vec<Vec<[f64; 3]>> {
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
    for (_, t) in mesh.faces() {
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
    out
}

/// Per loop: the target edge length at each loop vertex (mean length of its
/// existing edges), and the existing edges between loop vertices in local
/// indices (so neither triangulation nor refinement duplicates one).
type LoopContext = (Vec<Vec<f64>>, Vec<BTreeSet<(usize, usize)>>);

fn loop_context(mesh: &Mesh, loops: &[&BoundaryLoop]) -> LoopContext {
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
    for (_, t) in mesh.faces() {
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
    (sigma, existing)
}

impl Operation for FillHoles {
    fn id(&self) -> &'static str {
        "mesh.fill_holes"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(&self, input: &Mesh, target: &Target, params: &Params) -> Result<OpOutput, OpError> {
        let max_edges = params.get_int("max_loop_edges")?.max(3) as usize;
        let refine = params.get_bool("refine")?;
        let topo = input.topology();
        let mut fill: Vec<&BoundaryLoop> = Vec::new();
        let mut skipped = Vec::new();
        match target {
            Target::WholeObject => {
                for l in &topo.boundary_loops {
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
                                "loop {id} passes through the same vertex twice; fill its parts separately"
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

        let normals = outer_normals(input, &fill);
        let (sigma, existing) = loop_context(input, &fill);
        let mut edit = input.edit();
        let mut filled = Vec::new();
        for (li, (l, outer)) in fill.iter().zip(&normals).enumerate() {
            let points: Vec<[f64; 3]> = l
                .vertices
                .iter()
                .map(|&v| input.corner_position(v))
                .collect();
            let Some(tris) = triangulate::triangulate(&points, outer, &existing[li]) else {
                if matches!(target, Target::Loops(_)) {
                    return Err(failed(
                        "loop_untriangulable",
                        format!(
                            "loop {} cannot be closed without adding a face to an existing edge (the result would be non-manifold)",
                            l.id
                        ),
                    ));
                }
                skipped
                    .push(json!({"id": l.id, "edges": l.edge_count, "reason": "untriangulable"}));
                continue;
            };
            let mut patch = refine::Patch {
                n_boundary: points.len(),
                sigma: Vec::new(),
                tris,
                positions: points,
            };
            if refine {
                patch.sigma = sigma[li].clone();
                patch.refine(&existing[li]);
            }
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
                    },
                )
                .map_err(|e| failed("invalid_edit", e.to_string()))?;
            }
            filled.push(json!({"id": l.id, "edges": l.edge_count, "faces": patch.tris.len()}));
        }
        let report = json!({
            "filled_loops": filled,
            "skipped_loops": skipped,
        });
        if filled.is_empty() {
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
            "created_faces": prov.created_faces.len(),
            "created_vertices": prov.created_vertices.len(),
        });
        Ok(OpOutput {
            change: Some((mesh, prov)),
            report,
        })
    }
}
