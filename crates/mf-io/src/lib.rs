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
    /// Text the file itself offers about its exporter or units
    /// (MF-ISSUE-045): an STL header ([`stl::header_text`]) or an OBJ
    /// `# unit` comment ([`obj::unit_comment`]). `None` for 3MF (units
    /// are in the file, never a guess) and when the source has nothing
    /// to show.
    pub exporter_hint: Option<String>,
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
    /// An object's transform puts a vertex out of range (a coordinate
    /// that isn't a finite number) when applied for an STL/OBJ export.
    #[error(
        "the object's transform puts vertex {vertex} out of range (a coordinate overflows); \
         check the object's scale"
    )]
    TransformOutOfRange { vertex: usize },
    #[error("unsupported file format: {0}")]
    UnsupportedFormat(PathBuf),
    /// The import was cancelled through its [`RunContext`]; nothing was
    /// produced.
    #[error("cancelled by the user")]
    Cancelled,
    /// A declared or measured size exceeded a [`Limits`] bound. Always
    /// raised before allocating for `found` (MF-IO-002): a hostile file
    /// never gets to make the reader allocate on its say-so.
    #[error("{what} is {found}, over the limit of {limit}")]
    LimitExceeded {
        /// What was measured, e.g. "declared triangle count" or
        /// "decompressed 3MF model part size".
        what: &'static str,
        limit: u64,
        found: u64,
    },
}

/// Bounds readers enforce before allocating or decompressing, so a hostile
/// file fails fast with a structured [`IoError::LimitExceeded`] instead of
/// an unbounded allocation or a hang (MF-IO-002, review focus 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest declared triangle count a reader will act on (STL's header
    /// count, or an OBJ/3MF vertex index implying this many triangles).
    pub max_triangles: u64,
    /// Largest a 3MF archive's model part may be once decompressed.
    pub max_model_part_bytes: u64,
    /// Largest ratio of decompressed to compressed bytes a 3MF model part
    /// may have (a zip-bomb signature: tiny compressed, huge inflated).
    pub max_compression_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_triangles: 50_000_000,
            max_model_part_bytes: 4 * (1 << 30),
            max_compression_ratio: 200,
        }
    }
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
    /// The source file's own unit, if it declares one. 3MF always does
    /// (`Some`). STL and OBJ never do (consolidation plan C06,
    /// units-audit.md §3): `None` means "undeclared", not "millimetres" —
    /// callers that need a concrete unit (importing into a document,
    /// `--units`) choose one explicitly instead of a silent default.
    pub unit: Option<threemf::Unit>,
    pub source: SourceInfo,
    pub warnings: Vec<String>,
    /// The model's own `<metadata>` ([`threemf::Model::metadata`]): empty
    /// for STL and OBJ, which have none.
    pub metadata: std::collections::BTreeMap<String, String>,
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
            metadata: Default::default(),
            name: stem.clone(),
            mesh,
            transform: threemf::IDENTITY,
        }]
    };
    let (objects, unit, precision, format, warnings, metadata, exporter_hint) = match format {
        Format::Obj => {
            let text = std::str::from_utf8(&bytes).map_err(|e| IoError::Parse {
                line: 0,
                message: format!("not valid UTF-8: {e}"),
            })?;
            let hint = obj::unit_comment(text);
            let (mesh, precision, warnings) = obj::read_with(text, ctx)?;
            (
                single(mesh),
                None,
                precision,
                Format::Obj,
                warnings,
                Default::default(),
                hint,
            )
        }
        Format::StlBinary | Format::StlAscii => {
            let hint = stl::header_text(&bytes);
            let (mesh, precision, warnings) = stl::read_with(&bytes, ctx)?;
            let format = match precision {
                SourcePrecision::F32 => Format::StlBinary,
                SourcePrecision::Decimal { .. } => Format::StlAscii,
            };
            (
                single(mesh),
                None,
                precision,
                format,
                warnings,
                Default::default(),
                hint,
            )
        }
        Format::ThreeMf => {
            let (model, digits, warnings) = threemf::read_with(&bytes, ctx)?;
            let precision = SourcePrecision::Decimal {
                max_significant_digits: digits,
            };
            (
                model.objects,
                Some(model.unit),
                precision,
                Format::ThreeMf,
                warnings,
                model.metadata,
                None,
            )
        }
    };
    Ok(ImportedModel {
        metadata,
        objects,
        unit,
        source: SourceInfo {
            path: path.to_owned(),
            content_hash,
            format,
            precision,
            exporter_hint,
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

/// The progress stage of [`transform_mesh`].
pub const TRANSFORMING: &str = "applying the position";

/// Apply a 3MF row-major 3×4 affine (see [`threemf::IDENTITY`]) to `mesh`'s
/// positions, in f64, for formats (STL, OBJ) that have no transform of
/// their own to carry it (MF-ISSUE-035). A mirrored transform (negative
/// determinant of the linear part) flips every triangle's winding so
/// normals stay outward after the transform.
///
/// O(n): run it in the export job, not on the UI thread (M4 final review
/// I3). Reports [`TRANSFORMING`] progress and stops when `ctx` is
/// cancelled. A transform that puts a vertex out of range (an overflowing
/// scale, as a corrupt or hostile 3MF can carry) fails with
/// [`IoError::TransformOutOfRange`].
pub fn transform_mesh(
    mesh: &Mesh,
    transform: &[f64; 12],
    ctx: &RunContext,
) -> Result<Mesh, IoError> {
    let span = ctx.span(
        TRANSFORMING,
        0.0,
        1.0,
        mesh.vertex_count() + mesh.face_count(),
    );
    if *transform == threemf::IDENTITY {
        ctx.checkpoint()?;
        span.finish();
        return Ok(mesh.clone());
    }
    let t = transform;
    let apply = |p: [f64; 3]| {
        [
            t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9],
            t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
            t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11],
        ]
    };
    // Determinant of the linear part (3MF's row-major rows are this
    // matrix's columns, which doesn't change the determinant).
    let det = t[0] * (t[4] * t[8] - t[5] * t[7]) - t[1] * (t[3] * t[8] - t[5] * t[6])
        + t[2] * (t[3] * t[7] - t[4] * t[6]);
    // Live vertices renumbered densely in slot order, as `Mesh::to_arrays`.
    let mut slots = Vec::new();
    let mut positions = Vec::with_capacity(mesh.vertex_count());
    for (n, (id, p)) in mesh.vertices().enumerate() {
        span.at(n)?;
        let q = apply(p);
        if !q.iter().all(|c| c.is_finite()) {
            return Err(IoError::TransformOutOfRange { vertex: n });
        }
        let slot = id.0 as usize;
        if slots.len() <= slot {
            slots.resize(slot + 1, u32::MAX);
        }
        slots[slot] = n as u32;
        positions.push(q);
    }
    let base = positions.len();
    let mut triangles = Vec::with_capacity(mesh.face_count());
    for (n, (_, tri)) in mesh.faces().enumerate() {
        span.at(base + n)?;
        let mut tri = tri.map(|v| slots[v as usize]);
        if det < 0.0 {
            tri.swap(1, 2);
        }
        triangles.push(tri);
    }
    let mesh = Mesh::new_with(positions, triangles, ctx)??;
    span.finish();
    Ok(mesh)
}

/// Export `mesh`, never leaving a partial file behind. `unit` is the
/// document's unit. A 3MF export writes it as the model's own unit
/// (MF-ISSUE-035). STL and OBJ have no unit of their own, so by the slicer
/// convention they get millimetre coordinates: if `unit` isn't already
/// `Millimeter`, the mesh is scaled before writing (consolidation plan
/// C06). Use [`write_path_keep_doc_unit`] to write the raw doc-unit
/// coordinates instead.
///
/// The file is encoded in memory, re-parsed to verify it, written to a temp
/// file in the same directory, flushed to disk, and renamed over `path`.
/// If any step fails, an existing file at `path` is left unchanged.
pub fn write_path(
    mesh: &Mesh,
    path: &Path,
    format: Format,
    unit: threemf::Unit,
) -> Result<(), IoError> {
    write_path_with(mesh, path, format, unit, &RunContext::none())
}

/// As [`write_path`], but a unitless (STL/OBJ) export writes `unit`'s raw
/// doc-unit coordinates rather than converting to millimetres. There is no
/// cancellable `_with` variant: callers who need one can scale the mesh
/// themselves (see [`write_path_with`]'s conversion) and call
/// [`write_path_with`] with `Unit::Millimeter`.
pub fn write_path_keep_doc_unit(
    mesh: &Mesh,
    path: &Path,
    format: Format,
    unit: threemf::Unit,
) -> Result<(), IoError> {
    write_path_with_opts(mesh, path, format, unit, true, &RunContext::none())
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
    unit: threemf::Unit,
    ctx: &RunContext,
) -> Result<(), IoError> {
    write_path_with_opts(mesh, path, format, unit, false, ctx)
}

/// As [`write_path_with`], with `keep_doc_unit` true meaning "write a
/// unitless (STL/OBJ) export's raw doc-unit coordinates", false meaning
/// "convert to millimetres first" (the slicer convention, and the
/// default every public entry point above uses).
fn write_path_with_opts(
    mesh: &Mesh,
    path: &Path,
    format: Format,
    unit: threemf::Unit,
    keep_doc_unit: bool,
    ctx: &RunContext,
) -> Result<(), IoError> {
    let fail = |reason: String| IoError::Export {
        path: path.to_owned(),
        reason,
    };
    ctx.progress.report(WRITING, 0.0);
    ctx.checkpoint()?;
    // STL and OBJ carry no unit: write millimetres (what every slicer
    // assumes) unless the caller asked to keep the document's raw
    // coordinates. A 3MF records `unit` itself, so its coordinates are
    // never rescaled here.
    let scaled = if !keep_doc_unit
        && matches!(format, Format::StlBinary | Format::StlAscii | Format::Obj)
        && unit != threemf::Unit::Millimeter
    {
        let factor = unit.mm_per();
        let scale = [
            factor, 0.0, 0.0, 0.0, factor, 0.0, 0.0, 0.0, factor, 0.0, 0.0, 0.0,
        ];
        Some(transform_mesh(mesh, &scale, ctx)?)
    } else {
        None
    };
    let mesh = scaled.as_ref().unwrap_or(mesh);
    let bytes = match format {
        Format::StlBinary => stl::write_binary(mesh).map_err(fail)?,
        Format::StlAscii => stl::write_ascii(mesh).into_bytes(),
        Format::Obj => obj::write(mesh).into_bytes(),
        Format::ThreeMf => {
            let model = threemf::Model {
                metadata: Default::default(),
                unit,
                objects: vec![threemf::ModelObject {
                    metadata: Default::default(),
                    name: "object".into(),
                    mesh: mesh.clone(),
                    transform: threemf::IDENTITY,
                }],
            };
            return write_model_path_with(&model, path, ctx).map(|_warnings| ());
        }
    };
    ctx.progress.report(WRITING, WRITE_FROM * 0.8);
    ctx.checkpoint()?;
    verify_encoded(mesh, &bytes, format).map_err(fail)?;
    atomic_write(path, &bytes, ctx)
}

/// Export a whole model as 3MF (objects, transforms, unit), verified and
/// atomic like [`write_path`]. Empty objects are left out, named in a
/// warning (MF-ISSUE-034); see [`write_model_path_with`] for the warnings.
pub fn write_model_path(model: &threemf::Model, path: &Path) -> Result<(), IoError> {
    write_model_path_with(model, path, &RunContext::none()).map(|_warnings| ())
}

/// As [`write_model_path`], cancellable like [`write_path_with`], returning
/// warnings on success (for example, an empty object left out of the
/// export, named — MF-ISSUE-034).
pub fn write_model_path_with(
    model: &threemf::Model,
    path: &Path,
    ctx: &RunContext,
) -> Result<Vec<String>, IoError> {
    let fail = |reason: String| IoError::Export {
        path: path.to_owned(),
        reason,
    };
    ctx.progress.report(WRITING, 0.0);
    ctx.checkpoint()?;
    let (bytes, write_warnings) = threemf::write_filtering_empty(model).map_err(fail)?;
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
    // Compared against the model with empty objects already left out
    // (MF-ISSUE-034), since those never make it into `bytes`.
    let kept = threemf::Model {
        metadata: Default::default(),
        unit: model.unit,
        objects: model
            .objects
            .iter()
            .filter(|o| !o.mesh.is_empty())
            .cloned()
            .collect(),
    };
    if !models_match_for_verification(&kept, &back) {
        return Err(fail("verification found a different model".into()));
    }
    atomic_write(path, &bytes, ctx)?;
    Ok(write_warnings)
}

/// Whether two models, as read back after an export, describe the same
/// data: the same unit, the same objects in the same order with the same
/// name and transform, and meshes with exactly the same triangle indices
/// (not just the same face count and the same set of positions) and vertex
/// positions (MF-ISSUE-033).
pub fn models_match_for_verification(asked: &threemf::Model, got: &threemf::Model) -> bool {
    asked.unit == got.unit
        && asked.objects.len() == got.objects.len()
        && asked.objects.iter().zip(&got.objects).all(|(a, b)| {
            a.name == b.name
                && a.transform == b.transform
                && a.mesh.to_arrays() == b.mesh.to_arrays()
        })
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
