//! Headless rendering to an image, for the CLI and snapshot tests.

use image::RgbaImage;
use mf_geometry::Mesh;

use crate::{GpuMesh, MeshRenderer, OrbitCamera, RenderError, RenderTargetFormat};

pub const BACKGROUND_RGBA: [u8; 4] = [38, 41, 46, 255];
const COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

fn device() -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|_| RenderError::NoAdapter)?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|e| RenderError::Device(e.to_string()))
}

/// Render `mesh` framed by a default camera.
pub fn render_image(
    mesh: &Mesh,
    width: u32,
    height: u32,
    show_boundaries: bool,
) -> Result<RgbaImage, RenderError> {
    let (device, queue) = device()?;
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = |format, usage, label| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = texture(
        COLOR,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        "mf offscreen color",
    );
    let depth = texture(
        DEPTH,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
        "mf offscreen depth",
    );
    let color_view = color.create_view(&Default::default());
    let depth_view = depth.create_view(&Default::default());

    let renderer = MeshRenderer::new(
        &device,
        RenderTargetFormat {
            color: COLOR,
            depth: DEPTH,
            samples: 1,
        },
    );
    let gpu = GpuMesh::upload(&device, mesh);
    if let Some(bb) = mesh.bounding_box() {
        renderer.prepare(
            &queue,
            &gpu,
            &OrbitCamera::fit(&bb),
            width as f32 / height as f32,
        );
    }

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
    {
        let bg = BACKGROUND_RGBA.map(|c| c as f64 / 255.0);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mf offscreen"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: bg[0],
                        g: bg[1],
                        b: bg[2],
                        a: bg[3],
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !mesh.is_empty() {
            renderer.paint(&mut pass, &gpu, show_boundaries);
        }
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
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
        size,
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
