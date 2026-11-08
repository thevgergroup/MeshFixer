//! Core geometry types for MeshFixer.
//!
//! `mf-geometry` depends on nothing else in the workspace (spec §2). Working
//! geometry uses `f64` positions and `u32` indices (MF-GEO-001).

mod hash;
mod mesh;

pub use hash::ContentHash;
pub use mesh::{Aabb, GeometryError, Mesh};
