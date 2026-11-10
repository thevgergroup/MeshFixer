use serde::{Deserialize, Serialize};

/// A face slot. Stable across revisions: a kept face keeps its ID, a removed
/// face's slot is tombstoned and never reused (MF-SEL-001, ADR-0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FaceId(pub u32);

/// A vertex slot. Same stability rules as [`FaceId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VertexId(pub u32);

/// What a created face was made from (input faces and/or boundary loops).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    /// Faces of the same object's previous revision.
    pub faces: Vec<FaceId>,
    pub loops: Vec<u32>,
    /// Faces of the command's operands, by operand index (0 is the
    /// command's target) and the face's id in that operand's input
    /// revision (MF-P1-SEL-001). A face of a newly created object names
    /// all of its sources here. Absent in Phase 0 records.
    #[serde(default)]
    pub operands: Vec<(u32, FaceId)>,
}

/// Old-slot → new-slot maps (`None` = the slot is gone). Produced by
/// compaction, the one change that renumbers kept elements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRemap {
    pub faces: Vec<Option<FaceId>>,
    pub vertices: Vec<Option<VertexId>>,
}

/// How an edit changed a mesh (MF-SEL-002). Kept faces are implicit: live
/// in both revisions with the same ID (and, unless listed in
/// `modified_faces`, the same indices). Lists are sorted by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub removed_faces: Vec<FaceId>,
    pub created_faces: Vec<(FaceId, Sources)>,
    /// Kept faces whose vertex indices changed (same ID, so selections keep
    /// them): edge flips, welds.
    pub modified_faces: Vec<FaceId>,
    pub removed_vertices: Vec<VertexId>,
    pub created_vertices: Vec<VertexId>,
    pub moved_vertices: Vec<VertexId>,
    /// Set only when slots were renumbered (compaction). Applied after the
    /// lists above, which use the old numbering.
    pub remap: Option<SlotRemap>,
}

impl Provenance {
    /// Provenance of a compaction: nothing is created or removed, every
    /// live slot is renumbered (ADR-0001, MF-ISSUE-020).
    pub fn from_compaction(map: &SlotRemap) -> Provenance {
        Provenance {
            remap: Some(map.clone()),
            ..Provenance::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.removed_faces.is_empty()
            && self.created_faces.is_empty()
            && self.modified_faces.is_empty()
            && self.remap.is_none()
            && self.removed_vertices.is_empty()
            && self.created_vertices.is_empty()
            && self.moved_vertices.is_empty()
    }
}
