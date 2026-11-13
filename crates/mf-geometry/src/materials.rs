//! Per-face materials and colours (MF-ISSUE-091, UI plan §11B): a mesh
//! carries a palette, an optional object-wide default and optional
//! per-face overrides. Multi-material and multi-colour printers need them
//! to survive every operation; the command layer carries them through
//! provenance (`mf_operations::materials`).
//!
//! Faces are stored as palette indices in copy-on-write chunks alongside
//! the faces, so an edit shares them like everything else. A mesh with no
//! palette, default or overrides has no material data, and its hashes are
//! exactly what they were before materials existed.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chunked::Chunked;

/// "No material": a face that uses the mesh's default (if any).
pub const NO_MATERIAL: u32 = u32::MAX;

/// One material or colour: a name (empty for a plain colour) and an sRGB
/// colour with alpha.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub color: [u8; 4],
}

impl Material {
    /// `#RRGGBBAA`.
    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.color;
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    /// Parses `#RRGGBB` or `#RRGGBBAA` (3MF's colour form).
    pub fn parse_color(text: &str) -> Option<[u8; 4]> {
        let t = text.trim().strip_prefix('#')?;
        if !(t.len() == 6 || t.len() == 8) || !t.is_ascii() {
            return None;
        }
        // allow-swallow: any bad hex digit makes the whole colour invalid; the caller reports it by name
        let byte = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
        Some([
            byte(0)?,
            byte(2)?,
            byte(4)?,
            if t.len() == 8 { byte(6)? } else { 255 },
        ])
    }
}

/// A mesh's material data (see the module docs).
#[derive(Clone)]
pub(crate) struct MaterialData {
    pub(crate) palette: Arc<Vec<Material>>,
    pub(crate) default: u32,
    /// Per face slot, a palette index or [`NO_MATERIAL`]. `None`: every
    /// face uses the default.
    pub(crate) faces: Option<Chunked<u32>>,
}

impl Default for MaterialData {
    fn default() -> Self {
        MaterialData {
            palette: Arc::new(Vec::new()),
            default: NO_MATERIAL,
            faces: None,
        }
    }
}

impl MaterialData {
    pub(crate) fn is_empty(&self) -> bool {
        self.palette.is_empty() && self.default == NO_MATERIAL && self.faces.is_none()
    }

    pub(crate) fn hash_into(&self, h: &mut blake3::Hasher) {
        if self.is_empty() {
            return;
        }
        h.update(b"mf-materials-v1");
        h.update(&(self.palette.len() as u64).to_le_bytes());
        for m in self.palette.iter() {
            h.update(&(m.name.len() as u64).to_le_bytes());
            h.update(m.name.as_bytes());
            h.update(&m.color);
        }
        h.update(&self.default.to_le_bytes());
        match &self.faces {
            None => {
                h.update(&[0]);
            }
            Some(f) => {
                h.update(&[1]);
                f.hash_into(h);
            }
        }
    }
}
