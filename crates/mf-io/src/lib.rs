//! STL and OBJ import/export (MF-IO-001).
//!
//! Readers never panic on malformed input. They return structured
//! [`IoError`]s and check declared sizes before allocating (MF-IO-002).

use std::path::{Path, PathBuf};

use mf_geometry::{ContentHash, GeometryError, Mesh};
use mf_runtime::{Cancelled, RunContext};
use serde::Serialize;

pub mod obj;
pub mod stl;
pub mod threemf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum Format {
    StlBinary,
    StlAscii,
    Obj,
    ThreeMf,
}

impl Format {
    /// Format for writing, by extension. `.stl` writes binary STL.
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "stl" => Some(Format::StlBinary),
            "obj" => Some(Format::Obj),
            "3mf" => Some(Format::ThreeMf),
            _ => None,
        }
    }
}

/// Precision of the coordinates as stored in the source file (MF-GEO-001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SourcePrecision {
    F32,
    Decimal { max_significant_digits: u8 },
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub path: PathBuf,
    /// BLAKE3 of the file bytes.
    pub content_hash: ContentHash,
    pub format: Format,
    pub precision: SourcePrecision,
}

#[derive(Debug, Clone)]
pub struct Imported {
    pub mesh: Mesh,
    pub source: SourceInfo,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("file is truncated: expected {expected} bytes, found {actual}{detail}")]
    Truncated {
        expected: usize,
        actual: usize,
        /// Extra context, starting with a space (may be empty).
        detail: String,
    },
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("export to {path} failed: {reason} (any existing file was left unchanged)")]
    Export { path: PathBuf, reason: String },
    #[error("unsupported file format: {0}")]
    UnsupportedFormat(PathBuf),
    /// The import was cancelled through its [`RunContext`]; nothing was
    /// produced.
    #[error("cancelled by the user")]
    Cancelled,
}

impl From<Cancelled> for IoError {
    fn from(_: Cancelled) -> Self {
        IoError::Cancelled
    }
}

/// Every object of a source file (STL and OBJ have one), with the model
/// unit and transforms (MF-DOC-004).
#[derive(Debug, Clone)]
pub struct ImportedModel {
    pub objects: Vec<threemf::ModelObject>,
    pub unit: threemf::Unit,
    pub source: SourceInfo,
    pub warnings: Vec<String>,
}

pub fn read_model(path: &Path) -> Result<ImportedModel, IoError> {
    read_model_with(path, &RunContext::none())
}

/// As [`read_model`], cancellable, reporting progress as stages
/// `"reading"` (by bytes consumed) and `"welding"`.
///
/// Reading the file is the first half of `"reading"`; parsing text formats
/// (by bytes consumed) is the second. Binary and ASCII STL then weld
/// corners into vertices (`"welding"`); OBJ and 3MF index their vertices
/// already, so their import ends with `"reading"`.
pub fn read_model_with(path: &Path, ctx: &RunContext) -> Result<ImportedModel, IoError> {
    let format =
        Format::from_path(path).ok_or_else(|| IoError::UnsupportedFormat(path.to_owned()))?;
    let (bytes, content_hash) = read_file(path, ctx)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("object")
        .to_owned();
    let single = |mesh: Mesh| {
        vec![threemf::ModelObject {
            name: stem.clone(),
            mesh,
            transform: threemf::IDENTITY,
        }]
    };
    let (objects, unit, precision, format, warnings) = match format {
        Format::Obj => {
            let text = std::str::from_utf8(&bytes).map_err(|e| IoError::Parse {
                line: 0,
                message: format!("not valid UTF-8: {e}"),
            })?;
            let (mesh, precision, warnings) = obj::read_with(text, ctx)?;
            (
                single(mesh),
                threemf::Unit::Millimeter,
                precision,
                Format::Obj,
                warnings,
            )
        }
        Format::StlBinary | Format::StlAscii => {
            let (mesh, precision, warnings) = stl::read_with(&bytes, ctx)?;
            let format = match precision {
                SourcePrecision::F32 => Format::StlBinary,
                SourcePrecision::Decimal { .. } => Format::StlAscii,
            };
            (
                single(mesh),
                threemf::Unit::Millimeter,
                precision,
                format,
                warnings,
            )
        }
        Format::ThreeMf => {
            let (model, digits, warnings) = threemf::read_with(&bytes, ctx)?;
            let precision = SourcePrecision::Decimal {
                max_significant_digits: digits,
            };
            (
                model.objects,
                model.unit,
                precision,
                Format::ThreeMf,
                warnings,
            )
        }
    };
    Ok(ImportedModel {
        objects,
        unit,
        source: SourceInfo {
            path: path.to_owned(),
            content_hash,
            format,
            precision,
        },
        warnings,
    })
}

/// Stage of every reader's parsing pass; see [`read_model_with`].
pub(crate) const READING: &str = "reading";
/// Stage of the STL readers' welding pass.
pub(crate) const WELDING: &str = "welding";
/// Where in `"reading"` parsing starts (reading the file comes first).
pub(crate) const PARSE_FROM: f64 = 0.5;

/// Read a whole file in blocks, hashing as it goes, so a cancel is seen
/// between blocks and the hash costs no second pass. Progress is the first
/// half of `"reading"`.
fn read_file(path: &Path, ctx: &RunContext) -> Result<(Vec<u8>, ContentHash), IoError> {
    use std::io::Read;
    // Small enough to hash in a few milliseconds even unoptimised.
    const BLOCK: u64 = 2 << 20;
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len() as usize;
    let span = ctx.span(READING, 0.0, PARSE_FROM, len);
    let mut bytes = Vec::with_capacity(len);
    let mut hasher = blake3::Hasher::new();
    loop {
        span.step(bytes.len())?;
        let start = bytes.len();
        if (&mut file).take(BLOCK).read_to_end(&mut bytes)? == 0 {
            break;
        }
        hasher.update(&bytes[start..]);
    }
    span.finish();
    Ok((bytes, ContentHash(*hasher.finalize().as_bytes())))
}

/// Progress through text being parsed: `consumed` bytes of `text` map onto
/// the parsing half of `"reading"`. Checkpoints every call.
pub(crate) fn parsed(ctx: &RunContext, text: &str, line: &str) -> Result<(), Cancelled> {
    let consumed = (line.as_ptr() as usize).saturating_sub(text.as_ptr() as usize);
    let f = consumed as f64 / text.len().max(1) as f64;
    ctx.progress
        .report(READING, PARSE_FROM + (1.0 - PARSE_FROM) * f);
    ctx.checkpoint()
}

/// Read a file's first (usually only) mesh. A 3MF with more objects adds a
/// warning naming how many were left out; use [`read_model`] for all of them.
pub fn read_path(path: &Path) -> Result<Imported, IoError> {
    let mut model = read_model(path)?;
    if model.objects.is_empty() {
        return Err(IoError::Parse {
            line: 0,
            message: format!("{} contains no mesh objects", path.display()),
        });
    }
    if model.objects.len() > 1 {
        model.warnings.push(format!(
            "{} has {} objects; only the first ('{}') was imported",
            path.display(),
            model.objects.len(),
            model.objects[0].name
        ));
    }
    let first = model.objects.swap_remove(0);
    Ok(Imported {
        mesh: first.mesh,
        source: model.source,
        warnings: model.warnings,
    })
}

/// Export `mesh`, never leaving a partial file behind.
///
/// The file is encoded in memory, re-parsed to verify it, written to a temp
/// file in the same directory, flushed to disk, and renamed over `path`.
/// If any step fails, an existing file at `path` is left unchanged.
pub fn write_path(mesh: &Mesh, path: &Path, format: Format) -> Result<(), IoError> {
    write_path_with(mesh, path, format, &RunContext::none())
}

/// Progress stage of [`write_path_with`] and [`write_model_path_with`]:
/// encoding and verifying take its first half, writing the second.
pub const WRITING: &str = "writing";
const WRITE_FROM: f64 = 0.5;

/// As [`write_path`], cancellable, reporting progress as stage
/// [`WRITING`]. Cancelling before the final rename returns
/// [`IoError::Cancelled`] and leaves nothing behind: no file at `path`
/// (an existing one is unchanged) and no temp file. Once renamed, the
/// export is done and cancelling has no effect.
pub fn write_path_with(
    mesh: &Mesh,
    path: &Path,
    format: Format,
    ctx: &RunContext,
) -> Result<(), IoError> {
    let fail = |reason: String| IoError::Export {
        path: path.to_owned(),
        reason,
    };
    ctx.progress.report(WRITING, 0.0);
    ctx.checkpoint()?;
    let bytes = match format {
        Format::StlBinary => stl::write_binary(mesh).map_err(fail)?,
        Format::StlAscii => stl::write_ascii(mesh).into_bytes(),
        Format::Obj => obj::write(mesh).into_bytes(),
        Format::ThreeMf => {
            let model = threemf::Model {
                unit: threemf::Unit::Millimeter,
                objects: vec![threemf::ModelObject {
                    name: "object".into(),
                    mesh: mesh.clone(),
                    transform: threemf::IDENTITY,
                }],
            };
            return write_model_path_with(&model, path, ctx);
        }
    };
    ctx.progress.report(WRITING, WRITE_FROM * 0.8);
    ctx.checkpoint()?;
    verify_encoded(mesh, &bytes, format).map_err(fail)?;
    atomic_write(path, &bytes, ctx)
}

/// Export a whole model as 3MF (objects, transforms, unit), verified and
/// atomic like [`write_path`].
pub fn write_model_path(model: &threemf::Model, path: &Path) -> Result<(), IoError> {
    write_model_path_with(model, path, &RunContext::none())
}

/// As [`write_model_path`], cancellable like [`write_path_with`].
pub fn write_model_path_with(
    model: &threemf::Model,
    path: &Path,
    ctx: &RunContext,
) -> Result<(), IoError> {
    let fail = |reason: String| IoError::Export {
        path: path.to_owned(),
        reason,
    };
    ctx.progress.report(WRITING, 0.0);
    ctx.checkpoint()?;
    let bytes = threemf::write(model).map_err(fail)?;
    ctx.progress.report(WRITING, WRITE_FROM * 0.5);
    ctx.checkpoint()?;
    let (back, warnings) =
        threemf::read(&bytes).map_err(|e| fail(format!("verification re-read failed: {e}")))?;
    if !warnings.is_empty() {
        return Err(fail(format!(
            "verification re-read warned: {}",
            warnings.join("; ")
        )));
    }
    let same = back.unit == model.unit
        && back.objects.len() == model.objects.len()
        && back.objects.iter().zip(&model.objects).all(|(b, m)| {
            b.transform == m.transform
                && b.mesh.face_count() == m.mesh.face_count()
                && b.mesh
                    .vertices()
                    .map(|(_, p)| p)
                    .eq(m.mesh.vertices().map(|(_, p)| p))
        });
    if !same {
        return Err(fail("verification found a different model".into()));
    }
    atomic_write(path, &bytes, ctx)
}

/// Bytes written between checkpoints.
const WRITE_BLOCK: usize = 4 << 20;

/// Write to a temp file in the same directory, flush, then rename over
/// `path`; on failure or cancellation an existing file is left unchanged
/// and the temp file is removed.
fn atomic_write(path: &Path, bytes: &[u8], ctx: &RunContext) -> Result<(), IoError> {
    let fail = |reason: String| IoError::Export {
        path: path.to_owned(),
        reason,
    };
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_owned(),
        _ => PathBuf::from("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| fail("output path has no file name".into()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        // allow-swallow: a clock before 1970 only affects the temp name, which also carries the pid
        .unwrap_or(0);
    let tmp = dir.join(format!(
        ".{}.mf-tmp-{}-{nanos}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let span = ctx.span(WRITING, WRITE_FROM, 1.0, bytes.len());
    let result = (|| -> Result<(), IoError> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        let mut done = 0;
        for block in bytes.chunks(WRITE_BLOCK) {
            span.step(done)?;
            f.write_all(block)?;
            done += block.len();
        }
        f.sync_all()?;
        drop(f);
        // The last moment a cancel can still leave nothing behind.
        ctx.checkpoint()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if let Err(e) = result {
        // allow-swallow: best-effort cleanup on an error path; the original error is returned
        let _ = std::fs::remove_file(&tmp);
        return Err(match e {
            IoError::Cancelled => IoError::Cancelled,
            e => fail(e.to_string()),
        });
    }
    span.finish();
    Ok(())
}

/// Check encoded bytes describe the same mesh, without a full re-import
/// where a cheaper exact check exists.
fn verify_encoded(mesh: &Mesh, bytes: &[u8], format: Format) -> Result<(), String> {
    match format {
        Format::StlBinary => stl::verify_binary(bytes, mesh.triangle_count()),
        Format::Obj => {
            let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
            obj::verify(text, mesh)
        }
        Format::ThreeMf => Err("3MF is verified by write_model_path".into()),
        Format::StlAscii => {
            let (decoded, _, warnings) =
                stl::read(bytes).map_err(|e| format!("verification re-read failed: {e}"))?;
            if !warnings.is_empty() {
                return Err(format!(
                    "verification re-read warned: {}",
                    warnings.join("; ")
                ));
            }
            if decoded.triangle_count() != mesh.triangle_count() {
                return Err(format!(
                    "verification found {} triangles, expected {}",
                    decoded.triangle_count(),
                    mesh.triangle_count()
                ));
            }
            Ok(())
        }
    }
}

/// Count significant digits in a decimal literal's mantissa.
pub(crate) fn significant_digits(token: &str) -> u8 {
    let mantissa = token.split(['e', 'E']).next().unwrap_or("");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.trim_start_matches('0').len().min(u8::MAX as usize) as u8
}
