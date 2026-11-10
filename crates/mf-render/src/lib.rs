//! wgpu mesh rendering, independent of any UI toolkit (MF-RENDER-001).
//!
//! Positions are uploaded as `f32` relative to a per-mesh local origin, so
//! precision holds far from the world origin (MF-GEO-001). Render buffers
//! are never read back as geometry.

pub mod bench;
mod bvh;
mod camera;
mod gizmo;
mod gpu_mesh;
pub mod offscreen;
mod overlay;
mod pick;
mod renderer;
mod target;
mod timer;
mod transform;

pub use bvh::{Bvh, LEAF_MAX};
pub use camera::{CameraMove, OrbitCamera, clip_radius, ease_out};
pub use gizmo::{
    Decomposed, Drag, Gizmo, GizmoMode, Handle, HandleShape, PlateSnap, Snap, compose, decompose,
};
pub use gpu_mesh::{
    ChunkVertices, FaceStates, GpuLines, GpuMesh, LineVertex, MeshVertices, ORIGIN_REACH,
    SurfaceVertex, UploadStats, boundary_line_vertices, boundary_line_vertices_skipping,
    chunk_ranges, edge_line_vertices, loop_line_vertices, render_origin, surface_vertices,
    wait_for_submitted,
};
pub use overlay::{
    GIZMO_LINE_WIDTH_PT, GizmoPalette, GizmoVertex, Plate, PlateVertex, gizmo_vertices,
    plate_vertices,
};
pub use pick::{
    EdgeCandidate, LoopEdgeCandidate, edge_candidates, edge_occluded, edge_occluded_with,
    loop_edge_candidates, pick_loop, ray_pick, ray_pick_with, screen_position,
};
pub use renderer::{
    DrawItem, FaceTints, Layer, MeshRenderer, Palette, RenderTargetFormat, SelectionCue,
};
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
