//! Headless rendering to an image, for the CLI and snapshot tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glam::DMat4;
use image::RgbaImage;
use mf_geometry::Mesh;

pub use crate::target::BACKGROUND_RGBA;
use crate::{DrawItem, GpuMesh, MeshRenderer, OrbitCamera, Palette, RenderError, RenderTarget};

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

/// One headless device per process, shared by all offscreen renders.
/// Creating and dropping devices concurrently crashes Mesa's lavapipe Vulkan
/// driver (reproduced in CI: 3/15 runs segfaulted with per-call devices); a
/// wgpu device is safe to share across threads.
///
/// Only working devices are cached (MF-ISSUE-024): a failed creation is
/// returned and retried next call, and a device that wgpu reports lost is
/// replaced on the next call.
pub fn shared_device() -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    struct Shared {
        device: wgpu::Device,
        queue: wgpu::Queue,
        lost: Arc<AtomicBool>,
    }
    static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
    // A panic while holding this lock leaves at worst a stale `Some`, which
    // the `lost` check below handles, so a poisoned lock is safe to reuse.
    let mut slot = SHARED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(s) = slot.as_ref() {
        // wgpu delivers device-lost callbacks while polling; a non-blocking
        // poll flushes any pending notification before we decide to reuse.
        // A failed poll also means the device is unusable.
        let polled = s.device.poll(wgpu::PollType::Poll).is_ok();
        if polled && !s.lost.load(Ordering::SeqCst) {
            return Ok((s.device.clone(), s.queue.clone()));
        }
    }
    let (device, queue) = create_device()?;
    let lost = Arc::new(AtomicBool::new(false));
    let flag = lost.clone();
    device.set_device_lost_callback(move |_reason, _message| flag.store(true, Ordering::SeqCst));
    *slot = Some(Shared {
        device: device.clone(),
        queue: queue.clone(),
        lost,
    });
    Ok((device, queue))
}

/// True if a GPU adapter can be created (used to gate GPU tests).
pub fn gpu_available() -> bool {
    shared_device().is_ok()
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
    let (device, queue) = shared_device()?;
    let target = RenderTarget::new(&device, width, height, SAMPLES)?;
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(SAMPLES), Palette::HEADLESS);
    let limit = max_chunk_bytes.min(device.limits().max_buffer_size);
    let gpu = GpuMesh::from_vertices_with_limit(&device, &crate::MeshVertices::build(mesh), limit);
    let camera = mesh.bounding_box().map(|bb| OrbitCamera::fit(&bb));
    let items = [DrawItem {
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
    }];
    let items = if mesh.is_empty() { &items[..0] } else { &items };
    target.draw(
        &device,
        &queue,
        &mut renderer,
        camera.as_ref().filter(|_| !mesh.is_empty()),
        items,
        show_boundaries,
        &[],
    );
    read_back(&device, &queue, &target)
}

/// Render several objects, each placed by its own object-to-world matrix
/// (MF-ISSUE-042), framed by `cam` (`cam.target` is the frame every mesh's
/// local origin and `model` are shifted into). For tests that need to
/// render more than one transformed object without going through a full
/// document.
pub fn render_objects(
    objects: &[(&Mesh, DMat4)],
    cam: &OrbitCamera,
    width: u32,
    height: u32,
    show_boundaries: bool,
) -> Result<RgbaImage, RenderError> {
    let (device, queue) = shared_device()?;
    let target = RenderTarget::new(&device, width, height, SAMPLES)?;
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(SAMPLES), Palette::HEADLESS);
    let gpus: Vec<GpuMesh> = objects
        .iter()
        .map(|(mesh, _)| GpuMesh::upload(&device, mesh))
        .collect();
    let items: Vec<DrawItem<'_>> = gpus
        .iter()
        .zip(objects)
        .map(|(gpu, (_, model))| DrawItem {
            mesh: gpu,
            model: *model,
            inactive: false,
        })
        .collect();
    target.draw(
        &device,
        &queue,
        &mut renderer,
        Some(cam),
        &items,
        show_boundaries,
        &[],
    );
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
        // allow-swallow: the receiver is read right after this; a send can only fail if it was dropped
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
