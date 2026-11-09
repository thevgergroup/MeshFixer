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

/// Rotate point `p` about the axis through `o` with unit direction `k`.
fn rotate(p: [f64; 3], o: [f64; 3], k: [f64; 3], angle: f64) -> [f64; 3] {
    let v = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
    let (s, c) = angle.sin_cos();
    let dot = k[0] * v[0] + k[1] * v[1] + k[2] * v[2];
    let cross = [
        k[1] * v[2] - k[2] * v[1],
        k[2] * v[0] - k[0] * v[2],
        k[0] * v[1] - k[1] * v[0],
    ];
    [0, 1, 2].map(|i| o[i] + v[i] * c + cross[i] * s + k[i] * dot * (1.0 - c))
}

#[test]
fn pinched_loops_survive_a_steeply_folded_fan() {
    // The bowtie fixture, with everything on the lower-left side of the
    // diagonal through (2,2) folded 150° about that diagonal. The fan
    // structure is unchanged; only the geometry is folded.
    let n = 4;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let flat = without(&grid(n), &[cell(1, 1), cell(2, 2) + 1]);
    let (mut positions, triangles) = flat.to_arrays();
    let o = [2.0, 2.0, 0.0];
    let k = [
        std::f64::consts::FRAC_1_SQRT_2,
        -std::f64::consts::FRAC_1_SQRT_2,
        0.0,
    ];
    for p in positions.iter_mut() {
        if p[0] + p[1] < 4.0 - 1e-9 {
            *p = rotate(*p, o, k, 150f64.to_radians());
        }
    }
    let folded = Mesh::new(positions, triangles).unwrap();
    let d = inspect(&folded);
    assert_eq!(d.untraced_boundary_edges, 0, "{:?}", d.boundary_loops);
    let mut sizes: Vec<usize> = d.boundary_loops.iter().map(|l| l.edge_count).collect();
    sizes.sort();
    assert_eq!(sizes, vec![3, 3, 16]);
}

#[test]
fn each_pinched_loop_is_exactly_its_removed_triangle() {
    let n = 6;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let base = grid(n);
    let holes = [cell(2, 2), cell(3, 3), cell(2, 3)];
    let m = without(&base, &holes);
    let topo = m.topology();
    let mut expected: Vec<Vec<u32>> = holes
        .iter()
        .map(|&f| {
            let mut t = base.face(FaceId(f)).unwrap().to_vec();
            t.sort();
            t
        })
        .collect();
    expected.sort();
    let mut got: Vec<Vec<u32>> = topo
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count == 3)
        .map(|l| {
            let mut v = l.vertices.clone();
            v.sort();
            v
        })
        .collect();
    got.sort();
    assert_eq!(got, expected);
}

#[test]
fn pinched_pairing_never_pairs_a_fan_with_itself() {
    // Three triangular holes meeting at (3,3) (alternate fan triangles
    // removed). Then move two vertices of one remaining fan triangle past
    // each other so that fan's projected wedge is reversed. Topology is
    // unchanged, so the three 3-edge holes must still be traced as such.
    let n = 6;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let holes = [cell(2, 2), cell(3, 3), cell(2, 3)];
    let flat = without(&grid(n), &holes);
    let (mut positions, triangles) = flat.to_arrays();
    let idx = |i: u32, j: u32| (j * (n + 1) + i) as usize;
    positions[idx(4, 3)] = [3.3, 3.9, 0.2];
    positions[idx(4, 4)] = [4.2, 3.2, -0.2];
    let bent = Mesh::new(positions, triangles).unwrap();
    let d = inspect(&bent);
    let small = d
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count == 3)
        .count();
    assert_eq!(
        (small, d.untraced_boundary_edges),
        (3, 0),
        "{:?}",
        d.boundary_loops
    );
}

/// Non-manifold vertices from corner fans alone (no >2-face edge), and the
/// look-alikes that are manifold (M4 Task 9 review I1). Each case is a
/// mesh and its expected `non_manifold_vertices`.
mod fans {
    use mf_geometry::{Mesh, Topology};

    fn nmv(positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>) -> usize {
        let topo = Topology::compute(&Mesh::new(positions, triangles).unwrap());
        assert!(topo.non_manifold_edges.is_empty(), "{topo:?}");
        topo.non_manifold_vertices
    }

    /// A closed fan around hub `base`: a pyramid with apex `base` and rim
    /// `base + 1..=4`, closed by a two-triangle base, offset by `d`.
    fn closed_fan(base: u32, d: [f64; 3]) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let p = vec![
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
        ];
        let p = p
            .into_iter()
            .map(|q| [q[0] + d[0], q[1] + d[1], q[2] + d[2]])
            .collect();
        let b = |i: u32| base + i;
        let t = vec![
            [b(0), b(1), b(2)],
            [b(0), b(2), b(3)],
            [b(0), b(3), b(4)],
            [b(0), b(4), b(1)],
            [b(1), b(3), b(2)],
            [b(1), b(4), b(3)],
        ];
        (p, t)
    }

    #[test]
    fn two_closed_solids_touching_at_a_vertex() {
        // Two pyramids (octahedron halves closed by a base) sharing apex 0.
        let (mut p, mut t) = closed_fan(0, [0.0; 3]);
        // The second one, upside down, reusing vertex 0 as its apex.
        let base = p.len() as u32;
        for q in [
            [1.0, 0.0, 2.0],
            [0.0, 1.0, 2.0],
            [-1.0, 0.0, 2.0],
            [0.0, -1.0, 2.0],
        ] {
            p.push(q);
        }
        let r = |i: u32| base + i;
        t.extend([
            [0, r(1), r(0)],
            [0, r(2), r(1)],
            [0, r(3), r(2)],
            [0, r(0), r(3)],
            [r(0), r(1), r(2)],
            [r(0), r(2), r(3)],
        ]);
        assert_eq!(nmv(p, t), 1);
    }

    #[test]
    fn bowtie() {
        let p = vec![
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, -1.0, 0.0],
        ];
        assert_eq!(nmv(p, vec![[0, 2, 1], [0, 3, 4]]), 1);
    }

    #[test]
    fn closed_fan_plus_a_triangle_touching_only_its_hub() {
        let (mut p, mut t) = closed_fan(0, [0.0; 3]);
        p.extend([[3.0, 3.0, 1.0], [3.0, 4.0, 1.0]]);
        t.push([0, 5, 6]);
        assert_eq!(nmv(p, t), 1);
    }

    #[test]
    fn manifold_look_alikes_have_none() {
        // Open fan: four triangles around hub 0, rim not closed.
        let p = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
        ];
        let open = vec![[0, 1, 2], [0, 2, 3], [0, 3, 4]];
        assert_eq!(nmv(p.clone(), open.clone()), 0, "open fan");
        // Closed fan (around the hub of a closed solid).
        let (cp, ct) = closed_fan(0, [0.0; 3]);
        assert_eq!(nmv(cp.clone(), ct.clone()), 0, "closed fan");
        // Inconsistent orientation: one face of the closed fan flipped.
        let mut flipped = ct.clone();
        flipped[1] = [flipped[1][0], flipped[1][2], flipped[1][1]];
        assert_eq!(nmv(cp.clone(), flipped), 0, "inconsistent fan");
        // A face and its flipped duplicate: a closed two-face sheet.
        let tri = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(
            nmv(tri.clone(), vec![[0, 1, 2], [0, 2, 1]]),
            0,
            "flipped pair"
        );
        // Same-orientation duplicate pair.
        assert_eq!(nmv(tri, vec![[0, 1, 2], [0, 1, 2]]), 0, "duplicate pair");
        // A collapsed face at the hub doesn't count as a fan.
        let mut collapsed = open;
        collapsed.push([0, 0, 1]);
        assert_eq!(nmv(p, collapsed), 0, "collapsed face at the hub");
    }
}
