//! wgpu mesh rendering, independent of any UI toolkit (MF-RENDER-001).
//!
//! Positions are uploaded as `f32` relative to a per-mesh local origin, so
//! precision holds far from the world origin (MF-GEO-001). Render buffers
//! are never read back as geometry.

mod camera;
mod gpu_mesh;
pub mod offscreen;
mod renderer;
mod target;

pub use camera::OrbitCamera;
pub use gpu_mesh::{
    GpuMesh, LineVertex, MeshVertices, SurfaceVertex, boundary_line_vertices, chunk_ranges,
    surface_vertices,
};
pub use renderer::{MeshRenderer, RenderTargetFormat};
pub use target::RenderTarget;

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
