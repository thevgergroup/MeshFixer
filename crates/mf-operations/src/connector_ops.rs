//! `connector.join` (Phase 1 Task 28, MF-MODEL-005): fold a connector's
//! parts into the two objects it joins, in one command, and remove the
//! connector object.
//!
//! The connector object's own mesh isn't read: `connector_id` and
//! `connector_params` (recorded at generation time, D16, like a fit
//! profile's resolved values) are enough to regenerate the exact same
//! [`mf_parts::ConnectorParts`] deterministically, so replay never reads
//! a connector's live state. Each part's role decides what happens to it
//! (`mf_parts::JoinKind`): `Union` is unioned into its side (`Side::A` →
//! `side_a`, `Side::B` → `side_b`); `Subtract` is subtracted from its
//! side; `Separate` becomes its own new object, placed at the connector
//! target's transform (a loose pin, printed apart).
//!
//! After every part is applied, `side_a` and `side_b` must not intersect,
//! and their minimum distance (measured at the connector, the first
//! `Side::A`/`Side::B` part pair) must be at least `gap` minus the chord
//! tolerance — otherwise the join is refused as `moving_parts_fused`
//! (D4: never silently ship fused moving parts).

use glam::DMat4;
use mf_boolean::{BoolOp, BooleanError, operand_in_frame};
use mf_geometry::{Bvh, Mesh, Provenance, Sources};
use mf_parts::{Connector, ConnectorPart, JoinKind, Side};
use mf_spec::Params;

use crate::{
    DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext, Target,
    needs_document, unsupported_target,
};

pub struct ConnectorJoin;

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// The built-in connector named `id` (the plugin host will chain its own
/// registry after this, MF-PLUG-001; `connector.join` only needs the
/// built-ins for now).
fn find_connector(id: &str) -> Option<&'static dyn Connector> {
    mf_parts::connectors().into_iter().find(|c| c.id() == id)
}

/// `mesh op tool`, `tool` already in `mesh`'s frame.
fn apply(mesh: &Mesh, tool: &Mesh, op: BoolOp, ctx: &RunContext) -> Result<Mesh, OpError> {
    let kernel = crate::boolean_kernel();
    let out = mf_boolean::boolean(mesh, tool, op, kernel, ctx).map_err(|e| match e {
        BooleanError::Cancelled => OpError::Cancelled,
        e => failed(
            e.code(),
            format!(
                "Joining the connector failed: {}. {}",
                e,
                crate::boolean_refusal_copy(e.code())
                    .map_or("Move the connector or adjust its parameters", |c| c.1)
            ),
        ),
    })?;
    Ok(out.mesh)
}

impl Operation for ConnectorJoin {
    fn id(&self) -> &'static str {
        "connector.join"
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
        let [target, side_a, side_b] = inputs else {
            return Err(failed(
                "missing_operand",
                format!(
                    "{} needs the connector target, side_a and side_b",
                    self.id()
                ),
            ));
        };
        for o in [target, side_a, side_b] {
            if o.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &o.target));
            }
        }
        let connector_id = params.get_text("connector_id")?;
        let connector = find_connector(connector_id).ok_or_else(|| {
            failed(
                "unknown_connector",
                format!("'{connector_id}' isn't a built-in connector"),
            )
        })?;
        let connector_params_json = params.get_text("connector_params")?;
        let raw_params: std::collections::BTreeMap<String, mf_spec::ParamValue> =
            serde_json::from_str(connector_params_json).map_err(|e| {
                failed(
                    "invalid_connector_params",
                    format!("connector_params isn't valid: {e}"),
                )
            })?;
        let connector_params = mf_spec::Params(raw_params);
        let gap = params.get_float("gap")?;

        let generated = connector
            .generate(&connector_params, ctx)
            .map_err(|e| match e {
                mf_parts::ConnectorError::Failed { code, message } => failed(code, message),
            })?;

        let mut mesh_a = side_a.mesh.clone();
        let mut mesh_b = side_b.mesh.clone();
        let mut prov_a = Provenance::default();
        let mut prov_b = Provenance::default();
        let mut created = Vec::new();

        for ConnectorPart {
            role,
            mesh,
            side,
            join,
        } in generated.parts
        {
            ctx.checkpoint()?;
            match join {
                JoinKind::Separate => {
                    let in_target = operand_in_frame(&mesh, &target.transform, &target.transform)
                        .unwrap_or(mesh);
                    let provenance = Provenance {
                        created_faces: in_target
                            .faces()
                            .map(|(f, _)| (f, Sources::default()))
                            .collect(),
                        created_vertices: in_target.vertices().map(|(v, _)| v).collect(),
                        ..Provenance::default()
                    };
                    created.push(ObjectChange::Create {
                        name: format!("{} {role}", target.name),
                        mesh: in_target,
                        transform: target.transform,
                        provenance,
                    });
                    continue;
                }
                JoinKind::Union | JoinKind::Subtract => {}
            }
            let (dest_mesh, dest_transform, dest_prov): (&mut Mesh, &[f64; 12], &mut Provenance) =
                match side {
                    Side::A => (&mut mesh_a, &side_a.transform, &mut prov_a),
                    Side::B => (&mut mesh_b, &side_b.transform, &mut prov_b),
                    Side::Neither => {
                        return Err(failed(
                            "connector_part_unassigned",
                            format!(
                                "the '{role}' part joins neither side; connector.join has no \
                                 object to apply it to"
                            ),
                        ));
                    }
                };
            let in_frame =
                operand_in_frame(&mesh, &target.transform, dest_transform).ok_or_else(|| {
                    failed(
                        "singular_transform",
                        "a target's transform can't be inverted",
                    )
                })?;
            let op = match join {
                JoinKind::Union => BoolOp::Union,
                JoinKind::Subtract => BoolOp::Subtract,
                JoinKind::Separate => unreachable!("handled above"),
            };
            *dest_mesh = apply(dest_mesh, &in_frame, op, ctx)?;
            *dest_prov = compose_created(dest_prov, dest_mesh);
        }

        // The exact post-join check (D4, owner requirements 2026-11-11):
        // measure the real gap between side_a and side_b where the
        // connector sits, never assume it from the parameters.
        let a_bvh = Bvh::build(&mesh_a, ctx).map_err(|_| OpError::Cancelled)?;
        let b_in_a = operand_in_frame(&mesh_b, &side_b.transform, &side_a.transform)
            .ok_or_else(|| failed("singular_transform", "side_b's transform can't be inverted"))?;
        let measured =
            mf_geometry::minimum_distance(&mesh_a, &a_bvh, &b_in_a, &DMat4::IDENTITY, ctx)
                .map_err(|_| OpError::Cancelled)?;
        let chord_tolerance = (gap * 0.1).max(1e-6);
        if let Some(d) = measured
            && d < gap - chord_tolerance
        {
            return Err(failed(
                "moving_parts_fused",
                format!(
                    "The hinge would print fused to the lid here ({d:.3} mm, expected at least \
                     {gap:.3} mm); increase the gap or move the hinge."
                ),
            ));
        }

        let mut changes = vec![
            ObjectChange::Geometry {
                operand: 1,
                mesh: mesh_a,
                provenance: prov_a,
            },
            ObjectChange::Geometry {
                operand: 2,
                mesh: mesh_b,
                provenance: prov_b,
            },
            ObjectChange::Remove { operand: 0 },
        ];
        changes.extend(created);

        Ok(DocOutput {
            report: serde_json::json!({
                "connector_id": connector_id,
                "measured_gap": measured,
                "expected_gap": gap,
            }),
            changes,
        })
    }
}

/// `compose_created`: a provenance that names every face of `mesh` not
/// already recorded as kept as a created face (no source) — a rough but
/// correct-by-construction provenance for a sequence of booleans applied
/// to one side across several connector parts, where tracking exact
/// per-boolean sources isn't this task's concern (TODO, follow-up issue:
/// thread the boolean's own provenance through instead of recomputing).
fn compose_created(prev: &Provenance, mesh: &Mesh) -> Provenance {
    let mut p = prev.clone();
    p.created_faces = mesh.faces().map(|(f, _)| (f, Sources::default())).collect();
    p
}
