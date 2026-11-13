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
                operands: vec![],
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

#[test]
fn removing_a_vertex_created_in_the_same_edit_is_checked() {
    let m = cube(1.0);
    let mut e = m.edit();
    let v = e.add_vertex([0.5, 0.5, 2.0]);
    e.add_face([4, 5, v.0], Sources::default()).unwrap();
    e.remove_vertex(v).unwrap();
    assert!(
        matches!(
            e.finish(),
            Err(mf_geometry::GeometryError::FaceUsesRemovedVertex { .. })
        ),
        "a live face still uses the removed vertex"
    );
}

#[test]
fn mixed_create_remove_sequences_never_leave_dangling_faces() {
    let base = cube(1.0);
    let mut seed = 99u64;
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 33) % n
    };
    for _ in 0..200 {
        let mut e = base.edit();
        let mut mine = Vec::new();
        for _ in 0..next(6) + 1 {
            match next(3) {
                0 => mine.push(e.add_vertex([next(5) as f64, 1.0, 2.0])),
                1 if !mine.is_empty() => {
                    let v = mine[next(mine.len() as u64) as usize];
                    let _ = e.add_face([0, 1, v.0], Sources::default());
                }
                _ if !mine.is_empty() => {
                    let v = mine[next(mine.len() as u64) as usize];
                    let _ = e.remove_vertex(v);
                }
                _ => {}
            }
        }
        if let Ok((out, _)) = e.finish() {
            for (_, t) in out.faces() {
                assert!(
                    t.iter().all(|&v| out.position(VertexId(v)).is_some()),
                    "face uses a dead vertex"
                );
            }
            let _ = out.compacted();
        }
    }
}

#[test]
fn edit_bookkeeping_is_not_quadratic() {
    // 100k moves and 20k create-then-remove pairs. Quadratic bookkeeping
    // takes tens of seconds here even in release; linear takes milliseconds.
    let m = uv_sphere(200, 500); // ~99.5k vertices
    let start = std::time::Instant::now();
    let mut e = m.edit();
    for v in 0..m.vertex_slots().min(100_000) {
        e.move_vertex(VertexId(v), [0.0, 0.0, 1.0]).unwrap();
    }
    for _ in 0..20_000 {
        let v = e.add_vertex([0.0, 0.0, 3.0]);
        let f = e.add_face([0, 1, v.0], Sources::default()).unwrap();
        e.remove_face(f).unwrap();
        e.remove_vertex(v).unwrap();
    }
    let (_, prov) = e.finish().unwrap();
    let elapsed = start.elapsed();
    assert!(prov.created_faces.is_empty() && prov.created_vertices.is_empty());
    assert_eq!(
        prov.moved_vertices.len(),
        m.vertex_slots().min(100_000) as usize
    );
    assert!(
        elapsed.as_secs_f64() < 3.0,
        "edit bookkeeping took {elapsed:?}"
    );
}

#[test]
fn set_face_keeps_the_id_and_records_a_modification() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.set_face(FaceId(0), [0, 3, 2]).unwrap();
    let (out, prov) = e.finish().unwrap();
    assert_eq!(out.face(FaceId(0)), Some([0, 3, 2]));
    assert_eq!(prov.modified_faces, vec![FaceId(0)]);
    assert!(prov.removed_faces.is_empty() && prov.created_faces.is_empty());
    let mut e = m.edit();
    assert!(e.set_face(FaceId(99), [0, 1, 2]).is_err());
    assert!(e.set_face(FaceId(1), [0, 1, 77]).is_err());
}

#[test]
fn compaction_is_expressible_as_provenance() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.remove_face(FaceId(0)).unwrap();
    let (tomb, _) = e.finish().unwrap();
    let (compact, map) = tomb.compacted();
    let prov = mf_geometry::Provenance::from_compaction(&map);
    let remap = prov.remap.as_ref().unwrap();
    assert_eq!(remap.faces[1], Some(FaceId(0)));
    assert_eq!(remap.faces[0], None);
    assert_eq!(compact.face(FaceId(0)), tomb.face(FaceId(1)));
}

#[test]
fn revision_hashing_only_touches_copied_chunks() {
    let m = uv_sphere(160, 320); // 4 face chunks, 2 position chunks
    let _ = m.content_hash();
    let _ = m.structure_hash();
    let before = mf_geometry::chunk_hashes_computed();
    let mut e = m.edit();
    e.remove_face(FaceId(5)).unwrap();
    let (out, _) = e.finish().unwrap();
    let _ = out.content_hash();
    let _ = out.structure_hash();
    let hashed = mf_geometry::chunk_hashes_computed() - before;
    assert_eq!(hashed, 1, "only the face-liveness chunk changed");
    assert_ne!(out.content_hash(), m.content_hash());
    assert_ne!(out.structure_hash(), m.structure_hash());
}

#[test]
fn structure_hash_ignores_positions_only() {
    let m = cube(1.0);
    let mut e = m.edit();
    e.move_vertex(VertexId(0), [-0.5, 0.0, 0.0]).unwrap();
    let (moved, _) = e.finish().unwrap();
    assert_ne!(moved.content_hash(), m.content_hash());
    assert_eq!(moved.structure_hash(), m.structure_hash());
    let mut e = m.edit();
    e.set_face(FaceId(0), [0, 1, 2]).unwrap(); // flip winding
    let (flipped, _) = e.finish().unwrap();
    assert_ne!(flipped.structure_hash(), m.structure_hash());
}

#[test]
fn sources_must_be_faces_of_the_input_revision() {
    // MF-ISSUE-023: a source that is itself created (and maybe later removed)
    // in the same edit would dangle. Sources name input faces only.
    let m = cube(1.0);
    let mut e = m.edit();
    let v = e.add_vertex([0.5, 0.5, 2.0]);
    let a = e.add_face([4, 5, v.0], Sources::default()).unwrap();
    let err = e.add_face(
        [5, 6, v.0],
        Sources {
            faces: vec![a],
            loops: vec![],
            operands: vec![],
        },
    );
    assert!(
        matches!(err, Err(mf_geometry::GeometryError::InvalidSource(f)) if f == a.0),
        "{err:?}"
    );
    let err = e.add_face(
        [5, 6, v.0],
        Sources {
            faces: vec![FaceId(500)],
            loops: vec![],
            operands: vec![],
        },
    );
    assert!(
        matches!(err, Err(mf_geometry::GeometryError::InvalidSource(500))),
        "{err:?}"
    );
    // Removed input faces are valid sources (retriangulation).
    e.remove_face(FaceId(2)).unwrap();
    assert!(
        e.add_face(
            [5, 6, v.0],
            Sources {
                faces: vec![FaceId(2)],
                loops: vec![],
                operands: vec![],
            }
        )
        .is_ok()
    );
}

/// What a renderer may reuse (MF-RENDER-003): a face chunk is shared only if
/// its faces *and* their liveness are, so a removal (which only flips a
/// tombstone bit) still counts as a change to that chunk, and only to it.
#[test]
fn face_chunk_shared_includes_liveness() {
    let m = uv_sphere(160, 320); // 101,760 faces: 4 face chunks
    let mut e = m.edit();
    e.remove_face(FaceId((CHUNK + 5) as u32)).unwrap();
    let (removed, _) = e.finish().unwrap();
    assert!(removed.shares_face_chunk(&m, 1), "storage is still shared");
    let shared: Vec<bool> = (0..m.face_chunk_count())
        .map(|i| removed.face_chunk_shared(&m, i))
        .collect();
    assert_eq!(shared, vec![true, false, true, true]);
    assert!(m.face_chunk_shared(&m, 0));
    assert!(
        !m.face_chunk_shared(&m, m.face_chunk_count()),
        "past the end"
    );

    // Appending a face copies the last chunk only.
    let mut e = m.edit();
    e.add_face([0, 1, 2], Sources::default()).unwrap();
    let (added, _) = e.finish().unwrap();
    let last = m.face_chunk_count() - 1;
    assert!(added.face_chunk_shared(&m, last - 1));
    assert!(!added.face_chunk_shared(&m, last));

    let mut e = m.edit();
    e.move_vertex(VertexId(40_000), [0.0, 0.0, 3.0]).unwrap();
    let (moved, _) = e.finish().unwrap();
    let vertex_shared: Vec<bool> = (0..m.position_chunk_count())
        .map(|j| moved.vertex_chunk_shared(&m, j))
        .collect();
    assert_eq!(vertex_shared, vec![true, false]);
    assert!((0..m.face_chunk_count()).all(|i| moved.face_chunk_shared(&m, i)));
}

/// Appending vertices copies the last vertex chunk, but keeps every
/// position already in it: faces built for the old revision are still right.
#[test]
fn vertex_chunk_kept_survives_appends_not_moves() {
    let m = uv_sphere(160, 320); // 50,882 vertices: 2 vertex chunks
    let mut e = m.edit();
    e.add_vertex([0.0, 0.0, 2.0]);
    let (appended, _) = e.finish().unwrap();
    assert!(!appended.vertex_chunk_shared(&m, 1), "copied on write");
    assert!(appended.vertex_chunk_kept(&m, 1));
    assert!(appended.vertex_chunk_kept(&m, 0));

    let mut e = appended.edit();
    e.move_vertex(VertexId(50_000), [0.0, 0.0, 3.0]).unwrap();
    let (moved, _) = e.finish().unwrap();
    assert!(!moved.vertex_chunk_kept(&m, 1));
    assert!(!moved.vertex_chunk_kept(&appended, 1));
    assert!(!m.vertex_chunk_kept(&m, 2), "past the end");
}

/// Task 16b (M4 review I2, MF-SCULPT-004, MF-DOC-002): 1,000 small strokes,
/// each a new revision moving a 7 × 7-vertex patch of a ~1M-triangle
/// sphere, are held in at most 1.5 × one revision plus the strokes'
/// footprints. A footprint is the position storage a stroke had to copy:
/// the [`STORAGE_CHUNK`]s holding its moved vertices (24 KB each). A
/// patch spans 7 rings of 1,000 vertices, so 7 storage chunks; with the
/// old 32,768-element chunks the same history copies over 4× more (5.5×
/// measured).
#[test]
fn thousand_small_strokes_history_within_budget() {
    use mf_geometry::STORAGE_CHUNK;
    let (rings, segments) = (500_u32, 1000_u32);
    let base = uv_sphere(rings, segments);
    let one = base.heap_bytes();
    let vertex = |r: u32, s: u32| 1 + (r - 1) * segments + s % segments;
    let mut seed = 0x5eed_u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 33
    };
    let mut history = vec![base.clone()];
    let (mut footprint, mut old_footprint) = (0u64, 0u64);
    let element = std::mem::size_of::<[f64; 3]>() as u64;
    for _ in 0..1000 {
        let prev = history.last().unwrap().clone();
        let (r0, s0) = (4 + next() as u32 % (rings - 8), next() as u32 % segments);
        let mut e = prev.edit();
        let (mut chunks, mut groups) = (BTreeSet::new(), BTreeSet::new());
        for r in r0 - 3..=r0 + 3 {
            for s in s0..s0 + 7 {
                let v = vertex(r, s);
                let p = prev.corner_position(v);
                e.move_vertex(VertexId(v), [p[0] * 1.001, p[1] * 1.001, p[2] * 1.001])
                    .unwrap();
                chunks.insert(v as usize / STORAGE_CHUNK);
                groups.insert(v as usize / CHUNK);
            }
        }
        footprint += chunks.len() as u64 * STORAGE_CHUNK as u64 * element;
        old_footprint += groups.len() as u64 * CHUNK as u64 * element;
        history.push(e.finish().unwrap().0);
    }
    let mut seen = HashSet::new();
    let total: u64 = history.iter().map(|m| m.unique_heap_bytes(&mut seen)).sum();
    let mb = |b: u64| b as f64 / (1 << 20) as f64;
    eprintln!(
        "1,000 strokes: history {:.1} MB; one revision {:.1} MB; footprints {:.1} MB \
         (with 32,768-element chunks: {:.1} MB)",
        mb(total),
        mb(one),
        mb(footprint),
        mb(old_footprint)
    );
    assert!(
        total as f64 <= 1.5 * one as f64 + footprint as f64,
        "history {total} > 1.5 × {one} + {footprint}"
    );
    assert!(old_footprint > 4 * footprint);
}
