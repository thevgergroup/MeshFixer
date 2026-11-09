use bytemuck::{Pod, Zeroable};
use glam::{DMat3, DMat4, DVec3, Mat4, Vec4};

use crate::camera::OrbitCamera;
use crate::gpu_mesh::{GpuLines, GpuMesh, LineVertex, SurfaceVertex};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniforms {
    view_proj: [[f32; 4]; 4],
    eye_dir: [f32; 4],
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
    /// Colour multiplier (RGB) applied in the fragment shader; inactive
    /// objects are dimmed. Alpha is unused.
    tint: [f32; 4],
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
    /// Colour multiplier for the whole object: 1.0 is full colour (the
    /// active object), lower values dim inactive ones.
    pub tint: f32,
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
    highlight_pipeline: wgpu::RenderPipeline,
    draw_bgl: wgpu::BindGroupLayout,
    scene_uniforms: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,
    /// Grown on demand to cover the most objects drawn in one frame.
    draw_slots: Vec<DrawSlot>,
}

impl MeshRenderer {
    pub fn new(device: &wgpu::Device, target: RenderTargetFormat) -> MeshRenderer {
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

        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                        topology: wgpu::PrimitiveTopology,
                        front_face: wgpu::FrontFace| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
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
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Ccw,
        );
        let surface_pipeline_cw = pipeline(
            "mf surface cw",
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::FrontFace::Cw,
        );
        let line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3],
        })];
        let line_pipeline = pipeline(
            "mf boundary lines",
            "vs_line",
            "fs_line",
            &line_buffers,
            wgpu::PrimitiveTopology::LineList,
            wgpu::FrontFace::Ccw,
        );
        let highlight_pipeline = pipeline(
            "mf highlight lines",
            "vs_line",
            "fs_highlight",
            &line_buffers,
            wgpu::PrimitiveTopology::LineList,
            wgpu::FrontFace::Ccw,
        );
        MeshRenderer {
            surface_pipeline_ccw,
            surface_pipeline_cw,
            line_pipeline,
            highlight_pipeline,
            draw_bgl,
            scene_uniforms,
            scene_bind_group,
            draw_slots: Vec::new(),
        }
    }

    /// Write the scene (view-projection, eye direction) uniforms. Call once
    /// per frame before the render pass begins. `world_origin` is the
    /// shared reference point every mesh's local origin is shifted from
    /// (the camera target's frame), kept close to the drawn geometry so the
    /// `f32` GPU matrix holds enough precision (MF-GEO-001).
    pub fn prepare(&self, queue: &wgpu::Queue, camera: &OrbitCamera, aspect: f32) {
        let eye_dir = (camera.eye() - camera.target).normalize_or_zero().as_vec3();
        let u = SceneUniforms {
            view_proj: camera.view_proj(aspect, camera.target).to_cols_array_2d(),
            eye_dir: Vec4::from((eye_dir, 0.0)).to_array(),
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
    /// (`camera.target`), then the active object's boundary lines and
    /// highlight, if any.
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
        highlight: Option<(&GpuLines, DMat4, DVec3)>,
    ) {
        self.ensure_slots(device, items.len() + highlight.is_some() as usize);

        for (slot, item) in self.draw_slots.iter().zip(items) {
            let model = shifted_model(item.model, item.mesh.origin, world_origin);
            let u = DrawUniforms {
                model: model.to_cols_array_2d(),
                normal_matrix: normal_matrix(item.model).to_cols_array_2d(),
                tint: [item.tint, item.tint, item.tint, 1.0],
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
            for (buf, count) in &item.mesh.surface {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..*count, 0..1);
            }
            if show_boundaries {
                pass.set_pipeline(&self.line_pipeline);
                for (buf, count) in &item.mesh.lines {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..*count, 0..1);
                }
            }
        }
        if let Some((lines, model, mesh_origin)) = highlight {
            let slot = &self.draw_slots[items.len()];
            let shifted = shifted_model(model, mesh_origin, world_origin);
            let u = DrawUniforms {
                model: shifted.to_cols_array_2d(),
                // Highlight lines don't shade by normal.
                normal_matrix: Mat4::IDENTITY.to_cols_array_2d(),
                tint: [1.0, 1.0, 1.0, 1.0],
            };
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.highlight_pipeline);
            for (buf, count) in &lines.chunks {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..*count, 0..1);
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
