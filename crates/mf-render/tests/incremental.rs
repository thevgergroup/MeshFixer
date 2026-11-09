#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Per-chunk GPU geometry with partial re-upload (MF-RENDER-003). A new
//! revision uploads only the face chunks whose faces, or the vertices they
//! reference, changed; everything on the GPU always equals a full rebuild.
//!
//! "The GPU side" here is read back from the GPU buffers themselves (the
//! CPU mirror of what was uploaded), chunk by chunk, and compared with a
//! full rebuild of the same revision at the same local origin.

use glam::DVec3;
use mf_fixtures::{cube, uv_sphere};
use mf_geometry::{CHUNK, FaceId, Mesh, Sources, VertexId};
use mf_render::{
    ChunkVertices, GpuMesh, MeshVertices, SurfaceVertex, UploadStats, boundary_line_vertices,
    offscreen, render_origin,
};

/// Bytes of one full face chunk's surface (three vertices per face).
const SURFACE_CHUNK_BYTES: u64 = (CHUNK * 3 * std::mem::size_of::<SurfaceVertex>()) as u64;
/// What the desktop paces uploads at.
const PACE: u64 = 16 << 20;

fn gpu(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    match offscreen::shared_device() {
        Ok(d) => Some(d),
        Err(_) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
    }
}

/// Every face chunk on the GPU equals that chunk built from scratch for
/// `mesh` at the GPU mesh's origin, and nothing is left past the end.
fn assert_matches_full_rebuild(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    gpu: &GpuMesh,
    mesh: &Mesh,
    what: &str,
) {
    assert_eq!(gpu.chunk_count(), mesh.face_chunk_count(), "{what}: chunks");
    assert_eq!(gpu.triangle_count, mesh.triangle_count(), "{what}");
    for i in 0..mesh.face_chunk_count() {
        let on_gpu = gpu.read_surface(device, queue, i).unwrap();
        let rebuilt = ChunkVertices::build(mesh, i, gpu.origin);
        assert!(
            on_gpu == rebuilt.surface,
            "{what}: face chunk {i} differs from a full rebuild ({} vs {} vertices)",
            on_gpu.len(),
            rebuilt.surface.len()
        );
    }
    assert_eq!(
        gpu.line_vertex_count(),
        boundary_line_vertices(mesh, gpu.origin).len(),
        "{what}: outlines"
    );
}

/// A strip hole of four quads in the middle of a UV sphere's `ring`.
fn punch_hole(mesh: &Mesh, segments: u32, ring: u32) -> Mesh {
    let first = segments + (ring - 1) * 2 * segments + 2 * (segments / 3);
    let mut e = mesh.edit();
    for f in first..first + 8 {
        e.remove_face(FaceId(f)).unwrap();
    }
    e.finish().unwrap().0
}

/// What a fill does to the storage: one new (centre) vertex, appended, and a
/// fan of new faces, appended. The surrounding faces are untouched.
fn fill_one_hole(mesh: &Mesh) -> Mesh {
    let topo = mesh.topology();
    assert_eq!(topo.boundary_loops.len(), 1);
    let l = &topo.boundary_loops[0];
    let mut c = [0.0; 3];
    for &v in &l.vertices {
        let p = mesh.corner_position(v);
        (0..3).for_each(|k| c[k] += p[k] / l.vertices.len() as f64);
    }
    let mut e = mesh.edit();
    let centre = e.add_vertex(c);
    let n = l.vertices.len();
    for k in 0..n {
        let tri = [l.vertices[(k + 1) % n], l.vertices[k], centre.0];
        e.add_face(tri, Sources::default()).unwrap();
    }
    e.finish().unwrap().0
}

#[test]
fn filling_one_hole_reuploads_only_touched_chunks() {
    let Some((device, queue)) = gpu("filling_one_hole_reuploads_only_touched_chunks") else {
        return;
    };
    let holed = punch_hole(&uv_sphere(500, 1000), 1000, 250); // ~1M triangles
    let filled = fill_one_hole(&holed);
    assert!(holed.triangle_count() > 990_000);

    let before = MeshVertices::build_from(None, &holed);
    let mut gpu = GpuMesh::from_vertices(&device, &before);
    let full = gpu.total_uploaded();
    let after = MeshVertices::build_from(Some((&holed, &before)), &filled);
    assert_eq!(after.origin, before.origin, "the origin is kept");
    let stats = gpu.sync(&device, &queue, &after);
    eprintln!(
        "1M one-hole fill: {} surface bytes re-uploaded of {} ({} chunks of {}), {} line bytes",
        stats.surface_bytes,
        full,
        holed.face_chunk_count(),
        SURFACE_CHUNK_BYTES,
        stats.line_bytes,
    );
    assert!(stats.surface_bytes > 0, "the new faces were uploaded");
    assert!(
        stats.surface_bytes <= 3 * SURFACE_CHUNK_BYTES,
        "at most two touched chunks plus the appended one: {stats:?}"
    );
    // In fact only the chunk the fan was appended to: the centre vertex was
    // appended too, which copies the last vertex chunk but keeps every
    // position the other chunks' faces use.
    let rebuilt: Vec<usize> = (0..after.chunks.len())
        .filter(|&i| {
            !before
                .chunks
                .get(i)
                .is_some_and(|c| std::sync::Arc::ptr_eq(c, &after.chunks[i]))
        })
        .collect();
    assert_eq!(rebuilt, vec![filled.face_chunk_count() - 1]);
    assert_eq!(stats.state_bytes, 0);
    assert_eq!(gpu.total_uploaded(), full + stats.total());
    assert_matches_full_rebuild(&device, &queue, &gpu, &filled, "synced");
    drop(gpu);
    drop((before, after));

    // The workers' path (no CPU copy of the whole mesh) decides the same.
    let shown =
        GpuMesh::from_previous(&device, &queue, None, &holed, gpu_origin(&holed), &[], PACE);
    let origin = render_origin(Some(shown.origin), filled.bounding_box());
    let next = GpuMesh::from_previous(
        &device,
        &queue,
        Some((&holed, &shown)),
        &filled,
        origin,
        &[],
        PACE,
    );
    assert_eq!(next.last_upload().surface_bytes, stats.surface_bytes);
    assert_eq!(
        next.total_uploaded(),
        shown.total_uploaded() + next.last_upload().total()
    );
    assert_matches_full_rebuild(&device, &queue, &next, &filled, "from_previous");
}

fn gpu_origin(mesh: &Mesh) -> DVec3 {
    render_origin(None, mesh.bounding_box())
}

/// What is on screen: a revision and both ways of having uploaded it.
struct Shown {
    mesh: Mesh,
    vertices: MeshVertices,
    synced: GpuMesh,
    streamed: GpuMesh,
}

impl Shown {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) -> Shown {
        let vertices = MeshVertices::build_from(None, mesh);
        let synced = GpuMesh::from_vertices(device, &vertices);
        let streamed =
            GpuMesh::from_previous(device, queue, None, mesh, gpu_origin(mesh), &[], PACE);
        Shown {
            mesh: mesh.clone(),
            vertices,
            synced,
            streamed,
        }
    }

    /// Replace what is shown with `next`, both ways, and check both against
    /// a full rebuild. Returns the upload.
    fn show(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        next: &Mesh,
        what: &str,
    ) -> UploadStats {
        let vertices = MeshVertices::build_from(Some((&self.mesh, &self.vertices)), next);
        let stats = self.synced.sync(device, queue, &vertices);
        let origin = render_origin(Some(self.streamed.origin), next.bounding_box());
        let streamed = GpuMesh::from_previous(
            device,
            queue,
            Some((&self.mesh, &self.streamed)),
            next,
            origin,
            &[],
            PACE,
        );
        assert_eq!(
            streamed.last_upload(),
            stats,
            "{what}: both paths reuse the same chunks"
        );
        assert_matches_full_rebuild(device, queue, &self.synced, next, what);
        assert_matches_full_rebuild(device, queue, &streamed, next, what);
        self.mesh = next.clone();
        self.vertices = vertices;
        self.streamed = streamed;
        stats
    }
}

/// Review Focus 2: A → B → undo → branch C → checkout B. Reuse is decided
/// against the revision being replaced on screen, never by assuming the
/// new one descends from it, so no step leaves a stale chunk behind.
#[test]
fn incremental_upload_matches_full_rebuild_after_checkout() {
    let Some((device, queue)) = gpu("incremental_upload_matches_full_rebuild_after_checkout")
    else {
        return;
    };
    let a = uv_sphere(256, 512); // 261,120 faces: 8 face chunks, 4 vertex chunks
    assert_eq!(a.face_chunk_count(), 8);
    assert_eq!(a.position_chunk_count(), 4);
    let full = a.face_chunk_count() as u64 * SURFACE_CHUNK_BYTES;

    // B moves a vertex in vertex chunk 1 and appends a face.
    let mut e = a.edit();
    e.move_vertex(VertexId(40_000), [0.0, 0.0, 1.5]).unwrap();
    e.add_face([40_000, 40_001, 40_513], Sources::default())
        .unwrap();
    let (b, _) = e.finish().unwrap();
    // C, a branch off A: removes a face in chunk 4 (a tombstone bit only),
    // moves a vertex in the last vertex chunk and appends two faces into
    // the same slots as B's one, with different contents.
    let mut e = a.edit();
    e.remove_face(FaceId(4 * CHUNK as u32 + 7)).unwrap();
    e.move_vertex(VertexId(120_000), [0.0, 1.5, 0.0]).unwrap();
    e.add_face([120_000, 120_001, 120_513], Sources::default())
        .unwrap();
    e.add_face([1, 2, 3], Sources::default()).unwrap();
    let (c, _) = e.finish().unwrap();
    // Only the removal tells chunk 4 apart: its storage and every vertex
    // chunk it references are shared with A.
    assert!(c.shares_face_chunk(&a, 4));
    let chunk4 = ChunkVertices::build(&a, 4, DVec3::ZERO).vertex_chunks;
    assert!(
        chunk4
            .iter()
            .all(|&j| c.vertex_chunk_shared(&a, j as usize))
    );

    let mut shown = Shown::new(&device, &queue, &a);
    assert_matches_full_rebuild(&device, &queue, &shown.synced, &a, "A");
    let partial = |shown: &mut Shown, next: &Mesh, what: &str| {
        let stats = shown.show(&device, &queue, next, what);
        eprintln!("{what}: {} of {full} surface bytes", stats.surface_bytes);
        assert!(stats.surface_bytes > 0, "{what}: something changed");
        assert!(
            stats.surface_bytes < full,
            "{what}: some chunks were reused"
        );
    };
    partial(&mut shown, &b, "A → B");
    partial(&mut shown, &a, "undo to A");
    partial(&mut shown, &c, "branch C");
    let c_vertices = shown.vertices.clone();
    partial(&mut shown, &b, "checkout B");

    // Identity, not history: B's buffers synced straight to C's vertices
    // (built earlier, on the other branch) land exactly on C.
    let mut synced = shown.synced;
    synced.sync(&device, &queue, &c_vertices);
    assert_matches_full_rebuild(&device, &queue, &synced, &c, "B's buffers synced to C");
}

/// A vertex is shared by faces in several chunks: moving it re-uploads every
/// chunk that references its vertex chunk, and only those.
#[test]
fn moved_vertex_invalidates_every_chunk_that_references_it() {
    let Some((device, queue)) = gpu("moved_vertex_invalidates_every_chunk_that_references_it")
    else {
        return;
    };
    // Three full face chunks of separate triangles; face f uses vertices
    // 1 + 3f ..= 3 + 3f, so face chunk 1 references vertex chunks 3–6 only.
    // Vertex 0 (vertex chunk 0) is also a corner of one face in chunk 2.
    let faces = 3 * CHUNK;
    let mut positions = vec![[0.0, 0.0, 0.0]];
    let mut triangles = Vec::with_capacity(faces);
    for f in 0..faces {
        let base = 1 + 3 * f as u32;
        let x = f as f64 * 0.001;
        positions.extend([[x, 0.0, 0.0], [x + 0.001, 0.0, 0.0], [x, 0.001, 0.0]]);
        triangles.push([base, base + 1, base + 2]);
    }
    triangles[2 * CHUNK + 10][0] = 0;
    let a = Mesh::new(positions, triangles).unwrap();
    let before = MeshVertices::build_from(None, &a);
    assert_eq!(before.chunks.len(), 3);
    assert_eq!(before.chunks[0].vertex_chunks, vec![0, 1, 2, 3]);
    assert_eq!(before.chunks[1].vertex_chunks, vec![3, 4, 5, 6]);
    assert!(before.chunks[2].vertex_chunks.contains(&0));
    let mut gpu = GpuMesh::from_vertices(&device, &before);

    let mut e = a.edit();
    e.move_vertex(VertexId(0), [0.0, 0.0, 1.0]).unwrap();
    let (moved, _) = e.finish().unwrap();
    let after = MeshVertices::build_from(Some((&a, &before)), &moved);
    let reused: Vec<bool> = (0..3)
        .map(|i| std::sync::Arc::ptr_eq(&before.chunks[i], &after.chunks[i]))
        .collect();
    assert_eq!(reused, vec![false, true, false]);
    let stats = gpu.sync(&device, &queue, &after);
    assert_eq!(stats.surface_bytes, 2 * SURFACE_CHUNK_BYTES);
    assert_matches_full_rebuild(&device, &queue, &gpu, &moved, "moved vertex 0");

    // Same through the workers' path.
    let shown = GpuMesh::from_previous(&device, &queue, None, &a, before.origin, &[], PACE);
    let next = GpuMesh::from_previous(
        &device,
        &queue,
        Some((&a, &shown)),
        &moved,
        before.origin,
        &[],
        PACE,
    );
    assert_eq!(next.last_upload().surface_bytes, 2 * SURFACE_CHUNK_BYTES);
    assert_matches_full_rebuild(&device, &queue, &next, &moved, "from_previous");
}

/// The local origin stays put across edits, so reused chunks stay valid,
/// while every point of the mesh is within 10³ diagonals of it (`f32` local
/// coordinates keep ~10⁻⁴ of a diagonal there). Once the mesh reaches
/// farther, the origin moves to the new centre and nothing is reused.
#[test]
fn origin_is_kept_unless_the_mesh_outgrows_f32_precision() {
    let a = uv_sphere(256, 512);
    let va = MeshVertices::build_from(None, &a);
    assert_eq!(va.origin, gpu_origin(&a));

    // A vertex pulled out to 10⁴ units: the bounds grow with it, so the
    // mesh is still well within 10³ (new) diagonals of the old origin.
    let mut e = a.edit();
    e.move_vertex(VertexId(1), [1.0e4, 0.0, 0.0]).unwrap();
    let (grown, _) = e.finish().unwrap();
    let vg = MeshVertices::build_from(Some((&a, &va)), &grown);
    assert_eq!(vg.origin, va.origin);
    assert!(std::sync::Arc::ptr_eq(&va.chunks[7], &vg.chunks[7]));

    // The whole sphere (diagonal ~3.5) moved 10⁵ units away: recomputed.
    let mut e = a.edit();
    for v in 0..a.vertex_slots() {
        let p = a.corner_position(v);
        e.move_vertex(VertexId(v), [p[0] + 1.0e5, p[1], p[2]])
            .unwrap();
    }
    let (far, _) = e.finish().unwrap();
    let vf = MeshVertices::build_from(Some((&a, &va)), &far);
    assert_eq!(vf.origin, gpu_origin(&far));
    assert!((0..8).all(|i| !std::sync::Arc::ptr_eq(&va.chunks[i], &vf.chunks[i])));

    let moved = mf_fixtures::translated(&cube(1.0), [3.0e5, 0.0, 0.0]);
    assert_eq!(
        render_origin(Some(DVec3::ZERO), moved.bounding_box()),
        gpu_origin(&moved)
    );
    let near = mf_fixtures::translated(&cube(1.0), [100.0, 0.0, 0.0]);
    assert_eq!(
        render_origin(Some(DVec3::ZERO), near.bounding_box()),
        DVec3::ZERO
    );
    // Nothing to draw: nothing to move.
    assert_eq!(render_origin(Some(DVec3::ONE), None), DVec3::ONE);
}
