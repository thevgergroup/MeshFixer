use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use mf_document::{Document, Geometry, IDENTITY_TRANSFORM, Object, ObjectId, RevisionId, Unit};
use mf_geometry::{Mesh, Provenance};
use mf_io::threemf::ModelObject;
use mf_io::{Imported, ImportedModel, SourceInfo};
use mf_operations::{OpError, OpOutput, Operation, Target};
use mf_runtime::RunContext;
use mf_selection::{Selection, SelectionKind, Step, TranslateError, Translation};
use mf_spec::{Params, SpecError};

use crate::record::{
    AffectedSummary, CommandId, CommandRecord, CommandStatus, ImportRecord, platform,
};

/// A request to run an operation, as issued by any frontend.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub op_id: String,
    pub target: ObjectId,
    /// What part of the object to act on; `None` is the whole object. Must be
    /// bound to the current revision (translate it first).
    pub selection: Option<Selection>,
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
            TranslateError::KindNotTranslatable => "its kind can't be translated".into(),
            TranslateError::NotInSource { ids } => {
                format!("{ids:?} aren't elements of the selection's own revision")
            }
        },
    }
}

/// A validated command, ready to run on any thread.
pub struct Prepared {
    base_revision: RevisionId,
    target: ObjectId,
    op: &'static dyn Operation,
    input: Mesh,
    target_spec: Target,
    selection: Option<Selection>,
    params: Params,
}

impl Prepared {
    pub fn op_id(&self) -> &'static str {
        self.op.id()
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
        let output = self
            .op
            .execute(&self.input, &self.target_spec, &self.params, ctx)?;
        // Cheap: built from per-chunk hashes cached on shared chunks, so only
        // the chunks this command copied are hashed (review I6). Still
        // cancellable: the first hash of a freshly imported mesh is O(n).
        let (input_hash, input_structure_hash) = self.input.hashes_with(ctx)?;
        let (input_hash, input_structure_hash) =
            (input_hash.to_hex(), input_structure_hash.to_hex());
        let output_hashes = match &output.change {
            Some((m, _)) => {
                let (content, structure) = m.hashes_with(ctx)?;
                Some((content.to_hex(), structure.to_hex()))
            }
            None => None,
        };
        let kept = output
            .change
            .as_ref()
            .map_or(0, |(m, p)| (m.face_count() - p.created_faces.len()) as u64);
        Ok(Completed {
            base_revision: self.base_revision.0,
            target: self.target,
            op_id: self.op.id(),
            op_version: self.op.version(),
            selection: self.selection,
            params: self.params,
            output,
            input_hash,
            input_structure_hash,
            output_hashes,
            kept,
            duration: start.elapsed(),
        })
    }
}

pub struct Completed {
    pub base_revision: u64,
    pub target: ObjectId,
    pub op_id: &'static str,
    pub op_version: u32,
    pub selection: Option<Selection>,
    pub params: Params,
    pub output: OpOutput,
    input_hash: String,
    input_structure_hash: String,
    output_hashes: Option<(String, String)>,
    kept: u64,
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
        let mut document = self.document().with_object(Object {
            id,
            name: name.clone(),
            geometry,
            source: source.clone(),
            transform,
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
        });
        id
    }

    pub fn prepare(&self, inv: Invocation) -> Result<Prepared, CommandError> {
        let spec = mf_spec::builtin::get(&inv.op_id)
            .ok_or_else(|| CommandError::UnknownOperation(inv.op_id.clone()))?;
        let params = spec.validate(&inv.raw_params)?;
        self.prepare_validated(&inv.op_id, inv.target, inv.selection, params)
    }

    /// As [`Session::prepare`], with parameters already validated.
    fn prepare_validated(
        &self,
        op_id: &str,
        target: ObjectId,
        selection: Option<Selection>,
        params: Params,
    ) -> Result<Prepared, CommandError> {
        let spec = mf_spec::builtin::get(op_id)
            .ok_or_else(|| CommandError::UnknownOperation(op_id.to_owned()))?;
        let op = mf_operations::find(op_id)
            .ok_or_else(|| CommandError::UnknownOperation(op_id.to_owned()))?;
        let object = self
            .document()
            .object(target)
            .ok_or(CommandError::UnknownObject(target))?;
        let kind = object.geometry.kind().as_str();
        let unsupported = || CommandError::UnsupportedGeometry {
            op: spec.id.clone(),
            kind: kind.to_owned(),
        };
        if !spec.geometry_kinds.iter().any(|k| k == kind) {
            return Err(unsupported());
        }
        // Every Phase 0 operation is a mesh operation; the spec check above
        // guarantees the object is a mesh here.
        let input = object.mesh().ok_or_else(unsupported)?.clone();
        let target_spec = match &selection {
            None if spec.target.whole_object => Target::WholeObject,
            None => {
                return Err(CommandError::UnsupportedTarget {
                    op: spec.id.clone(),
                    kind: "the whole object".into(),
                });
            }
            Some(sel) => {
                if sel.object != target {
                    return Err(CommandError::SelectionMismatch {
                        selection: sel.object,
                        target,
                    });
                }
                if sel.revision != self.cursor {
                    return Err(CommandError::StaleSelection {
                        selection: sel.revision,
                        current: self.cursor,
                    });
                }
                let allowed = match spec_kind(sel.kind) {
                    None => spec.target.whole_object,
                    Some(k) => spec.target.kinds.iter().any(|x| x == k),
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
        Ok(Prepared {
            base_revision: self.cursor,
            target,
            op,
            input,
            target_spec,
            selection,
            params,
        })
    }

    /// The command whose revision last changed `object`'s geometry at or
    /// before `rev`, or `None` if its current geometry came from the import.
    fn producer(&self, object: ObjectId, rev: RevisionId) -> Option<CommandId> {
        let mut cur = Some(rev);
        while let Some(r) = cur {
            let node = &self.nodes[r.0 as usize];
            if node.import.as_ref().is_some_and(|i| i.object == object) {
                return None;
            }
            if let Some(rec) = &node.record
                && rec.target == object
            {
                return Some(rec.id);
            }
            cur = node.parent;
        }
        None
    }

    pub fn commit(&mut self, c: Completed) -> Result<CommitOutcome, CommandError> {
        if c.base_revision != self.cursor.0 {
            return Err(CommandError::StaleInput {
                expected: c.base_revision,
                actual: self.cursor.0,
            });
        }
        let Some((mesh, provenance)) = c.output.change else {
            let read_only = mf_spec::builtin::get(c.op_id).is_some_and(|s| s.is_read_only());
            return Ok(if read_only {
                CommitOutcome::Report(c.output.report)
            } else {
                CommitOutcome::NoChange {
                    report: c.output.report,
                }
            });
        };
        let (output_hash, output_structure_hash) = c
            .output_hashes
            .clone()
            .expect("a change always has output hashes");
        let document = self
            .document()
            .with_geometry(c.target, Geometry::Mesh(mesh))
            .ok_or(CommandError::UnknownObject(c.target))?;
        let id = CommandId(self.next_command);
        self.next_command += 1;
        let input_revision = self.cursor;
        let record = CommandRecord {
            id,
            op_id: c.op_id.to_owned(),
            op_version: c.op_version,
            target: c.target,
            selection: c.selection.clone(),
            params: c.params.clone(),
            input_revision,
            input_hash: c.input_hash,
            input_structure_hash: c.input_structure_hash,
            output_revision: RevisionId(self.nodes.len() as u64),
            output_hash,
            output_structure_hash,
            output_summary: None,
            depends_on: self
                .producer(c.target, input_revision)
                .into_iter()
                .collect(),
            affected_objects: vec![c.target],
            affected: AffectedSummary {
                removed: provenance.removed_faces.len() as u64,
                created: provenance.created_faces.len() as u64,
                kept: c.kept,
            },
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: platform(),
            duration_ms: c.duration.as_secs_f64() * 1000.0,
            status: CommandStatus::Ok,
        };
        let params: Vec<String> = c.params.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.push(RevisionNode {
            id: RevisionId(0),
            parent: None,
            children: Vec::new(),
            label: format!("{} ({})", c.op_id, params.join(", ")),
            record: Some(record),
            import: None,
            document,
            provenance: BTreeMap::from([(c.target, provenance)]),
        });
        Ok(CommitOutcome::NewRevision {
            report: c.output.report,
        })
    }

    pub fn execute(&mut self, inv: Invocation) -> Result<CommitOutcome, CommandError> {
        let completed = self.prepare(inv)?.run(&RunContext::none())?;
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
        let back = self.cursor;
        self.checkout(onto);
        let result = self
            .prepare_validated(&record.op_id, record.target, selection, params)
            .and_then(|p| p.run(&RunContext::none()))
            .and_then(|c| self.commit(c));
        if result.is_err() {
            self.checkout(back);
        }
        result
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
