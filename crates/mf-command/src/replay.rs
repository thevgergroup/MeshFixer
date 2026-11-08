//! Headless replay of a command log with per-command verification
//! (MF-CMD-004, MF-CMD-005).

use std::path::Path;

use crate::log::{Event, LOG_FORMAT, LogError, Manifest};
use crate::record::{CommandId, GeometrySummary};
use crate::session::{CommitOutcome, Invocation, Session};

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Same output hash: byte-identical geometry.
    Exact,
    /// Different bits, same invariants, extent and area within `1e-9 ×`
    /// the bounding-box diagonal (expected across platforms).
    Equivalent { max_deviation: f64 },
    Mismatch {
        expected_hash: String,
        actual_hash: String,
    },
    /// The command could not run; later commands were not attempted.
    Failed { error: String },
}

impl Verdict {
    pub fn is_verified(&self) -> bool {
        matches!(self, Verdict::Exact | Verdict::Equivalent { .. })
    }
}

pub struct ReplayReport {
    pub results: Vec<(CommandId, Verdict)>,
    pub session: Session,
}

impl ReplayReport {
    pub fn all_verified(&self) -> bool {
        self.results.iter().all(|(_, v)| v.is_verified())
    }
}

/// Re-import the sources (checking their hashes) and re-run every command.
/// A changed or missing source is a hard error, never skipped.
pub fn replay(dir: &Path) -> Result<ReplayReport, LogError> {
    let manifest_text = std::fs::read_to_string(dir.join("manifest.json"))?;
    let manifest: Manifest = serde_json::from_str(&manifest_text).map_err(|e| LogError::Json {
        file: "manifest.json".into(),
        line: e.line(),
        message: e.to_string(),
    })?;
    if manifest.format != LOG_FORMAT {
        return Err(LogError::UnsupportedFormat(manifest.format));
    }
    let commands = std::fs::read_to_string(dir.join("commands.jsonl"))?;
    let mut session = Session::new();
    let mut results = Vec::new();
    for (i, line) in commands
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let event: Event = serde_json::from_str(line).map_err(|e| LogError::Json {
            file: "commands.jsonl".into(),
            line: i + 1,
            message: e.to_string(),
        })?;
        match event {
            Event::Import { object, name } => {
                let entry = manifest
                    .sources
                    .iter()
                    .find(|s| s.object == object)
                    .ok_or(LogError::MissingSourceEntry(object))?;
                let path = if entry.path.is_absolute() {
                    entry.path.clone()
                } else {
                    dir.join(&entry.path)
                };
                let imported = mf_io::read_path(&path).map_err(|error| LogError::SourceRead {
                    path: path.clone(),
                    error,
                })?;
                let actual = imported.source.content_hash.to_hex();
                if actual != entry.content_hash {
                    return Err(LogError::SourceChanged {
                        path,
                        expected: entry.content_hash.clone(),
                        actual,
                    });
                }
                let id = session.import(imported, name);
                if id != object {
                    return Err(LogError::ObjectIdMismatch {
                        expected: object,
                        actual: id,
                    });
                }
            }
            Event::Command(record) => {
                let available = mf_operations::find(&record.op_id).map(|o| o.version());
                if let Some(v) = available
                    && v != record.op_version
                {
                    return Err(LogError::VersionMismatch {
                        op: record.op_id,
                        recorded: record.op_version,
                        available: v,
                    });
                }
                let inv = Invocation {
                    op_id: record.op_id.clone(),
                    target: record.target,
                    raw_params: record
                        .params
                        .0
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_string()))
                        .collect(),
                };
                let verdict = match session.execute(inv) {
                    Ok(CommitOutcome::NewRevision { .. }) => {
                        let mesh = session
                            .document()
                            .object(record.target)
                            .and_then(|o| o.mesh())
                            .ok_or(LogError::MissingSourceEntry(record.target))?;
                        let actual_hash = mesh.content_hash().to_hex();
                        if actual_hash == record.output_hash {
                            Verdict::Exact
                        } else {
                            let tolerance = 1e-9 * record.output_summary.diagonal().max(1.0);
                            match GeometrySummary::of(mesh).deviation(&record.output_summary) {
                                Some(d) if d <= tolerance => {
                                    Verdict::Equivalent { max_deviation: d }
                                }
                                _ => Verdict::Mismatch {
                                    expected_hash: record.output_hash.clone(),
                                    actual_hash,
                                },
                            }
                        }
                    }
                    Ok(_) => Verdict::Mismatch {
                        expected_hash: record.output_hash.clone(),
                        actual_hash: "no change".into(),
                    },
                    Err(e) => Verdict::Failed {
                        error: e.to_string(),
                    },
                };
                let stop = matches!(verdict, Verdict::Failed { .. });
                results.push((record.id, verdict));
                if stop {
                    break;
                }
            }
        }
    }
    Ok(ReplayReport { results, session })
}
