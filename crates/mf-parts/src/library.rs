//! The parts library (D16): the built-ins plus the user's folder of parts,
//! each `<dir>/<id>/part.yaml` next to its mesh file.

use std::path::{Path, PathBuf};

use mf_geometry::Mesh;
use mf_runtime::{Cancelled, RunContext};

use crate::builtin::{Builtin, builtins};
use crate::manifest::PartManifest;

/// The manifest's file name in a part's folder.
pub const MANIFEST_FILE: &str = "part.yaml";

#[derive(Debug, thiserror::Error)]
pub enum PartError {
    #[error("can't read part '{id}': {reason}")]
    Unreadable { id: String, reason: String },
    #[error("part '{0}' isn't in the library")]
    NotFound(String),
    #[error("a part named '{0}' already exists in the library")]
    Exists(String),
    #[error("the part isn't valid: {0}")]
    Invalid(String),
    #[error("can't save the part: {0}")]
    Unwritable(String),
    #[error("cancelled by the user")]
    Cancelled,
}

impl From<Cancelled> for PartError {
    fn from(_: Cancelled) -> Self {
        PartError::Cancelled
    }
}

/// A user part that couldn't be loaded, by name and why. Never dropped
/// silently.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryWarning {
    pub path: PathBuf,
    pub reason: String,
}

enum Source {
    Builtin(Builtin),
    User {
        dir: PathBuf,
        manifest_bytes: Vec<u8>,
    },
}

pub struct LibraryPart {
    pub manifest: PartManifest,
    source: Source,
}

impl LibraryPart {
    pub fn is_builtin(&self) -> bool {
        matches!(self.source, Source::Builtin(_))
    }

    /// The part's mesh, in part space.
    pub fn mesh(&self, ctx: &RunContext) -> Result<Mesh, PartError> {
        match &self.source {
            Source::Builtin(b) => {
                ctx.checkpoint()?;
                Ok(b.mesh())
            }
            Source::User { dir, .. } => {
                let path = dir.join(&self.manifest.mesh);
                mf_io::read_path(&path)
                    .map(|i| i.mesh)
                    .map_err(|e| PartError::Unreadable {
                        id: self.manifest.id.clone(),
                        reason: format!("{}: {e}", path.display()),
                    })
            }
        }
    }

    /// What the part is, as a hex BLAKE3 hash: a built-in's generator id,
    /// version and parameters; a user part's manifest and mesh file bytes.
    /// A command records it, so replay verifies it uses the same part.
    pub fn content_hash(&self) -> Result<String, PartError> {
        let mut h = blake3::Hasher::new();
        match &self.source {
            Source::Builtin(b) => {
                h.update(b"mf-part-builtin-v1\0");
                h.update(b.manifest.id.as_bytes());
                h.update(&b.manifest.version.to_le_bytes());
                h.update(b.params.as_bytes());
            }
            Source::User {
                dir,
                manifest_bytes,
            } => {
                let mesh = std::fs::read(dir.join(&self.manifest.mesh)).map_err(|e| {
                    PartError::Unreadable {
                        id: self.manifest.id.clone(),
                        reason: e.to_string(),
                    }
                })?;
                h.update(b"mf-part-user-v1\0");
                h.update(&(manifest_bytes.len() as u64).to_le_bytes());
                h.update(manifest_bytes);
                h.update(&mesh);
            }
        }
        Ok(h.finalize().to_hex().to_string())
    }
}

pub struct Library {
    pub parts: Vec<LibraryPart>,
}

impl Library {
    /// The built-ins, then the user's parts from `user_dir` (each a folder
    /// with `part.yaml`), sorted by folder name. A part that can't be read,
    /// is malformed, or repeats an id is left out with a warning naming it.
    pub fn load(user_dir: Option<&Path>, ctx: &RunContext) -> (Library, Vec<LibraryWarning>) {
        let mut parts: Vec<LibraryPart> = builtins()
            .into_iter()
            .map(|b| LibraryPart {
                manifest: b.manifest.clone(),
                source: Source::Builtin(b),
            })
            .collect();
        let mut warnings = Vec::new();
        let Some(dir) = user_dir else {
            return (Library { parts }, warnings);
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Library { parts }, warnings);
            }
            Err(e) => {
                warnings.push(LibraryWarning {
                    path: dir.to_owned(),
                    reason: format!("can't read the parts folder: {e}"),
                });
                return (Library { parts }, warnings);
            }
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|e| match e {
                Ok(e) => Some(e.path()),
                Err(err) => {
                    warnings.push(LibraryWarning {
                        path: dir.to_owned(),
                        reason: format!("can't list a parts folder entry: {err}"),
                    });
                    None
                }
            })
            .filter(|p| p.is_dir())
            .collect();
        folders.sort();
        for folder in folders {
            if ctx.checkpoint().is_err() {
                break;
            }
            match load_user(&folder) {
                Ok(part) => {
                    if parts.iter().any(|p| p.manifest.id == part.manifest.id) {
                        warnings.push(LibraryWarning {
                            path: folder,
                            reason: format!(
                                "another part already has the id '{}'",
                                part.manifest.id
                            ),
                        });
                    } else {
                        parts.push(part);
                    }
                }
                Err(reason) => warnings.push(LibraryWarning {
                    path: folder,
                    reason,
                }),
            }
        }
        (Library { parts }, warnings)
    }

    pub fn get(&self, id: &str) -> Option<&LibraryPart> {
        self.parts.iter().find(|p| p.manifest.id == id)
    }
}

fn load_user(folder: &Path) -> Result<LibraryPart, String> {
    let path = folder.join(MANIFEST_FILE);
    let bytes = std::fs::read(&path).map_err(|e| format!("can't read {MANIFEST_FILE}: {e}"))?;
    let text =
        std::str::from_utf8(&bytes).map_err(|e| format!("{MANIFEST_FILE} isn't text: {e}"))?;
    let manifest: PartManifest =
        serde_saphyr::from_str(text).map_err(|e| format!("{MANIFEST_FILE} is malformed: {e}"))?;
    manifest.check()?;
    if manifest.mesh.is_empty() || manifest.mesh.contains(['/', '\\']) {
        return Err("`mesh` must name a file in the part's folder".into());
    }
    if !folder.join(&manifest.mesh).is_file() {
        return Err(format!("its mesh file '{}' is missing", manifest.mesh));
    }
    Ok(LibraryPart {
        manifest,
        source: Source::User {
            dir: folder.to_owned(),
            manifest_bytes: bytes,
        },
    })
}

/// Saves `mesh` as a user part `dir/<id>/` (`part.yaml` and `part.3mf`,
/// which keeps its colours). Refuses an id that's already there.
pub fn save_object_as_part(
    mesh: &Mesh,
    manifest: &PartManifest,
    dir: &Path,
) -> Result<PathBuf, PartError> {
    manifest.check().map_err(PartError::Invalid)?;
    if mesh.is_empty() {
        return Err(PartError::Invalid("the object has no geometry".into()));
    }
    let folder = dir.join(&manifest.id);
    if folder.exists() {
        return Err(PartError::Exists(manifest.id.clone()));
    }
    std::fs::create_dir_all(&folder).map_err(|e| PartError::Unwritable(e.to_string()))?;
    let mut m = manifest.clone();
    m.mesh = "part.3mf".into();
    let model = mf_io::threemf::Model {
        unit: mf_io::threemf::Unit::Millimeter,
        objects: vec![mf_io::threemf::ModelObject {
            name: m.name.clone(),
            mesh: mesh.clone(),
            transform: mf_io::threemf::IDENTITY,
        }],
    };
    mf_io::write_model_path(&model, &folder.join(&m.mesh))
        .map_err(|e| PartError::Unwritable(e.to_string()))?;
    let text = serde_saphyr::to_string(&m).map_err(|e| PartError::Unwritable(e.to_string()))?;
    std::fs::write(folder.join(MANIFEST_FILE), text)
        .map_err(|e| PartError::Unwritable(e.to_string()))?;
    Ok(folder)
}

/// The user's parts folder: `<config dir>/meshfixer/parts`.
pub fn user_parts_dir() -> Result<PathBuf, crate::dirs::DirsError> {
    Ok(crate::dirs::meshfixer_dir()?.join("parts"))
}
