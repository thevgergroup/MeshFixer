//! Headless replay of a command log with per-command verification
//! (MF-CMD-004, MF-CMD-005).

use std::path::Path;

use crate::log::{Event, LogError, Manifest, READABLE_LOG_FORMATS};
use crate::record::{CommandId, CommandRecord, GeometrySummary, ObjectOutput};
use crate::session::{CommitOutcome, Session};

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
    if !READABLE_LOG_FORMATS.contains(&manifest.format.as_str()) {
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
            Event::SetOpenings { object, ids, label } => {
                if !session.set_openings(object, ids, label) {
                    return Err(LogError::Command(
                        crate::session::CommandError::UnknownObject(object),
                    ));
                }
            }
            Event::Command(mut record) => {
                // An earlier version's record replays with the parameters
                // that make this version do what it did (or not at all).
                if let Some(op) = mf_operations::find(&record.op_id)
                    && !op.upgrade_params(record.op_version, &mut record.params)
                {
                    return Err(LogError::VersionMismatch {
                        op: record.op_id,
                        recorded: record.op_version,
                        available: op.version(),
                    });
                }
                // Check the input before running: an output comparison is
                // meaningless if the input already differs (review I4). A
                // content difference with identical structure is expected
                // after an earlier `Equivalent` verdict (platform drift).
                let drifted = results
                    .iter()
                    .any(|(_, v)| matches!(v, Verdict::Equivalent { .. }));
                let input_verdict = check_inputs(&session, &record, drifted);
                let verdict = match input_verdict {
                    Some(v) => v,
                    None => run_and_classify(&mut session, &record, drifted)?,
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

/// Every operand the record read, as `(object, content hash, structure
/// hash, transform)`. A record from before Phase 1 (no `operands`) read
/// only its target, at whatever transform (it didn't record one). The
/// target's object and hashes are always the record's own `target`,
/// `input_hash` and `input_structure_hash` (MF-CMD-004's fields, which
/// operand 0 repeats).
fn recorded_inputs(
    record: &CommandRecord,
) -> Vec<(mf_document::ObjectId, &str, &str, Option<[f64; 12]>)> {
    if record.operands.is_empty() {
        return vec![(
            record.target,
            record.input_hash.as_str(),
            record.input_structure_hash.as_str(),
            None,
        )];
    }
    record
        .operands
        .iter()
        .enumerate()
        .map(|(k, o)| {
            if k == 0 {
                (
                    record.target,
                    record.input_hash.as_str(),
                    record.input_structure_hash.as_str(),
                    Some(o.input_transform),
                )
            } else {
                (
                    o.object,
                    o.input_revision_hash.as_str(),
                    o.input_structure_hash.as_str(),
                    Some(o.input_transform),
                )
            }
        })
        .collect()
}

/// `None` if every operand the record read is in the replayed session as
/// it was recorded; otherwise why the command can't be compared.
fn check_inputs(session: &Session, record: &CommandRecord, drifted: bool) -> Option<Verdict> {
    for (object, hash, structure, transform) in recorded_inputs(record) {
        let Some(o) = session.document().object(object) else {
            return Some(Verdict::Failed {
                error: format!(
                    "the log's {} object {object} does not exist in the replayed session",
                    if object == record.target {
                        "target"
                    } else {
                        "operand"
                    }
                ),
            });
        };
        let Some(m) = o.mesh() else {
            return Some(Verdict::Failed {
                error: format!("object {object} is not a mesh in the replayed session"),
            });
        };
        let content = m.content_hash().to_hex();
        let structure_ok = m.structure_hash().to_hex() == structure;
        if !(content == hash || (drifted && structure_ok)) {
            return Some(Verdict::InputMismatch {
                expected_hash: hash.to_owned(),
                actual_hash: content,
            });
        }
        if let Some(t) = transform
            && !compare_transforms(&t, &o.transform, drifted, || mesh_diagonal(m)).is_verified()
        {
            return Some(Verdict::InputMismatch {
                expected_hash: format!("{hash} at transform {t:?}"),
                actual_hash: format!("{content} at transform {:?}", o.transform),
            });
        }
    }
    None
}

/// The bounding-box diagonal of `mesh`'s own coordinates (0 when empty).
fn mesh_diagonal(mesh: &mf_geometry::Mesh) -> f64 {
    mesh.bounding_box().map_or(0.0, |b| b.diagonal())
}

/// Compare a recorded transform with a replayed one (D7). Bit-equal is
/// `Exact`. After earlier drift (an `Equivalent` verdict upstream), a
/// transform computed from drifted geometry (drop to plate, say) may
/// differ in its last bits: it is `Equivalent` when every linear entry is
/// within `1e-9` and every translation within `1e-9 ×` the object's
/// bounding-box diagonal (at least 1), the tolerance geometry gets (review
/// I2). Anything else is a `Mismatch`.
fn compare_transforms(
    expected: &[f64; 12],
    actual: &[f64; 12],
    drifted: bool,
    diagonal: impl FnOnce() -> f64,
) -> Verdict {
    if expected.map(f64::to_bits) == actual.map(f64::to_bits) {
        return Verdict::Exact;
    }
    let mismatch = Verdict::Mismatch {
        expected_hash: format!("transform {expected:?}"),
        actual_hash: format!("transform {actual:?}"),
    };
    if !drifted {
        return mismatch;
    }
    let diff = |k: usize| (expected[k] - actual[k]).abs();
    let linear = (0..9).map(diff).fold(0.0, f64::max);
    let translation = (9..12).map(diff).fold(0.0, f64::max);
    let tolerance = 1e-9 * diagonal().max(1.0);
    if linear <= 1e-9 && translation <= tolerance {
        Verdict::Equivalent {
            max_deviation: translation.max(linear),
        }
    } else {
        mismatch
    }
}

/// How bad a verdict is, for "the worst verdict among its objects".
fn severity(v: &Verdict) -> u8 {
    match v {
        Verdict::Exact => 0,
        Verdict::Equivalent { .. } => 1,
        Verdict::Mismatch { .. } => 2,
        Verdict::InputMismatch { .. } => 3,
        Verdict::Failed { .. } => 4,
    }
}

/// The worse of two verdicts (two `Equivalent`s keep the larger deviation).
fn worst(a: Verdict, b: Verdict) -> Verdict {
    match (&a, &b) {
        (Verdict::Equivalent { max_deviation: x }, Verdict::Equivalent { max_deviation: y }) => {
            Verdict::Equivalent {
                max_deviation: x.max(*y),
            }
        }
        _ if severity(&b) > severity(&a) => b,
        _ => a,
    }
}

/// Compare a replayed mesh with a recorded output. `Exact`: same content
/// hash. `Equivalent`: same structure (slots, liveness, face indices, so
/// orientation too) with extent within `1e-9 ×` diagonal and area within
/// `1e-9` relative. Anything else is a `Mismatch`.
fn classify_mesh(
    mesh: &mf_geometry::Mesh,
    hash: &str,
    structure_hash: &str,
    summary: Option<&GeometrySummary>,
) -> Verdict {
    let actual_hash = mesh.content_hash().to_hex();
    let mismatch = |actual_hash: String| Verdict::Mismatch {
        expected_hash: hash.to_owned(),
        actual_hash,
    };
    if actual_hash == hash {
        return Verdict::Exact;
    }
    if mesh.structure_hash().to_hex() != structure_hash {
        return mismatch(actual_hash);
    }
    match summary {
        None => mismatch(actual_hash),
        Some(expected) => {
            let tolerance = 1e-9 * expected.diagonal().max(1.0);
            match GeometrySummary::of(mesh).deviation(expected) {
                Some((bbox, area)) if bbox <= tolerance && area <= 1e-9 => Verdict::Equivalent {
                    max_deviation: bbox,
                },
                _ => mismatch(actual_hash),
            }
        }
    }
}

/// One recorded object output against the replayed document. A removed
/// object must be gone; any other must have matching geometry and exactly
/// the recorded transform (bit for bit: D7, replay runs no trigonometry).
fn classify_output(
    session: &Session,
    record: &CommandRecord,
    out: &ObjectOutput,
    drifted: bool,
) -> Verdict {
    let object = session.document().object(out.object);
    if out.removed {
        return match object {
            None => Verdict::Exact,
            Some(o) => Verdict::Mismatch {
                expected_hash: format!("object {} removed", out.object),
                actual_hash: o.geometry.content_hash().to_hex(),
            },
        };
    }
    let Some(o) = object else {
        return Verdict::Mismatch {
            expected_hash: out.hash.clone(),
            actual_hash: format!("object {} missing", out.object),
        };
    };
    let Some(mesh) = o.mesh() else {
        return Verdict::Mismatch {
            expected_hash: out.hash.clone(),
            actual_hash: format!("object {} is not a mesh", out.object),
        };
    };
    // The target's output is the record's own `output_hash`,
    // `output_structure_hash` and `output_summary` (MF-CMD-004's fields,
    // which its entry here repeats); other objects carry theirs.
    let (hash, structure_hash, summary) = if out.object == record.target {
        (
            record.output_hash.as_str(),
            record.output_structure_hash.as_str(),
            record.output_summary.as_ref().or(out.summary.as_ref()),
        )
    } else {
        (
            out.hash.as_str(),
            out.structure_hash.as_str(),
            out.summary.as_ref(),
        )
    };
    let geometry = classify_mesh(mesh, hash, structure_hash, summary);
    let placement = compare_transforms(&out.transform, &o.transform, drifted, || {
        summary.map_or_else(|| mesh_diagonal(mesh), GeometrySummary::diagonal)
    });
    worst(geometry, placement)
}

/// The objects a command changed, created or removed, as
/// `(object, created, removed)`, sorted: a record's own outputs, or for a
/// record from before Phase 1, just its target.
fn changed_set(record: &CommandRecord) -> Vec<(mf_document::ObjectId, bool, bool)> {
    let mut set: Vec<_> = if record.outputs.is_empty() {
        vec![(record.target, false, false)]
    } else {
        record
            .outputs
            .iter()
            .map(|o| (o.object, o.created, o.removed))
            .collect()
    };
    set.sort();
    set
}

/// Run one recorded command and compare its output with the record: each
/// recorded object output (D6), or, for a record from before Phase 1, the
/// target's geometry. The command's verdict is the worst among its
/// objects.
fn run_and_classify(
    session: &mut Session,
    record: &CommandRecord,
    drifted: bool,
) -> Result<Verdict, LogError> {
    // The inputs matched (checked by the caller), so the recorded ids name
    // the same elements here; only the revision number can differ (replay
    // numbers revisions afresh: marks, undo and branches all shift it).
    let rebind = |sel: &Option<mf_selection::Selection>, rev| {
        sel.clone().map(|mut sel| {
            sel.revision = rev;
            sel
        })
    };
    let current = session.current();
    let selection = rebind(&record.selection, current);
    let operands = record
        .operands
        .iter()
        .skip(1)
        .map(|o| (o.name.clone(), o.object, rebind(&o.selection, current)))
        .collect();
    // `execute_recorded_with_operands`, not `execute`/`Invocation`:
    // `record.params` are already in document units (converted once, at
    // the original `prepare`, D20). Round-tripping them through
    // `Invocation`'s raw string params would re-validate through
    // `Session::prepare`, which converts again — corrupting a replayed
    // length, including when replaying onto a document in another unit.
    // Created objects get the ids the record gave them (review C1).
    let created = record
        .outputs
        .iter()
        .filter(|o| o.created)
        .map(|o| o.object)
        .collect();
    let outcome = session.execute_recorded_with_operands(
        &record.op_id,
        record.target,
        selection,
        operands,
        record.params.clone(),
        created,
    );
    // The replayed command must change exactly the objects the record
    // says: an extra (or missing) change is a `Mismatch`, whatever the
    // recorded objects look like (review I1).
    if let Ok(CommitOutcome::NewRevision { .. }) = &outcome {
        let replayed = session
            .node(session.current())
            .and_then(|n| n.record.as_ref())
            .map(changed_set)
            .unwrap_or_default(); // allow-swallow: a NewRevision always carries its record; an empty set then mismatches by name below
        let expected = changed_set(record);
        if replayed != expected {
            return Ok(Verdict::Mismatch {
                expected_hash: format!("changes to {expected:?} (object, created, removed)"),
                actual_hash: format!("changes to {replayed:?}"),
            });
        }
    }
    Ok(match outcome {
        Ok(CommitOutcome::NewRevision { .. }) if !record.outputs.is_empty() => record
            .outputs
            .iter()
            .map(|out| classify_output(session, record, out, drifted))
            .fold(Verdict::Exact, worst),
        Ok(CommitOutcome::NewRevision { .. }) => {
            let mesh = session
                .document()
                .object(record.target)
                .and_then(|o| o.mesh())
                .ok_or(LogError::TargetMissing(record.target))?;
            classify_mesh(
                mesh,
                &record.output_hash,
                &record.output_structure_hash,
                record.output_summary.as_ref(),
            )
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
