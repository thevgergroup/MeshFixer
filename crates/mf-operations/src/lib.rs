//! Operation implementations. Each one has exactly one spec in
//! `specs/operations/` with a matching id and version (MF-SPEC-002).
//!
//! Operations never mutate their input. A mutating operation returns a new
//! [`Mesh`] (MF-DOC-001).

use mf_document::ObjectId;
use mf_geometry::{FaceId, Mesh, Provenance};
pub use mf_runtime::{Cancelled, RunContext};
use mf_spec::{Params, SpecError};

mod boolean;
mod compact;
pub use boolean::{kernel as boolean_kernel, refusal_copy as boolean_refusal_copy};
mod fill_holes;
pub use fill_holes::{CURVATURE_ACCURATE_EDGES, curvature_limit_warning};

/// Most bytes `mesh.fill_holes` spends on triangulation tables at once:
/// holes are triangulated in parallel only while their summed O(n²)
/// tables fit (one hole bigger than this runs alone).
pub const TRIANGULATION_BUDGET_BYTES: u64 = 64 << 20;
mod inspect;
mod make_solid;
pub use make_solid::{
    MAX_GRID_POINTS as MAKE_SOLID_MAX_GRID_POINTS, STAGES as MAKE_SOLID_STAGES,
    TRADE_OFF as MAKE_SOLID_TRADE_OFF,
};
mod remove_small;
#[cfg(feature = "test-kinds")]
mod test_consume_tool;
#[cfg(feature = "test-kinds")]
mod test_unit_conversion;
mod transform;

pub use transform::{world_min_z, world_min_z_with};

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

/// One operand of a command, as an operation sees it (D6,
/// MF-P1-CMD-001). Operand 0 is the command's target; the rest follow in
/// the order the spec's `operands:` list names them (an absent optional
/// operand is skipped).
#[derive(Debug, Clone)]
pub struct OperandInput<'a> {
    pub object: ObjectId,
    /// The object's geometry, in its own (object) space.
    pub mesh: &'a Mesh,
    /// The object's placement: absolute 3MF row-major 3×4 affine (D7).
    pub transform: [f64; 12],
    /// The part of the operand the command acts on.
    pub target: Target,
}

/// One change an operation makes to the document. `operand` indexes the
/// [`OperandInput`]s the operation was given.
#[derive(Debug, Clone)]
pub enum ObjectChange {
    /// The operand's geometry becomes `mesh`; `provenance` says how it
    /// differs from the operand's input mesh (MF-SEL-002).
    Geometry {
        operand: usize,
        mesh: Mesh,
        provenance: Provenance,
    },
    /// The operand's placement becomes `transform` (absolute, D7). The
    /// geometry is unchanged and stays shared.
    Transform {
        operand: usize,
        transform: [f64; 12],
    },
    /// The operand's object is removed from the document (undo restores
    /// it with the same id).
    Remove { operand: usize },
    /// A new object. Its provenance lists every face as created, with
    /// its sources in [`mf_geometry::Sources::operands`] (MF-P1-SEL-001).
    Create {
        name: String,
        mesh: Mesh,
        transform: [f64; 12],
        provenance: Provenance,
    },
}

/// What a document-level run produced: every object change, in order,
/// and the operation's report. No changes means nothing changed.
#[derive(Debug, Clone)]
pub struct DocOutput {
    pub changes: Vec<ObjectChange>,
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

    /// Runs the operation on every operand (D6). `inputs[0]` is the
    /// command's target. The default runs [`Operation::execute`] on
    /// operand 0 and reports its geometry change, so single-object
    /// operations implement only `execute`. Same cancellation contract as
    /// `execute`.
    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let target = inputs.first().ok_or_else(|| OpError::Failed {
            code: "missing_target",
            message: format!("{} was run with no target", self.id()),
        })?;
        let out = self.execute(target.mesh, &target.target, params, ctx)?;
        Ok(DocOutput {
            changes: out
                .change
                .map(|(mesh, provenance)| ObjectChange::Geometry {
                    operand: 0,
                    mesh,
                    provenance,
                })
                .into_iter()
                .collect(),
            report: out.report,
        })
    }
}

/// Error for an object-level operation run through the single-mesh
/// [`Operation::execute`]: it needs the object's placement, so it only
/// runs through [`Operation::execute_doc`].
pub(crate) fn needs_document(op: &str) -> OpError {
    OpError::Failed {
        code: "needs_document",
        message: format!("{op} changes an object, not a mesh; run it as a command"),
    }
}

static REGISTRY: &[&dyn Operation] = &[
    &boolean::Boolean,
    &compact::Compact,
    &fill_holes::FillHoles,
    &inspect::Inspect,
    &make_solid::MakeSolid,
    &remove_small::RemoveSmallComponents,
    &transform::SetTransform,
    &transform::DropToPlate,
    &transform::Delete,
    #[cfg(feature = "test-kinds")]
    &test_consume_tool::TestConsumeTool,
    #[cfg(feature = "test-kinds")]
    &test_unit_conversion::TestUnitConversion,
];

pub fn registry() -> &'static [&'static dyn Operation] {
    REGISTRY
}

pub fn find(id: &str) -> Option<&'static dyn Operation> {
    REGISTRY.iter().copied().find(|o| o.id() == id)
}
