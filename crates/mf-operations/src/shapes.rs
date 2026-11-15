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
    ChangeObject, DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation,
    RunContext, Target, needs_document, unsupported_target,
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
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        _inputs: &[OperandInput],
        params: &Params,
        _doc: &DocContext,
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

/// A generator `object.attach` builds and joins in the same step, with
/// no member objects placed first: every part it makes goes to the
/// target its `attach_to` names (A the command's target, B `side_b`), a
/// loose part becoming its own object at `transform`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GeneratedMember {
    pub generator: GeneratorRecord,
    /// Where the generator's own frame is placed.
    pub transform: [f64; 12],
}

/// `object.attach`'s `generated` parameter, as JSON.
pub fn generated_param(members: &[GeneratedMember]) -> String {
    serde_json::to_string(members).expect("generated members always serialize")
}

/// What [`attach`] joins: the two targets (operand index and input; B
/// optional), the member objects (operand index, side 0/1, hole) and the
/// generators built in place.
pub(crate) struct AttachJob<'a> {
    pub a: (usize, &'a OperandInput<'a>),
    pub b: Option<(usize, &'a OperandInput<'a>)>,
    pub members: Vec<(usize, usize, bool)>,
    pub generated: Vec<GeneratedMember>,
    /// The gap the two targets must keep (0: not checked unless
    /// `always_measure`).
    pub gap: f64,
    pub always_measure: bool,
}

/// The one apply command's body (`object.attach`, and `connector.join`
/// replaying old logs): N parts — member objects and generated ones —
/// folded into their target slots in one step ([`crate::apply_parts`]),
/// each member removed, each loose generated part created; then the
/// moving-parts gap check. Returns the changes and the measured gap.
pub(crate) fn attach(
    job: &AttachJob,
    inputs: &[OperandInput],
    ctx: &RunContext,
) -> Result<(Vec<ObjectChange>, Option<f64>), OpError> {
    use std::borrow::Cow;
    let mut parts: Vec<crate::PartIn> = job
        .members
        .iter()
        .map(|&(k, host, hole)| crate::PartIn {
            name: inputs[k].name.to_owned(),
            mesh: Cow::Borrowed(inputs[k].mesh),
            transform: inputs[k].transform,
            role: if hole {
                mf_parts::ShapeRole::Hole
            } else {
                mf_parts::ShapeRole::Solid
            },
            host,
            operand: Some(k),
        })
        .collect();
    let mut loose = Vec::new();
    for g in &job.generated {
        ctx.checkpoint()?;
        let generator = mf_parts::find_generator(&g.generator.id).ok_or_else(|| {
            failed(
                "unknown_connector",
                format!("'{}' isn't a built-in connector", g.generator.id),
            )
        })?;
        if generator.version() != g.generator.version {
            return Err(failed(
                "generator_version",
                format!(
                    "'{}' was made with version {}, but this build has version {}",
                    g.generator.id,
                    g.generator.version,
                    generator.version()
                ),
            ));
        }
        let params = mf_parts::generator::upgraded(generator, &g.generator.params)
            .map_err(generator_error)?;
        let made = generator
            .generate(&params, &mf_parts::GenContext::new(ctx))
            .map_err(generator_error)?;
        for part in made {
            let host = match part.attach_to {
                mf_parts::TargetSlot::A => 0,
                mf_parts::TargetSlot::B => 1,
                mf_parts::TargetSlot::Neither => {
                    loose.push((part.name, part.mesh, g.transform));
                    continue;
                }
            };
            parts.push(crate::PartIn {
                name: part.name.to_owned(),
                mesh: Cow::Owned(part.mesh),
                transform: g.transform,
                role: part.role,
                host,
                operand: None,
            });
        }
    }
    let applied = crate::apply_parts(
        [Some(job.a), job.b],
        &parts,
        crate::ApplyOrder::Sequential,
        ctx,
    )?;
    let mut changes = Vec::new();
    for r in &applied {
        let operand = if r.host == 0 {
            job.a.0
        } else {
            job.b.map_or(0, |b| b.0)
        };
        changes.push(ObjectChange::Geometry {
            operand,
            mesh: r.mesh.clone(),
            provenance: r.provenance.clone(),
        });
        for &(k, host, _) in &job.members {
            if host == r.host {
                changes.push(ObjectChange::Remove { operand: k });
            }
        }
    }
    let mut measured = None;
    if let Some(b) = job.b
        && (job.gap > 0.0 || job.always_measure)
    {
        let mesh_of = |host: usize, input: &OperandInput| {
            applied
                .iter()
                .find(|r| r.host == host)
                .map_or_else(|| input.mesh.clone(), |r| r.mesh.clone())
        };
        let (am, bm) = (mesh_of(0, job.a.1), mesh_of(1, b.1));
        measured = crate::check_moving_gap((&am, job.a.1), (&bm, b.1), job.gap, ctx)?;
    }
    for (name, mesh, transform) in loose {
        changes.push(ObjectChange::Create {
            name: format!("{} {name}", job.a.1.name),
            provenance: created(&mesh),
            mesh,
            transform,
        });
    }
    Ok((changes, measured))
}

/// `object.attach` (parametric-shapes brief, "Connectors become shape
/// groups"; parts-architecture-review.md, "One generic apply"): N parts
/// joined onto the targets they connect, in one command — a linked
/// group's member objects (each removed once joined) and generators
/// built in place, each part to its target slot, solids joined and holes
/// cut. With a `gap`, the two results must stay at least that far apart
/// where they meet (D4: never silently ship fused moving parts).
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
        let roles: Vec<AttachMember> = serde_json::from_str(params.get_text("members")?)
            .map_err(|e| failed("invalid_members", format!("members isn't valid: {e}")))?;
        let generated: Vec<GeneratedMember> =
            serde_json::from_str(params.get_text("generated").unwrap_or("[]"))
                .map_err(|e| failed("invalid_generated", format!("generated isn't valid: {e}")))?;
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
        let job = AttachJob {
            a: (0, &inputs[0]),
            b: side_b.map(|k| (k, &inputs[k])),
            members: members
                .iter()
                .zip(&roles)
                .map(|(&k, r)| (k, usize::from(r.side == 'b'), r.hole))
                .collect(),
            generated,
            gap: params.get_float("gap")?,
            always_measure: false,
        };
        let (changes, measured) = attach(&job, inputs, ctx)?;
        Ok(DocOutput {
            report: serde_json::json!({
                "attached": members.len() + job.generated.len(),
                "measured_gap": measured,
                "expected_gap": job.gap,
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
