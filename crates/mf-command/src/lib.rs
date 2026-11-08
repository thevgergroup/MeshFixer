//! The single mutation path for MeshFixer documents (MF-CMD-001).
//!
//! Frontends read the [`Document`] and change it only through a [`Session`]:
//! `prepare` (validate, on the caller's thread) → `run` (any thread) →
//! `commit` (back on the owner's thread). Nothing is committed if any step
//! fails (MF-CMD-006).

mod document;
mod session;

pub use document::{Document, Object, ObjectId};
pub use session::{CommandError, CommitOutcome, Completed, Invocation, Prepared, Session};
