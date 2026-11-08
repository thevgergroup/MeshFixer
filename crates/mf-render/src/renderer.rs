use bytemuck::{Pod, Zeroable};
use glam::Vec4;

use crate::camera::OrbitCamera;
use crate::gpu_mesh::{GpuMesh, LineVertex, SurfaceVertex};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    eye_dir: [f32; 4],
}

/// The render target the pipelines are built for. It must match the render
/// pass they're used in.
#[derive(Debug, Clone, Copy)]
pub struct RenderTargetFormat {
    pub color: wgpu::TextureFormat,
    pub depth: wgpu::TextureFormat,
    pub samples: u32,
}

pub struct MeshRenderer {
    surface_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl MeshRenderer {
    pub fn new(device: &wgpu::Device, target: RenderTargetFormat) -> MeshRenderer {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mf uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mf uniforms"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mf uniforms"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                        topology: wgpu::PrimitiveTopology| {
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
        let surface_pipeline = pipeline(
            "mf surface",
            "vs_surface",
            "fs_surface",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<SurfaceVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            })],
            wgpu::PrimitiveTopology::TriangleList,
        );
        let line_pipeline = pipeline(
            "mf boundary lines",
            "vs_line",
            "fs_line",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3],
            })],
            wgpu::PrimitiveTopology::LineList,
        );
        MeshRenderer {
            surface_pipeline,
            line_pipeline,
            uniforms,
            bind_group,
        }
    }

    /// Write per-frame uniforms. Call before the render pass begins.
    pub fn prepare(&self, queue: &wgpu::Queue, mesh: &GpuMesh, camera: &OrbitCamera, aspect: f32) {
        let eye_dir = (camera.eye() - camera.target).normalize_or_zero().as_vec3();
        let u = Uniforms {
            view_proj: camera.view_proj(aspect, mesh.origin).to_cols_array_2d(),
            eye_dir: Vec4::from((eye_dir, 0.0)).to_array(),
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }

    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>, mesh: &GpuMesh, show_boundaries: bool) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.surface_pipeline);
        for (buf, count) in &mesh.surface {
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..*count, 0..1);
        }
        if show_boundaries {
            pass.set_pipeline(&self.line_pipeline);
            for (buf, count) in &mesh.lines {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..*count, 0..1);
            }
        }
    }
}
