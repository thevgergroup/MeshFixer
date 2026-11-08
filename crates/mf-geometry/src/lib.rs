//! Core geometry types for MeshFixer.
//!
//! `mf-geometry` depends on nothing else in the workspace (spec §2). Working
//! geometry uses `f64` positions and `u32` indices (MF-GEO-001).

mod chunked;
mod compare;
mod diagnostics;
mod edit;
mod grid;
mod hash;
mod ids;
mod mesh;
mod topology;

pub use chunked::{CHUNK, chunk_hashes_computed};
pub use compare::{Equivalence, equivalent};
pub use diagnostics::{Diagnostics, LoopSummary, VolumeStatus, inspect};
pub use edit::MeshEdit;
pub use hash::ContentHash;
pub use ids::{FaceId, Provenance, SlotRemap, Sources, VertexId};
pub use mesh::{Aabb, Compaction, GeometryError, Mesh};
pub use topology::{BoundaryLoop, Topology};
