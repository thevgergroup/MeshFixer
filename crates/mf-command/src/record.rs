//! Command records (MF-CMD-002) and the information needed to re-execute
//! a command against a different input revision later (MF-HIST-002).

use mf_document::{ObjectId, RevisionId};
use mf_io::SourceInfo;
use mf_selection::Selection;
use mf_spec::Params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedSummary {
    pub removed: u64,
    pub created: u64,
    pub kept: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Ok,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: CommandId,
    pub op_id: String,
    pub op_version: u32,
    pub target: ObjectId,
    /// `None` means the whole object.
    pub selection: Option<Selection>,
    pub params: Params,
    pub input_revision: RevisionId,
    /// Content hash (hex) of the target's geometry before the command.
    pub input_hash: String,
    /// Structure hash (hex): everything but positions (MF-CMD-004).
    pub input_structure_hash: String,
    pub output_revision: RevisionId,
    pub output_hash: String,
    pub output_structure_hash: String,
    /// Extent and area of the output, used when a replay's structure matches
    /// but positions differ. Computed when the log is saved, not on commit,
    /// so commits stay cheap on large meshes.
    pub output_summary: Option<GeometrySummary>,
    /// Commands that produced this command's inputs (empty if an import did).
    pub depends_on: Vec<CommandId>,
    pub affected_objects: Vec<ObjectId>,
    pub affected: AffectedSummary,
    pub engine_version: String,
    /// `<arch>-<os>`.
    pub platform: String,
    pub duration_ms: f64,
    pub status: CommandStatus,
    /// Every operand the command read (MF-P1-SEL-001), index-aligned with
    /// the operation's operands: entry 0 is the target (named `target`),
    /// the rest follow the spec's `operands:` order. Empty in records
    /// written before Phase 1 (`mfx-log/1`), which had only the target.
    #[serde(default)]
    pub operands: Vec<RecordedOperand>,
    /// Every object the command changed, created or removed, with what it
    /// became (D6). A replay compares each, and the command's verdict is
    /// the worst of them. Empty in `mfx-log/1` records, whose only output
    /// is the target's (`output_hash`).
    #[serde(default)]
    pub outputs: Vec<ObjectOutput>,
}

/// One operand of a recorded command, as it was read (MF-P1-SEL-001).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedOperand {
    /// `target` for operand 0; otherwise the spec's operand name.
    pub name: String,
    pub object: ObjectId,
    /// `None` means the whole object.
    pub selection: Option<Selection>,
    /// Content hash (hex) of the operand's geometry when it was read.
    pub input_revision_hash: String,
    /// Structure hash (hex) of the same.
    pub input_structure_hash: String,
    /// The operand's placement when it was read (D7).
    pub input_transform: [f64; 12],
}

/// One object a command changed, created or removed, as it was after the
/// command (D6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectOutput {
    pub object: ObjectId,
    /// The command created this object.
    pub created: bool,
    /// The command removed this object; `hash` and `structure_hash` are
    /// empty and `transform` is its last placement.
    pub removed: bool,
    /// Content hash (hex) of its geometry after the command (unchanged
    /// geometry keeps its input hash).
    pub hash: String,
    pub structure_hash: String,
    /// Its placement after the command.
    pub transform: [f64; 12],
    /// Extent and area, for an `Equivalent` verdict across platforms;
    /// computed when the log is saved, like `output_summary`.
    #[serde(default)]
    pub summary: Option<GeometrySummary>,
}

/// How an object entered the session.
#[derive(Debug, Clone, Serialize)]
pub struct ImportRecord {
    pub object: ObjectId,
    pub name: String,
    pub source: Option<SourceInfo>,
    /// Which object of the source file (3MF can hold several).
    pub part: usize,
}

pub fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Compact description of a mesh revision: topological invariants plus
/// extent and area. Enough to tell "same geometry, different bits" (another
/// platform's floating point) from "different result".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometrySummary {
    pub faces: usize,
    pub vertices: usize,
    pub components: usize,
    pub boundary_loops: usize,
    pub euler: i64,
    /// `[min, max]`, `None` for an empty mesh.
    pub bbox: Option<[[f64; 3]; 2]>,
    pub surface_area: Option<f64>,
}

impl GeometrySummary {
    pub fn of(mesh: &mf_geometry::Mesh) -> GeometrySummary {
        let d = mf_geometry::inspect(mesh);
        GeometrySummary {
            faces: d.triangle_count,
            vertices: d.vertex_count,
            components: d.components,
            boundary_loops: d.boundary_loops.len(),
            euler: mesh.vertex_count() as i64 - mesh.topology().edge_count as i64
                + mesh.face_count() as i64,
            bbox: d.bounding_box.map(|b| [b.min, b.max]),
            surface_area: d.surface_area,
        }
    }

    /// `(largest bounding-box coordinate difference, relative area
    /// difference)` if the invariants match exactly; `None` if they don't.
    pub fn deviation(&self, other: &GeometrySummary) -> Option<(f64, f64)> {
        let same = self.faces == other.faces
            && self.vertices == other.vertices
            && self.components == other.components
            && self.boundary_loops == other.boundary_loops
            && self.euler == other.euler;
        if !same {
            return None;
        }
        let bbox = match (self.bbox, other.bbox) {
            (Some(a), Some(b)) => (0..2)
                .flat_map(|i| (0..3).map(move |k| (i, k)))
                .map(|(i, k)| (a[i][k] - b[i][k]).abs())
                .fold(0.0, f64::max),
            (None, None) => 0.0,
            _ => return None,
        };
        let area = match (self.surface_area, other.surface_area) {
            (Some(a), Some(b)) => (a - b).abs() / a.abs().max(f64::MIN_POSITIVE),
            (None, None) => 0.0,
            _ => return None,
        };
        Some((bbox, area))
    }

    /// Scale for tolerances: the bounding-box diagonal.
    pub fn diagonal(&self) -> f64 {
        self.bbox.map_or(0.0, |[a, b]| {
            ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt()
        })
    }
}
