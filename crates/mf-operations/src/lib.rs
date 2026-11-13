//! Operation implementations. Each one has exactly one spec in
//! `specs/operations/` with a matching id and version (MF-SPEC-002).
//!
//! Operations never mutate their input. A mutating operation returns a new
//! [`Mesh`] (MF-DOC-001).

use mf_document::ObjectId;
use mf_geometry::{FaceId, Mesh, Provenance};
pub use mf_runtime::{Cancelled, RunContext};
use mf_spec::{Params, SpecError};

mod add_part;
mod affine;
pub mod blend;
mod boolean;
mod combine;
pub use combine::{Applied, ApplyOrder, PartIn, apply_parts, check_moving_gap};
mod compact;
mod connector_ops;
pub mod materials;
pub use boolean::{kernel as boolean_kernel, refusal_copy as boolean_refusal_copy};
mod fill_holes;
pub use fill_holes::fairing::{Failed as FairFailed, Style as FairStyle, fair_region};
pub use fill_holes::{CURVATURE_ACCURATE_EDGES, curvature_limit_warning};

/// Most bytes `mesh.fill_holes` spends on triangulation tables at once:
/// holes are triangulated in parallel only while their summed O(n²)
/// tables fit (one hole bigger than this runs alone).
pub const TRIANGULATION_BUDGET_BYTES: u64 = 64 << 20;
mod inspect;
mod intersections;
mod make_solid;
pub use make_solid::{
    MAX_GRID_POINTS as MAKE_SOLID_MAX_GRID_POINTS, STAGES as MAKE_SOLID_STAGES,
    TRADE_OFF as MAKE_SOLID_TRADE_OFF,
};
pub mod layout;
mod remove_small;
pub mod sculpt;
pub mod shapes;
mod split;
#[cfg(feature = "test-kinds")]
mod test_consume_tool;
#[cfg(feature = "test-kinds")]
mod test_unit_conversion;
mod transform;

pub use split::joints::{
    ADD_SECOND, ANCHOR, CapRegion, CapTargets, Check, CheckSettings, HOLE_PROFILES, JointCheck,
    JointFix, JointFrame, JointSide, Piece, PlacedPart, Placement, SINGLE_ROUND_PIN, cap_middle,
    cap_region, check_id, check_placement, fix, fix_joint, footprint_2d, generator_checks,
    joint_checks, oriented_params, patched, placed_parts, placement_generator, placement_params,
    placement_to_world, wanted_clearance,
};
pub use split::pegs::{
    HolePlacement, PegError, PegShape, dowel_size, place_holes, pole_of_inaccessibility,
};
pub use split::volume::{BOX_FACES, inside_name, outside_name};
pub use split::{AXIS_ALIGN_DEGREES, SPLIT_STAGES, SideNames, split_names};
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
    /// The operand slot this object fills: `target` for the target (and a
    /// `many` target's extra targets), otherwise the spec's operand name.
    /// A `many` slot's objects are consecutive inputs with one slot name.
    pub slot: &'a str,
    pub object: ObjectId,
    /// The object's name, for naming what the operation makes from it
    /// ("scan (top)").
    pub name: &'a str,
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
    /// The operand's object gets a new name (a split's "scan (top)").
    Rename { operand: usize, name: String },
    /// A new object. Its provenance lists every face as created, with
    /// its sources in [`mf_geometry::Sources::operands`] (MF-P1-SEL-001).
    Create {
        name: String,
        mesh: Mesh,
        transform: [f64; 12],
        provenance: Provenance,
    },
    /// An object property set (`Some`) or removed (`None`), such as a
    /// parametric shape's recipe or its Solid/Hole role: on an operand,
    /// or on an object this output creates (named by its place among the
    /// output's [`ObjectChange::Create`]s, which must come first).
    Property {
        object: ChangeObject,
        key: String,
        value: Option<String>,
    },
}

/// Which object an [`ObjectChange::Property`] is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeObject {
    /// The operand at this index.
    Operand(usize),
    /// The `n`th object this output creates (0 is the first `Create`).
    Created(usize),
}

/// Where one object of the document sits (read-only scene context, for
/// operations that place new things clear of everything already there:
/// dowels, `object.arrange_on_plate`).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneObject {
    pub object: ObjectId,
    /// The object's world bounding box (its object-space box's corners,
    /// placed by its transform: conservative for rotated objects).
    pub world_min: [f64; 3],
    pub world_max: [f64; 3],
}

impl SceneObject {
    /// Its world XY footprint, `[min_x, min_y, max_x, max_y]`.
    pub fn footprint(&self) -> [f64; 4] {
        [
            self.world_min[0],
            self.world_min[1],
            self.world_max[0],
            self.world_max[1],
        ]
    }

    /// The world bounding box of `mesh` placed by `transform`; `None` for
    /// an empty mesh.
    pub fn of(
        object: ObjectId,
        mesh: &Mesh,
        transform: &[f64; 12],
        ctx: &RunContext,
    ) -> Result<Option<SceneObject>, Cancelled> {
        let Some(bb) = mesh.bounding_box_with(ctx)? else {
            return Ok(None);
        };
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for c in 0..8 {
            let p = [0, 1, 2].map(|a| {
                if c & (1 << a) != 0 {
                    bb.max[a]
                } else {
                    bb.min[a]
                }
            });
            let w = affine::apply(transform, p);
            for a in 0..3 {
                lo[a] = lo[a].min(w[a]);
                hi[a] = hi[a].max(w[a]);
            }
        }
        Ok(Some(SceneObject {
            object,
            world_min: lo,
            world_max: hi,
        }))
    }
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
    /// Whether the operation reads the scene (every object's world box):
    /// the command layer then builds it on the worker and calls
    /// [`Operation::execute_scene`]. Default: no.
    fn wants_scene(&self) -> bool {
        false
    }

    /// [`Operation::execute_doc`] with the scene: every object of the
    /// document at the command's base revision, operands included, in
    /// document order (read-only: changes still go through operands).
    /// Derived from the document, so replay sees the same scene. The
    /// default ignores it.
    fn execute_scene(
        &self,
        inputs: &[OperandInput],
        _scene: &[SceneObject],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        self.execute_doc(inputs, params, ctx)
    }

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
    &combine::Combine,
    &compact::Compact,
    &fill_holes::FillHoles,
    &inspect::Inspect,
    &intersections::Intersections,
    &make_solid::MakeSolid,
    &remove_small::RemoveSmallComponents,
    &transform::SetTransform,
    &transform::DropToPlate,
    &transform::ArrangeOnPlate,
    &transform::Delete,
    &split::SplitPlane,
    &split::region::SplitRegion,
    &split::region::DeleteFaces,
    &split::volume::SplitBox,
    &split::joints::PlaceConnectors,
    &sculpt::SculptStroke,
    &add_part::AddPart,
    &shapes::AddShape,
    &shapes::SetShapeParams,
    &shapes::Attach,
    &shapes::MoveGroup,
    &connector_ops::ConnectorJoin,
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
