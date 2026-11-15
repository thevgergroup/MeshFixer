//! `mesh.compact`: reclaim tombstoned slots (ADR-0001, MF-ISSUE-020).
//!
//! The one operation that renumbers kept elements. Its provenance is only a
//! slot remap, so selections translate through it like through any edit.

use mf_geometry::{Mesh, Provenance};
use mf_spec::Params;
use serde_json::json;

use crate::{DocContext, OpError, OpOutput, Operation, RunContext, Target, unsupported_target};

pub struct Compact;

impl Operation for Compact {
    fn id(&self) -> &'static str {
        "mesh.compact"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        _params: &Params,
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        const STAGE: &str = "compacting";
        if *target != Target::WholeObject {
            return Err(unsupported_target(self.id(), target));
        }
        let reclaimed_faces = input.face_slots() as usize - input.face_count();
        let reclaimed_vertices = input.vertex_slots() as usize - input.vertex_count();
        let report = json!({
            "reclaimed_faces": reclaimed_faces,
            "reclaimed_vertices": reclaimed_vertices,
        });
        if input.is_compact() {
            ctx.progress.report(STAGE, 1.0);
            return Ok(OpOutput {
                change: None,
                report,
            });
        }
        ctx.progress.report(STAGE, 0.0);
        let (mesh, remap) = input.compacted_with(ctx)?;
        ctx.progress.report(STAGE, 1.0);
        let provenance = Provenance {
            remap: Some(remap),
            ..Provenance::default()
        };
        Ok(OpOutput {
            change: Some((mesh, provenance)),
            report,
        })
    }
}
