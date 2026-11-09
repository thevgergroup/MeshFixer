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
    let mut out = Vec::with_capacity(mesh.triangle_count() * 3);
    for (_, t) in mesh.faces() {
        push_face(&mut out, mesh, t, origin);
    }
    out
}

/// One flat-shaded triangle's three vertices.
fn push_face(out: &mut Vec<SurfaceVertex>, mesh: &Mesh, t: [u32; 3], origin: DVec3) {
    let pos = |v: u32| mesh.corner_position(v);
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

/// Line-list vertices for every traced boundary loop edge.
pub fn boundary_line_vertices(mesh: &Mesh, origin: DVec3) -> Vec<LineVertex> {
    boundary_line_vertices_skipping(mesh, origin, &[])
}

/// As [`boundary_line_vertices`], leaving out the loops in `skip` (by ID;
/// for example narrow cracks, which are drawn in their own colour).
pub fn boundary_line_vertices_skipping(
    mesh: &Mesh,
    origin: DVec3,
    skip: &[u32],
) -> Vec<LineVertex> {
    let pos = |v: u32| mesh.corner_position(v);
    let mut out = Vec::new();
    for l in mesh
        .topology()
        .boundary_loops
        .iter()
        .filter(|l| !skip.contains(&l.id))
    {
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
        MeshVertices::build_skipping(mesh, &[])
    }

    /// As [`MeshVertices::build`], without boundary outlines for the loops
    /// in `skip` (by ID).
    pub fn build_skipping(mesh: &Mesh, skip: &[u32]) -> MeshVertices {
        let bounds = mesh.bounding_box();
        let origin = bounds
            .map(|b| DVec3::from(b.center()))
            .unwrap_or(DVec3::ZERO);
        MeshVertices {
            bounds,
            origin,
            triangle_count: mesh.triangle_count(),
            surface: surface_vertices(mesh, origin),
            lines: boundary_line_vertices_skipping(mesh, origin, skip),
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
        GpuLines {
            chunks: upload_chunks(device, None, "mf highlight lines", lines, u64::MAX, 2),
        }
    }

    /// As [`GpuLines::upload`], paced like [`GpuMesh::from_vertices_paced`].
    pub fn upload_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        lines: &[LineVertex],
        chunk_bytes: u64,
    ) -> GpuLines {
        GpuLines {
            chunks: upload_chunks(
                device,
                Some(queue),
                "mf highlight lines",
                lines,
                chunk_bytes,
                2,
            ),
        }
    }

    /// Vertices uploaded (two per line).
    pub fn vertex_count(&self) -> usize {
        self.chunks.iter().map(|(_, n)| *n as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.vertex_count() == 0
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
        GpuFaces {
            chunks: upload_chunks(device, None, "mf overlay faces", faces, u64::MAX, 3),
        }
    }

    /// As [`GpuFaces::upload`], paced like [`GpuMesh::from_vertices_paced`].
    pub fn upload_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        faces: &[SurfaceVertex],
        chunk_bytes: u64,
    ) -> GpuFaces {
        GpuFaces {
            chunks: upload_chunks(
                device,
                Some(queue),
                "mf overlay faces",
                faces,
                chunk_bytes,
                3,
            ),
        }
    }

    /// Vertices uploaded (three per face).
    pub fn vertex_count(&self) -> usize {
        self.chunks.iter().map(|(_, n)| *n as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.vertex_count() == 0
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
/// maximum buffer size. Building one creates and fills its buffers, which
/// for a big mesh is hundreds of megabytes: wgpu devices are `Send + Sync`,
/// so frontends build it on a worker, never the UI thread (MF-RT-001).
pub struct GpuMesh {
    pub origin: DVec3,
    pub triangle_count: usize,
    pub(crate) surface: Vec<(wgpu::Buffer, u32)>,
    pub(crate) lines: Vec<(wgpu::Buffer, u32)>,
}

impl GpuMesh {
    /// Surface vertices uploaded (three per face), and in how many buffers.
    pub fn surface_vertex_count(&self) -> (usize, usize) {
        let n = self.surface.iter().map(|(_, n)| *n as usize).sum();
        (n, self.surface.len())
    }

    /// Boundary-outline vertices uploaded (two per edge).
    pub fn line_vertex_count(&self) -> usize {
        self.lines.iter().map(|(_, n)| *n as usize).sum()
    }

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
        GpuMesh::build(device, None, v, max_bytes)
    }

    /// As [`GpuMesh::from_vertices`], for a worker thread, in buffers of at
    /// most `chunk_bytes`, each one's upload submitted and waited for
    /// before the next is made. A buffer's contents reach the GPU as a copy
    /// queued with the next submit, whoever makes it: made all at once, a
    /// 5M-triangle mesh's copy (hundreds of megabytes) would land in a UI
    /// frame. Paced, no frame waits for more than one chunk's copy.
    ///
    /// Panics if the device is lost while waiting (callers on workers
    /// report the panic).
    pub fn from_vertices_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        v: &MeshVertices,
        chunk_bytes: u64,
    ) -> GpuMesh {
        GpuMesh::build(device, Some(queue), v, chunk_bytes)
    }

    /// As [`GpuMesh::from_vertices_paced`], straight from `mesh` (vertices
    /// relative to `origin`, no outlines for the loops in `skip`): the
    /// surface is generated one chunk at a time, so at most one chunk's
    /// vertices exist on the CPU, not the whole mesh's (hundreds of
    /// megabytes at 5M triangles). Same buffers as
    /// [`MeshVertices::build_skipping`] then [`GpuMesh::from_vertices`].
    pub fn from_mesh_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mesh: &Mesh,
        origin: DVec3,
        skip: &[u32],
        chunk_bytes: u64,
    ) -> GpuMesh {
        let stride = std::mem::size_of::<SurfaceVertex>() as u64;
        let max = chunk_bytes.min(device.limits().max_buffer_size);
        let faces_per_chunk = ((max / (3 * stride)).max(1)) as usize;
        let mut surface = Vec::new();
        let mut batch: Vec<SurfaceVertex> = Vec::with_capacity(faces_per_chunk * 3);
        let several = mesh.triangle_count() > faces_per_chunk;
        let mut faces = mesh.faces().peekable();
        while faces.peek().is_some() {
            batch.clear();
            for (_, t) in faces.by_ref().take(faces_per_chunk) {
                push_face(&mut batch, mesh, t, origin);
            }
            surface.extend(upload_chunks(device, None, "mf surface", &batch, max, 3));
            if several {
                flush(device, queue);
            }
        }
        let lines = boundary_line_vertices_skipping(mesh, origin, skip);
        GpuMesh {
            origin,
            triangle_count: mesh.triangle_count(),
            surface,
            lines: upload_chunks(device, Some(queue), "mf boundary lines", &lines, max, 2),
        }
    }

    fn build(
        device: &wgpu::Device,
        pace: Option<&wgpu::Queue>,
        v: &MeshVertices,
        max_bytes: u64,
    ) -> GpuMesh {
        GpuMesh {
            origin: v.origin,
            triangle_count: v.triangle_count,
            surface: upload_chunks(device, pace, "mf surface", &v.surface, max_bytes, 3),
            lines: upload_chunks(device, pace, "mf boundary lines", &v.lines, max_bytes, 2),
        }
    }
}

/// Vertex buffers for `data`, split under `max_bytes` (and the device's
/// limit) without splitting a primitive. With `pace`, and more than one
/// buffer, each buffer's upload is submitted and waited for before the next
/// is made (see [`GpuMesh::from_vertices_paced`]).
fn upload_chunks<T: Pod>(
    device: &wgpu::Device,
    pace: Option<&wgpu::Queue>,
    label: &str,
    data: &[T],
    max_bytes: u64,
    primitive: u32,
) -> Vec<(wgpu::Buffer, u32)> {
    let stride = std::mem::size_of::<T>() as u64;
    let max_bytes = max_bytes.min(device.limits().max_buffer_size);
    let ranges = chunk_ranges(data.len() as u32, stride, max_bytes, primitive);
    // One chunk's copy may land in someone's frame anyway: only pace more.
    let pace = pace.filter(|_| ranges.len() > 1);
    ranges
        .into_iter()
        .map(|r| {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&data[r.start as usize..r.end as usize]),
                usage: wgpu::BufferUsages::VERTEX,
            });
            if let Some(queue) = pace {
                flush(device, queue);
            }
            (buffer, r.len() as u32)
        })
        .collect()
}

/// Submit the uploads made so far and wait until the GPU has them.
fn flush(device: &wgpu::Device, queue: &wgpu::Queue) {
    let submission = queue.submit([]);
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .expect("the GPU device was lost while uploading a mesh");
}
