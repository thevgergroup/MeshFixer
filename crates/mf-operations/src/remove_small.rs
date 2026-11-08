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
        let max_triangles = params.get_int("max_triangles")?.max(0) as usize;
        let keep_largest = params.get_bool("keep_largest")?;
        let topo = input.topology();

        // Which components are candidates: all of them, or the selected ones.
        // A selected component is removed only if it is under the threshold;
        // otherwise that's an error naming it, never a silent no-op.
        // `keep_largest` applies to whole-object runs only: an explicit
        // selection already says which component to remove.
        let candidates: Vec<usize> = match target {
            Target::WholeObject => (0..topo.components.len())
                .filter(|&i| !(keep_largest && i == 0))
                .filter(|&i| topo.components[i].len() < max_triangles)
                .collect(),
            Target::Components(ids) => {
                let mut picked = Vec::new();
                for &c in ids {
                    let comp = topo
                        .components
                        .get(c as usize)
                        .ok_or_else(|| OpError::Failed {
                            code: "invalid_target",
                            message: format!("component {c} does not exist"),
                        })?;
                    if comp.len() >= max_triangles {
                        return Err(OpError::Failed {
                            code: "component_too_large",
                            message: format!(
                                "component {c} has {} triangles, not fewer than max_triangles {max_triangles}",
                                comp.len()
                            ),
                        });
                    }
                    picked.push(c as usize);
                }
                picked.sort_unstable();
                picked.dedup();
                picked
            }
            other => return Err(unsupported_target(self.id(), other)),
        };

        let mut keep_face = vec![true; input.face_slots() as usize];
        let mut removed_components = 0;
        let mut removed_triangles = 0;
        for &i in &candidates {
            for &f in &topo.components[i] {
                keep_face[f as usize] = false;
            }
            removed_components += 1;
            removed_triangles += topo.components[i].len();
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
