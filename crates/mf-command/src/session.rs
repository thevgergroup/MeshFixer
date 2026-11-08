use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mf_geometry::Mesh;
use mf_io::Imported;
use mf_operations::{OpError, OpOutput, Operation};
use mf_spec::{Params, SpecError};

use mf_document::{Document, Geometry, Object, ObjectId};
use mf_io::SourceInfo;

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
    base_revision: u64,
    target: ObjectId,
    op: &'static dyn Operation,
    input: Mesh,
    params: Params,
}

impl Prepared {
    pub fn op_id(&self) -> &'static str {
        self.op.id()
    }

    pub fn run(self) -> Result<Completed, CommandError> {
        let start = Instant::now();
        let output = self.op.execute(&self.input, &self.params)?;
        Ok(Completed {
            base_revision: self.base_revision,
            target: self.target,
            op_id: self.op.id(),
            params: self.params,
            output,
            duration: start.elapsed(),
        })
    }
}

pub struct Completed {
    pub base_revision: u64,
    pub target: ObjectId,
    pub op_id: &'static str,
    pub params: Params,
    pub output: OpOutput,
    pub duration: Duration,
}

#[derive(Debug)]
pub enum CommitOutcome {
    /// Read-only operation: nothing was added to history.
    Report(serde_json::Value),
    NewRevision {
        report: serde_json::Value,
    },
}

struct Entry {
    revision: u64,
    label: String,
    document: Document,
}

/// Linear undo/redo over immutable document states (MF-CMD-003).
///
/// Temporary for M1: M2 replaces the stack with a revision DAG that retains
/// abandoned branches (MF-HIST-001).
pub struct Session {
    entries: Vec<Entry>,
    cursor: usize,
    next_revision: u64,
    next_object: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            entries: vec![Entry {
                revision: 0,
                label: "New document".into(),
                document: Document::default(),
            }],
            cursor: 0,
            next_revision: 1,
            next_object: 1,
        }
    }

    pub fn document(&self) -> &Document {
        &self.entries[self.cursor].document
    }

    /// Unique ID of the current state. Never repeats, even across undo + new command.
    pub fn revision(&self) -> u64 {
        self.entries[self.cursor].revision
    }

    fn push(&mut self, label: String, document: Document) {
        self.entries.truncate(self.cursor + 1);
        self.entries.push(Entry {
            revision: self.next_revision,
            label,
            document,
        });
        self.next_revision += 1;
        self.cursor = self.entries.len() - 1;
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
        let doc = self.document().with_object(Object {
            id,
            name: name.clone(),
            geometry,
            source,
        });
        self.push(format!("Import {name}"), doc);
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
        if !spec.geometry_kinds.iter().any(|k| k == kind) {
            return Err(CommandError::UnsupportedGeometry {
                op: spec.id.clone(),
                kind: kind.to_owned(),
            });
        }
        // Every Phase 0 operation is a mesh operation; the spec check above
        // guarantees the object is a mesh here.
        let input = object
            .mesh()
            .ok_or_else(|| CommandError::UnsupportedGeometry {
                op: spec.id.clone(),
                kind: kind.to_owned(),
            })?
            .clone();
        Ok(Prepared {
            base_revision: self.revision(),
            target: inv.target,
            op,
            input,
            params,
        })
    }

    pub fn commit(&mut self, c: Completed) -> Result<CommitOutcome, CommandError> {
        if c.base_revision != self.revision() {
            return Err(CommandError::StaleInput {
                expected: c.base_revision,
                actual: self.revision(),
            });
        }
        let Some(mesh) = c.output.mesh else {
            return Ok(CommitOutcome::Report(c.output.report));
        };
        let doc = self
            .document()
            .with_geometry(c.target, Geometry::Mesh(mesh))
            .ok_or(CommandError::UnknownObject(c.target))?;
        let params: Vec<String> = c.params.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
        self.push(format!("{} ({})", c.op_id, params.join(", ")), doc);
        Ok(CommitOutcome::NewRevision {
            report: c.output.report,
        })
    }

    pub fn execute(&mut self, inv: Invocation) -> Result<CommitOutcome, CommandError> {
        let completed = self.prepare(inv)?.run()?;
        self.commit(completed)
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn undo(&mut self) -> bool {
        let ok = self.can_undo();
        if ok {
            self.cursor -= 1;
        }
        ok
    }

    pub fn redo(&mut self) -> bool {
        let ok = self.can_redo();
        if ok {
            self.cursor += 1;
        }
        ok
    }

    pub fn history_labels(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.label.clone()).collect()
    }

    /// Index of the current entry within `history_labels()`.
    pub fn history_cursor(&self) -> usize {
        self.cursor
    }
}
