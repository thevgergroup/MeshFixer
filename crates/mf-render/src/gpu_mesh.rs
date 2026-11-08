use std::ops::Range;

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

/// Split `count` vertices into consecutive ranges that fit in `max_bytes`,
/// never splitting a primitive of `primitive` vertices.
pub fn chunk_ranges(count: u32, stride: u64, max_bytes: u64, primitive: u32) -> Vec<Range<u32>> {
    let fit = (max_bytes / stride.max(1)).min(u32::MAX as u64) as u32;
    let per = (fit / primitive * primitive).max(primitive);
    (0..count)
        .step_by(per as usize)
        .map(|start| start..(start + per).min(count))
        .collect()
}

/// GPU buffers for one mesh revision, split into chunks under the device's
/// maximum buffer size.
pub struct GpuMesh {
    pub origin: DVec3,
    pub triangle_count: usize,
    pub(crate) surface: Vec<(wgpu::Buffer, u32)>,
    pub(crate) lines: Vec<(wgpu::Buffer, u32)>,
}

impl GpuMesh {
    pub fn upload(device: &wgpu::Device, mesh: &Mesh) -> GpuMesh {
        GpuMesh::from_vertices(device, &MeshVertices::build(mesh))
    }

    pub fn from_vertices(device: &wgpu::Device, v: &MeshVertices) -> GpuMesh {
        GpuMesh::from_vertices_with_limit(device, v, device.limits().max_buffer_size)
    }

    pub fn from_vertices_with_limit(
        device: &wgpu::Device,
        v: &MeshVertices,
        max_bytes: u64,
    ) -> GpuMesh {
        fn chunks<T: Pod>(
            device: &wgpu::Device,
            label: &str,
            data: &[T],
            max_bytes: u64,
            primitive: u32,
        ) -> Vec<(wgpu::Buffer, u32)> {
            let stride = std::mem::size_of::<T>() as u64;
            chunk_ranges(data.len() as u32, stride, max_bytes, primitive)
                .into_iter()
                .map(|r| {
                    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(label),
                        contents: bytemuck::cast_slice(&data[r.start as usize..r.end as usize]),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                    (buffer, r.len() as u32)
                })
                .collect()
        }
        GpuMesh {
            origin: v.origin,
            triangle_count: v.triangle_count,
            surface: chunks(device, "mf surface", &v.surface, max_bytes, 3),
            lines: chunks(device, "mf boundary lines", &v.lines, max_bytes, 2),
        }
    }
}
