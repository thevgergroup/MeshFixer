//! Test-only (`test-kinds` feature): `test.consume_tool`, a stand-in for a
//! boolean in multi-object command tests (Phase 1 Task 3, Review Focus 5).
//! It removes its `tool` operand (operand 1, role `consumed`) and creates
//! one new object holding the target's faces followed by the tool's, the
//! tool's brought into the target's frame (D10's frame rule, without the
//! boolean). Every created face names its source in
//! [`Sources::operands`], so provenance is total over both operands
//! (MF-P1-SEL-001). The target itself is unchanged.

use mf_geometry::{Mesh, Provenance, Sources};
use mf_spec::Params;

use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

pub struct TestConsumeTool;

/// `a * b` for 3MF row-vector affines (`p * a * b`: `a` first).
fn compose(a: &[f64; 12], b: &[f64; 12]) -> [f64; 12] {
    let mut out = [0.0; 12];
    for r in 0..4 {
        for c in 0..3 {
            let row = |k: usize| if r < 3 { a[3 * r + k] } else { a[9 + k] };
            out[3 * r + c] = row(0) * b[c]
                + row(1) * b[3 + c]
                + row(2) * b[6 + c]
                + if r == 3 { b[9 + c] } else { 0.0 };
        }
    }
    out
}

/// Inverse of a 3MF row-vector affine, `None` if singular.
fn inverse(t: &[f64; 12]) -> Option<[f64; 12]> {
    let m = [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let mut inv = [[0.0; 3]; 3];
    for (r, row) in inv.iter_mut().enumerate() {
        for (c, x) in row.iter_mut().enumerate() {
            *x = cof(c, r) / det;
        }
    }
    let tr = [t[9], t[10], t[11]];
    let it: Vec<f64> = (0..3)
        .map(|c| -(tr[0] * inv[0][c] + tr[1] * inv[1][c] + tr[2] * inv[2][c]))
        .collect();
    Some([
        inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1],
        inv[2][2], it[0], it[1], it[2],
    ])
}

fn apply(t: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    [
        t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9],
        t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
        t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11],
    ]
}

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
