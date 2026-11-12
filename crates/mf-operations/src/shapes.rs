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
    /// The object this shape joins when its group is attached (a
    /// connector member placed on a known side: Connect's peg on side A
    /// and its hole on side B). Saved as the `attach_to` property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<u64>,
    /// This shape's own placement, instead of the command's `transform`
    /// (several connectors placed at once, each where it goes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<[f64; 12]>,
    /// This shape's own linked group, instead of the command's `group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// The property naming the object (by id number) a group member joins
/// when the group is attached.
pub const ATTACH_TO: &str = "attach_to";

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
        let shapes: Vec<ShapeToAdd> = serde_json::from_str(params.get_text("shapes")?)
            .map_err(|e| failed("invalid_shapes", format!("shapes isn't valid: {e}")))?;
        if shapes.is_empty() {
            return Err(failed("nothing_to_add", "No shapes were given to add."));
        }
        let common_group = params.get_text("group")?;
        let mut changes = Vec::new();
        let mut properties = Vec::new();
        for (n, s) in shapes.iter().enumerate() {
            ctx.checkpoint()?;
            let transform = s.transform.unwrap_or(transform);
            let det = determinant(&transform);
            if det == 0.0 || !det.is_finite() {
                return Err(failed(
                    "singular_transform",
                    format!(
                        "'{}': its transform flattens it; give it a non-zero scale.",
                        s.name
                    ),
                ));
            }
            let group = s.group.as_deref().unwrap_or(common_group);
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
            if let Some(to) = s.attach_to {
                properties.push(ObjectChange::Property {
                    object: on,
                    key: ATTACH_TO.into(),
                    value: Some(to.to_string()),
                });
            }
        }
        changes.extend(properties);
        Ok(DocOutput {
            report: serde_json::json!({
                "shapes": shapes.iter().map(|s| s.generator.id.clone()).collect::<Vec<_>>(),
                "group": common_group,
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

/// One member `object.attach` joins: which side, and whether it's cut
/// (a hole) or joined (a solid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttachMember {
    /// `'a'` (the target) or `'b'` (`side_b`).
    pub side: char,
    pub hole: bool,
}

/// `object.attach`'s `members` parameter, as JSON.
pub fn attach_param(members: &[AttachMember]) -> String {
    serde_json::to_string(members).expect("attach members always serialize")
}

/// `object.attach` (parametric-shapes brief, "Connectors become shape
/// groups"): a linked group's members joined onto the two objects they
/// connect, in one command — each side combined exactly as
/// `mesh.combine` does (its solids unioned, then its holes cut), the
/// members removed. With a `gap`, the two results must stay at least
/// that far apart where they meet (D4: never silently ship fused moving
/// parts, as `connector.join`).
pub struct Attach;

impl Operation for Attach {
    fn id(&self) -> &'static str {
        "object.attach"
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
        let roles: Vec<AttachMember> = serde_json::from_str(params.get_text("members")?)
            .map_err(|e| failed("invalid_members", format!("members isn't valid: {e}")))?;
        let side_b = inputs.iter().position(|o| o.slot == "side_b");
        let members: Vec<usize> = (0..inputs.len())
            .filter(|&k| inputs[k].slot == "members")
            .collect();
        if roles.len() != members.len() {
            return Err(failed(
                "members_mismatch",
                format!(
                    "{} member roles were given for {} members",
                    roles.len(),
                    members.len()
                ),
            ));
        }
        let gap = params.get_float("gap")?;
        let keep = Params(std::collections::BTreeMap::from([(
            "keep_holes".to_owned(),
            mf_spec::ParamValue::Bool(false),
        )]));
        let mut changes = Vec::new();
        let mut results: Vec<(usize, Mesh)> = Vec::new();
        for (side, host) in [('a', Some(0)), ('b', side_b)] {
            let chosen: Vec<(usize, bool)> = members
                .iter()
                .zip(&roles)
                .filter(|(_, r)| r.side == side)
                .map(|(&k, r)| (k, r.hole))
                .collect();
            if chosen.is_empty() {
                continue;
            }
            let Some(host) = host else {
                return Err(failed(
                    "missing_operand",
                    "Some members join side B, but no side B object was given.",
                ));
            };
            // The combine's own operands: its target, then solids, then
            // holes; `global[i]` is local operand i's index here.
            let mut local = vec![OperandInput {
                slot: "target",
                ..inputs[host].clone()
            }];
            let mut global = vec![host];
            for &(k, hole) in &chosen {
                local.push(OperandInput {
                    slot: if hole { "holes" } else { "solids" },
                    ..inputs[k].clone()
                });
                global.push(k);
            }
            let out = crate::combine::Combine.execute_doc(&local, &keep, ctx)?;
            for change in out.changes {
                changes.push(match change {
                    ObjectChange::Geometry {
                        operand,
                        mesh,
                        mut provenance,
                    } => {
                        for (_, sources) in &mut provenance.created_faces {
                            for (o, _) in &mut sources.operands {
                                *o = global[*o as usize] as u32;
                            }
                        }
                        results.push((global[operand], mesh.clone()));
                        ObjectChange::Geometry {
                            operand: global[operand],
                            mesh,
                            provenance,
                        }
                    }
                    ObjectChange::Remove { operand } => ObjectChange::Remove {
                        operand: global[operand],
                    },
                    other => other,
                });
            }
        }
        let mut measured = None;
        if gap > 0.0
            && let (Some(b), Some((_, a_mesh))) = (side_b, results.iter().find(|(k, _)| *k == 0))
        {
            let b_mesh = results
                .iter()
                .find(|(k, _)| *k == b)
                .map_or(inputs[b].mesh, |(_, m)| m);
            let bvh = mf_geometry::Bvh::build(a_mesh, ctx)?;
            let b_in_a =
                mf_boolean::operand_in_frame(b_mesh, &inputs[b].transform, &inputs[0].transform)
                    .ok_or_else(|| {
                        failed("singular_transform", "side B's transform can't be inverted")
                    })?;
            measured =
                mf_geometry::minimum_distance(a_mesh, &bvh, &b_in_a, &glam::DMat4::IDENTITY, ctx)?;
            let tolerance = (gap * 0.1).max(1e-6);
            if let Some(d) = measured
                && d < gap - tolerance
            {
                return Err(failed(
                    "moving_parts_fused",
                    format!(
                        "'{}' and '{}' would print fused ({d:.3} mm apart where they meet, at \
                         least {gap:.3} mm expected); move the parts apart or pick a looser Fit \
                         class.",
                        inputs[0].name, inputs[b].name
                    ),
                ));
            }
        }
        Ok(DocOutput {
            report: serde_json::json!({
                "attached": members.len(),
                "measured_gap": measured,
                "expected_gap": gap,
            }),
            changes,
        })
    }
}

/// `object.move_group`: several objects placed at once (a linked group
/// moved together, so a peg and its hole stay aligned), one absolute
/// transform each.
pub struct MoveGroup;

/// `object.move_group`'s `transforms` parameter, as JSON.
pub fn transforms_param(transforms: &[[f64; 12]]) -> String {
    serde_json::to_string(transforms).expect("transforms always serialize")
}

impl Operation for MoveGroup {
    fn id(&self) -> &'static str {
        "object.move_group"
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
        let transforms: Vec<[f64; 12]> = serde_json::from_str(params.get_text("transforms")?)
            .map_err(|e| failed("invalid_transforms", format!("transforms isn't valid: {e}")))?;
        if transforms.len() != inputs.len() {
            return Err(failed(
                "transforms_mismatch",
                format!(
                    "{} transforms were given for {} objects",
                    transforms.len(),
                    inputs.len()
                ),
            ));
        }
        ctx.checkpoint()?;
        let mut changes = Vec::new();
        for (k, (input, t)) in inputs.iter().zip(&transforms).enumerate() {
            let det = determinant(t);
            if det == 0.0 || !det.is_finite() {
                return Err(failed(
                    "singular_transform",
                    format!("'{}': the transform would flatten it", input.name),
                ));
            }
            if *t != input.transform {
                changes.push(ObjectChange::Transform {
                    operand: k,
                    transform: *t,
                });
            }
        }
        Ok(DocOutput {
            report: serde_json::json!({ "moved": changes.len() }),
            changes,
        })
    }
}
