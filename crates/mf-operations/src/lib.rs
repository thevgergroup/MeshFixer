//! Operation implementations. Each one has exactly one spec in
//! `specs/operations/` with a matching id and version (MF-SPEC-002).
//!
//! Operations never mutate their input. A mutating operation returns a new
//! [`Mesh`] (MF-DOC-001).

use mf_geometry::{FaceId, Mesh, Provenance};
use mf_spec::{Params, SpecError};

mod inspect;
mod remove_small;

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
}

pub trait Operation: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn execute(&self, input: &Mesh, target: &Target, params: &Params) -> Result<OpOutput, OpError>;
}

static REGISTRY: &[&dyn Operation] = &[&inspect::Inspect, &remove_small::RemoveSmallComponents];

pub fn registry() -> &'static [&'static dyn Operation] {
    REGISTRY
}

pub fn find(id: &str) -> Option<&'static dyn Operation> {
    REGISTRY.iter().copied().find(|o| o.id() == id)
}
