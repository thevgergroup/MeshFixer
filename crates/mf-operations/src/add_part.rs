//! `object.add_part` (MF-MODEL-004, Task 21): a library part as a new
//! object, placed by an absolute transform (D7), next to the object it's
//! placed on (the target, unchanged).
//!
//! The part is named by id and recorded with its content hash, like an
//! import's source: running the command (and replaying it) looks the part
//! up in the library, built-ins first, then the user's parts folder, and
//! refuses by name if it's missing or its hash differs.

use mf_geometry::{Mesh, Provenance, Sources};
use mf_parts::Library;
use mf_spec::Params;

use crate::affine::determinant;
use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, scale_mesh,
};

pub struct AddPart;

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// The library a command reads: built-ins and the user's parts folder.
fn library(ctx: &RunContext) -> Library {
    // allow-swallow: without a settings folder there are no user parts; built-ins still load
    let dir = mf_parts::user_parts_dir().ok();
    // Warnings name broken user parts; the command reports a missing part
    // by name if it was one of them.
    Library::load(dir.as_deref(), ctx).0
}

impl Operation for AddPart {
    fn id(&self) -> &'static str {
        "object.add_part"
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
        _inputs: &[OperandInput],
        params: &Params,
        doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let id = params.get_text("part_id")?;
        let want = params.get_text("part_hash")?;
        let transform = params.get_affine("transform")?;
        let det = determinant(&transform);
        if det == 0.0 || !det.is_finite() {
            return Err(failed(
                "singular_transform",
                "The part's transform flattens it; give it a non-zero scale.",
            ));
        }
        let lib = library(ctx);
        let part = lib.get(id).ok_or_else(|| {
            failed(
                "part_missing",
                format!("The part '{id}' isn't in the library. Add it to the parts folder again."),
            )
        })?;
        let have = part
            .content_hash()
            .map_err(|e| failed("part_unreadable", e.to_string()))?;
        if !want.is_empty() && have != want {
            return Err(failed(
                "part_changed",
                format!(
                    "The part '{id}' in the library isn't the one recorded (its content changed). \
                     Restore the original part, or add it again."
                ),
            ));
        }
        let mesh = part.mesh(ctx).map_err(|e| match e {
            mf_parts::PartError::Cancelled => OpError::Cancelled,
            e => failed("part_unreadable", e.to_string()),
        })?;
        // `LibraryPart::mesh` always returns millimetres (D16): scale into
        // the placing document's own unit (consolidation plan C10), so a
        // 10 mm part in a metres document lands at 0.01 doc units, not 10.
        let mesh = scale_mesh(mesh, doc.mm_to_doc())
            .map_err(|e| failed("part_unreadable", format!("can't scale the part: {e}")))?;
        let provenance = Provenance {
            created_faces: mesh.faces().map(|(f, _)| (f, Sources::default())).collect(),
            created_vertices: mesh.vertices().map(|(v, _)| v).collect(),
            ..Provenance::default()
        };
        Ok(DocOutput {
            report: serde_json::json!({
                "part": id,
                "part_hash": have,
                "faces": mesh.face_count(),
            }),
            changes: vec![ObjectChange::Create {
                name: part.manifest.name.clone(),
                mesh,
                transform,
                provenance,
            }],
        })
    }
}
