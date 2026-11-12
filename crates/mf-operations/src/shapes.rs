//! Parametric shapes (parametric-shapes brief, owner-approved
//! 2026-11-11): `object.add_shape` places one or more shapes as new
//! objects that keep their recipe (`mf_parts::GeneratorRecord`, the
//! `generator` property), and `object.set_shape_params` regenerates
//! objects from new recipes, keeping their ids, names and transforms.
//!
//! Both are pure functions of their recorded parameters: the recipes are
//! the parameters, so replay builds the same meshes (Exact) without
//! reading any object's live state.

use mf_geometry::{Mesh, Provenance, Sources, VertexId};
use mf_parts::GeneratorRecord;
use mf_spec::Params;

use crate::affine::determinant;
use crate::{
    ChangeObject, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

fn generator_error(e: mf_parts::ConnectorError) -> OpError {
    match e {
        mf_parts::ConnectorError::Failed { code, message } => failed(code, message),
    }
}

/// One shape `object.add_shape` places.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShapeToAdd {
    pub generator: GeneratorRecord,
    pub name: String,
    /// Placed as a hole (cut on Combine) rather than a solid.
    #[serde(default)]
    pub hole: bool,
}

/// `object.add_shape`'s `shapes` parameter, as JSON.
pub fn shapes_param(shapes: &[ShapeToAdd]) -> String {
    serde_json::to_string(shapes).expect("shapes always serialize")
}

/// `object.set_shape_params`'s `generators` parameter, as JSON.
pub fn generators_param(records: &[GeneratorRecord]) -> String {
    serde_json::to_string(records).expect("generator records always serialize")
}

/// `mesh` as a replacement for all of `input`: every input face and
/// vertex removed, every face of `mesh` created (so a selection on the
/// old shape is reported lost, never silently mapped onto new faces).
fn replace(input: &Mesh, mesh: &Mesh) -> Result<(Mesh, Provenance), OpError> {
    let geometry = |e: mf_geometry::GeometryError| failed("invalid_mesh", e.to_string());
    let mut edit = input.edit();
    for (f, _) in input.faces() {
        edit.remove_face(f).map_err(geometry)?;
    }
    for (v, _) in input.vertices() {
        edit.remove_vertex(v).map_err(geometry)?;
    }
    let (positions, triangles) = mesh.to_arrays();
    let ids: Vec<VertexId> = positions.iter().map(|&p| edit.add_vertex(p)).collect();
    for t in triangles {
        edit.add_face(t.map(|v| ids[v as usize].0), Sources::default())
            .map_err(geometry)?;
    }
    edit.finish().map_err(geometry)
}

fn created(mesh: &Mesh) -> Provenance {
    Provenance {
        created_faces: mesh.faces().map(|(f, _)| (f, Sources::default())).collect(),
        created_vertices: mesh.vertices().map(|(v, _)| v).collect(),
        ..Provenance::default()
    }
}

pub struct AddShape;

impl Operation for AddShape {
    fn id(&self) -> &'static str {
        "object.add_shape"
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
        _inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let transform = params.get_affine("transform")?;
        let det = determinant(&transform);
        if det == 0.0 || !det.is_finite() {
            return Err(failed(
                "singular_transform",
                "The shape's transform flattens it; give it a non-zero scale.",
            ));
        }
        let shapes: Vec<ShapeToAdd> = serde_json::from_str(params.get_text("shapes")?)
            .map_err(|e| failed("invalid_shapes", format!("shapes isn't valid: {e}")))?;
        if shapes.is_empty() {
            return Err(failed("nothing_to_add", "No shapes were given to add."));
        }
        let group = params.get_text("group")?;
        let mut changes = Vec::new();
        let mut properties = Vec::new();
        for (n, s) in shapes.iter().enumerate() {
            ctx.checkpoint()?;
            let mesh = mf_parts::regenerate(&s.generator, ctx).map_err(generator_error)?;
            let provenance = created(&mesh);
            changes.push(ObjectChange::Create {
                name: s.name.clone(),
                mesh,
                transform,
                provenance,
            });
            let on = ChangeObject::Created(n);
            properties.push(ObjectChange::Property {
                object: on,
                key: mf_document::GENERATOR.into(),
                value: Some(s.generator.to_json()),
            });
            if s.hole {
                properties.push(ObjectChange::Property {
                    object: on,
                    key: mf_document::ROLE.into(),
                    value: Some(mf_document::HOLE.into()),
                });
            }
            if !group.is_empty() {
                properties.push(ObjectChange::Property {
                    object: on,
                    key: mf_document::GROUP.into(),
                    value: Some(group.to_owned()),
                });
            }
        }
        changes.extend(properties);
        Ok(DocOutput {
            report: serde_json::json!({
                "shapes": shapes.iter().map(|s| s.generator.id.clone()).collect::<Vec<_>>(),
                "group": group,
            }),
            changes,
        })
    }
}

pub struct SetShapeParams;

impl Operation for SetShapeParams {
    fn id(&self) -> &'static str {
        "object.set_shape_params"
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
        let records: Vec<GeneratorRecord> = serde_json::from_str(params.get_text("generators")?)
            .map_err(|e| failed("invalid_generators", format!("generators isn't valid: {e}")))?;
        if records.len() != inputs.len() {
            return Err(failed(
                "generators_mismatch",
                format!(
                    "{} recipes were given for {} shapes; give one per shape, in order",
                    records.len(),
                    inputs.len()
                ),
            ));
        }
        let mut changes = Vec::new();
        for (k, (input, record)) in inputs.iter().zip(&records).enumerate() {
            ctx.checkpoint()?;
            if input.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &input.target));
            }
            let fresh = mf_parts::regenerate(record, ctx).map_err(|e| match e {
                mf_parts::ConnectorError::Failed { code, message } => {
                    failed(code, format!("'{}': {message}", input.name))
                }
            })?;
            let (mesh, provenance) = replace(input.mesh, &fresh)?;
            changes.push(ObjectChange::Geometry {
                operand: k,
                mesh,
                provenance,
            });
            changes.push(ObjectChange::Property {
                object: ChangeObject::Operand(k),
                key: mf_document::GENERATOR.into(),
                value: Some(record.to_json()),
            });
        }
        Ok(DocOutput {
            report: serde_json::json!({ "regenerated": inputs.len() }),
            changes,
        })
    }
}
