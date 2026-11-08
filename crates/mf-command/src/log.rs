//! Command log on disk (OD-3, MF-CMD-004).
//!
//! `<name>.mfx.log/manifest.json` lists the source files with their content
//! hashes. `commands.jsonl` holds one event per line, root to current: an
//! import or a command record. Source paths are stored relative to the log
//! directory where possible, so a project folder can be moved as a whole.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use mf_document::ObjectId;
use serde::{Deserialize, Serialize};

use crate::record::CommandRecord;
use crate::session::{CommandError, Session};

pub const LOG_FORMAT: &str = "mfx-log/1";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: String,
    pub engine_version: String,
    pub sources: Vec<SourceEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SourceEntry {
    pub object: ObjectId,
    /// Relative to the log directory when possible, otherwise absolute.
    /// Relative paths always use `/`, so logs move between operating systems.
    pub path: String,
    /// BLAKE3 of the file bytes, hex.
    pub content_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Event {
    Import { object: ObjectId, name: String },
    Command(Box<CommandRecord>),
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{file} line {line}: {message}")]
    Json {
        file: String,
        line: usize,
        message: String,
    },
    #[error("unsupported log format '{0}' (expected {LOG_FORMAT})")]
    UnsupportedFormat(String),
    #[error("object {object} has no source file, so the session can't be replayed")]
    NoSource { object: ObjectId },
    #[error(
        "source {path} changed since the log was written (expected {expected}, found {actual})"
    )]
    SourceChanged {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("reading source {path}: {error}")]
    SourceRead {
        path: PathBuf,
        error: mf_io::IoError,
    },
    #[error("log references source for object {0}, which the manifest doesn't list")]
    MissingSourceEntry(ObjectId),
    #[error("object {0} vanished from the replayed session after its command ran")]
    TargetMissing(ObjectId),
    #[error("replay allocated object {actual}, but the log expected {expected}")]
    ObjectIdMismatch {
        expected: ObjectId,
        actual: ObjectId,
    },
    #[error("{op} is version {available}, but the log recorded version {recorded}")]
    VersionMismatch {
        op: String,
        recorded: u32,
        available: u32,
    },
    #[error(transparent)]
    Command(#[from] CommandError),
}

/// `path` expressed relative to directory `base`, if both are absolute.
pub(crate) fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    // allow-swallow: a path that can't be canonicalized has no relative form; the caller stores it absolute
    let path = path.canonicalize().ok()?;
    // allow-swallow: as above, for the log directory
    let base = base.canonicalize().ok()?;
    let p: Vec<Component> = path.components().collect();
    let b: Vec<Component> = base.components().collect();
    // Different roots (e.g. Windows drives) can't be related.
    if p.first() != b.first() {
        return None;
    }
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut rel = PathBuf::new();
    for _ in common..b.len() {
        rel.push("..");
    }
    for c in &p[common..] {
        rel.push(c.as_os_str());
    }
    Some(rel)
}

/// Relative path with `/` separators on every OS.
pub(crate) fn portable(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl Session {
    /// Write the path from the root to the current revision as a log.
    pub fn save_log(&self, dir: &Path) -> Result<(), LogError> {
        let mut chain = Vec::new();
        let mut cur = Some(self.current());
        while let Some(r) = cur {
            let node = self
                .node(r)
                .ok_or(CommandError::UnknownObject(ObjectId(0)))?;
            chain.push(node);
            cur = node.parent;
        }
        chain.reverse();
        std::fs::create_dir_all(dir)?;

        let mut sources = Vec::new();
        let mut events = Vec::new();
        for node in chain {
            if let Some(import) = &node.import {
                let source = import.source.as_ref().ok_or(LogError::NoSource {
                    object: import.object,
                })?;
                let path = match relative_to(&source.path, dir) {
                    Some(rel) => portable(&rel),
                    None => source.path.to_string_lossy().into_owned(),
                };
                sources.push(SourceEntry {
                    object: import.object,
                    path,
                    content_hash: source.content_hash.to_hex(),
                });
                events.push(Event::Import {
                    object: import.object,
                    name: import.name.clone(),
                });
            }
            if let Some(record) = &node.record {
                let mut record = record.clone();
                // Summaries are computed here, once, rather than on every commit.
                if record.output_summary.is_none()
                    && let Some(mesh) = node.document.object(record.target).and_then(|o| o.mesh())
                {
                    record.output_summary = Some(crate::record::GeometrySummary::of(mesh));
                }
                events.push(Event::Command(Box::new(record)));
            }
        }
        let manifest = Manifest {
            format: LOG_FORMAT.into(),
            engine_version: env!("CARGO_PKG_VERSION").into(),
            sources,
        };
        let json = |e: serde_json::Error| LogError::Json {
            file: "manifest.json".into(),
            line: 0,
            message: e.to_string(),
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).map_err(json)?,
        )?;
        let mut f = std::fs::File::create(dir.join("commands.jsonl"))?;
        for e in &events {
            let line = serde_json::to_string(e).map_err(|e| LogError::Json {
                file: "commands.jsonl".into(),
                line: 0,
                message: e.to_string(),
            })?;
            writeln!(f, "{line}")?;
        }
        f.sync_all()?;
        Ok(())
    }
}
