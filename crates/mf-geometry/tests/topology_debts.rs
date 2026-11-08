#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Pinched boundary loops, tolerance and face duplicates, non-finite values,
//! deterministic parallel topology (MF-ISSUE-007, -012, -008, -003).

use mf_geometry::{FaceId, Mesh, VolumeStatus, inspect};

/// A flat (n+1)×(n+1) vertex grid in z=0, two CCW triangles per cell.
/// Cell (i, j) has faces 2*(j*n+i) (lower-right) and 2*(j*n+i)+1 (upper-left).
fn grid(n: u32) -> Mesh {
    let mut positions = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            positions.push([i as f64, j as f64, 0.0]);
        }
    }
    let v = |i: u32, j: u32| j * (n + 1) + i;
    let mut tris = Vec::new();
    for j in 0..n {
        for i in 0..n {
            tris.push([v(i, j), v(i + 1, j), v(i + 1, j + 1)]);
            tris.push([v(i, j), v(i + 1, j + 1), v(i, j + 1)]);
        }
    }
    Mesh::new(positions, tris).unwrap()
}

fn without(m: &Mesh, faces: &[u32]) -> Mesh {
    let mut e = m.edit();
    for &f in faces {
        e.remove_face(FaceId(f)).unwrap();
    }
    e.finish().unwrap().0
}

#[test]
fn bowtie_holes_trace_as_two_loops() {
    // 4×4 grid; vertex (2,2) is shared by cells (1,1) and (2,2). Remove the
    // upper-left triangle of cell (1,1) and the lower-right of cell (2,2):
    // two triangular holes touching only at (2,2).
    let n = 4;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let m = without(&grid(n), &[cell(1, 1), cell(2, 2) + 1]);
    let d = inspect(&m);
    assert_eq!(d.untraced_boundary_edges, 0);
    let mut sizes: Vec<usize> = d.boundary_loops.iter().map(|l| l.edge_count).collect();
    sizes.sort();
    assert_eq!(
        sizes,
        vec![3, 3, 16],
        "two pinched triangle holes + outer boundary"
    );
}

#[test]
fn three_loops_meeting_at_one_vertex() {
    // 6×6 grid: vertex (3,3) has six incident triangles in fan order. Remove
    // alternate ones (lower-right of cells (2,2), (3,3) and (2,3)) so the
    // three holes share only that vertex, never an edge.
    let n = 6;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let holes = [cell(2, 2), cell(3, 3), cell(2, 3)];
    let m = without(&grid(n), &holes);
    let d = inspect(&m);
    assert_eq!(d.untraced_boundary_edges, 0);
    let small: Vec<usize> = d
        .boundary_loops
        .iter()
        .map(|l| l.edge_count)
        .filter(|&e| e == 3)
        .collect();
    assert_eq!(small.len(), 3, "{:?}", d.boundary_loops);
    // Deterministic IDs and order: recomputing gives the same loops.
    let again = inspect(&without(&grid(n), &holes));
    assert_eq!(d.boundary_loops, again.boundary_loops);
}

#[test]
fn near_duplicates_counted() {
    // Two pairs within 1e-9, one pair 1e-3 apart (beyond 1e-6 × diagonal).
    let m = Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1e-9, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1e-9, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0 + 1e-3, 0.0],
        ],
        vec![[0, 2, 4], [1, 3, 5]],
    )
    .unwrap();
    assert_eq!(inspect(&m).duplicate_vertices_tolerance, 2);
    assert_eq!(inspect(&m).duplicate_vertices_exact, 0);
}

#[test]
fn duplicate_face_counted() {
    let m = Mesh::new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 2], [1, 2, 0], [0, 2, 1]],
    )
    .unwrap();
    assert_eq!(inspect(&m).duplicate_faces, 2);
}

#[test]
fn huge_coordinates_report_non_finite() {
    let big = 1e200;
    let m = Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [big, 0.0, 0.0],
            [0.0, big, 0.0],
            [0.0, 0.0, big],
        ],
        vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]],
    )
    .unwrap();
    let d = inspect(&m);
    assert_eq!(d.volume_status, VolumeStatus::NonFinite);
    assert_eq!(d.signed_volume, None);
    assert_eq!(d.surface_area, None);
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["volume_status"], "non_finite");
}

#[test]
fn topology_identical_at_1_and_8_threads() {
    let m = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(60_000, 3));
    let run = |threads| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| mf_geometry::Topology::compute(&m))
    };
    assert_eq!(run(1), run(8));
}
