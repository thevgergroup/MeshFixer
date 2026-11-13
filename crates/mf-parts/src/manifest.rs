//! A part's manifest (D16): `part.yaml` next to its mesh.

use mf_spec::Params;
use serde::{Deserialize, Serialize};

/// Whether a part is a closed solid or an open "stamp" (capped when
/// joined, D14), or a connector (Task 28, MF-MODEL-005): "Save to
/// library" on a connector object writes `Connector { connector,
/// params }` instead of a mesh file, and dropping it from the library
/// regenerates its parts through [`crate::Connector::generate`] (and
/// `object.add_connector`) rather than reading a stored mesh — a
/// connector's library entry is its recipe, not a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Solid,
    Open,
    Connector { connector: String, params: Params },
}

/// Where a part attaches, in part space: the point that sits on the target
/// surface, the direction pointing out of that surface, and the part's
/// "up" (which way the tangent points).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    pub point: [f64; 3],
    pub normal: [f64; 3],
    pub up: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartManifest {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub licence: String,
    /// Where the part came from (author, URL, "MeshFixer built-in").
    pub source: String,
    pub kind: PartKind,
    pub attach: Attach,
    pub default_scale: f64,
    /// The mesh file next to `part.yaml` (user parts; empty for built-ins).
    #[serde(default)]
    pub mesh: String,
    /// Where it's filed in the library ("Brackets"); empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// What it is, in the person's own words; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl PartManifest {
    /// Problems that make the manifest unusable, in plain words.
    pub fn check(&self) -> Result<(), String> {
        let id_ok = !self.id.is_empty()
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if !id_ok {
            return Err(format!(
                "id '{}' must be letters, digits, '-', '_' or '.'",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("the name is empty".into());
        }
        if !(self.default_scale > 0.0 && self.default_scale.is_finite()) {
            return Err(format!(
                "default_scale {} must be more than 0",
                self.default_scale
            ));
        }
        let len = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let direction = |v: [f64; 3]| len(v) > 0.0 && len(v).is_finite();
        if !direction(self.attach.normal) || !direction(self.attach.up) {
            return Err("attach normal and up must be directions (not zero)".into());
        }
        if !self.attach.point.iter().all(|c| c.is_finite()) {
            return Err("attach point must be finite".into());
        }
        Ok(())
    }
}
