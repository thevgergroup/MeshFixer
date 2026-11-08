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
    /// The command's input didn't match the log, so its output can't be
    /// compared; later commands were not attempted.
    InputMismatch {
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
            Event::Import { object, name, part } => {
                let entry = manifest
                    .sources
                    .iter()
                    .find(|s| s.object == object)
                    .ok_or(LogError::MissingSourceEntry(object))?;
                let stored = Path::new(&entry.path);
                let path = if stored.is_absolute() {
                    stored.to_path_buf()
                } else {
                    // Relative paths are stored with `/`; rebuild per OS.
                    entry
                        .path
                        .split('/')
                        .fold(dir.to_path_buf(), |p, part| p.join(part))
                };
                let mut model = mf_io::read_model(&path).map_err(|error| LogError::SourceRead {
                    path: path.clone(),
                    error,
                })?;
                let actual = model.source.content_hash.to_hex();
                if actual != entry.content_hash {
                    return Err(LogError::SourceChanged {
                        path,
                        expected: entry.content_hash.clone(),
                        actual,
                    });
                }
                if part >= model.objects.len() {
                    return Err(LogError::MissingPart {
                        path,
                        part,
                        objects: model.objects.len(),
                    });
                }
                let mut obj = model.objects.swap_remove(part);
                obj.name = name;
                let id = session.import_object(obj, model.unit, model.source, part);
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
                // Check the input before running: an output comparison is
                // meaningless if the input already differs (review I4). A
                // content difference with identical structure is expected
                // after an earlier `Equivalent` verdict (platform drift).
                let input = session
                    .document()
                    .object(record.target)
                    .and_then(|o| o.mesh())
                    .cloned();
                let drifted = results
                    .iter()
                    .any(|(_, v)| matches!(v, Verdict::Equivalent { .. }));
                let input_verdict = match &input {
                    None => Some(Verdict::Failed {
                        error: if session.document().object(record.target).is_none() {
                            format!(
                                "the log's target object {} does not exist in the replayed session",
                                record.target
                            )
                        } else {
                            format!(
                                "object {} is not a mesh in the replayed session",
                                record.target
                            )
                        },
                    }),
                    Some(m) => {
                        let content = m.content_hash().to_hex();
                        let structure_ok =
                            m.structure_hash().to_hex() == record.input_structure_hash;
                        if content == record.input_hash || (drifted && structure_ok) {
                            None
                        } else {
                            Some(Verdict::InputMismatch {
                                expected_hash: record.input_hash.clone(),
                                actual_hash: content,
                            })
                        }
                    }
                };
                let verdict = match input_verdict {
                    Some(v) => v,
                    None => run_and_classify(&mut session, &record)?,
                };
                let stop = matches!(
                    verdict,
                    Verdict::Failed { .. } | Verdict::InputMismatch { .. }
                );
                results.push((record.id, verdict));
                if stop {
                    break;
                }
            }
        }
    }
    Ok(ReplayReport { results, session })
}

/// Run one recorded command and compare its output with the record.
/// `Exact`: same content hash. `Equivalent`: same structure (slots, liveness,
/// face indices, so orientation too) with extent within `1e-9 ×` diagonal and
/// area within `1e-9` relative. Anything else is a `Mismatch`.
fn run_and_classify(
    session: &mut Session,
    record: &crate::record::CommandRecord,
) -> Result<Verdict, LogError> {
    let inv = Invocation {
        selection: record.selection.clone(),
        op_id: record.op_id.clone(),
        target: record.target,
        raw_params: record
            .params
            .0
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect(),
    };
    Ok(match session.execute(inv) {
        Ok(CommitOutcome::NewRevision { .. }) => {
            let mesh = session
                .document()
                .object(record.target)
                .and_then(|o| o.mesh())
                .ok_or(LogError::TargetMissing(record.target))?;
            let actual_hash = mesh.content_hash().to_hex();
            let mismatch = |actual_hash: String| Verdict::Mismatch {
                expected_hash: record.output_hash.clone(),
                actual_hash,
            };
            if actual_hash == record.output_hash {
                Verdict::Exact
            } else if mesh.structure_hash().to_hex() != record.output_structure_hash {
                mismatch(actual_hash)
            } else {
                match &record.output_summary {
                    None => mismatch(actual_hash),
                    Some(expected) => {
                        let tolerance = 1e-9 * expected.diagonal().max(1.0);
                        match GeometrySummary::of(mesh).deviation(expected) {
                            Some((bbox, area)) if bbox <= tolerance && area <= 1e-9 => {
                                Verdict::Equivalent {
                                    max_deviation: bbox,
                                }
                            }
                            _ => mismatch(actual_hash),
                        }
                    }
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
    })
}
