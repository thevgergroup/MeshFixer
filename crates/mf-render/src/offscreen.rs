//! Headless rendering to an image, for the CLI and snapshot tests.

use image::RgbaImage;
use mf_geometry::Mesh;

pub use crate::target::BACKGROUND_RGBA;
use crate::{GpuMesh, MeshRenderer, OrbitCamera, RenderError, RenderTarget};

/// Offscreen renders use MSAA like the desktop view, so the same path is tested.
const SAMPLES: u32 = 4;

fn create_device() -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|_| RenderError::NoAdapter)?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|e| RenderError::Device(e.to_string()))
}

/// One headless device per process, created on first use and shared by all
/// offscreen renders. Creating and dropping devices concurrently crashes
/// Mesa's lavapipe Vulkan driver (reproduced in CI: 3/15 runs segfaulted with
/// per-call devices); a wgpu device is safe to share across threads.
fn device() -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    static DEVICE: std::sync::OnceLock<Result<(wgpu::Device, wgpu::Queue), RenderError>> =
        std::sync::OnceLock::new();
    DEVICE.get_or_init(create_device).clone()
}

/// True if a GPU adapter can be created (used to gate GPU tests).
pub fn gpu_available() -> bool {
    device().is_ok()
}

/// Render `mesh` framed by a default camera.
pub fn render_image(
    mesh: &Mesh,
    width: u32,
    height: u32,
    show_boundaries: bool,
) -> Result<RgbaImage, RenderError> {
    render_image_with_chunk_limit(mesh, width, height, show_boundaries, u64::MAX)
}

/// As [`render_image`], with GPU buffers capped at `max_chunk_bytes` (testing).
pub fn render_image_with_chunk_limit(
    mesh: &Mesh,
    width: u32,
    height: u32,
    show_boundaries: bool,
    max_chunk_bytes: u64,
) -> Result<RgbaImage, RenderError> {
    let (device, queue) = device()?;
    let target = RenderTarget::new(&device, width, height, SAMPLES)?;
    let renderer = MeshRenderer::new(&device, RenderTarget::format(SAMPLES));
    let limit = max_chunk_bytes.min(device.limits().max_buffer_size);
    let gpu = GpuMesh::from_vertices_with_limit(&device, &crate::MeshVertices::build(mesh), limit);
    let camera = mesh.bounding_box().map(|bb| OrbitCamera::fit(&bb));
    let drawable = camera
        .as_ref()
        .filter(|_| !mesh.is_empty())
        .map(|c| (&gpu, c));
    target.draw(&device, &queue, &renderer, drawable, show_boundaries);
    read_back(&device, &queue, &target)
}

/// Copy a target's resolved colour into an image.
pub fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
) -> Result<RgbaImage, RenderError> {
    let (width, height) = (target.width, target.height);
    let row = width * 4;
    let padded =
        row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mf readback"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    rx.recv()
        .map_err(|e| RenderError::Readback(e.to_string()))?
        .map_err(|e| RenderError::Readback(e.to_string()))?;

    let data = readback
        .get_mapped_range(..)
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let mut pixels = Vec::with_capacity((row * height) as usize);
    for y in 0..height {
        let start = (y * padded) as usize;
        pixels.extend_from_slice(&data[start..start + row as usize]);
    }
    drop(data);
    readback.unmap();
    RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| RenderError::Readback("image size mismatch".into()))
}
