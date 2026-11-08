use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use mf_document::{Document, Geometry, Object, ObjectId, RevisionId};
use mf_geometry::{Mesh, Provenance};
use mf_io::{Imported, SourceInfo};
use mf_operations::{OpError, OpOutput, Operation, Target};
use mf_selection::{Selection, Step, TranslateError, Translation};
use mf_spec::{Params, SpecError};

use crate::record::{
    AffectedSummary, CommandId, CommandRecord, CommandStatus, GeometrySummary, ImportRecord,
    platform,
};

/// A request to run an operation, as issued by any frontend.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub op_id: String,
    pub target: ObjectId,
    pub raw_params: BTreeMap<String, String>,
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
    Operation(#[from] OpError),
    #[error(
        "the document changed while the command ran (expected revision {expected}, now {actual})"
    )]
    StaleInput { expected: u64, actual: u64 },
    #[error("{op} does not support {kind} geometry")]
    UnsupportedGeometry { op: String, kind: String },
}

/// A validated command, ready to run on any thread.
pub struct Prepared {
    base_revision: RevisionId,
    target: ObjectId,
    op: &'static dyn Operation,
    input: Mesh,
    selection: Target,
    params: Params,
}

impl Prepared {
    pub fn op_id(&self) -> &'static str {
        self.op.id()
    }

    /// Run the operation. Safe on any thread. Hashing happens here too, so
    /// the UI thread never does O(n) work on commit (MF-RT-001).
    pub fn run(self) -> Result<Completed, CommandError> {
        let start = Instant::now();
        let output = self
            .op
            .execute(&self.input, &self.selection, &self.params)?;
        let input_hash = self.input.content_hash().to_hex();
        let output_hash = output
            .change
            .as_ref()
            .map(|(m, _)| m.content_hash().to_hex());
        let output_summary = output.change.as_ref().map(|(m, _)| GeometrySummary::of(m));
        let kept = output
            .change
            .as_ref()
            .map_or(0, |(m, p)| (m.face_count() - p.created_faces.len()) as u64);
        Ok(Completed {
            base_revision: self.base_revision.0,
            target: self.target,
            op_id: self.op.id(),
            op_version: self.op.version(),
            params: self.params,
            output,
            input_hash,
            output_hash,
            output_summary,
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
    pub params: Params,
    pub output: OpOutput,
    input_hash: String,
    output_hash: Option<String>,
    output_summary: Option<GeometrySummary>,
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
        id
    }

    pub fn import(&mut self, imported: Imported, name: String) -> ObjectId {
        self.add_geometry(Geometry::Mesh(imported.mesh), name, Some(imported.source))
    }

    /// Add an object of any geometry kind (a history entry, like an import).
    pub fn add_geometry(
        &mut self,
        geometry: Geometry,
        name: String,
        source: Option<SourceInfo>,
    ) -> ObjectId {
        let id = ObjectId(self.next_object);
        self.next_object += 1;
        let document = self.document().with_object(Object {
            id,
            name: name.clone(),
            geometry,
            source: source.clone(),
        });
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
            }),
            document,
            provenance: BTreeMap::new(),
        });
        id
    }

    pub fn prepare(&self, inv: Invocation) -> Result<Prepared, CommandError> {
        let spec = mf_spec::builtin::get(&inv.op_id)
            .ok_or_else(|| CommandError::UnknownOperation(inv.op_id.clone()))?;
        let op = mf_operations::find(&inv.op_id)
            .ok_or_else(|| CommandError::UnknownOperation(inv.op_id.clone()))?;
        let params = spec.validate(&inv.raw_params)?;
        let object = self
            .document()
            .object(inv.target)
            .ok_or(CommandError::UnknownObject(inv.target))?;
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
        Ok(Prepared {
            base_revision: self.cursor,
            target: inv.target,
            op,
            input,
            selection: Target::WholeObject,
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
            selection: None,
            params: c.params.clone(),
            input_revision,
            input_hash: c.input_hash,
            output_revision: RevisionId(self.nodes.len() as u64),
            output_hash: c.output_hash.unwrap_or_default(),
            output_summary: c
                .output_summary
                .expect("a change always has an output summary"),
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
        let completed = self.prepare(inv)?.run()?;
        self.commit(completed)
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
        }
        ok
    }

    /// Root → current path, then the redo chain (most recent children).
    fn linear_view(&self) -> Vec<RevisionId> {
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
            .unwrap_or(0)
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
