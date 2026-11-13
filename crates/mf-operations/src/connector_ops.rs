//! `connector.join` (Phase 1 Task 28, MF-MODEL-005): fold a connector's
//! parts into the two objects it joins, in one command, and remove the
//! connector object.
//!
//! Kept for the logs that recorded it: it is now a thin alias of the one
//! generic apply, `object.attach`'s body (`crate::shapes::attach`), with
//! the connector built in place at the connector object's placement from
//! `connector_id` and `connector_params` (recorded at generation time,
//! D16, so replay never reads a connector's live state). Every part goes
//! to its target slot (A `side_a`, B `side_b`), solids joined and holes
//! cut; a loose part becomes its own object at the connector's
//! placement; then `side_a` and `side_b` must stay at least `gap` apart
//! where they meet (`moving_parts_fused` otherwise, D4).

use mf_geometry::Mesh;
use mf_spec::Params;

use crate::shapes::{AttachJob, GeneratedMember, attach};
use crate::{
    DocOutput, OpError, OpOutput, OperandInput, Operation, RunContext, Target, needs_document,
    unsupported_target,
};

pub struct ConnectorJoin;

fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
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
        let generator = mf_parts::find_generator(connector_id)
            .filter(|g| !g.is_shape())
            .ok_or_else(|| {
                failed(
                    "unknown_connector",
                    format!("'{connector_id}' isn't a built-in connector"),
                )
            })?;
        let raw_params: std::collections::BTreeMap<String, mf_spec::ParamValue> =
            serde_json::from_str(params.get_text("connector_params")?).map_err(|e| {
                failed(
                    "invalid_connector_params",
                    format!("connector_params isn't valid: {e}"),
                )
            })?;
        let gap = params.get_float("gap")?;
        // The generic apply (`object.attach`'s body), with the connector
        // built in place at the connector object's own placement: every
        // part to its target slot, loose parts as new objects, then the
        // connector object removed.
        let job = AttachJob {
            a: (1, side_a),
            b: Some((2, side_b)),
            members: Vec::new(),
            generated: vec![GeneratedMember {
                generator: mf_parts::GeneratorRecord {
                    id: connector_id.to_owned(),
                    version: generator.version(),
                    params: mf_spec::Params(raw_params),
                    part: None,
                },
                transform: target.transform,
            }],
            gap,
            always_measure: true,
        };
        let (mut changes, measured) = attach(&job, inputs, ctx)?;
        // Loose parts are named after the connector object, not side A.
        for c in &mut changes {
            if let crate::ObjectChange::Create { name, .. } = c
                && let Some(role) = name.strip_prefix(side_a.name)
            {
                *name = format!("{}{role}", target.name);
            }
        }
        let creates = changes
            .iter()
            .position(|c| matches!(c, crate::ObjectChange::Create { .. }))
            .unwrap_or(changes.len());
        changes.insert(creates, crate::ObjectChange::Remove { operand: 0 });
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
