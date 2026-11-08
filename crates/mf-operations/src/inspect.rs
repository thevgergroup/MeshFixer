use mf_geometry::Mesh;
use mf_spec::Params;

use crate::{OpError, OpOutput, Operation};

pub struct Inspect;

impl Operation for Inspect {
    fn id(&self) -> &'static str {
        "mesh.inspect"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(&self, input: &Mesh, _params: &Params) -> Result<OpOutput, OpError> {
        let report =
            serde_json::to_value(mf_geometry::inspect(input)).map_err(|e| OpError::Failed {
                code: "report_serialization",
                message: e.to_string(),
            })?;
        Ok(OpOutput { mesh: None, report })
    }
}
