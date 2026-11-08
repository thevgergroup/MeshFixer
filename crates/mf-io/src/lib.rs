//! STL and OBJ import/export (MF-IO-001).
//!
//! Readers never panic on malformed input. They return structured
//! [`IoError`]s and check declared sizes before allocating (MF-IO-002).

use std::path::{Path, PathBuf};

use mf_geometry::{ContentHash, GeometryError, Mesh};
use serde::Serialize;

pub mod obj;
pub mod stl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Format {
    StlBinary,
    StlAscii,
    Obj,
}

impl Format {
    /// Format for writing, by extension. `.stl` writes binary STL.
    pub fn from_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "stl" => Some(Format::StlBinary),
            "obj" => Some(Format::Obj),
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
    #[error("file is truncated: expected {expected} bytes, found {actual}")]
    Truncated { expected: usize, actual: usize },
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("unsupported file format: {0}")]
    UnsupportedFormat(PathBuf),
}

pub fn read_path(path: &Path) -> Result<Imported, IoError> {
    let format =
        Format::from_path(path).ok_or_else(|| IoError::UnsupportedFormat(path.to_owned()))?;
    let bytes = std::fs::read(path)?;
    let content_hash = ContentHash(*blake3::hash(&bytes).as_bytes());
    let (mesh, precision, format, warnings) = match format {
        Format::Obj => {
            let text = std::str::from_utf8(&bytes).map_err(|e| IoError::Parse {
                line: 0,
                message: format!("not valid UTF-8: {e}"),
            })?;
            let (mesh, precision, warnings) = obj::read(text)?;
            (mesh, precision, Format::Obj, warnings)
        }
        Format::StlBinary | Format::StlAscii => {
            let (mesh, precision) = stl::read(&bytes)?;
            let format = match precision {
                SourcePrecision::F32 => Format::StlBinary,
                SourcePrecision::Decimal { .. } => Format::StlAscii,
            };
            (mesh, precision, format, Vec::new())
        }
    };
    Ok(Imported {
        mesh,
        source: SourceInfo {
            path: path.to_owned(),
            content_hash,
            format,
            precision,
        },
        warnings,
    })
}

pub fn write_path(mesh: &Mesh, path: &Path, format: Format) -> Result<(), IoError> {
    let bytes = match format {
        Format::StlBinary => stl::write_binary(mesh),
        Format::StlAscii => stl::write_ascii(mesh).into_bytes(),
        Format::Obj => obj::write(mesh).into_bytes(),
    };
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Count significant digits in a decimal literal's mantissa.
pub(crate) fn significant_digits(token: &str) -> u8 {
    let mantissa = token.split(['e', 'E']).next().unwrap_or("");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.trim_start_matches('0').len().min(u8::MAX as usize) as u8
}
