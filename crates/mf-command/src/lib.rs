//! The single mutation path for MeshFixer documents (MF-CMD-001).
//!
//! Frontends read the [`Document`] and change it only through a [`Session`]:
//! `prepare` (validate, on the caller's thread) → `run` (any thread) →
//! `commit` (back on the owner's thread). Nothing is committed if any step
//! fails (MF-CMD-006).

mod record;
mod session;

pub use mf_document::RevisionId;
pub use mf_document::{Document, Geometry, GeometryKind, Object, ObjectId};
pub use record::{AffectedSummary, CommandId, CommandRecord, CommandStatus, ImportRecord};
pub use session::{
    CommandError, CommitOutcome, Completed, Invocation, Prepared, RevisionNode, Session,
};
