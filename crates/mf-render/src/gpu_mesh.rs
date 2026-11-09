use std::ops::Range;
use std::sync::{Arc, Weak};

use bytemuck::{Pod, Zeroable};
use glam::DVec3;
use mf_geometry::{Aabb, CHUNK, Mesh};
use wgpu::util::DeviceExt;

use crate::RenderError;

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

/// Non-indexed, flat-shaded triangles relative to `origin`, for the whole
/// mesh (the GPU holds them per face chunk: see [`ChunkVertices`]).
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

/// The vertices of one face chunk (face slots `i·CHUNK..(i+1)·CHUNK`),
/// relative to the mesh's local origin: what one GPU chunk holds
/// (MF-RENDER-003).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkVertices {
    /// Flat-shaded triangles of the chunk's live faces, in ID order.
    pub surface: Vec<SurfaceVertex>,
    /// The vertex chunks its faces reference, ascending: the chunk is only
    /// valid for another revision if all of them are shared with it.
    pub vertex_chunks: Vec<u32>,
}

impl ChunkVertices {
    /// Face chunk `i` of `mesh`, relative to `origin`. O(faces in the chunk).
    pub fn build(mesh: &Mesh, i: usize, origin: DVec3) -> ChunkVertices {
        let slots = (mesh.face_slots() as usize).saturating_sub(i * CHUNK);
        let mut surface = Vec::with_capacity(3 * slots.min(CHUNK));
        let mut referenced = vec![false; mesh.position_chunk_count()];
        for (_, t) in mesh.faces_in_chunk(i) {
            push_face(&mut surface, mesh, t, origin);
            for v in t {
                referenced[v as usize / CHUNK] = true;
            }
        }
        ChunkVertices {
            surface,
            vertex_chunks: (0..referenced.len() as u32)
                .filter(|&j| referenced[j as usize])
                .collect(),
        }
    }
}

/// Which face chunks built for `prev` are still right for `next`.
struct Reuse<'a> {
    prev: &'a Mesh,
    next: &'a Mesh,
    /// Per vertex chunk of `prev`: are all its positions kept in `next`?
    kept: Vec<bool>,
}

impl<'a> Reuse<'a> {
    fn new(prev: &'a Mesh, next: &'a Mesh) -> Reuse<'a> {
        Reuse {
            prev,
            next,
            kept: (0..prev.position_chunk_count())
                .map(|j| next.vertex_chunk_kept(prev, j))
                .collect(),
        }
    }

    /// True if face chunk `i` built for `prev` is also face chunk `i` of
    /// `next`: its faces (with their liveness) are shared, and every vertex
    /// chunk they reference still holds the same positions (shared, or
    /// only appended to). Decided by storage identity and content, so it
    /// holds whatever the revisions' history (undo, redo, another branch).
    fn chunk(&self, i: usize, vertex_chunks: &[u32]) -> bool {
        self.next.face_chunk_shared(self.prev, i)
            && vertex_chunks
                .iter()
                .all(|&j| self.kept.get(j as usize).copied().unwrap_or(false))
    }
}

/// How far, in diagonals, a mesh may reach from its local origin before the
/// origin is moved: `f32` local coordinates keep about 10⁻⁴ of a diagonal
/// there (24-bit mantissa).
pub const ORIGIN_REACH: f64 = 1.0e3;

/// The local origin for a revision with `bounds`, given the one it replaces
/// (`prev`): `prev` is kept, so chunks built relative to it stay valid,
/// unless some point of `bounds` is more than [`ORIGIN_REACH`] diagonals
/// from it; then (or with no `prev`) the centre of `bounds`.
pub fn render_origin(prev: Option<DVec3>, bounds: Option<Aabb>) -> DVec3 {
    let Some(b) = bounds else {
        return prev.unwrap_or(DVec3::ZERO);
    };
    let reach = |o: DVec3| {
        let (lo, hi) = (DVec3::from(b.min) - o, DVec3::from(b.max) - o);
        lo.abs().max(hi.abs()).length()
    };
    match prev {
        Some(o) if reach(o) <= ORIGIN_REACH * b.diagonal() => o,
        _ => DVec3::from(b.center()),
    }
}

/// CPU-side vertex data for one mesh revision, one entry per face chunk.
/// Building it is O(n) (O(changed chunks) with [`MeshVertices::build_from`]),
/// so frontends do it off the UI thread (MF-RT-001).
#[derive(Debug, Clone)]
pub struct MeshVertices {
    /// The mesh's bounding box (object space), so frontends can frame it
    /// without an O(n) pass on the UI thread.
    pub bounds: Option<Aabb>,
    pub origin: DVec3,
    pub triangle_count: usize,
    /// One per face chunk; an unchanged chunk is the previous revision's
    /// `Arc`, which is how [`GpuMesh::sync`] knows not to upload it.
    pub chunks: Vec<Arc<ChunkVertices>>,
    pub lines: Vec<LineVertex>,
}

impl MeshVertices {
    pub fn build(mesh: &Mesh) -> MeshVertices {
        MeshVertices::build_from(None, mesh)
    }

    /// As [`MeshVertices::build`], without boundary outlines for the loops
    /// in `skip` (by ID).
    pub fn build_skipping(mesh: &Mesh, skip: &[u32]) -> MeshVertices {
        MeshVertices::build_from_skipping(None, mesh, skip)
    }

    /// The vertices of `next`, reusing (the same `Arc`) every chunk of
    /// `prev` (a revision and the vertices built for it) whose face chunk is
    /// shared and whose referenced vertex chunks all are (or were only
    /// appended to: see [`Mesh::vertex_chunk_kept`]); the rest are
    /// rebuilt. The origin is `prev`'s unless `next` outgrows it (see
    /// [`render_origin`]), in which case everything is rebuilt.
    pub fn build_from(prev: Option<(&Mesh, &MeshVertices)>, next: &Mesh) -> MeshVertices {
        MeshVertices::build_from_skipping(prev, next, &[])
    }

    /// As [`MeshVertices::build_from`], without boundary outlines for the
    /// loops in `skip` (by ID).
    pub fn build_from_skipping(
        prev: Option<(&Mesh, &MeshVertices)>,
        next: &Mesh,
        skip: &[u32],
    ) -> MeshVertices {
        let bounds = next.bounding_box();
        let origin = render_origin(prev.map(|(_, v)| v.origin), bounds);
        if let Some((mesh, v)) = prev {
            assert_eq!(
                v.chunks.len(),
                mesh.face_chunk_count(),
                "previous vertices were built for another mesh"
            );
        }
        let reuse = prev
            .filter(|(_, v)| v.origin == origin)
            .map(|(mesh, v)| (Reuse::new(mesh, next), v));
        let chunks = (0..next.face_chunk_count())
            .map(|i| match &reuse {
                Some((reuse, v))
                    if v.chunks
                        .get(i)
                        .is_some_and(|c| reuse.chunk(i, &c.vertex_chunks)) =>
                {
                    v.chunks[i].clone()
                }
                _ => Arc::new(ChunkVertices::build(next, i, origin)),
            })
            .collect();
        MeshVertices {
            bounds,
            origin,
            triangle_count: next.triangle_count(),
            chunks,
            lines: boundary_line_vertices_skipping(next, origin, skip),
        }
    }

    /// Every chunk's surface vertices, concatenated (a copy).
    pub fn surface(&self) -> Vec<SurfaceVertex> {
        self.chunks
            .iter()
            .flat_map(|c| c.surface.iter().copied())
            .collect()
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

/// Bytes uploaded by one update of a [`GpuMesh`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadStats {
    /// Surface vertices of the face chunks that were (re)built.
    pub surface_bytes: u64,
    /// Per-face state (selection and overlay tints); none yet.
    pub state_bytes: u64,
    /// Boundary outlines (always rebuilt: they're small).
    pub line_bytes: u64,
}

impl UploadStats {
    pub fn total(&self) -> u64 {
        self.surface_bytes + self.state_bytes + self.line_bytes
    }
}

/// One face chunk on the GPU: one buffer set per face chunk (split further
/// only under a tiny buffer limit). Shared, through `Arc`, by every
/// revision that reuses it.
#[derive(Clone)]
struct GpuChunk {
    /// What it was uploaded from, for identity only: a `Weak` keeps the
    /// address from being reused without keeping the vertices in memory.
    source: Weak<ChunkVertices>,
    vertex_chunks: Arc<[u32]>,
    buffers: Arc<[(wgpu::Buffer, u32)]>,
}

/// GPU buffers for one mesh revision: one set per face chunk, so a new
/// revision re-uploads only the chunks that changed (MF-RENDER-003), and
/// shares the rest with the revision it replaces. Creating and filling
/// buffers for a big mesh is hundreds of megabytes: wgpu devices are
/// `Send + Sync`, so frontends do it on a worker, never the UI thread
/// (MF-RT-001).
pub struct GpuMesh {
    pub origin: DVec3,
    pub triangle_count: usize,
    chunks: Vec<GpuChunk>,
    pub(crate) lines: Vec<(wgpu::Buffer, u32)>,
    /// Largest buffer made.
    max_bytes: u64,
    /// Uploads are waited for every this many bytes (see
    /// [`GpuMesh::from_vertices_paced`]).
    pace_bytes: u64,
    last: UploadStats,
    total: u64,
}

impl GpuMesh {
    fn new(origin: DVec3, max_bytes: u64, pace_bytes: u64) -> GpuMesh {
        GpuMesh {
            origin,
            triangle_count: 0,
            chunks: Vec::new(),
            lines: Vec::new(),
            max_bytes,
            pace_bytes,
            last: UploadStats::default(),
            total: 0,
        }
    }

    /// Every surface buffer and its vertex count, in face-chunk order.
    pub(crate) fn surface_buffers(&self) -> impl Iterator<Item = &(wgpu::Buffer, u32)> {
        self.chunks.iter().flat_map(|c| c.buffers.iter())
    }

    /// Surface vertices uploaded (three per face), and in how many buffers.
    pub fn surface_vertex_count(&self) -> (usize, usize) {
        let n = self.surface_buffers().map(|(_, n)| *n as usize).sum();
        (n, self.surface_buffers().count())
    }

    /// Boundary-outline vertices uploaded (two per edge).
    pub fn line_vertex_count(&self) -> usize {
        self.lines.iter().map(|(_, n)| *n as usize).sum()
    }

    /// Face chunks held (one per face chunk of the mesh).
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// What the last update (or the first build) uploaded.
    pub fn last_upload(&self) -> UploadStats {
        self.last
    }

    /// Bytes uploaded for this mesh and every revision it was updated
    /// from, cumulatively (metrics and tests).
    pub fn total_uploaded(&self) -> u64 {
        self.total
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
        let mut gpu = GpuMesh::new(v.origin, max_bytes, u64::MAX);
        let mut up = Uploader::new(device, None, &gpu);
        gpu.apply(&mut up, v);
        gpu.finish(up, v.triangle_count);
        gpu
    }

    /// As [`GpuMesh::from_vertices`], for a worker thread, in buffers of at
    /// most `chunk_bytes`, the uploads submitted and waited for every
    /// `chunk_bytes`. A buffer's contents reach the GPU as a copy queued
    /// with the next submit, whoever makes it: made all at once, a
    /// 5M-triangle mesh's copy (hundreds of megabytes) would land in a UI
    /// frame. Paced, no frame waits for more than `chunk_bytes` of copies.
    ///
    /// Panics if the device is lost while waiting (callers on workers
    /// report the panic).
    pub fn from_vertices_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        v: &MeshVertices,
        chunk_bytes: u64,
    ) -> GpuMesh {
        let mut gpu = GpuMesh::new(v.origin, chunk_bytes, chunk_bytes);
        let mut up = Uploader::new(device, Some(queue), &gpu);
        gpu.apply(&mut up, v);
        gpu.finish(up, v.triangle_count);
        gpu
    }

    /// Make the GPU side equal `v`: uploads exactly the chunks whose `Arc`
    /// differs from the one on the GPU (all of them if the origin moved),
    /// drops chunks past `v`'s count, and re-uploads the outlines. Paced
    /// like the constructor this mesh was made with.
    pub fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        v: &MeshVertices,
    ) -> UploadStats {
        let mut up = Uploader::new(device, Some(queue), self);
        self.apply(&mut up, v);
        self.finish(up, v.triangle_count);
        self.last
    }

    fn apply(&mut self, up: &mut Uploader<'_>, v: &MeshVertices) {
        let same_origin = self.origin == v.origin;
        self.origin = v.origin;
        self.refresh(
            up,
            v.chunks.len(),
            |i, c| same_origin && std::ptr::eq(c.source.as_ptr(), Arc::as_ptr(&v.chunks[i])),
            |i| v.chunks[i].clone(),
        );
        self.lines = up.lines(&v.lines);
    }

    /// As [`GpuMesh::from_vertices_paced`], straight from `mesh` (vertices
    /// relative to `origin`, no outlines for the loops in `skip`). See
    /// [`GpuMesh::from_previous`].
    pub fn from_mesh_paced(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mesh: &Mesh,
        origin: DVec3,
        skip: &[u32],
        chunk_bytes: u64,
    ) -> GpuMesh {
        GpuMesh::from_previous(device, queue, None, mesh, origin, skip, chunk_bytes)
    }

    /// The GPU mesh for `next`, made on a worker from `prev`: the revision
    /// on screen and its GPU mesh, which is left as it is (it may still be
    /// drawn). Every chunk of `prev` that is still valid for `next` (see
    /// [`MeshVertices::build_from`]) is shared, not uploaded; the others
    /// are generated and uploaded one chunk at a time, so at most one
    /// chunk's vertices exist on the CPU, not the whole mesh's (hundreds
    /// of megabytes at 5M triangles). Uploads are paced every
    /// `chunk_bytes` (see [`GpuMesh::from_vertices_paced`]). With an
    /// `origin` other than `prev`'s, nothing is reused.
    ///
    /// Gives the same buffers as [`MeshVertices::build_from_skipping`] then
    /// [`GpuMesh::sync`]; [`GpuMesh::last_upload`] says what was uploaded.
    pub fn from_previous(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prev: Option<(&Mesh, &GpuMesh)>,
        next: &Mesh,
        origin: DVec3,
        skip: &[u32],
        chunk_bytes: u64,
    ) -> GpuMesh {
        let mut gpu = GpuMesh::new(origin, chunk_bytes, chunk_bytes);
        let mut reuse = None;
        if let Some((mesh, shown)) = prev {
            assert_eq!(
                shown.chunks.len(),
                mesh.face_chunk_count(),
                "the previous GPU mesh was built for another mesh"
            );
            gpu.chunks = shown.chunks.clone();
            gpu.total = shown.total;
            reuse = (shown.origin == origin).then(|| Reuse::new(mesh, next));
        }
        let mut up = Uploader::new(device, Some(queue), &gpu);
        gpu.refresh(
            &mut up,
            next.face_chunk_count(),
            |i, c| reuse.as_ref().is_some_and(|r| r.chunk(i, &c.vertex_chunks)),
            |i| Arc::new(ChunkVertices::build(next, i, origin)),
        );
        let lines = boundary_line_vertices_skipping(next, origin, skip);
        gpu.lines = up.lines(&lines);
        gpu.finish(up, next.triangle_count());
        gpu
    }

    /// Keep each of the first `n` chunks that `keep` says is still valid,
    /// upload `build(i)` in place of the others, and drop any past `n`.
    fn refresh(
        &mut self,
        up: &mut Uploader<'_>,
        n: usize,
        keep: impl Fn(usize, &GpuChunk) -> bool,
        build: impl Fn(usize) -> Arc<ChunkVertices>,
    ) {
        let mut old = std::mem::take(&mut self.chunks).into_iter();
        self.chunks = (0..n)
            .map(|i| match old.next() {
                Some(c) if keep(i, &c) => c,
                _ => up.surface(&build(i)),
            })
            .collect();
    }

    /// The last uploads (under `pace_bytes`) aren't waited for: they go
    /// with the next submit, which on the desktop is the worker's priming
    /// draw. (Waiting for them here measured slower first UI frames.)
    fn finish(&mut self, up: Uploader<'_>, triangle_count: usize) {
        self.triangle_count = triangle_count;
        self.last = up.stats;
        self.total += up.stats.total();
    }

    /// Read face chunk `i`'s surface vertices back from the GPU: what was
    /// actually uploaded (tests and diagnostics; waits for the GPU).
    pub fn read_surface(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        i: usize,
    ) -> Result<Vec<SurfaceVertex>, RenderError> {
        let Some(chunk) = self.chunks.get(i) else {
            return Err(RenderError::Readback(format!(
                "no face chunk {i} (there are {})",
                self.chunks.len()
            )));
        };
        let mut out = Vec::new();
        for (buffer, _) in chunk.buffers.iter() {
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mf surface readback"),
                size: buffer.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(buffer, 0, &readback, 0, buffer.size());
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                // allow-swallow: the receiver is read right after this; a send can only fail if it was dropped
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
            out.extend_from_slice(bytemuck::cast_slice::<u8, SurfaceVertex>(&data));
            drop(data);
            readback.unmap();
        }
        Ok(out)
    }
}

/// Surface buffers can be read back ([`GpuMesh::read_surface`]).
const SURFACE: wgpu::BufferUsages = wgpu::BufferUsages::VERTEX.union(wgpu::BufferUsages::COPY_SRC);

/// Creates and fills buffers for one update, counting the bytes and, when
/// paced, waiting for the uploads every `pace_bytes`.
struct Uploader<'a> {
    device: &'a wgpu::Device,
    pace: Option<&'a wgpu::Queue>,
    pace_bytes: u64,
    max_bytes: u64,
    unflushed: u64,
    stats: UploadStats,
}

impl<'a> Uploader<'a> {
    fn new(device: &'a wgpu::Device, pace: Option<&'a wgpu::Queue>, gpu: &GpuMesh) -> Self {
        Uploader {
            device,
            pace,
            pace_bytes: gpu.pace_bytes,
            max_bytes: gpu.max_bytes.min(device.limits().max_buffer_size),
            unflushed: 0,
            stats: UploadStats::default(),
        }
    }

    fn surface(&mut self, v: &Arc<ChunkVertices>) -> GpuChunk {
        let buffers = self.buffers("mf surface", &v.surface, 3, SURFACE);
        self.stats.surface_bytes += std::mem::size_of_val(&v.surface[..]) as u64;
        GpuChunk {
            source: Arc::downgrade(v),
            vertex_chunks: v.vertex_chunks.clone().into(),
            buffers: buffers.into(),
        }
    }

    fn lines(&mut self, lines: &[LineVertex]) -> Vec<(wgpu::Buffer, u32)> {
        self.stats.line_bytes += std::mem::size_of_val(lines) as u64;
        self.buffers("mf boundary lines", lines, 2, wgpu::BufferUsages::VERTEX)
    }

    /// Vertex buffers for `data`, split under the buffer limit without
    /// splitting a primitive.
    fn buffers<T: Pod>(
        &mut self,
        label: &str,
        data: &[T],
        primitive: u32,
        usage: wgpu::BufferUsages,
    ) -> Vec<(wgpu::Buffer, u32)> {
        let stride = std::mem::size_of::<T>() as u64;
        chunk_ranges(data.len() as u32, stride, self.max_bytes, primitive)
            .into_iter()
            .map(|r| {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(label),
                        contents: bytemuck::cast_slice(&data[r.start as usize..r.end as usize]),
                        usage,
                    });
                self.unflushed += stride * r.len() as u64;
                if let Some(queue) = self.pace
                    && self.unflushed >= self.pace_bytes
                {
                    flush(self.device, queue);
                    self.unflushed = 0;
                }
                (buffer, r.len() as u32)
            })
            .collect()
    }
}

/// Vertex buffers for `data` (overlays), as [`Uploader::buffers`]: paced
/// with `pace` every `max_bytes`.
fn upload_chunks<T: Pod>(
    device: &wgpu::Device,
    pace: Option<&wgpu::Queue>,
    label: &str,
    data: &[T],
    max_bytes: u64,
    primitive: u32,
) -> Vec<(wgpu::Buffer, u32)> {
    let mut up = Uploader {
        device,
        pace,
        pace_bytes: max_bytes,
        max_bytes: max_bytes.min(device.limits().max_buffer_size),
        unflushed: 0,
        stats: UploadStats::default(),
    };
    up.buffers(label, data, primitive, wgpu::BufferUsages::VERTEX)
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
