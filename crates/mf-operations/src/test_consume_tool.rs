//! Test-only (`test-kinds` feature): `test.consume_tool`, a stand-in for a
//! boolean in multi-object command tests (Phase 1 Task 3, Review Focus 5).
//! It removes its `tool` operand (operand 1, role `consumed`) and creates
//! one new object holding the target's faces followed by the tool's, the
//! tool's brought into the target's frame (D10's frame rule, without the
//! boolean). Every created face names its source in
//! [`Sources::operands`], so provenance is total over both operands
//! (MF-P1-SEL-001). The target itself is unchanged.

use mf_geometry::affine::{apply, compose, inverse};
use mf_geometry::{Mesh, Provenance, Sources};
use mf_spec::Params;

use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

pub struct TestConsumeTool;

impl Operation for TestConsumeTool {
    fn id(&self) -> &'static str {
        "test.consume_tool"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let bad_source = params.get_bool("bad_source")?;
        let [target, tool] = inputs else {
            return Err(OpError::Failed {
                code: "missing_operand",
                message: format!("{} needs a target and a tool", self.id()),
            });
        };
        for o in [target, tool] {
            if o.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &o.target));
            }
        }
        let into_target = inverse(&target.transform)
            .map(|inv| compose(&tool.transform, &inv))
            .ok_or_else(|| OpError::Failed {
                code: "singular_transform",
                message: "the target's transform can't be inverted".into(),
            })?;
        let mirrored = {
            let t = &into_target;
            t[0] * (t[4] * t[8] - t[5] * t[7]) - t[1] * (t[3] * t[8] - t[5] * t[6])
                + t[2] * (t[3] * t[7] - t[4] * t[6])
                < 0.0
        };
        let mut edit = Mesh::empty().edit();
        let mut faces = 0;
        for (k, (input, frame)) in [(target, None), (tool, Some(&into_target))]
            .into_iter()
            .enumerate()
        {
            ctx.checkpoint()?;
            let mut slot = vec![u32::MAX; input.mesh.vertex_slots() as usize];
            for (id, p) in input.mesh.vertices() {
                let p = frame.map_or(p, |t| apply(t, p));
                slot[id.0 as usize] = edit.add_vertex(p).0;
            }
            for (f, tri) in input.mesh.faces() {
                let mut tri = tri.map(|v| slot[v as usize]);
                if frame.is_some() && mirrored {
                    tri.swap(1, 2);
                }
                edit.add_face(
                    tri,
                    Sources {
                        operands: if bad_source {
                            vec![(k as u32, f), (7, f)]
                        } else {
                            vec![(k as u32, f)]
                        },
                        ..Sources::default()
                    },
                )
                .map_err(|e| OpError::Failed {
                    code: "invalid_face",
                    message: e.to_string(),
                })?;
                faces += 1;
            }
        }
        let (mesh, provenance): (Mesh, Provenance) =
            edit.finish().map_err(|e| OpError::Failed {
                code: "invalid_mesh",
                message: e.to_string(),
            })?;
        debug_assert_eq!(mesh.face_count(), faces);
        Ok(DocOutput {
            changes: vec![
                ObjectChange::Remove { operand: 1 },
                ObjectChange::Create {
                    name: "combined".into(),
                    mesh,
                    transform: target.transform,
                    provenance,
                },
            ],
            report: serde_json::json!({ "faces": faces }),
        })
    }
}
