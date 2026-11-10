//! Operation implementations. Each one has exactly one spec in
//! `specs/operations/` with a matching id and version (MF-SPEC-002).
//!
//! Operations never mutate their input. A mutating operation returns a new
//! [`Mesh`] (MF-DOC-001).

use mf_geometry::{FaceId, Mesh, Provenance};
pub use mf_runtime::{Cancelled, RunContext};
use mf_spec::{Params, SpecError};

mod compact;
mod fill_holes;
pub use fill_holes::{CURVATURE_ACCURATE_EDGES, curvature_limit_warning};

/// Most bytes `mesh.fill_holes` spends on triangulation tables at once:
/// holes are triangulated in parallel only while their summed O(n²)
/// tables fit (one hole bigger than this runs alone).
pub const TRIANGULATION_BUDGET_BYTES: u64 = 64 << 20;
mod inspect;
mod remove_small;
#[cfg(feature = "test-kinds")]
mod test_unit_conversion;

pub use remove_small::{ComponentSizes, RemovalPreview, removal_preview};

/// What an operation acts on. Phase 0 M2 operations accept only
/// `WholeObject`; the targeted variants exist now so M3's surgical operations
/// don't change the signature (MF-OP-003).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    WholeObject,
    Faces(Vec<FaceId>),
    Components(Vec<u32>),
    Loops(Vec<u32>),
}

pub struct OpOutput {
    /// The new revision and how it differs from the input. `None` when
    /// nothing changed: read-only operations, or edits with nothing to do.
    pub change: Option<(Mesh, Provenance)>,
    pub report: serde_json::Value,
}

/// Error for operations that don't support a target kind yet.
pub(crate) fn unsupported_target(op: &str, target: &Target) -> OpError {
    OpError::Failed {
        code: "unsupported_target",
        message: format!("{op} does not support target {target:?} yet"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error("{code}: {message}")]
    Failed { code: &'static str, message: String },
    /// The run was cancelled through its [`RunContext`]; nothing was
    /// produced (MF-CMD-006).
    #[error("cancelled by the user")]
    Cancelled,
}

impl From<Cancelled> for OpError {
    fn from(_: Cancelled) -> Self {
        OpError::Cancelled
    }
}

pub trait Operation: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    /// Makes `params`, recorded by version `recorded` of this operation,
    /// the parameters that make this version do exactly what that one did
    /// (a parameter added since gets the value that means "as before").
    /// Returns false for a version it can't replay: the caller reports a
    /// version mismatch rather than guess. The default replays only its own
    /// version.
    fn upgrade_params(&self, recorded: u32, _params: &mut Params) -> bool {
        recorded == self.version()
    }
    /// Runs the operation. Long work checks `ctx` for cancellation at least
    /// every 100 ms (MF-RT-002) and reports progress through it (MF-RT-003).
    /// A cancelled run returns [`OpError::Cancelled`], never a partial
    /// result.
    fn execute(
        &self,
        input: &Mesh,
        target: &Target,
        params: &Params,
        ctx: &RunContext,
    ) -> Result<OpOutput, OpError>;
}

static REGISTRY: &[&dyn Operation] = &[
    &compact::Compact,
    &fill_holes::FillHoles,
    &inspect::Inspect,
    &remove_small::RemoveSmallComponents,
    #[cfg(feature = "test-kinds")]
    &test_unit_conversion::TestUnitConversion,
];

pub fn registry() -> &'static [&'static dyn Operation] {
    REGISTRY
}

pub fn find(id: &str) -> Option<&'static dyn Operation> {
    REGISTRY.iter().copied().find(|o| o.id() == id)
}
