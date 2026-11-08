use mf_geometry::{FaceId, Mesh, VertexId};
use mf_spec::Params;
use serde_json::json;

use crate::{OpError, OpOutput, Operation, Target, unsupported_target};

pub struct RemoveSmallComponents;

impl Operation for RemoveSmallComponents {
    fn id(&self) -> &'static str {
        "mesh.remove_small_components"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(&self, input: &Mesh, target: &Target, params: &Params) -> Result<OpOutput, OpError> {
        if *target != Target::WholeObject {
            return Err(unsupported_target(self.id(), target));
        }
        let max_triangles = params.get_int("max_triangles")?.max(0) as usize;
        let keep_largest = params.get_bool("keep_largest")?;
        let topo = input.topology();

        let mut keep_face = vec![false; input.face_slots() as usize];
        let mut removed_components = 0;
        let mut removed_triangles = 0;
        // Components are ordered largest first, so index 0 is the largest.
        for (i, comp) in topo.components.iter().enumerate() {
            if comp.len() >= max_triangles || (keep_largest && i == 0) {
                for &f in comp {
                    keep_face[f as usize] = true;
                }
            } else {
                removed_components += 1;
                removed_triangles += comp.len();
            }
        }

        let report = json!({
            "removed_components": removed_components,
            "removed_triangles": removed_triangles,
        });
        if removed_components == 0 {
            return Ok(OpOutput {
                change: None,
                report,
            });
        }

        // Tombstone removed faces, and vertices used only by removed faces.
        // Kept faces and vertices keep their IDs (ADR-0001, MF-OP-003).
        let mut used_by_kept = vec![false; input.vertex_slots() as usize];
        let mut used_by_removed = vec![false; input.vertex_slots() as usize];
        for (FaceId(f), t) in input.faces() {
            let used = if keep_face[f as usize] {
                &mut used_by_kept
            } else {
                &mut used_by_removed
            };
            for v in t {
                used[v as usize] = true;
            }
        }
        let mut edit = input.edit();
        let fail = |e: mf_geometry::GeometryError| OpError::Failed {
            code: "invalid_edit",
            message: e.to_string(),
        };
        for (FaceId(f), _) in input.faces() {
            if !keep_face[f as usize] {
                edit.remove_face(FaceId(f)).map_err(fail)?;
            }
        }
        for v in 0..input.vertex_slots() {
            if used_by_removed[v as usize] && !used_by_kept[v as usize] {
                edit.remove_vertex(VertexId(v)).map_err(fail)?;
            }
        }
        let change = edit.finish().map_err(fail)?;
        Ok(OpOutput {
            change: Some(change),
            report,
        })
    }
}
