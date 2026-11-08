use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use mf_geometry::Mesh;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct SurfaceVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct LineVertex {
    pub position: [f32; 3],
}

fn local(p: [f64; 3], origin: DVec3) -> [f32; 3] {
    (DVec3::from(p) - origin).as_vec3().to_array()
}

/// Non-indexed, flat-shaded triangles relative to `origin`.
///
/// Temporary for M1: M4 replaces this with chunked, indexed buffers that
/// support partial re-upload (MF-RENDER-003).
pub fn surface_vertices(mesh: &Mesh, origin: DVec3) -> Vec<SurfaceVertex> {
    let pos = mesh.positions();
    let mut out = Vec::with_capacity(mesh.triangle_count() * 3);
    for t in mesh.triangles() {
        let [a, b, c] = t.map(|i| DVec3::from(pos[i as usize]));
        let n = (b - a)
            .cross(c - a)
            .normalize_or_zero()
            .as_vec3()
            .to_array();
        for i in t {
            out.push(SurfaceVertex {
                position: local(pos[*i as usize], origin),
                normal: n,
            });
        }
    }
    out
}

/// Line-list vertices for every traced boundary loop edge.
pub fn boundary_line_vertices(mesh: &Mesh, origin: DVec3) -> Vec<LineVertex> {
    let pos = mesh.positions();
    let mut out = Vec::new();
    for l in &mesh.topology().boundary_loops {
        let n = l.vertices.len();
        for k in 0..n {
            for v in [l.vertices[k], l.vertices[(k + 1) % n]] {
                out.push(LineVertex {
                    position: local(pos[v as usize], origin),
                });
            }
        }
    }
    out
}

/// CPU-side vertex data for one mesh revision. Building it is O(n), so
/// frontends do it off the UI thread and only upload on the render thread
/// (MF-RT-001).
#[derive(Debug, Clone)]
pub struct MeshVertices {
    pub origin: DVec3,
    pub triangle_count: usize,
    pub surface: Vec<SurfaceVertex>,
    pub lines: Vec<LineVertex>,
}

impl MeshVertices {
    pub fn build(mesh: &Mesh) -> MeshVertices {
        let origin = mesh
            .bounding_box()
            .map(|b| DVec3::from(b.center()))
            .unwrap_or(DVec3::ZERO);
        MeshVertices {
            origin,
            triangle_count: mesh.triangle_count(),
            surface: surface_vertices(mesh, origin),
            lines: boundary_line_vertices(mesh, origin),
        }
    }
}

/// GPU buffers for one mesh revision.
pub struct GpuMesh {
    pub origin: DVec3,
    pub triangle_count: usize,
    pub(crate) surface: Option<wgpu::Buffer>,
    pub(crate) surface_vertices: u32,
    pub(crate) lines: Option<wgpu::Buffer>,
    pub(crate) line_vertices: u32,
}

impl GpuMesh {
    pub fn upload(device: &wgpu::Device, mesh: &Mesh) -> GpuMesh {
        GpuMesh::from_vertices(device, &MeshVertices::build(mesh))
    }

    pub fn from_vertices(device: &wgpu::Device, v: &MeshVertices) -> GpuMesh {
        let buffer = |label: &str, bytes: &[u8]| {
            (!bytes.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytes,
                    usage: wgpu::BufferUsages::VERTEX,
                })
            })
        };
        GpuMesh {
            origin: v.origin,
            triangle_count: v.triangle_count,
            surface: buffer("mf surface", bytemuck::cast_slice(&v.surface)),
            surface_vertices: v.surface.len() as u32,
            lines: buffer("mf boundary lines", bytemuck::cast_slice(&v.lines)),
            line_vertices: v.lines.len() as u32,
        }
    }
}
