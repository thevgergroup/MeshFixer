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
    pub faces: Vec<FaceId>,
    pub loops: Vec<u32>,
}

/// How an edit changed a mesh (MF-SEL-002). Kept faces are implicit: live
/// in both revisions with the same ID and indices. Lists are sorted by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub removed_faces: Vec<FaceId>,
    pub created_faces: Vec<(FaceId, Sources)>,
    pub removed_vertices: Vec<VertexId>,
    pub created_vertices: Vec<VertexId>,
    pub moved_vertices: Vec<VertexId>,
}

impl Provenance {
    pub fn is_empty(&self) -> bool {
        self.removed_faces.is_empty()
            && self.created_faces.is_empty()
            && self.removed_vertices.is_empty()
            && self.created_vertices.is_empty()
            && self.moved_vertices.is_empty()
    }
}
