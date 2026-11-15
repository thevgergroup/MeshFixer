//! Core geometry types for MeshFixer.
//!
//! `mf-geometry` depends on nothing else in the workspace except `mf-runtime`
//! (cancellation and progress; spec §2). Working geometry uses `f64`
//! positions and `u32` indices (MF-GEO-001).

pub mod affine;
mod bvh;
mod chunked;
mod compare;
mod diagnostics;
mod distance;
mod edit;
pub mod exact;
mod grid;
mod hash;
mod ids;
mod intersect;
mod materials;
mod mesh;
mod topology;
mod validate;

pub use bvh::ray as bvh_ray;
pub use bvh::{Bvh, LEAF_MAX};
pub use chunked::{CHUNK, STORAGE_CHUNK, chunk_hashes_computed};
pub use compare::{Equivalence, equivalent};
pub use diagnostics::{Diagnostics, LoopSummary, VolumeStatus, inspect, inspect_with};
pub use distance::{minimum_distance, minimum_distance_within};
pub use edit::MeshEdit;
pub use hash::ContentHash;
pub use ids::{FaceId, Provenance, SlotRemap, Sources, VertexId};
pub use intersect::{
    IntersectionRegion, IntersectionReport, TriTri, intersections_between, self_intersections,
    tri_tri_intersect,
};
pub use materials::{Material, NO_MATERIAL};
pub use mesh::{Aabb, Compaction, GeometryError, Mesh};
pub use topology::{BoundaryLoop, Topology};
pub use validate::{SolidDefect, signed_volume, validate_solid, validate_solid_with};
