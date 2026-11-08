//! Command records (MF-CMD-002) and the information needed to re-execute
//! a command against a different input revision later (MF-HIST-002).

use mf_document::{ObjectId, RevisionId};
use mf_io::SourceInfo;
use mf_selection::Selection;
use mf_spec::Params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedSummary {
    pub removed: u64,
    pub created: u64,
    pub kept: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Ok,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: CommandId,
    pub op_id: String,
    pub op_version: u32,
    pub target: ObjectId,
    /// `None` means the whole object.
    pub selection: Option<Selection>,
    pub params: Params,
    pub input_revision: RevisionId,
    /// Content hash (hex) of the target's geometry before the command.
    pub input_hash: String,
    pub output_revision: RevisionId,
    pub output_hash: String,
    /// Commands that produced this command's inputs (empty if an import did).
    pub depends_on: Vec<CommandId>,
    pub affected_objects: Vec<ObjectId>,
    pub affected: AffectedSummary,
    pub engine_version: String,
    /// `<arch>-<os>`.
    pub platform: String,
    pub duration_ms: f64,
    pub status: CommandStatus,
}

/// How an object entered the session.
#[derive(Debug, Clone, Serialize)]
pub struct ImportRecord {
    pub object: ObjectId,
    pub name: String,
    pub source: Option<SourceInfo>,
}

pub fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}
