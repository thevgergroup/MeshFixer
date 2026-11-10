//! An offscreen colour + depth target the 3D view renders into.
//!
//! Frontends display the resolved colour texture, so the renderer never
//! depends on a UI toolkit's render pass formats (MF-RENDER-001).

use crate::{
    DrawItem, GpuTimer, Layer, MeshRenderer, OrbitCamera, RenderError, RenderTargetFormat,
};

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The headless palette's background ([`crate::Palette::HEADLESS`]).
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

    /// Largest width or height this device can render.
    pub fn max_dimension(device: &wgpu::Device) -> u32 {
        device.limits().max_texture_dimension_2d
    }

    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        samples: u32,
    ) -> Result<RenderTarget, RenderError> {
        let (width, height) = (width.max(1), height.max(1));
        let max = Self::max_dimension(device);
        if width > max || height > max {
            return Err(RenderError::TooLarge { width, height, max });
        }
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
        Ok(RenderTarget {
            width,
            height,
            samples,
            msaa_view,
            resolved_view: resolved.create_view(&Default::default()),
            resolved,
            depth_view,
        })
    }

    /// Bytes of the target's textures: the resolved colour, the
    /// multisampled colour (with MSAA) and the depth buffer.
    pub fn texture_bytes(&self) -> u64 {
        let pixels = u64::from(self.width) * u64::from(self.height);
        let samples = u64::from(self.samples.max(1));
        let color = 4; // Rgba8Unorm
        let depth = 4; // Depth32Float
        let msaa = if self.msaa_view.is_some() {
            pixels * samples * color
        } else {
            0
        };
        pixels * color + msaa + pixels * samples * depth
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.resolved
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.resolved_view
    }

    /// The depth attachment, for callers that open their own render pass
    /// instead of [`Self::draw`]/[`Self::draw_timed`] (the gizmo and plate
    /// offscreen tests, which also draw [`crate::MeshRenderer::draw_plate`]
    /// and [`crate::MeshRenderer::draw_gizmo`] into it).
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    /// The colour (MSAA, if any) attachment and resolve target a manual
    /// render pass should use, matching [`Self::draw_timed`]'s own choice.
    pub fn color_attachment(&self) -> (&wgpu::TextureView, Option<&wgpu::TextureView>) {
        match &self.msaa_view {
            Some(msaa) => (msaa, Some(&self.resolved_view)),
            None => (&self.resolved_view, None),
        }
    }

    /// Render one frame and submit it. `renderer` must have been built with
    /// [`RenderTarget::format`] for the same sample count. Every item of
    /// `items` is drawn at its own transform (MF-ISSUE-042), shifted into
    /// `camera.target`'s frame, then every overlay of `layers`. The clear
    /// colour is the renderer's palette background.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut MeshRenderer,
        camera: Option<&OrbitCamera>,
        items: &[DrawItem<'_>],
        show_boundaries: bool,
        layers: &[Layer<'_>],
    ) {
        self.draw_timed(
            device,
            queue,
            renderer,
            camera,
            items,
            show_boundaries,
            layers,
            None,
        );
    }

    /// As [`RenderTarget::draw`], timing the pass on the GPU as frame
    /// `frame` of `timer` (see [`GpuTimer::collect`]). Returns whether the
    /// pass was timed (it isn't while every read-back is still busy).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_timed(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut MeshRenderer,
        camera: Option<&OrbitCamera>,
        items: &[DrawItem<'_>],
        show_boundaries: bool,
        layers: &[Layer<'_>],
        timer: Option<(&mut GpuTimer, u64)>,
    ) -> bool {
        self.draw_with(
            device,
            queue,
            renderer,
            camera,
            items,
            show_boundaries,
            layers,
            &SceneExtras::default(),
            timer,
        )
    }

    /// As [`RenderTarget::draw_timed`], with the build plate under the
    /// model and the transform gizmo over everything ([`SceneExtras`]).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut MeshRenderer,
        camera: Option<&OrbitCamera>,
        items: &[DrawItem<'_>],
        show_boundaries: bool,
        layers: &[Layer<'_>],
        extras: &SceneExtras<'_>,
        mut timer: Option<(&mut GpuTimer, u64)>,
    ) -> bool {
        if let Some(camera) = camera {
            renderer.prepare(queue, camera, (self.width, self.height));
        }
        let bg = renderer.palette().background.map(f64::from);
        let (view, resolve_target) = match &self.msaa_view {
            Some(msaa) => (msaa, Some(&self.resolved_view)),
            None => (&self.resolved_view, None),
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        let timestamp_writes = timer.as_mut().and_then(|(t, frame)| t.begin(*frame));
        let timed = timestamp_writes.is_some();
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
                timestamp_writes,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(camera) = camera {
                if let Some(plate) = extras.plate {
                    renderer.draw_plate(device, queue, &mut pass, plate, camera.target);
                }
                renderer.paint(
                    device,
                    queue,
                    &mut pass,
                    camera.target,
                    items,
                    show_boundaries,
                    layers,
                );
                if let Some(g) = &extras.gizmo {
                    renderer.draw_gizmo(device, &mut pass, g.shapes, g.highlight, g.palette);
                }
            }
        }
        if let Some((t, _)) = timer.as_mut() {
            t.resolve(&mut encoder);
        }
        queue.submit([encoder.finish()]);
        if let Some((t, _)) = timer {
            t.submitted();
        }
        timed
    }
}

/// What [`RenderTarget::draw_with`] draws besides the model and its
/// overlay lines: the build plate (under the model, depth-tested) and the
/// transform gizmo (last, never hidden). Both are a few hundred vertices,
/// uploaded as they're drawn.
#[derive(Default)]
pub struct SceneExtras<'a> {
    pub plate: Option<&'a crate::Plate>,
    pub gizmo: Option<GizmoDraw<'a>>,
}

/// The gizmo's handles in target pixels, the one to highlight, and its colours.
pub struct GizmoDraw<'a> {
    pub shapes: &'a [crate::HandleShape],
    pub highlight: Option<crate::Handle>,
    pub palette: &'a crate::GizmoPalette,
}
