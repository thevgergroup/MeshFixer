use bytemuck::{Pod, Zeroable};
use glam::{DMat3, DMat4, DVec3, Mat4, Vec4};

use crate::camera::OrbitCamera;
use crate::gpu_mesh::{
    FaceStates, GpuLines, GpuMesh, LineVertex, SurfaceVertex, face_state_layout,
};

/// The colours the 3D view draws with. `mf-render` has no theme of its own
/// in the app: the desktop passes its colour tokens in (theme.rs), so one
/// colour means one thing everywhere. Components are linear 0–1 RGBA as
/// written to the (non-sRGB) render target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: [f32; 4],
    /// Front faces of the model.
    pub surface: [f32; 4],
    /// Back faces (seen through holes, or flipped).
    pub back_face: [f32; 4],
    /// Every traced boundary loop's outline (holes).
    pub boundary: [f32; 4],
    /// Saturation kept by an inactive object (0 = grey, 1 = unchanged).
    pub inactive_saturation: f32,
    /// Brightness multiplier of an inactive object.
    pub inactive_brightness: f32,
    /// Width of boundary outlines, in points.
    pub boundary_width: f32,
    /// Width of overlay lines (selection, cracks, tangled edges, hover), in points.
    pub overlay_width: f32,
    /// What each face-state flag tints a face with ([`FaceStates`]).
    pub tints: FaceTints,
}

/// The tint of each face-state flag: RGB, and in alpha how much of it
/// replaces the surface colour (0–1). A selected face shows only the
/// selection tint (even when hovered), a hovered one only the hover tint;
/// otherwise later tints in this list draw over earlier ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceTints {
    pub small_component: [f32; 4],
    pub non_manifold_adj: [f32; 4],
    pub recent_repair: [f32; 4],
    pub selected: [f32; 4],
    pub hover: [f32; 4],
}

impl Palette {
    /// Headless renders (the CLI's `render`, offscreen tests) have no app
    /// theme; these are the renderer's own neutral colours for them.
    pub const HEADLESS: Palette = Palette {
        background: [38.0 / 255.0, 41.0 / 255.0, 46.0 / 255.0, 1.0],
        surface: [0.70, 0.72, 0.76, 1.0],
        back_face: [0.62, 0.30, 0.30, 1.0],
        boundary: [1.0, 0.55, 0.05, 1.0],
        inactive_saturation: 0.0,
        inactive_brightness: 0.55,
        boundary_width: 1.0,
        overlay_width: 1.0,
        tints: FaceTints {
            small_component: [1.0, 0.78, 0.16, 0.6],
            non_manifold_adj: [0.92, 0.2, 0.2, 0.35],
            recent_repair: [0.31, 0.8, 0.47, 0.45],
            selected: [0.16, 0.82, 1.0, 0.6],
            hover: [0.91, 0.93, 0.96, 0.35],
        },
    };
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniforms {
    view_proj: [[f32; 4]; 4],
    eye_dir: [f32; 4],
    surface: [f32; 4],
    back_face: [f32; 4],
    boundary: [f32; 4],
    /// x: inactive saturation, y: inactive brightness.
    inactive: [f32; 4],
    /// Render target size in pixels (x, y), for screen-space line widths.
    viewport: [f32; 4],
    /// [`FaceTints`], in its order.
    tints: [[f32; 4]; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawUniforms {
    model: [[f32; 4]; 4],
    /// Inverse-transpose of `model`'s linear (3x3) part, for normals: the
    /// model matrix itself is only correct for normals under a rotation or
    /// uniform scale, not a non-uniform scale or shear (both legal in a
    /// 3MF transform). The translation column is unused padding.
    normal_matrix: [[f32; 4]; 4],
    /// A layer's colour (unused for an object's own surface and outlines).
    color: [f32; 4],
    /// x: 1 for an inactive object (desaturated). y: line width in pixels.
    params: [f32; 4],
    /// x: the face-state flags shown ([`DrawItem::shown_states`]).
    states: [u32; 4],
}

/// The render target the pipelines are built for. It must match the render
/// pass they're used in.
#[derive(Debug, Clone, Copy)]
pub struct RenderTargetFormat {
    pub color: wgpu::TextureFormat,
    pub depth: wgpu::TextureFormat,
    pub samples: u32,
}

/// One mesh to draw this frame, placed by its object-to-world transform
/// (already in column-vector form; see [`crate::model_matrix`]).
pub struct DrawItem<'a> {
    pub mesh: &'a GpuMesh,
    pub model: DMat4,
    /// An inactive object is drawn desaturated (see [`Palette`]).
    pub inactive: bool,
    /// Which face-state flags ([`FaceStates`]) tint it: the overlays that
    /// are switched on. [`FaceStates::ALL`] shows every one.
    pub shown_states: u32,
}

/// One line overlay on an object (selected loops, cracks, tangled edges,
/// hover), drawn after every object's own surface, in the order given (a
/// later layer draws over an earlier one). Face tints aren't layers: they
/// are the mesh's face states ([`FaceStates`]).
pub struct Layer<'a> {
    pub lines: &'a GpuLines,
    /// The object's object-to-world transform.
    pub model: DMat4,
    /// The local origin the layer's vertices are relative to.
    pub mesh_origin: DVec3,
    pub color: [f32; 4],
}

/// A per-draw uniform buffer and its bind group, reused across frames.
struct DrawSlot {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub struct MeshRenderer {
    surface_pipeline_ccw: wgpu::RenderPipeline,
    surface_pipeline_cw: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    overlay_line_pipeline: wgpu::RenderPipeline,
    palette: Palette,
    /// Physical pixels per point (line widths are in points).
    pixels_per_point: f32,
    draw_bgl: wgpu::BindGroupLayout,
    scene_uniforms: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,
    /// Grown on demand to cover the most objects drawn in one frame.
    draw_slots: Vec<DrawSlot>,
}

impl MeshRenderer {
    pub fn new(
        device: &wgpu::Device,
        target: RenderTargetFormat,
        palette: Palette,
    ) -> MeshRenderer {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let scene_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mf scene uniforms"),
            size: std::mem::size_of::<SceneUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mf scene uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mf scene uniforms"),
            layout: &scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_uniforms.as_entire_binding(),
            }],
        });
        let draw_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mf draw uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&draw_bgl)],
            immediate_size: 0,
        });
        // An object's own surface also reads its face states (group 2).
        let states_bgl = face_state_layout(device);
        let surface_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf surface pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&draw_bgl), Some(&states_bgl)],
            immediate_size: 0,
        });

        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                        topology: wgpu::PrimitiveTopology,
                        front_face: wgpu::FrontFace| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    front_face,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: target.depth,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: target.samples.max(1),
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(target.color.into())],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let surface_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SurfaceVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        })];
        // Two variants, differing only in which winding counts as the front
        // face: a draw whose model matrix has a negative determinant
        // (mirrored) uses `Cw` so `front_facing` (and its shading) stays
        // correct even though the triangles are now wound the other way.
        let surface_pipeline_ccw = pipeline(
            "mf surface ccw",
            &surface_layout,
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Ccw,
        );
        let surface_pipeline_cw = pipeline(
            "mf surface cw",
            &surface_layout,
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Cw,
        );
        // Lines are drawn as screen-space quads so they can be wider than a
        // pixel: each segment (a pair of consecutive line-list vertices) is
        // one instance of six vertices.
        let line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: 2 * std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        })];
        let line_pipeline = pipeline(
            "mf boundary lines",
            &layout,
            "vs_line",
            "fs_line",
            &line_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Ccw,
        );
        let overlay_line_pipeline = pipeline(
            "mf overlay lines",
            &layout,
            "vs_line",
            "fs_overlay_line",
            &line_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Ccw,
        );
        MeshRenderer {
            surface_pipeline_ccw,
            surface_pipeline_cw,
            line_pipeline,
            overlay_line_pipeline,
            palette,
            pixels_per_point: 1.0,
            draw_bgl,
            scene_uniforms,
            scene_bind_group,
            draw_slots: Vec::new(),
        }
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Physical pixels per point of the target, so line widths (in points)
    /// look the same on every display.
    pub fn set_pixels_per_point(&mut self, ppp: f32) {
        self.pixels_per_point = ppp.max(0.1);
    }

    /// Write the scene (view-projection, eye direction) uniforms. Call once
    /// per frame before the render pass begins. `world_origin` is the
    /// shared reference point every mesh's local origin is shifted from
    /// (the camera target's frame), kept close to the drawn geometry so the
    /// `f32` GPU matrix holds enough precision (MF-GEO-001).
    pub fn prepare(&self, queue: &wgpu::Queue, camera: &OrbitCamera, size: (u32, u32)) {
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let eye_dir = (camera.eye() - camera.target).normalize_or_zero().as_vec3();
        let p = &self.palette;
        let u = SceneUniforms {
            view_proj: camera.view_proj(aspect, camera.target).to_cols_array_2d(),
            eye_dir: Vec4::from((eye_dir, 0.0)).to_array(),
            surface: p.surface,
            back_face: p.back_face,
            boundary: p.boundary,
            inactive: [p.inactive_saturation, p.inactive_brightness, 0.0, 0.0],
            viewport: [size.0 as f32, size.1 as f32, 0.0, 0.0],
            tints: [
                p.tints.small_component,
                p.tints.non_manifold_adj,
                p.tints.recent_repair,
                p.tints.selected,
                p.tints.hover,
            ],
        };
        queue.write_buffer(&self.scene_uniforms, 0, bytemuck::bytes_of(&u));
    }

    /// Grow the draw-slot pool to at least `n` entries.
    fn ensure_slots(&mut self, device: &wgpu::Device, n: usize) {
        while self.draw_slots.len() < n {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mf draw uniforms"),
                size: std::mem::size_of::<DrawUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mf draw uniforms"),
                layout: &self.draw_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            self.draw_slots.push(DrawSlot { buffer, bind_group });
        }
    }

    /// Draw every item of `items` at its transform (MF-ISSUE-042), each
    /// shifted into the frame [`Self::prepare`] was called with
    /// (`camera.target`), with its boundary outlines if `show_boundaries`,
    /// then every overlay of `layers` (see [`Layer`]).
    ///
    /// Call after [`Self::prepare`] and before the render pass ends. Must
    /// be called on the same device the renderer and `items` were built
    /// with.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        world_origin: DVec3,
        items: &[DrawItem<'_>],
        show_boundaries: bool,
        layers: &[Layer<'_>],
    ) {
        self.ensure_slots(device, items.len() + layers.len());

        for (slot, item) in self.draw_slots.iter().zip(items) {
            let model = shifted_model(item.model, item.mesh.origin, world_origin);
            let u = DrawUniforms {
                model: model.to_cols_array_2d(),
                normal_matrix: normal_matrix(item.model).to_cols_array_2d(),
                color: [0.0; 4],
                params: [
                    f32::from(u8::from(item.inactive)),
                    self.palette.boundary_width * self.pixels_per_point,
                    0.0,
                    0.0,
                ],
                states: [item.shown_states & u32::from(FaceStates::ALL), 0, 0, 0],
            };
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
        }
        let layer_slots = &self.draw_slots[items.len()..];
        for (slot, layer) in layer_slots.iter().zip(layers) {
            let shifted = shifted_model(layer.model, layer.mesh_origin, world_origin);
            let u = DrawUniforms {
                model: shifted.to_cols_array_2d(),
                normal_matrix: normal_matrix(layer.model).to_cols_array_2d(),
                color: layer.color,
                params: [
                    0.0,
                    self.palette.overlay_width * self.pixels_per_point,
                    0.0,
                    0.0,
                ],
                states: [0; 4],
            };
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
        }

        pass.set_bind_group(0, &self.scene_bind_group, &[]);
        for (slot, item) in self.draw_slots.iter().zip(items) {
            let pipeline = if item.model.determinant() < 0.0 {
                &self.surface_pipeline_cw
            } else {
                &self.surface_pipeline_ccw
            };
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(pipeline);
            let (states, buffers) = item.mesh.surface_draws();
            pass.set_bind_group(2, states, &[]);
            for (buf, count, first_face) in buffers {
                pass.set_vertex_buffer(0, buf.slice(..));
                // The instance index is where the buffer's first face's
                // state is in the mesh's state buffer.
                pass.draw(0..count, first_face..first_face + 1);
            }
            if show_boundaries {
                pass.set_pipeline(&self.line_pipeline);
                for (buf, count) in &item.mesh.lines {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..6, 0..*count / 2);
                }
            }
        }
        for (slot, layer) in layer_slots.iter().zip(layers) {
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.overlay_line_pipeline);
            for (buf, count) in &layer.lines.chunks {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*count / 2);
            }
        }
    }
}

/// `model` maps object space (as stored in 3MF) to world space. GPU vertex
/// data is stored relative to the mesh's own local origin (still in object
/// space), and the camera's view-projection is relative to `world_origin`.
/// This is the single matrix the shader needs to go straight from a local
/// vertex to the camera's frame: `shifted = T(-world_origin) * model *
/// T(mesh_origin)`.
fn shifted_model(model: DMat4, mesh_origin: DVec3, world_origin: DVec3) -> glam::Mat4 {
    (DMat4::from_translation(-world_origin) * model * DMat4::from_translation(mesh_origin))
        .as_mat4()
}

/// Inverse-transpose of `model`'s linear (3x3) part, computed in `f64`
/// before the cast to the GPU's `f32`. A rotation or uniform scale needs no
/// correction (the model matrix itself is already its own inverse-transpose
/// up to a scalar), but a non-uniform scale or shear — both legal in a 3MF
/// transform — tilts normals unless they go through this instead. The
/// translation never affects a direction, so only the 3x3 part matters;
/// the result is padded into a `Mat4` to keep the uniform layout simple.
fn normal_matrix(model: DMat4) -> Mat4 {
    let linear = DMat3::from_mat4(model);
    // A singular linear part (e.g. a transform that collapses a whole axis)
    // has no meaningful normal transform; falling back to the identity
    // keeps shading stable rather than propagating NaN/Inf into the frame.
    let it = if linear.determinant().abs() > 1e-18 {
        linear.inverse().transpose()
    } else {
        DMat3::IDENTITY
    };
    Mat4::from_mat3(it.as_mat3())
}
