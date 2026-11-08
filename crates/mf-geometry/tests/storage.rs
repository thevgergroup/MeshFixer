#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Stable-slot chunked storage, copy-on-write edits and provenance (ADR-0001).

use std::collections::{BTreeSet, HashSet};

use mf_fixtures::{cube, uv_sphere};
use mf_geometry::{CHUNK, FaceId, Mesh, Sources, VertexId};

#[test]
fn chunked_cow_shares_untouched_chunks() {
    let m = uv_sphere(160, 320); // 101,760 faces, 50,882 vertices
    assert!(m.face_slots() as usize > 3 * CHUNK);
    let all = |a: &Mesh, n: usize, f: &dyn Fn(&Mesh, &Mesh, usize) -> bool| {
        (0..n).filter(|&i| f(a, &m, i)).count()
    };

    // Removing a face only flips a tombstone bit: all geometry chunks stay shared.
    let mut e = m.edit();
    e.remove_face(FaceId(5)).unwrap();
    let (removed, _) = e.finish().unwrap();
    assert_eq!(
        all(&removed, removed.face_chunk_count(), &|a, b, i| a
            .shares_face_chunk(b, i)),
        m.face_chunk_count()
    );
    assert_eq!(
        all(&removed, removed.position_chunk_count(), &|a, b, i| a
            .shares_position_chunk(b, i)),
        m.position_chunk_count()
    );

    // Moving one vertex copies exactly one position chunk.
    let mut e = m.edit();
    e.move_vertex(VertexId(40_000), [0.0, 0.0, 3.0]).unwrap();
    let (moved, prov) = e.finish().unwrap();
    assert_eq!(
        all(&moved, moved.position_chunk_count(), &|a, b, i| a
            .shares_position_chunk(b, i)),
        m.position_chunk_count() - 1
    );
    assert_eq!(
        all(&moved, moved.face_chunk_count(), &|a, b, i| a
            .shares_face_chunk(b, i)),
        m.face_chunk_count()
    );
    assert_eq!(prov.moved_vertices, vec![VertexId(40_000)]);
}

#[test]
fn edit_remove_and_add_keeps_ids() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.remove_face(FaceId(2)).unwrap();
    e.remove_face(FaceId(3)).unwrap();
    let v = e.add_vertex([0.5, 0.5, 1.5]);
    let f = e
        .add_face(
            [4, 5, v.0],
            Sources {
                faces: vec![FaceId(2)],
                loops: vec![],
            },
        )
        .unwrap();
    let (out, prov) = e.finish().unwrap();
    assert_eq!(f, FaceId(12));
    assert_eq!(v, VertexId(8));
    assert_eq!(out.face_count(), 11);
    assert_eq!(out.face_slots(), 13);
    for (id, tri) in m.faces().filter(|(id, _)| id.0 != 2 && id.0 != 3) {
        assert_eq!(out.face(id), Some(tri), "kept face {id:?} unchanged");
    }
    assert_eq!(out.face(FaceId(2)), None);
    assert_eq!(prov.removed_faces, vec![FaceId(2), FaceId(3)]);
    assert_eq!(prov.created_faces.len(), 1);
    assert_eq!(prov.created_vertices, vec![VertexId(8)]);
}

#[test]
fn edit_rejects_invalid_references() {
    let m = cube(1.0);
    let mut e = m.edit();
    assert!(e.remove_face(FaceId(99)).is_err());
    assert!(e.add_face([0, 1, 42], Sources::default()).is_err());
    let mut e = m.edit();
    e.remove_vertex(VertexId(0)).unwrap();
    assert!(e.finish().is_err(), "vertex 0 is still used by live faces");
}

#[test]
fn provenance_is_total() {
    // Deterministic pseudo-random edit sequences.
    let base = uv_sphere(12, 24);
    let mut seed = 0x1234_5678u64;
    let mut next = |n: u32| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) % n as u64) as u32
    };
    for _ in 0..64 {
        let mut e = base.edit();
        let mut removed = BTreeSet::new();
        for _ in 0..next(20) {
            let f = next(base.face_slots());
            if removed.insert(f) {
                e.remove_face(FaceId(f)).unwrap();
            }
        }
        for _ in 0..next(5) {
            let a = next(base.vertex_slots());
            let v = e.add_vertex([0.0, 0.0, 2.0]);
            e.add_face([a, (a + 1) % base.vertex_slots(), v.0], Sources::default())
                .unwrap();
        }
        let (out, prov) = e.finish().unwrap();
        let created: HashSet<FaceId> = prov.created_faces.iter().map(|(f, _)| *f).collect();
        let removed: HashSet<FaceId> = prov.removed_faces.iter().copied().collect();
        for (id, tri) in out.faces() {
            let kept = base.face(id) == Some(tri);
            assert!(
                kept ^ created.contains(&id),
                "output face {id:?} must be kept xor created"
            );
        }
        for (id, _) in base.faces() {
            let kept = out.face(id) == base.face(id);
            assert!(
                kept ^ removed.contains(&id),
                "input face {id:?} must be kept xor removed"
            );
        }
    }
}

#[test]
fn compaction_maps_slots() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.remove_face(FaceId(0)).unwrap();
    let (out, _) = e.finish().unwrap();
    let (compact, map) = out.compacted();
    assert_eq!(compact.face_slots(), 11);
    assert_eq!(map.faces[0], None);
    assert_eq!(map.faces[1], Some(FaceId(0)));
    assert_eq!(compact.face(FaceId(0)), out.face(FaceId(1)));
}

#[test]
fn hash_includes_slot_identity() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.remove_face(FaceId(0)).unwrap();
    let (tomb, _) = e.finish().unwrap();
    let (compact, _) = tomb.compacted();
    assert_eq!(tomb.to_arrays(), compact.to_arrays(), "same live geometry");
    assert_ne!(
        tomb.content_hash(),
        compact.content_hash(),
        "different slot identity"
    );
}

#[test]
fn history_bytes_count_shared_chunks_once() {
    let m = uv_sphere(160, 320);
    let mut e = m.edit();
    e.remove_face(FaceId(0)).unwrap();
    let (out, _) = e.finish().unwrap();
    let mut seen = HashSet::new();
    let first = m.unique_heap_bytes(&mut seen);
    let second = out.unique_heap_bytes(&mut seen);
    assert!(first > 0);
    assert!(
        second < first / 4,
        "second revision adds only its copied chunks: {second} vs {first}"
    );
}

#[test]
fn new_rejects_counts_over_u32() {
    assert!(Mesh::check_counts(u32::MAX as usize + 1, 0).is_err());
    assert!(Mesh::check_counts(0, (u32::MAX as usize) / 3 + 1).is_err());
    assert!(Mesh::check_counts(10, 10).is_ok());
}

#[test]
fn removed_vertices_are_not_exported_or_counted() {
    let m = cube(1.0);
    let mut e = m.edit();
    let extra = e.add_vertex([9.0, 9.0, 9.0]);
    let (with_extra, _) = e.finish().unwrap();
    assert_eq!(with_extra.vertex_count(), 9);
    let mut e = with_extra.edit();
    e.remove_vertex(extra).unwrap();
    let (out, prov) = e.finish().unwrap();
    assert_eq!(out.vertex_count(), 8);
    assert_eq!(prov.removed_vertices, vec![extra]);
    assert_eq!(out.to_arrays().0.len(), 8);
}
