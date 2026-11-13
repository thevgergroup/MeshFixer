//! `mesh.split_region` (MF-MODEL-003, D11, Phase 1 Task 14): Meshmixer's
//! "Separate" plus closure. The selected faces become a new object; the
//! rest stays in the target. Each part's new opening (where the two were
//! joined) is closed with `mesh.fill_holes`' patch code (`cap_style`: flat
//! or smooth), within its 1000-edge algorithm limit (MF-ISSUE-043).
//! Openings the object already had are left as they were.
//!
//! And `mesh.delete_faces` (Meshmixer's "Discard"): removes a face set and
//! any vertices left unused; the opening it leaves is an ordinary hole.

use std::collections::{BTreeMap, HashSet};

use mf_geometry::{FaceId, Mesh, Provenance, VertexId};
use mf_spec::Params;

use super::pegs::compose;
use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document,
};

/// The fill limit (the current O(n³) patch triangulation, MF-ISSUE-043).
pub const MAX_CAP_EDGES: usize = 1000;

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

fn geometry(e: mf_geometry::GeometryError) -> OpError {
    failed("invalid_mesh", e.to_string())
}

/// `mesh` without `faces`, and vertices only they used.
fn without(mesh: &Mesh, faces: &HashSet<FaceId>) -> Result<(Mesh, Provenance), OpError> {
    let mut edit = mesh.edit();
    let mut used = vec![false; mesh.vertex_slots() as usize];
    for (f, t) in mesh.faces() {
        if faces.contains(&f) {
            edit.remove_face(f).map_err(geometry)?;
        } else {
            for v in t {
                used[v as usize] = true;
            }
        }
    }
    for (v, _) in mesh.vertices() {
        if !used[v.0 as usize] {
            edit.remove_vertex(v).map_err(geometry)?;
        }
    }
    edit.finish().map_err(geometry)
}

fn selected(target: &Target, mesh: &Mesh, op: &str) -> Result<HashSet<FaceId>, OpError> {
    let Target::Faces(faces) = target else {
        return Err(failed(
            "needs_face_selection",
            format!("{op} acts on selected faces; select the faces first"),
        ));
    };
    let set: HashSet<FaceId> = faces
        .iter()
        .copied()
        .filter(|&f| mesh.is_face_live(f))
        .collect();
    if set.is_empty() {
        return Err(failed("empty_selection", "No faces are selected."));
    }
    Ok(set)
}

/// Closes the openings of `mesh` that pass through `seam` vertices.
fn cap(
    mesh: &Mesh,
    seam: &HashSet<u32>,
    style: &str,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance), OpError> {
    let topo = mesh.topology_with(ctx)?;
    let mut ids = Vec::new();
    for l in &topo.boundary_loops {
        if !l.vertices.iter().any(|v| seam.contains(v)) {
            continue;
        }
        if l.edge_count > MAX_CAP_EDGES {
            return Err(failed(
                "region_boundary_too_long",
                format!(
                    "The selection's outline has {} edges; this fill method closes at most {MAX_CAP_EDGES} \
                     (an algorithm limit, MF-ISSUE-043). Select a smaller region, or split with a plane.",
                    l.edge_count
                ),
            ));
        }
        ids.push(l.id);
    }
    if ids.is_empty() {
        return Ok((mesh.clone(), Provenance::default()));
    }
    let spec = mf_spec::builtin::get("mesh.fill_holes")
        .ok_or_else(|| failed("internal", "fill_holes isn't built in"))?;
    let raw = BTreeMap::from([
        ("max_loop_edges".to_string(), MAX_CAP_EDGES.to_string()),
        ("fill_style".to_string(), style.to_string()),
    ]);
    let params = spec.validate(&raw)?;
    let out = crate::fill_holes::FillHoles.execute(mesh, &Target::Loops(ids), &params, ctx)?;
    if let Some(skipped) = out.report["skipped_loops"]
        .as_array()
        .filter(|s| !s.is_empty())
    {
        return Err(failed(
            "region_boundary_unfillable",
            format!(
                "The selection's outline couldn't be closed ({skipped:?}). Select a slightly different region."
            ),
        ));
    }
    Ok(out
        .change
        .unwrap_or_else(|| (mesh.clone(), Provenance::default()))) // allow-swallow: no change means nothing needed closing
}

pub struct SplitRegion;

impl Operation for SplitRegion {
    fn id(&self) -> &'static str {
        "mesh.split_region"
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
        let target = inputs
            .first()
            .ok_or_else(|| failed("missing_target", "mesh.split_region was run with no target"))?;
        let style = params.get_choice("cap_style")?;
        let mesh = target.mesh;
        let sel = selected(&target.target, mesh, self.id())?;
        if sel.len() == mesh.face_count() {
            return Err(failed(
                "selection_is_everything",
                "Every face is selected, so there's nothing to split off. Select part of the object.",
            ));
        }
        ctx.progress.report("separating", 0.0);
        let rest: HashSet<FaceId> = mesh
            .faces()
            .map(|(f, _)| f)
            .filter(|f| !sel.contains(f))
            .collect();
        // Vertices on both sides: the seam.
        let mut in_sel = vec![false; mesh.vertex_slots() as usize];
        let mut in_rest = vec![false; mesh.vertex_slots() as usize];
        for (f, t) in mesh.faces() {
            let side = if sel.contains(&f) {
                &mut in_sel
            } else {
                &mut in_rest
            };
            for v in t {
                side[v as usize] = true;
            }
        }
        let seam: HashSet<u32> = (0..in_sel.len() as u32)
            .filter(|&v| in_sel[v as usize] && in_rest[v as usize])
            .collect();
        if seam.is_empty() {
            return Err(failed(
                "selection_not_attached",
                "The selected faces aren't joined to the rest, so they're already a separate piece.",
            ));
        }
        let (kept, kept_prov) = without(mesh, &sel)?;
        let (piece, piece_prov) = without(mesh, &rest)?;
        ctx.checkpoint()?;
        ctx.progress.report("closing", 0.0);
        let (kept2, kept_cap) = cap(&kept, &seam, style, ctx)?;
        let (piece2, piece_cap) = cap(&piece, &seam, style, ctx)?;
        let kept_prov = compose(&kept_prov, &kept_cap);
        let mut piece_prov = compose(&piece_prov, &piece_cap);
        // As a created object: every face is created, naming its source.
        let created: HashSet<FaceId> = piece_prov.created_faces.iter().map(|(f, _)| *f).collect();
        for (f, _) in piece2.faces() {
            if !created.contains(&f) {
                piece_prov.created_faces.push((
                    f,
                    mf_geometry::Sources {
                        operands: vec![(0, f)],
                        ..Default::default()
                    },
                ));
            }
        }
        for (_, s) in &mut piece_prov.created_faces {
            let faces = std::mem::take(&mut s.faces);
            s.operands.extend(faces.into_iter().map(|f| (0, f)));
        }
        piece_prov.created_faces.sort_by_key(|(f, _)| *f);
        piece_prov.removed_faces.clear();
        piece_prov.removed_vertices.clear();
        piece_prov.created_vertices = piece2.vertices().map(|(v, _)| v).collect::<Vec<VertexId>>();
        piece_prov.moved_vertices.clear();
        piece_prov.modified_faces.clear();
        let report = serde_json::json!({
            "kept_faces": kept2.face_count(),
            "piece_faces": piece2.face_count(),
            "seam_vertices": seam.len(),
            "cap_style": style,
        });
        Ok(DocOutput {
            changes: vec![
                ObjectChange::Geometry {
                    operand: 0,
                    mesh: kept2,
                    provenance: kept_prov,
                },
                ObjectChange::Create {
                    name: format!("{} (piece)", target.name),
                    mesh: piece2,
                    transform: target.transform,
                    provenance: piece_prov,
                },
            ],
            report,
        })
    }
}

pub struct DeleteFaces;

impl Operation for DeleteFaces {
    fn id(&self) -> &'static str {
        "mesh.delete_faces"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        _: &Params,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        let sel = selected(target, input, self.id())?;
        ctx.checkpoint()?;
        let (mesh, provenance) = without(input, &sel)?;
        Ok(OpOutput {
            report: serde_json::json!({
                "removed_faces": provenance.removed_faces.len(),
                "removed_vertices": provenance.removed_vertices.len(),
            }),
            change: Some((mesh, provenance)),
        })
    }
}
