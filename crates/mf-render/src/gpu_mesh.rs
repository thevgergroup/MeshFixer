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
    let pos = |v: u32| mesh.corner_position(v);
    let mut out = Vec::with_capacity(mesh.triangle_count() * 3);
    for (_, t) in mesh.faces() {
        let [a, b, c] = t.map(|i| DVec3::from(pos(i)));
        let n = (b - a)
            .cross(c - a)
            .normalize_or_zero()
            .as_vec3()
            .to_array();
        for i in t {
            out.push(SurfaceVertex {
                position: local(pos(i), origin),
                normal: n,
            });
        }
    }
    out
}

/// Line-list vertices for every traced boundary loop edge.
pub fn boundary_line_vertices(mesh: &Mesh, origin: DVec3) -> Vec<LineVertex> {
    let pos = |v: u32| mesh.corner_position(v);
    let mut out = Vec::new();
    for l in &mesh.topology().boundary_loops {
        let n = l.vertices.len();
        for k in 0..n {
            for v in [l.vertices[k], l.vertices[(k + 1) % n]] {
                out.push(LineVertex {
                    position: local(pos(v), origin),
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
    /// The mesh's bounding box (object space), so frontends can frame it
    /// without an O(n) pass on the UI thread.
    pub bounds: Option<mf_geometry::Aabb>,
    pub origin: DVec3,
    pub triangle_count: usize,
    pub surface: Vec<SurfaceVertex>,
    pub lines: Vec<LineVertex>,
}

impl MeshVertices {
    pub fn build(mesh: &Mesh) -> MeshVertices {
        let bounds = mesh.bounding_box();
        let origin = bounds
            .map(|b| DVec3::from(b.center()))
            .unwrap_or(DVec3::ZERO);
        MeshVertices {
            bounds,
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

/// A separately uploaded line set (selection highlight, overlays). Changing the
/// selection uploads only this, never the mesh buffers (MF-RENDER-003).
pub struct GpuLines {
    pub(crate) chunks: Vec<(wgpu::Buffer, u32)>,
}

impl GpuLines {
    pub fn upload(device: &wgpu::Device, lines: &[LineVertex]) -> GpuLines {
        let max = device.limits().max_buffer_size;
        let stride = std::mem::size_of::<LineVertex>() as u64;
        let chunks = chunk_ranges(lines.len() as u32, stride, max, 2)
            .into_iter()
            .map(|r| {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mf highlight lines"),
                    contents: bytemuck::cast_slice(&lines[r.start as usize..r.end as usize]),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                (buffer, r.len() as u32)
            })
            .collect();
        GpuLines { chunks }
    }
}

/// Flat-shaded triangles for the given faces (by ID; dead IDs are skipped),
/// relative to `origin`: a face overlay (selection, loose pieces, recent
/// repairs). O(faces given).
pub fn face_surface_vertices(mesh: &Mesh, origin: DVec3, faces: &[u32]) -> Vec<SurfaceVertex> {
    let pos = |v: u32| mesh.corner_position(v);
    let mut out = Vec::with_capacity(faces.len() * 3);
    for &f in faces {
        let Some(t) = mesh.face(mf_geometry::FaceId(f)) else {
            continue;
        };
        let [a, b, c] = t.map(|i| DVec3::from(pos(i)));
        let n = (b - a)
            .cross(c - a)
            .normalize_or_zero()
            .as_vec3()
            .to_array();
        for i in t {
            out.push(SurfaceVertex {
                position: local(pos(i), origin),
                normal: n,
            });
        }
    }
    out
}

/// Line-list vertices for the given edges (vertex pairs), relative to `origin`.
pub fn edge_line_vertices(mesh: &Mesh, origin: DVec3, edges: &[[u32; 2]]) -> Vec<LineVertex> {
    edges
        .iter()
        .flat_map(|e| {
            e.map(|v| LineVertex {
                position: local(mesh.corner_position(v), origin),
            })
        })
        .collect()
}

/// A separately uploaded face overlay (see [`face_surface_vertices`]).
pub struct GpuFaces {
    pub(crate) chunks: Vec<(wgpu::Buffer, u32)>,
}

impl GpuFaces {
    pub fn upload(device: &wgpu::Device, faces: &[SurfaceVertex]) -> GpuFaces {
        let max = device.limits().max_buffer_size;
        let stride = std::mem::size_of::<SurfaceVertex>() as u64;
        let chunks = chunk_ranges(faces.len() as u32, stride, max, 3)
            .into_iter()
            .map(|r| {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mf overlay faces"),
                    contents: bytemuck::cast_slice(&faces[r.start as usize..r.end as usize]),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                (buffer, r.len() as u32)
            })
            .collect();
        GpuFaces { chunks }
    }
}

/// Line-list vertices for the given boundary loops (by ID), relative to `origin`.
pub fn loop_line_vertices(mesh: &Mesh, origin: DVec3, loop_ids: &[u32]) -> Vec<LineVertex> {
    let mut out = Vec::new();
    for l in mesh
        .topology()
        .boundary_loops
        .iter()
        .filter(|l| loop_ids.contains(&l.id))
    {
        let n = l.vertices.len();
        for k in 0..n {
            for v in [l.vertices[k], l.vertices[(k + 1) % n]] {
                out.push(LineVertex {
                    position: local(mesh.corner_position(v), origin),
                });
            }
        }
    }
    out
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
