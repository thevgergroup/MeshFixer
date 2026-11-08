//! An offscreen colour + depth target the 3D view renders into.
//!
//! Frontends display the resolved colour texture, so the renderer never
//! depends on a UI toolkit's render pass formats (MF-RENDER-001).

use crate::{GpuMesh, MeshRenderer, OrbitCamera, RenderTargetFormat};

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const BACKGROUND_RGBA: [u8; 4] = [38, 41, 46, 255];

pub struct RenderTarget {
    pub width: u32,
    pub height: u32,
    pub samples: u32,
    msaa_view: Option<wgpu::TextureView>,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
}

impl RenderTarget {
    pub fn format(samples: u32) -> RenderTargetFormat {
        RenderTargetFormat {
            color: COLOR_FORMAT,
            depth: DEPTH_FORMAT,
            samples,
        }
    }

    pub fn new(device: &wgpu::Device, width: u32, height: u32, samples: u32) -> RenderTarget {
        let (width, height) = (width.max(1), height.max(1));
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = |label, format, usage, sample_count| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let resolved = texture(
            "mf view color",
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            1,
        );
        let msaa_view = (samples > 1).then(|| {
            texture(
                "mf view msaa",
                COLOR_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                samples,
            )
            .create_view(&Default::default())
        });
        let depth_view = texture(
            "mf view depth",
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            samples.max(1),
        )
        .create_view(&Default::default());
        RenderTarget {
            width,
            height,
            samples,
            msaa_view,
            resolved_view: resolved.create_view(&Default::default()),
            resolved,
            depth_view,
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.resolved
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.resolved_view
    }

    /// Render one frame and submit it. `renderer` must have been built with
    /// [`RenderTarget::format`] for the same sample count.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &MeshRenderer,
        mesh: Option<(&GpuMesh, &OrbitCamera)>,
        show_boundaries: bool,
    ) {
        if let Some((gpu, camera)) = mesh {
            renderer.prepare(queue, gpu, camera, self.width as f32 / self.height as f32);
        }
        let bg = BACKGROUND_RGBA.map(|c| c as f64 / 255.0);
        let (view, resolve_target) = match &self.msaa_view {
            Some(msaa) => (msaa, Some(&self.resolved_view)),
            None => (&self.resolved_view, None),
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mf view"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
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
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some((gpu, _)) = mesh {
                renderer.paint(&mut pass, gpu, show_boundaries);
            }
        }
        queue.submit([encoder.finish()]);
    }
}
