//! The `Connector` trait (Phase 1 Task 25, MF-MODEL-005): a parametric
//! generator for a printable mechanism (a hinge, a clasp, ...), ready for
//! the plugin host (MF-PLUG-001) to add more without touching this crate.
//!
//! A connector produces one or more [`ConnectorPart`]s, each with a role
//! name, which side of the mating pair it belongs to, and how it joins
//! that side (`object.add_connector` makes one object with one component
//! per part; `connector.join`, Task 28, folds them into the model).

use std::ops::Range;

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};

/// Which side of the mating pair a part belongs to (D22): `A` and `B`
/// are the two things the connector joins (a body and a lid; two halves
/// of a box); `Neither` is a part that's temporary or stands alone (not
/// unioned into either side by `connector.join`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    A,
    B,
    Neither,
}

/// How a part joins its side, in `connector.join` (Task 28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Union,
    Subtract,
    /// Becomes its own new object instead of joining a side (a loose
    /// pin, a separate printed piece).
    Separate,
}

/// One generated piece of a connector.
pub struct ConnectorPart {
    pub role: &'static str,
    pub mesh: Mesh,
    pub side: Side,
    pub join: JoinKind,
}

/// Everything a connector's `generate` produces.
pub struct ConnectorParts {
    pub parts: Vec<ConnectorPart>,
}

/// An error generating a connector (refused by name, D4: never a plain
/// panic or a silently wrong part).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConnectorError {
    #[error("{code}: {message}")]
    Failed { code: &'static str, message: String },
}

/// A parametric connector generator. Registered in [`crate::connectors`];
/// the plugin host adds to the same registry (MF-PLUG-001).
pub trait Connector: Send + Sync {
    /// Stable id (`"hinge.pin"`, `"clasp.snap"`), namespaced like an
    /// operation's.
    fn id(&self) -> &'static str;
    /// Bumped when the generated geometry's meaning changes incompatibly
    /// (mirrors `Operation::version`); old saved connectors keep working
    /// (`object.edit_connector` records the version it was made with).
    fn version(&self) -> u32;
    /// The connector's own parameters, spec-driven like an operation's
    /// (MF-SPEC-001…003): only the `parameters:` section is used (no
    /// `target`/`operands`, since a connector has neither).
    fn spec(&self) -> &'static OperationSpec;
    /// Builds the connector's parts from validated `params` (already
    /// checked against `spec()`, with fit-profile defaults filled).
    fn generate(&self, params: &Params, ctx: &RunContext)
    -> Result<ConnectorParts, ConnectorError>;
}

/// Where in the generated mesh's face numbering a role's faces fall,
/// recorded on `ConnectorInstance.roles` so a saved connector's parts can
/// be told apart after `object.add_connector` merges them into one mesh.
pub type RoleFaceRange = Range<u32>;
