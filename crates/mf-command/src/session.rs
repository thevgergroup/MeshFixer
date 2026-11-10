use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use mf_document::{Document, Geometry, IDENTITY_TRANSFORM, Object, ObjectId, RevisionId, Unit};
use mf_geometry::{Mesh, Provenance};
use mf_io::threemf::ModelObject;
use mf_io::{Imported, ImportedModel, SourceInfo};
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, Operation, Target};
use mf_runtime::RunContext;
use mf_selection::{Selection, SelectionKind, Step, TranslateError, Translation};
use mf_spec::{OperandRole, OperationSpec, Params, SpecError};

use crate::record::{
    AffectedSummary, CommandId, CommandRecord, CommandStatus, ImportRecord, ObjectOutput,
    RecordedOperand, platform,
};

/// The name operand 0 (the command's target) is recorded under.
pub const TARGET_OPERAND: &str = "target";

/// A request to run an operation, as issued by any frontend.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub op_id: String,
    pub target: ObjectId,
    /// What part of the object to act on; `None` is the whole object. Must be
    /// bound to the current revision (translate it first).
    pub selection: Option<Selection>,
    /// The command's other operands (D6): `(name, object, selection)`,
    /// named as in the spec's `operands:` list, in any order. A `None`
    /// selection is the whole object; a selection must be bound to the
    /// current revision. Empty for single-object operations.
    pub operands: Vec<(String, ObjectId, Option<Selection>)>,
    pub raw_params: BTreeMap<String, String>,
}

/// The operation target a selection describes.
pub fn target_of(sel: &Selection) -> Target {
    match sel.kind {
        SelectionKind::Object => Target::WholeObject,
        SelectionKind::Faces => {
            Target::Faces(sel.ids.iter().map(|&f| mf_geometry::FaceId(f)).collect())
        }
        SelectionKind::Component => Target::Components(sel.ids.clone()),
        SelectionKind::BoundaryLoop => Target::Loops(sel.ids.clone()),
    }
}

/// Name of a selection kind in operation specs' `target.kinds`.
fn spec_kind(kind: SelectionKind) -> Option<&'static str> {
    match kind {
        SelectionKind::Object => None,
        SelectionKind::Faces => Some("face_set"),
        SelectionKind::Component => Some("component"),
        SelectionKind::BoundaryLoop => Some("boundary_loop"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error("unknown operation '{0}'")]
    UnknownOperation(String),
    #[error("object {0} does not exist")]
    UnknownObject(ObjectId),
    #[error(transparent)]
    Operation(OpError),
    /// The run was cancelled; nothing was produced and nothing commits
    /// (MF-CMD-006).
    #[error("cancelled by the user")]
    Cancelled,
    #[error(
        "the document changed while the command ran (expected revision {expected}, now {actual})"
    )]
    StaleInput { expected: u64, actual: u64 },
    #[error("{op} does not support {kind} geometry")]
    UnsupportedGeometry { op: String, kind: String },
    #[error("the selection is for object {selection}, but the command targets {target}")]
    SelectionMismatch {
        selection: ObjectId,
        target: ObjectId,
    },
    #[error(
        "the selection is bound to revision {selection}, but the document is at {current}; translate it first"
    )]
    StaleSelection {
        selection: RevisionId,
        current: RevisionId,
    },
    #[error("{op} can't target {kind}")]
    UnsupportedTarget { op: String, kind: String },
    #[error("revision {0} does not exist")]
    UnknownRevision(RevisionId),
    #[error("the recorded selection can't be applied to revision {onto}: {reason}")]
    Untranslatable { onto: RevisionId, reason: String },
    #[error(
        "the command was recorded with {op} version {recorded}, but this build has version {current}"
    )]
    OperationVersion {
        op: String,
        recorded: u32,
        current: u32,
    },
    #[error("{op} has no operand named '{name}'")]
    UnknownOperand { op: String, name: String },
    #[error("{op} needs its '{name}' operand")]
    MissingOperand { op: String, name: String },
    #[error("{op} was given operand '{name}' more than once")]
    RepeatedOperand { op: String, name: String },
    #[error("object {object} can't be two operands of one command ({op})")]
    RepeatedObject { op: String, object: ObjectId },
    /// A recorded object id (a replayed import, or an object a replayed or
    /// re-executed command created) is already an object at this
    /// revision: the log doesn't fit this history.
    #[error("the log's recorded object {object} already exists in revision {revision}")]
    RecordedIdTaken {
        object: ObjectId,
        revision: RevisionId,
    },
    /// A recorded object id is the largest id there is, so no later id
    /// could follow it (a corrupt log).
    #[error("the log's recorded object {object} is out of range (a corrupt log?)")]
    RecordedIdOutOfRange { object: ObjectId },
    /// The operation returned changes its spec doesn't allow (a defect in
    /// the operation, refused rather than committed).
    #[error("{op} returned invalid object changes: {reason}")]
    InvalidChanges { op: String, reason: String },
}

impl From<mf_runtime::Cancelled> for CommandError {
    fn from(_: mf_runtime::Cancelled) -> Self {
        CommandError::Cancelled
    }
}

impl From<OpError> for CommandError {
    fn from(e: OpError) -> Self {
        match e {
            OpError::Cancelled => CommandError::Cancelled,
            other => CommandError::Operation(other),
        }
    }
}

/// Why a translation can't be used to re-execute a command.
fn untranslatable_reason(t: &Translation) -> String {
    match t {
        Translation::Mapped(_) | Translation::Partial { .. } => "it translated".into(),
        Translation::Ambiguous { candidates } => {
            format!("it is ambiguous between {candidates:?}")
        }
        Translation::Failed(e) => match e {
            TranslateError::NoPath => "the revisions aren't connected".into(),
            TranslateError::ObjectMissing => "the object doesn't exist there".into(),
            TranslateError::AllLost { lost } => {
                format!("every selected element was removed ({lost:?})")
            }
            TranslateError::LoopClosed { faces } => {
                format!("the boundary loop was closed (by {} faces)", faces.len())
            }
            TranslateError::LoopRemoved => {
                "the boundary loop was removed along with the faces around it".into()
            }
            TranslateError::KindNotTranslatable => "its kind can't be translated".into(),
            TranslateError::NotInSource { ids } => {
                format!("{ids:?} aren't elements of the selection's own revision")
            }
        },
    }
}

/// One operand of a prepared command: operand 0 is the target.
struct PreparedOperand {
    name: String,
    /// `None` for the target; the spec's role otherwise.
    role: Option<OperandRole>,
    object: ObjectId,
    mesh: Mesh,
    transform: [f64; 12],
    target: Target,
    selection: Option<Selection>,
    /// The object's intentional openings at the input revision, translated
    /// to the output by `run` if its geometry changes (on the worker: it
    /// needs the new topology).
    openings: Vec<u32>,
}

/// A validated command, ready to run on any thread.
pub struct Prepared {
    base_revision: RevisionId,
    op: &'static dyn Operation,
    /// Operand 0 is the target; the rest in the spec's `operands:` order.
    operands: Vec<PreparedOperand>,
    params: Params,
    /// The ids a recorded run of this command gave the objects it created
    /// (replay and re-execution), so the same objects get the same ids.
    created_ids: Option<Vec<ObjectId>>,
}

/// What `run` worked out for one [`ObjectChange`], so `commit` does no
/// O(n) work (MF-RT-001).
#[derive(Debug, Clone, Default)]
struct ChangeInfo {
    /// Output hashes (hex) of a geometry or created mesh.
    hashes: Option<(String, String)>,
    /// A geometry change's operand's openings, translated to the output.
    openings: Option<Vec<u32>>,
}

impl Prepared {
    pub fn op_id(&self) -> &'static str {
        self.op.id()
    }

    /// The validated, already-converted parameters this will run with
    /// (D20: a length is in document units by the time anything sees
    /// `Prepared`). Exposed so callers — and tests proving `prepare`'s
    /// conversion is idempotent — can inspect what was actually recorded
    /// without re-deriving it.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Run the operation. Safe on any thread. Hashing happens here too, so
    /// the UI thread never does O(n) work on commit (MF-RT-001).
    ///
    /// Cancelling `ctx` while this runs returns [`CommandError::Cancelled`]
    /// and produces nothing. Once this has returned `Ok`, cancelling has no
    /// effect: the result is committed or dropped by the caller, so there
    /// is exactly one outcome.
    pub fn run(self, ctx: &RunContext) -> Result<Completed, CommandError> {
        let start = Instant::now();
        let inputs: Vec<OperandInput> = self
            .operands
            .iter()
            .map(|o| OperandInput {
                object: o.object,
                mesh: &o.mesh,
                transform: o.transform,
                target: o.target.clone(),
            })
            .collect();
        let output = self.op.execute_doc(&inputs, &self.params, ctx)?;
        drop(inputs);
        #[cfg(feature = "test-position-jitter")]
        let output = crate::jitter::perturb(output);
        check_changes(self.op.id(), &self.operands, &output.changes)?;
        // Cheap: built from per-chunk hashes cached on shared chunks, so only
        // the chunks this command copied are hashed (review I6). Still
        // cancellable: the first hash of a freshly imported mesh is O(n).
        let mut recorded = Vec::with_capacity(self.operands.len());
        for o in &self.operands {
            let (content, structure) = o.mesh.hashes_with(ctx)?;
            recorded.push(RecordedOperand {
                name: o.name.clone(),
                object: o.object,
                selection: o.selection.clone(),
                input_revision_hash: content.to_hex(),
                input_structure_hash: structure.to_hex(),
                input_transform: o.transform,
            });
        }
        let mut info = Vec::with_capacity(output.changes.len());
        for change in &output.changes {
            info.push(match change {
                ObjectChange::Geometry {
                    operand,
                    mesh,
                    provenance,
                } => {
                    let (content, structure) = mesh.hashes_with(ctx)?;
                    let o = &self.operands[*operand];
                    ChangeInfo {
                        hashes: Some((content.to_hex(), structure.to_hex())),
                        openings: Some(translate_openings(o, self.base_revision, mesh, provenance)),
                    }
                }
                ObjectChange::Create { mesh, .. } => {
                    let (content, structure) = mesh.hashes_with(ctx)?;
                    ChangeInfo {
                        hashes: Some((content.to_hex(), structure.to_hex())),
                        openings: None,
                    }
                }
                ObjectChange::Transform { .. } | ObjectChange::Remove { .. } => {
                    ChangeInfo::default()
                }
            });
        }
        let target = &self.operands[0];
        Ok(Completed {
            base_revision: self.base_revision.0,
            target: target.object,
            op_id: self.op.id(),
            op_version: self.op.version(),
            selection: target.selection.clone(),
            params: self.params,
            output,
            operands: recorded,
            info,
            created_ids: self.created_ids,
            duration: start.elapsed(),
        })
    }
}

/// The intentional openings of operand `o` translated through a geometry
/// change to `output`.
fn translate_openings(
    o: &PreparedOperand,
    base: RevisionId,
    output: &Mesh,
    provenance: &Provenance,
) -> Vec<u32> {
    if o.openings.is_empty() {
        return Vec::new();
    }
    let sel = Selection {
        object: o.object,
        revision: base,
        kind: SelectionKind::BoundaryLoop,
        ids: o.openings.clone(),
    };
    // The output's revision id isn't known yet; only the ids matter.
    match mf_selection::resolve(&sel, &o.mesh, output, &[Step::Forward(provenance)], base) {
        Translation::Mapped(t) | Translation::Partial { mapped: t, .. } => t.ids,
        // Filled, removed or merged away: no longer a hole to mark.
        Translation::Failed(_) | Translation::Ambiguous { .. } => Vec::new(),
    }
}

/// How many objects `changes` create.
fn changes_creating(changes: &[ObjectChange]) -> usize {
    changes
        .iter()
        .filter(|c| matches!(c, ObjectChange::Create { .. }))
        .count()
}

/// Refuse changes an operation isn't allowed to make: an operand index out
/// of range, two changes of one kind to one object, a change to a removed
/// object, or any change to an operand the spec marks `read` or `kept`
/// (and anything but removal of a `consumed` one).
fn check_changes(
    op: &str,
    operands: &[PreparedOperand],
    changes: &[ObjectChange],
) -> Result<(), CommandError> {
    let invalid = |reason: String| CommandError::InvalidChanges {
        op: op.to_owned(),
        reason,
    };
    // Every operand face a created face names must exist: an index in
    // range and a live face of that operand's input (MF-P1-SEL-001).
    for change in changes {
        let provenance = match change {
            ObjectChange::Geometry { provenance, .. } | ObjectChange::Create { provenance, .. } => {
                provenance
            }
            ObjectChange::Transform { .. } | ObjectChange::Remove { .. } => continue,
        };
        for (_, sources) in &provenance.created_faces {
            for &(k, face) in &sources.operands {
                let o = operands.get(k as usize).ok_or_else(|| {
                    invalid(format!(
                        "a created face names operand {k}, but there are {}",
                        operands.len()
                    ))
                })?;
                if !o.mesh.is_face_live(face) {
                    return Err(invalid(format!(
                        "a created face names face {} of operand '{}', which isn't a face of it",
                        face.0, o.name
                    )));
                }
            }
        }
    }
    // Per operand: (geometry, transform, removed).
    let mut seen = vec![(false, false, false); operands.len()];
    for change in changes {
        let (operand, slot) = match change {
            ObjectChange::Geometry { operand, .. } => (*operand, 0),
            ObjectChange::Transform { operand, .. } => (*operand, 1),
            ObjectChange::Remove { operand } => (*operand, 2),
            ObjectChange::Create { .. } => continue,
        };
        let o = operands.get(operand).ok_or_else(|| {
            invalid(format!(
                "operand {operand} doesn't exist (it has {})",
                operands.len()
            ))
        })?;
        match o.role {
            None => {}
            Some(OperandRole::Consumed) if slot == 2 => {}
            Some(role) => {
                return Err(invalid(format!(
                    "operand '{}' is {role:?}, so it can't be changed{}",
                    o.name,
                    if role == OperandRole::Consumed {
                        " (only removed)"
                    } else {
                        " or removed"
                    }
                )));
            }
        }
        let s = &mut seen[operand];
        let flags = [&mut s.0, &mut s.1, &mut s.2];
        if *flags[slot] {
            return Err(invalid(format!(
                "operand '{}' has two changes of one kind",
                o.name
            )));
        }
        *flags[slot] = true;
        if s.2 && (s.0 || s.1) {
            return Err(invalid(format!(
                "operand '{}' is both changed and removed",
                o.name
            )));
        }
    }
    Ok(())
}

pub struct Completed {
    pub base_revision: u64,
    pub target: ObjectId,
    pub op_id: &'static str,
    pub op_version: u32,
    pub selection: Option<Selection>,
    pub params: Params,
    /// The operation's object changes and report.
    pub output: DocOutput,
    /// Every operand as it was read (operand 0 is the target).
    operands: Vec<RecordedOperand>,
    /// Per entry of `output.changes`.
    info: Vec<ChangeInfo>,
    /// Recorded ids for the created objects (see `Prepared::created_ids`).
    created_ids: Option<Vec<ObjectId>>,
    pub duration: Duration,
}

#[derive(Debug)]
pub enum CommitOutcome {
    /// Read-only operation: nothing was added to history.
    Report(serde_json::Value),
    /// The operation ran but changed nothing: no revision was created.
    NoChange {
        report: serde_json::Value,
    },
    NewRevision {
        report: serde_json::Value,
    },
}

/// One document state in the history DAG (MF-HIST-001).
#[derive(Debug, Clone)]
pub struct RevisionNode {
    pub id: RevisionId,
    pub parent: Option<RevisionId>,
    /// In creation order; the last one is where redo goes.
    pub children: Vec<RevisionId>,
    pub label: String,
    pub record: Option<CommandRecord>,
    pub import: Option<ImportRecord>,
    pub document: Document,
    /// How each changed object's geometry differs from the parent revision.
    pub provenance: BTreeMap<ObjectId, Provenance>,
    /// A "mark intentional openings" step: the object and the loop ids it
    /// set (logged, so replay reaches the same marks).
    pub openings: Option<(ObjectId, Vec<u32>)>,
}

/// History as a DAG of immutable document states. Undo and redo move a
/// cursor along one path; running a command after undo starts a new branch
/// and **keeps** the old one (MF-HIST-001).
pub struct Session {
    nodes: Vec<RevisionNode>,
    cursor: RevisionId,
    next_object: u64,
    next_command: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            nodes: vec![RevisionNode {
                id: RevisionId(0),
                parent: None,
                children: Vec::new(),
                label: "New document".into(),
                record: None,
                import: None,
                document: Document::default(),
                provenance: BTreeMap::new(),
                openings: None,
            }],
            cursor: RevisionId(0),
            next_object: 1,
            next_command: 1,
        }
    }

    pub fn node(&self, id: RevisionId) -> Option<&RevisionNode> {
        self.nodes.get(id.0 as usize)
    }

    pub fn revisions(&self) -> &[RevisionNode] {
        &self.nodes
    }

    pub fn current(&self) -> RevisionId {
        self.cursor
    }

    fn current_node(&self) -> &RevisionNode {
        &self.nodes[self.cursor.0 as usize]
    }

    pub fn document(&self) -> &Document {
        &self.current_node().document
    }

    /// Unique ID of the current state, as a number (for stale checks).
    pub fn revision(&self) -> u64 {
        self.cursor.0
    }

    fn push(&mut self, mut node: RevisionNode) -> RevisionId {
        let id = RevisionId(self.nodes.len() as u64);
        node.id = id;
        node.parent = Some(self.cursor);
        self.nodes[self.cursor.0 as usize].children.push(id);
        self.nodes.push(node);
        self.cursor = id;
        self.trim_caches();
        id
    }

    /// Keep derived topology only for the current revision's meshes; other
    /// revisions recompute it on demand (MF-DOC-002, review I5).
    fn trim_caches(&mut self) {
        let current: Vec<Mesh> = self
            .document()
            .objects()
            .iter()
            .filter_map(|o| o.mesh().cloned())
            .collect();
        for node in &self.nodes {
            for object in node.document.objects() {
                if let Some(mesh) = object.mesh()
                    && !current.iter().any(|c| Mesh::ptr_eq(c, mesh))
                {
                    mesh.release_topology();
                }
            }
        }
    }

    pub fn import(&mut self, imported: Imported, name: String) -> ObjectId {
        self.add_object(
            Geometry::Mesh(imported.mesh),
            name,
            Some(imported.source),
            0,
            IDENTITY_TRANSFORM,
            None,
        )
    }

    /// Import every object of a model (one revision per object, so each
    /// import can be replayed from its part of the source file). Returns the
    /// new object IDs and any warnings (the file's own, plus a unit mismatch).
    pub fn import_model(&mut self, model: ImportedModel) -> (Vec<ObjectId>, Vec<String>) {
        let mut warnings = model.warnings;
        let first_import = self.document().objects().is_empty();
        if !first_import && self.document().units() != model.unit {
            warnings.push(format!(
                "{} uses {:?} but the document uses {:?}; coordinates are not converted",
                model.source.path.display(),
                model.unit,
                self.document().units()
            ));
        }
        let ids = model
            .objects
            .into_iter()
            .enumerate()
            .map(|(part, o)| {
                let units = first_import.then_some(model.unit);
                self.add_object(
                    Geometry::Mesh(o.mesh),
                    o.name,
                    Some(model.source.clone()),
                    part,
                    o.transform,
                    units,
                )
            })
            .collect();
        (ids, warnings)
    }

    /// Import one object of a source file (used by replay).
    pub fn import_object(
        &mut self,
        object: ModelObject,
        unit: Unit,
        source: SourceInfo,
        part: usize,
    ) -> ObjectId {
        let units = self.document().objects().is_empty().then_some(unit);
        self.add_object(
            Geometry::Mesh(object.mesh),
            object.name,
            Some(source),
            part,
            object.transform,
            units,
        )
    }

    /// As [`Session::import_object`], giving the object the id the command
    /// log recorded for it (replay): ids an abandoned branch used before
    /// this import leave a gap the log keeps (review I-A). Refused if that
    /// id is already an object here or is out of range.
    pub fn import_object_as(
        &mut self,
        object: ModelObject,
        unit: Unit,
        source: SourceInfo,
        part: usize,
        id: ObjectId,
    ) -> Result<ObjectId, CommandError> {
        self.claim_recorded_id(id)?;
        let units = self.document().objects().is_empty().then_some(unit);
        Ok(self.add_object_as(
            id,
            Geometry::Mesh(object.mesh),
            object.name,
            Some(source),
            part,
            object.transform,
            units,
        ))
    }

    /// Check a recorded id can be used now (no object has it, and an id
    /// can follow it) and move the counter past it.
    fn claim_recorded_id(&mut self, id: ObjectId) -> Result<(), CommandError> {
        if self.document().object(id).is_some() {
            return Err(CommandError::RecordedIdTaken {
                object: id,
                revision: self.cursor,
            });
        }
        let after =
            id.0.checked_add(1)
                .ok_or(CommandError::RecordedIdOutOfRange { object: id })?;
        self.next_object = self.next_object.max(after);
        Ok(())
    }

    /// Add an object of any geometry kind (a history entry, like an import).
    pub fn add_geometry(
        &mut self,
        geometry: Geometry,
        name: String,
        source: Option<SourceInfo>,
    ) -> ObjectId {
        self.add_object(geometry, name, source, 0, IDENTITY_TRANSFORM, None)
    }

    fn add_object(
        &mut self,
        geometry: Geometry,
        name: String,
        source: Option<SourceInfo>,
        part: usize,
        transform: [f64; 12],
        units: Option<Unit>,
    ) -> ObjectId {
        let id = ObjectId(self.next_object);
        self.next_object += 1;
        self.add_object_as(id, geometry, name, source, part, transform, units)
    }

    /// As `add_object`, with the id already chosen.
    #[allow(clippy::too_many_arguments)]
    fn add_object_as(
        &mut self,
        id: ObjectId,
        geometry: Geometry,
        name: String,
        source: Option<SourceInfo>,
        part: usize,
        transform: [f64; 12],
        units: Option<Unit>,
    ) -> ObjectId {
        let mut document = self.document().with_object(Object {
            id,
            name: name.clone(),
            geometry,
            source: source.clone(),
            transform,
            openings: Vec::new(),
        });
        if let Some(u) = units {
            document = document.with_units(u);
        }
        self.push(RevisionNode {
            id: RevisionId(0),
            parent: None,
            children: Vec::new(),
            label: format!("Import {name}"),
            record: None,
            import: Some(ImportRecord {
                object: id,
                name,
                source,
                part,
            }),
            document,
            provenance: BTreeMap::new(),
            openings: None,
        });
        id
    }

    /// Validate a fresh invocation's raw (string) parameters and convert
    /// every length (`unit: mm`) parameter to the document's units (D20;
    /// Global Constraints: "converted at prepare time. The converted
    /// value is what's recorded"). This runs exactly once, here: replay
    /// (`replay::run_and_classify`) and [`Session::reexecute`] both carry
    /// already-converted [`Params`] forward through [`Self::prepare_validated`]
    /// directly, never back through this conversion.
    pub fn prepare(&self, inv: Invocation) -> Result<Prepared, CommandError> {
        let spec = mf_spec::builtin::get(&inv.op_id)
            .ok_or_else(|| CommandError::UnknownOperation(inv.op_id.clone()))?;
        let mut params = spec.validate(&inv.raw_params)?;
        spec.to_document_units(&mut params, self.document().units());
        self.prepare_validated(&inv.op_id, inv.target, inv.selection, inv.operands, params)
    }

    /// As [`Session::prepare`], with parameters already validated.
    fn prepare_validated(
        &self,
        op_id: &str,
        target: ObjectId,
        selection: Option<Selection>,
        operands: Vec<(String, ObjectId, Option<Selection>)>,
        params: Params,
    ) -> Result<Prepared, CommandError> {
        let spec = mf_spec::builtin::get(op_id)
            .ok_or_else(|| CommandError::UnknownOperation(op_id.to_owned()))?;
        let op = mf_operations::find(op_id)
            .ok_or_else(|| CommandError::UnknownOperation(op_id.to_owned()))?;
        let mut supplied: BTreeMap<String, (ObjectId, Option<Selection>)> = BTreeMap::new();
        for (name, object, sel) in operands {
            if !spec.operands.iter().any(|o| o.name == name) {
                return Err(CommandError::UnknownOperand {
                    op: spec.id.clone(),
                    name,
                });
            }
            if supplied.contains_key(&name) {
                return Err(CommandError::RepeatedOperand {
                    op: spec.id.clone(),
                    name,
                });
            }
            supplied.insert(name, (object, sel));
        }
        let mut prepared = vec![self.prepare_operand(
            spec,
            TARGET_OPERAND,
            None,
            target,
            selection,
            spec.target.whole_object,
            &spec.target.kinds,
        )?];
        for o in &spec.operands {
            let Some((object, sel)) = supplied.remove(&o.name) else {
                if o.required {
                    return Err(CommandError::MissingOperand {
                        op: spec.id.clone(),
                        name: o.name.clone(),
                    });
                }
                continue;
            };
            if prepared.iter().any(|p| p.object == object) {
                return Err(CommandError::RepeatedObject {
                    op: spec.id.clone(),
                    object,
                });
            }
            let whole = o.kinds.iter().any(|k| k == "object");
            prepared.push(self.prepare_operand(
                spec,
                &o.name,
                Some(o.role),
                object,
                sel,
                whole,
                &o.kinds,
            )?);
        }
        Ok(Prepared {
            base_revision: self.cursor,
            op,
            operands: prepared,
            params,
            created_ids: None,
        })
    }

    /// Check one operand (its object, geometry kind and selection) against
    /// the spec and capture it for the run.
    #[allow(clippy::too_many_arguments)]
    fn prepare_operand(
        &self,
        spec: &OperationSpec,
        name: &str,
        role: Option<OperandRole>,
        object_id: ObjectId,
        selection: Option<Selection>,
        whole_object: bool,
        kinds: &[String],
    ) -> Result<PreparedOperand, CommandError> {
        let object = self
            .document()
            .object(object_id)
            .ok_or(CommandError::UnknownObject(object_id))?;
        let kind = object.geometry.kind().as_str();
        let unsupported = || CommandError::UnsupportedGeometry {
            op: spec.id.clone(),
            kind: kind.to_owned(),
        };
        if !spec.geometry_kinds.iter().any(|k| k == kind) {
            return Err(unsupported());
        }
        // Every operation so far is a mesh operation; the spec check above
        // guarantees the object is a mesh here.
        let mesh = object.mesh().ok_or_else(unsupported)?.clone();
        let target = match &selection {
            None if whole_object => Target::WholeObject,
            None => {
                return Err(CommandError::UnsupportedTarget {
                    op: spec.id.clone(),
                    kind: "the whole object".into(),
                });
            }
            Some(sel) => {
                if sel.object != object_id {
                    return Err(CommandError::SelectionMismatch {
                        selection: sel.object,
                        target: object_id,
                    });
                }
                if sel.revision != self.cursor {
                    return Err(CommandError::StaleSelection {
                        selection: sel.revision,
                        current: self.cursor,
                    });
                }
                let allowed = match spec_kind(sel.kind) {
                    None => whole_object,
                    Some(k) => kinds.iter().any(|x| x == k),
                };
                if !allowed {
                    return Err(CommandError::UnsupportedTarget {
                        op: spec.id.clone(),
                        kind: spec_kind(sel.kind).unwrap_or("the whole object").into(),
                    });
                }
                target_of(sel)
            }
        };
        Ok(PreparedOperand {
            name: name.to_owned(),
            role,
            object: object_id,
            mesh,
            transform: object.transform,
            target,
            selection,
            openings: object.openings.clone(),
        })
    }

    /// The command whose revision last changed, placed or created `object`
    /// at or before `rev`, or `None` if its current state came from the
    /// import.
    fn producer(&self, object: ObjectId, rev: RevisionId) -> Option<CommandId> {
        let mut cur = Some(rev);
        while let Some(r) = cur {
            let node = &self.nodes[r.0 as usize];
            if node.import.as_ref().is_some_and(|i| i.object == object) {
                return None;
            }
            if let Some(rec) = &node.record {
                let produced = if rec.outputs.is_empty() {
                    rec.target == object
                } else {
                    rec.outputs.iter().any(|o| o.object == object && !o.removed)
                };
                if produced {
                    return Some(rec.id);
                }
            }
            cur = node.parent;
        }
        None
    }

    /// Commit a completed run as one new revision: every object change at
    /// once, so one undo reverts them all (D6). O(changed objects): hashes
    /// were computed by `run`, and unchanged geometry keeps its own.
    pub fn commit(&mut self, c: Completed) -> Result<CommitOutcome, CommandError> {
        if c.base_revision != self.cursor.0 {
            return Err(CommandError::StaleInput {
                expected: c.base_revision,
                actual: self.cursor.0,
            });
        }
        if c.output.changes.is_empty() {
            let read_only = mf_spec::builtin::get(c.op_id).is_some_and(|s| s.is_read_only());
            return Ok(if read_only {
                CommitOutcome::Report(c.output.report)
            } else {
                CommitOutcome::NoChange {
                    report: c.output.report,
                }
            });
        }
        let base = self.document().clone();
        let mut document = base.clone();
        let mut provenance = BTreeMap::new();
        let mut next_object = self.next_object;
        // A replayed or re-executed command gives its created objects the
        // ids it recorded, so later commands (and selections) that name
        // them still find them, whatever ids other branches took (review
        // C1). Without a record, ids come from the session's counter.
        let creates = changes_creating(&c.output.changes);
        let mut recorded_ids = match c.created_ids {
            Some(ids) if ids.len() == creates => {
                if let Some(&taken) = ids.iter().find(|&&id| base.object(id).is_some()) {
                    return Err(CommandError::RecordedIdTaken {
                        object: taken,
                        revision: self.cursor,
                    });
                }
                Some(ids.into_iter())
            }
            // A different number of created objects: the replay's verdict
            // reports the difference (the outputs won't match).
            _ => None,
        };
        let mut affected = AffectedSummary {
            removed: 0,
            created: 0,
            kept: 0,
        };
        // Per operand: (output hashes, removed); created objects separately.
        let mut operand_out: Vec<Option<(String, String)>> = vec![None; c.operands.len()];
        let mut operand_changed = vec![false; c.operands.len()];
        let mut operand_removed = vec![false; c.operands.len()];
        let mut created: Vec<ObjectOutput> = Vec::new();
        let unknown = |id: ObjectId| CommandError::UnknownObject(id);
        let DocOutput { changes, report } = c.output;
        for (change, info) in changes.into_iter().zip(c.info) {
            match change {
                ObjectChange::Geometry {
                    operand,
                    mesh,
                    provenance: p,
                } => {
                    let id = c.operands[operand].object;
                    affected.removed += p.removed_faces.len() as u64;
                    affected.created += p.created_faces.len() as u64;
                    affected.kept += (mesh.face_count() - p.created_faces.len()) as u64;
                    let openings = info
                        .openings
                        .expect("run translates openings for every geometry change");
                    document = document
                        .with_geometry(id, Geometry::Mesh(mesh))
                        .and_then(|d| d.with_openings(id, openings))
                        .ok_or(unknown(id))?;
                    provenance.insert(id, p);
                    operand_out[operand] = info.hashes;
                    operand_changed[operand] = true;
                }
                ObjectChange::Transform { operand, transform } => {
                    let id = c.operands[operand].object;
                    document = document.with_transform(id, transform).ok_or(unknown(id))?;
                    operand_changed[operand] = true;
                }
                ObjectChange::Remove { operand } => {
                    let id = c.operands[operand].object;
                    affected.removed += base
                        .object(id)
                        .and_then(|o| o.mesh())
                        .map_or(0, |m| m.face_count() as u64);
                    document = document.without_object(id).ok_or(unknown(id))?;
                    operand_changed[operand] = true;
                    operand_removed[operand] = true;
                }
                ObjectChange::Create {
                    name,
                    mesh,
                    transform,
                    provenance: p,
                } => {
                    let id = match recorded_ids.as_mut().and_then(Iterator::next) {
                        Some(id) => id,
                        None => ObjectId(next_object),
                    };
                    let after = id.0.checked_add(1).ok_or_else(|| {
                        CommandError::InvalidChanges {
                            op: c.op_id.to_owned(),
                            reason: format!(
                                "the log's recorded object {id} is out of range (a corrupt log?)"
                            ),
                        }
                    })?;
                    next_object = next_object.max(after);
                    affected.created += mesh.face_count() as u64;
                    let (hash, structure_hash) =
                        info.hashes.expect("run hashes every created mesh");
                    document = document
                        .with_new_object(Object {
                            id,
                            name,
                            geometry: Geometry::Mesh(mesh),
                            source: None,
                            transform,
                            openings: Vec::new(),
                        })
                        .ok_or(CommandError::InvalidChanges {
                            op: c.op_id.to_owned(),
                            reason: format!("object {id} already exists"),
                        })?;
                    provenance.insert(id, p);
                    created.push(ObjectOutput {
                        object: id,
                        created: true,
                        removed: false,
                        hash,
                        structure_hash,
                        transform,
                        summary: None,
                    });
                }
            }
        }
        let mut outputs = Vec::new();
        for (k, operand) in c.operands.iter().enumerate() {
            if !operand_changed[k] {
                continue;
            }
            let removed = operand_removed[k];
            let (hash, structure_hash) = match (&operand_out[k], removed) {
                (_, true) => (String::new(), String::new()),
                (Some(h), false) => h.clone(),
                // Transform only: the geometry and its hash are unchanged.
                (None, false) => (
                    operand.input_revision_hash.clone(),
                    operand.input_structure_hash.clone(),
                ),
            };
            if !removed && operand_out[k].is_none() {
                affected.kept += base
                    .object(operand.object)
                    .and_then(|o| o.mesh())
                    .map_or(0, |m| m.face_count() as u64);
            }
            let transform = document
                .object(operand.object)
                .map_or(operand.input_transform, |o| o.transform);
            outputs.push(ObjectOutput {
                object: operand.object,
                created: false,
                removed,
                hash,
                structure_hash,
                transform,
                summary: None,
            });
        }
        outputs.extend(created);
        let target = &c.operands[0];
        let (output_hash, output_structure_hash) = outputs
            .iter()
            .find(|o| o.object == target.object)
            .map_or_else(
                || {
                    (
                        target.input_revision_hash.clone(),
                        target.input_structure_hash.clone(),
                    )
                },
                |o| (o.hash.clone(), o.structure_hash.clone()),
            );
        let id = CommandId(self.next_command);
        let input_revision = self.cursor;
        let mut depends_on: Vec<CommandId> = c
            .operands
            .iter()
            .filter_map(|o| self.producer(o.object, input_revision))
            .collect();
        depends_on.sort_unstable();
        depends_on.dedup();
        let record = CommandRecord {
            id,
            op_id: c.op_id.to_owned(),
            op_version: c.op_version,
            target: c.target,
            selection: c.selection.clone(),
            params: c.params.clone(),
            input_revision,
            input_hash: target.input_revision_hash.clone(),
            input_structure_hash: target.input_structure_hash.clone(),
            output_revision: RevisionId(self.nodes.len() as u64),
            output_hash,
            output_structure_hash,
            output_summary: None,
            depends_on,
            affected_objects: outputs.iter().map(|o| o.object).collect(),
            affected,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: platform(),
            duration_ms: c.duration.as_secs_f64() * 1000.0,
            status: CommandStatus::Ok,
            operands: c.operands,
            outputs,
        };
        self.next_command += 1;
        self.next_object = next_object;
        let params: Vec<String> = c.params.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.push(RevisionNode {
            id: RevisionId(0),
            parent: None,
            children: Vec::new(),
            label: format!("{} ({})", c.op_id, params.join(", ")),
            record: Some(record),
            import: None,
            document,
            provenance,
            openings: None,
        });
        Ok(CommitOutcome::NewRevision { report })
    }

    pub fn execute(&mut self, inv: Invocation) -> Result<CommitOutcome, CommandError> {
        let completed = self.prepare(inv)?.run(&RunContext::none())?;
        self.commit(completed)
    }

    /// Run already-converted, already-validated [`Params`] (a recorded
    /// command's own `params`, as replay and [`Self::reexecute`] both
    /// have them): no unit conversion, since a recorded length is
    /// already in document units (D20) and converting it again would
    /// corrupt it. This is the one path both replay and re-execution
    /// use, so conversion happens exactly once, in [`Self::prepare`].
    pub fn execute_recorded(
        &mut self,
        op_id: &str,
        target: ObjectId,
        selection: Option<Selection>,
        params: Params,
    ) -> Result<CommitOutcome, CommandError> {
        let completed = self
            .prepare_validated(op_id, target, selection, Vec::new(), params)?
            .run(&RunContext::none())?;
        self.commit(completed)
    }

    /// As [`Self::execute_recorded`], with the command's other operands
    /// (`(name, object, selection)`, as in [`Invocation::operands`]) and
    /// the ids its recorded run gave the objects it created (empty if it
    /// created none): a command that creates as many objects as were
    /// recorded gives them those ids (review C1).
    pub fn execute_recorded_with_operands(
        &mut self,
        op_id: &str,
        target: ObjectId,
        selection: Option<Selection>,
        operands: Vec<(String, ObjectId, Option<Selection>)>,
        params: Params,
        created: Vec<ObjectId>,
    ) -> Result<CommitOutcome, CommandError> {
        let mut prepared = self.prepare_validated(op_id, target, selection, operands, params)?;
        prepared.created_ids = Some(created);
        let completed = prepared.run(&RunContext::none())?;
        self.commit(completed)
    }

    /// Run a recorded command again on another revision (MF-HIST-002): the
    /// same operation and parameters, with its selection translated to
    /// `onto` (MF-SEL-003). A translation that keeps some or all of the
    /// selection proceeds; anything else is an error naming why. On success
    /// the cursor is on the new revision (a child of `onto`); on failure it
    /// hasn't moved.
    pub fn reexecute(
        &mut self,
        record: &CommandRecord,
        onto: RevisionId,
    ) -> Result<CommitOutcome, CommandError> {
        if self.node(onto).is_none() {
            return Err(CommandError::UnknownRevision(onto));
        }
        let op = mf_operations::find(&record.op_id)
            .ok_or_else(|| CommandError::UnknownOperation(record.op_id.clone()))?;
        let mut params = record.params.clone();
        if !op.upgrade_params(record.op_version, &mut params) {
            return Err(CommandError::OperationVersion {
                op: record.op_id.clone(),
                recorded: record.op_version,
                current: op.version(),
            });
        }
        let selection = match &record.selection {
            None => None,
            Some(sel) => match self.translate(sel, onto) {
                Translation::Mapped(s) | Translation::Partial { mapped: s, .. } => Some(s),
                other => {
                    return Err(CommandError::Untranslatable {
                        onto,
                        reason: untranslatable_reason(&other),
                    });
                }
            },
        };
        // The other operands (entry 0 is the target), their selections
        // translated the same way.
        let mut operands = Vec::new();
        for o in record.operands.iter().skip(1) {
            let sel = match &o.selection {
                None => None,
                Some(sel) => match self.translate(sel, onto) {
                    Translation::Mapped(s) | Translation::Partial { mapped: s, .. } => Some(s),
                    other => {
                        return Err(CommandError::Untranslatable {
                            onto,
                            reason: untranslatable_reason(&other),
                        });
                    }
                },
            };
            operands.push((o.name.clone(), o.object, sel));
        }
        let back = self.cursor;
        self.checkout(onto);
        let created = record
            .outputs
            .iter()
            .filter(|o| o.created)
            .map(|o| o.object)
            .collect();
        let result = self.execute_recorded_with_operands(
            &record.op_id,
            record.target,
            selection,
            operands,
            params,
            created,
        );
        if result.is_err() {
            self.checkout(back);
        }
        result
    }

    /// Mark `openings` (boundary-loop ids of `object`'s current mesh) as
    /// its intentional openings, replacing any marked before: a document
    /// command that changes no geometry, undone like any other. `label`
    /// names the history step. False if the object doesn't exist.
    pub fn set_openings(&mut self, object: ObjectId, openings: Vec<u32>, label: String) -> bool {
        let Some(document) = self.document().with_openings(object, openings) else {
            return false;
        };
        let marks = document
            .object(object)
            .map(|o| o.openings.clone())
            .expect("with_openings found the object");
        self.push(RevisionNode {
            id: RevisionId(0),
            parent: None,
            children: Vec::new(),
            label,
            record: None,
            import: None,
            document,
            provenance: BTreeMap::new(),
            openings: Some((object, marks)),
        });
        true
    }

    pub fn can_undo(&self) -> bool {
        self.current_node().parent.is_some()
    }

    pub fn can_redo(&self) -> bool {
        !self.current_node().children.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        match self.current_node().parent {
            Some(p) => {
                self.cursor = p;
                self.trim_caches();
                true
            }
            None => false,
        }
    }

    /// Move to the most recently created child.
    pub fn redo(&mut self) -> bool {
        match self.current_node().children.last() {
            Some(&c) => {
                self.cursor = c;
                self.trim_caches();
                true
            }
            None => false,
        }
    }

    /// Jump to any revision, including one on an abandoned branch.
    pub fn checkout(&mut self, id: RevisionId) -> bool {
        let ok = self.node(id).is_some();
        if ok {
            self.cursor = id;
            self.trim_caches();
        }
        ok
    }

    /// Root → current path, then the redo chain (most recent children).
    pub fn linear_view(&self) -> Vec<RevisionId> {
        let mut path = Vec::new();
        let mut cur = Some(self.cursor);
        while let Some(r) = cur {
            path.push(r);
            cur = self.nodes[r.0 as usize].parent;
        }
        path.reverse();
        let mut tip = self.cursor;
        while let Some(&c) = self.nodes[tip.0 as usize].children.last() {
            path.push(c);
            tip = c;
        }
        path
    }

    pub fn history_labels(&self) -> Vec<String> {
        self.linear_view()
            .iter()
            .map(|r| self.nodes[r.0 as usize].label.clone())
            .collect()
    }

    /// Index of the current revision within `history_labels()`.
    pub fn history_cursor(&self) -> usize {
        self.linear_view()
            .iter()
            .position(|&r| r == self.cursor)
            .expect("the linear view contains the cursor")
    }

    /// Command records from the root to the current revision.
    pub fn records_to_current(&self) -> Vec<CommandRecord> {
        let mut out = Vec::new();
        let mut cur = Some(self.cursor);
        while let Some(r) = cur {
            let node = &self.nodes[r.0 as usize];
            if let Some(rec) = &node.record {
                out.push(rec.clone());
            }
            cur = node.parent;
        }
        out.reverse();
        out
    }

    /// Heap bytes held by all revisions in the DAG: mesh storage with
    /// shared chunks counted once, plus each distinct revision's cached
    /// topology (MF-DOC-002, review I5).
    pub fn history_bytes(&self) -> u64 {
        let mut seen = std::collections::HashSet::new();
        let mut meshes_seen = std::collections::HashSet::new();
        let mut total = 0;
        for node in &self.nodes {
            for object in node.document.objects() {
                let Some(mesh) = object.mesh() else { continue };
                total += mesh.unique_heap_bytes(&mut seen);
                if meshes_seen.insert(mesh.content_hash()) && mesh.has_cached_topology() {
                    total += mesh.topology().heap_bytes();
                }
            }
        }
        total
    }

    fn ancestors(&self, rev: RevisionId) -> Vec<RevisionId> {
        let mut out = Vec::new();
        let mut cur = Some(rev);
        while let Some(r) = cur {
            out.push(r);
            cur = self.nodes[r.0 as usize].parent;
        }
        out
    }

    /// Translate a selection to another revision (MF-SEL-003): up from the
    /// selection's revision to the lowest common ancestor, then down to `to`.
    pub fn translate(&self, sel: &Selection, to: RevisionId) -> Translation {
        let (Some(from_node), Some(to_node)) = (self.node(sel.revision), self.node(to)) else {
            return Translation::Failed(TranslateError::NoPath);
        };
        let from_mesh = from_node.document.object(sel.object).and_then(|o| o.mesh());
        let to_mesh = to_node.document.object(sel.object).and_then(|o| o.mesh());
        let (Some(from_mesh), Some(to_mesh)) = (from_mesh, to_mesh) else {
            return Translation::Failed(TranslateError::ObjectMissing);
        };
        let up = self.ancestors(sel.revision);
        let down = self.ancestors(to);
        let down_set: BTreeSet<RevisionId> = down.iter().copied().collect();
        let Some(lca) = up.iter().copied().find(|r| down_set.contains(r)) else {
            return Translation::Failed(TranslateError::NoPath);
        };
        let provenance = |r: RevisionId| self.nodes[r.0 as usize].provenance.get(&sel.object);
        let mut steps: Vec<Step> = Vec::new();
        for &r in up.iter().take_while(|&&r| r != lca) {
            if let Some(p) = provenance(r) {
                steps.push(Step::Backward(p));
            }
        }
        let mut forward: Vec<Step> = Vec::new();
        for &r in down.iter().take_while(|&&r| r != lca) {
            if let Some(p) = provenance(r) {
                forward.push(Step::Forward(p));
            }
        }
        forward.reverse();
        steps.extend(forward);
        mf_selection::resolve(sel, from_mesh, to_mesh, &steps, to)
    }
}
