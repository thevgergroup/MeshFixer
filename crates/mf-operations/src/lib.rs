//! Operation implementations. Each one has exactly one spec in
//! `specs/operations/` with a matching id and version (MF-SPEC-002).
//!
//! Operations never mutate their input. A mutating operation returns a new
//! [`Mesh`] (MF-DOC-001).

use mf_geometry::Mesh;
use mf_spec::{Params, SpecError};

mod inspect;
mod remove_small;

pub struct OpOutput {
    /// `None` for read-only operations.
    pub mesh: Option<Mesh>,
    pub report: serde_json::Value,
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
    fn execute(&self, input: &Mesh, params: &Params) -> Result<OpOutput, OpError>;
}

static REGISTRY: &[&dyn Operation] = &[&inspect::Inspect, &remove_small::RemoveSmallComponents];

pub fn registry() -> &'static [&'static dyn Operation] {
    REGISTRY
}

pub fn find(id: &str) -> Option<&'static dyn Operation> {
    REGISTRY.iter().copied().find(|o| o.id() == id)
}
