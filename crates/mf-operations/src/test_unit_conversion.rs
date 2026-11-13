//! Test-only (`test-kinds` feature): an operation matching
//! `test.unit_conversion` in `specs/test-operations/`. It exists so
//! `mf-command`'s tests can exercise real unit-conversion wiring
//! (`Session::prepare`, replay, `reexecute`) against a parameter marked
//! `unit: mm`, without changing a real operation's parameters.
//!
//! It declares `geometry_changes` (not `read_only`) and always "changes"
//! geometry to an identical copy with empty provenance, purely so
//! `Session::commit` creates a real `CommandRecord` a test can inspect
//! (a read-only operation's result never becomes one, by design: see
//! `Session::commit`). Its report also echoes the `clearance`
//! parameter's value (already converted to document units by the time
//! `execute` sees it), so a test can check the operation's own view of
//! it, not just the recorded params.

use mf_geometry::{Mesh, Provenance};
use mf_spec::Params;

use crate::{OpError, OpOutput, Operation, RunContext, Target, unsupported_target};

pub struct TestUnitConversion;

impl Operation for TestUnitConversion {
    fn id(&self) -> &'static str {
        "test.unit_conversion"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        params: &Params,
        _ctx: &RunContext,
    ) -> Result<OpOutput, OpError> {
        if *target != Target::WholeObject {
            return Err(unsupported_target(self.id(), target));
        }
        let clearance = params.get_float("clearance")?;
        Ok(OpOutput {
            change: Some((input.clone(), Provenance::default())),
            report: serde_json::json!({ "clearance_seen_by_op": clearance }),
        })
    }
}
