//! wgpu mesh rendering, independent of any UI toolkit (MF-RENDER-001).
//!
//! Positions are uploaded as `f32` relative to a per-mesh local origin, so
//! precision holds far from the world origin (MF-GEO-001). Render buffers
//! are never read back as geometry.

mod bvh;
mod camera;
mod gpu_mesh;
pub mod offscreen;
mod pick;
mod renderer;
mod target;
mod timer;
mod transform;

pub use bvh::{Bvh, LEAF_MAX};
pub use camera::OrbitCamera;
pub use gpu_mesh::{
    ChunkVertices, FaceStates, GpuLines, GpuMesh, LineVertex, MeshVertices, ORIGIN_REACH,
    SurfaceVertex, UploadStats, boundary_line_vertices, boundary_line_vertices_skipping,
    chunk_ranges, edge_line_vertices, loop_line_vertices, render_origin, surface_vertices,
};
pub use pick::{
    EdgeCandidate, LoopEdgeCandidate, edge_candidates, edge_occluded, loop_edge_candidates,
    pick_loop, ray_pick, screen_position,
};
pub use renderer::{DrawItem, FaceTints, Layer, MeshRenderer, Palette, RenderTargetFormat};
pub use target::RenderTarget;
pub use timer::GpuTimer;
pub use transform::model_matrix;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no GPU adapter is available")]
    NoAdapter,
    #[error("failed to create GPU device: {0}")]
    Device(String),
    #[error("render size {width}x{height} exceeds the GPU's maximum texture size {max}")]
    TooLarge { width: u32, height: u32, max: u32 },
    #[error("failed to read back rendered image: {0}")]
    Readback(String),
}
