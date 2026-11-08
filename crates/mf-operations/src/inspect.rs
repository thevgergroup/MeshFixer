use mf_geometry::Mesh;
use mf_spec::Params;

use crate::{OpError, OpOutput, Operation, Target, unsupported_target};

pub struct Inspect;

impl Operation for Inspect {
    fn id(&self) -> &'static str {
        "mesh.inspect"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        _params: &Params,
    ) -> Result<OpOutput, OpError> {
        if *target != Target::WholeObject {
            return Err(unsupported_target(self.id(), target));
        }
        let report =
            serde_json::to_value(mf_geometry::inspect(input)).map_err(|e| OpError::Failed {
                code: "report_serialization",
                message: e.to_string(),
            })?;
        Ok(OpOutput {
            change: None,
            report,
        })
    }
}
