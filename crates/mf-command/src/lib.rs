//! The single mutation path for MeshFixer documents (MF-CMD-001).
//!
//! Frontends read the [`Document`] and change it only through a [`Session`]:
//! `prepare` (validate, on the caller's thread) → `run` (any thread) →
//! `commit` (back on the owner's thread). Nothing is committed if any step
//! fails (MF-CMD-006).

#[cfg(feature = "test-position-jitter")]
mod jitter;
mod log;
mod record;
mod replay;
mod session;

pub use log::{LOG_FORMAT, LogError, READABLE_LOG_FORMATS};
pub use mf_document::RevisionId;
pub use mf_document::{Document, Geometry, GeometryKind, Object, ObjectId};
pub use record::{
    AffectedSummary, CommandId, CommandRecord, CommandStatus, GeometrySummary, ImportRecord,
    ObjectOutput, RecordedOperand,
};
pub use replay::{ReplayReport, Verdict, replay};
pub use session::{
    CommandError, CommitOutcome, Completed, Invocation, Prepared, PropertyChange, RevisionNode,
    Session, TARGET_OPERAND, target_of,
};
