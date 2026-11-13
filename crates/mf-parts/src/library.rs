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
    Builtin(Box<Builtin>),
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

    /// The part's mesh, in part space: a built-in's generated mesh, a
    /// user part's stored mesh file, or — for a connector entry
    /// (`PartKind::Connector`) — every part the connector generates,
    /// merged into one mesh (a preview/placement shape; `object.
    /// add_connector`, Task 28, regenerates it with its parts kept
    /// separate so `connector.join` can act on each role).
    pub fn mesh(&self, ctx: &RunContext) -> Result<Mesh, PartError> {
        if let crate::manifest::PartKind::Connector { connector, params } = &self.manifest.kind {
            return self.connector_mesh(connector, params, ctx);
        }
        match &self.source {
            Source::Builtin(b) => {
                ctx.checkpoint()?;
                b.mesh().ok_or_else(|| {
                    PartError::Invalid(format!("'{}' has no mesh of its own", self.manifest.id))
                })
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

    fn connector_mesh(
        &self,
        connector: &str,
        params: &mf_spec::Params,
        ctx: &RunContext,
    ) -> Result<Mesh, PartError> {
        let c = crate::find_generator(connector).ok_or_else(|| {
            PartError::Invalid(format!("'{connector}' isn't a built-in connector"))
        })?;
        let generated = c
            .generate(params, &crate::GenContext::new(ctx))
            .map_err(|e| match e {
                crate::ConnectorError::Failed { message, .. } => PartError::Invalid(message),
            })?;
        let mut positions = Vec::new();
        let mut triangles = Vec::new();
        for part in generated {
            let base = positions.len() as u32;
            let (p, t) = part.mesh.to_arrays();
            positions.extend(p);
            triangles.extend(t.into_iter().map(|tri| tri.map(|i| i + base)));
        }
        Mesh::new(positions, triangles)
            .map_err(|e| PartError::Invalid(format!("the regenerated connector is invalid: {e}")))
    }

    /// What the part is, as a hex BLAKE3 hash: a built-in's generator id,
    /// version and parameters; a user part's manifest and mesh file bytes.
    /// A command records it, so replay verifies it uses the same part.
    pub fn content_hash(&self) -> Result<String, PartError> {
        let mut h = blake3::Hasher::new();
        if let crate::manifest::PartKind::Connector { connector, params } = &self.manifest.kind {
            // A connector's identity is its recipe, not a stored mesh:
            // the id and its own version, plus the recorded params.
            let c = crate::find_generator(connector).ok_or_else(|| {
                PartError::Invalid(format!("'{connector}' isn't a built-in connector"))
            })?;
            h.update(b"mf-part-connector-v1\0");
            h.update(connector.as_bytes());
            h.update(&c.version().to_le_bytes());
            let params_text = serde_saphyr::to_string(params)
                .map_err(|e| PartError::Invalid(format!("connector params: {e}")))?;
            h.update(params_text.as_bytes());
            return Ok(h.finalize().to_hex().to_string());
        }
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
                source: Source::Builtin(Box::new(b)),
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
    // A connector entry has no mesh file: its recipe (connector id and
    // params) regenerates the parts on demand (`LibraryPart::mesh`).
    if !matches!(manifest.kind, crate::manifest::PartKind::Connector { .. }) {
        if manifest.mesh.is_empty() || manifest.mesh.contains(['/', '\\']) {
            return Err("`mesh` must name a file in the part's folder".into());
        }
        if !folder.join(&manifest.mesh).is_file() {
            return Err(format!("its mesh file '{}' is missing", manifest.mesh));
        }
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
            metadata: Default::default(),
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

/// "Save to library" on a connector object (Task 28, MF-MODEL-005):
/// writes `dir/<id>/part.yaml` with `kind: Connector { connector,
/// params }` and no mesh file — dropping it back onto a model
/// regenerates its parts through the connector, from the exact
/// parameters recorded here, not a stored snapshot. `manifest.kind`
/// must already be `PartKind::Connector`; refuses an id that's already
/// there, like [`save_object_as_part`].
pub fn save_connector_as_part(manifest: &PartManifest, dir: &Path) -> Result<PathBuf, PartError> {
    manifest.check().map_err(PartError::Invalid)?;
    let crate::manifest::PartKind::Connector { connector, .. } = &manifest.kind else {
        return Err(PartError::Invalid(
            "save_connector_as_part needs a PartKind::Connector manifest".into(),
        ));
    };
    if crate::find_generator(connector).is_none_or(|g| g.is_shape()) {
        return Err(PartError::Invalid(format!(
            "'{connector}' isn't a built-in connector"
        )));
    }
    let folder = dir.join(&manifest.id);
    if folder.exists() {
        return Err(PartError::Exists(manifest.id.clone()));
    }
    std::fs::create_dir_all(&folder).map_err(|e| PartError::Unwritable(e.to_string()))?;
    let mut m = manifest.clone();
    m.mesh = String::new();
    let text = serde_saphyr::to_string(&m).map_err(|e| PartError::Unwritable(e.to_string()))?;
    std::fs::write(folder.join(MANIFEST_FILE), text)
        .map_err(|e| PartError::Unwritable(e.to_string()))?;
    Ok(folder)
}

/// A user part's folder in `dir`, checked: `id` names a folder there
/// (letters, digits, '-', '_', '.' only, so it can't reach outside
/// `dir`) whose `part.yaml` is that part's. A built-in isn't in `dir`
/// and so is refused as not found: built-ins can't be renamed or deleted.
fn user_part_folder(dir: &Path, id: &str) -> Result<(PathBuf, PartManifest), PartError> {
    let safe = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !safe || crate::builtins().iter().any(|b| b.manifest.id == id) {
        return Err(PartError::NotFound(id.to_owned()));
    }
    let folder = dir.join(id);
    let part = load_user(&folder).map_err(|reason| {
        if folder.join(MANIFEST_FILE).is_file() {
            PartError::Unreadable {
                id: id.to_owned(),
                reason,
            }
        } else {
            PartError::NotFound(id.to_owned())
        }
    })?;
    if part.manifest.id != id {
        return Err(PartError::Invalid(format!(
            "the folder '{id}' holds the part '{}'",
            part.manifest.id
        )));
    }
    Ok((folder, part.manifest))
}

/// Renames a user part, and sets its category and description (its id,
/// folder and mesh stay as they are, so anything placed from it keeps
/// its recorded hash meaning). Refuses a built-in.
pub fn update_user_part(
    dir: &Path,
    id: &str,
    name: &str,
    category: &str,
    description: &str,
) -> Result<PartManifest, PartError> {
    let (folder, mut manifest) = user_part_folder(dir, id)?;
    manifest.name = name.trim().to_owned();
    manifest.category = category.trim().to_owned();
    manifest.description = description.trim().to_owned();
    manifest.check().map_err(PartError::Invalid)?;
    let text =
        serde_saphyr::to_string(&manifest).map_err(|e| PartError::Unwritable(e.to_string()))?;
    std::fs::write(folder.join(MANIFEST_FILE), text)
        .map_err(|e| PartError::Unwritable(e.to_string()))?;
    Ok(manifest)
}

/// Deletes a user part: its `part.yaml` and mesh file, then its folder
/// (only if nothing else is left in it, so nothing the library didn't
/// write is ever removed). Refuses a built-in.
pub fn delete_user_part(dir: &Path, id: &str) -> Result<(), PartError> {
    let (folder, manifest) = user_part_folder(dir, id)?;
    let unwritable = |e: std::io::Error| PartError::Unwritable(e.to_string());
    if !manifest.mesh.is_empty() && !manifest.mesh.contains(['/', '\\']) {
        let mesh = folder.join(&manifest.mesh);
        if mesh.is_file() {
            std::fs::remove_file(mesh).map_err(unwritable)?;
        }
    }
    std::fs::remove_file(folder.join(MANIFEST_FILE)).map_err(unwritable)?;
    std::fs::remove_dir(&folder).map_err(|e| {
        PartError::Unwritable(format!(
            "the part is gone, but its folder {} still holds other files: {e}",
            folder.display()
        ))
    })
}

/// The user's parts folder: `<config dir>/meshfixer/parts`.
pub fn user_parts_dir() -> Result<PathBuf, crate::dirs::DirsError> {
    Ok(crate::dirs::meshfixer_dir()?.join("parts"))
}
