//! Deterministic procedural meshes for tests and benchmarks (spec §10).

use mf_geometry::Mesh;

/// Axis-aligned cube from the origin to `size`, outward CCW winding.
pub fn cube(size: f64) -> Mesh {
    let s = size;
    let positions = vec![
        [0.0, 0.0, 0.0],
        [s, 0.0, 0.0],
        [s, s, 0.0],
        [0.0, s, 0.0],
        [0.0, 0.0, s],
        [s, 0.0, s],
        [s, s, s],
        [0.0, s, s],
    ];
    let triangles = vec![
        [0, 2, 1],
        [0, 3, 2], // bottom (-z)
        [4, 5, 6],
        [4, 6, 7], // top (+z)
        [0, 1, 5],
        [0, 5, 4], // front (-y)
        [2, 3, 7],
        [2, 7, 6], // back (+y)
        [1, 2, 6],
        [1, 6, 5], // right (+x)
        [3, 0, 4],
        [3, 4, 7], // left (-x)
    ];
    Mesh::new(positions, triangles).expect("cube is valid")
}

/// Unit cube with its top face (two triangles) removed: one 4-edge hole.
pub fn cube_with_missing_face() -> Mesh {
    let c = cube(1.0);
    let tris: Vec<_> = c
        .triangles()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2 && *i != 3)
        .map(|(_, t)| *t)
        .collect();
    Mesh::new(c.positions().to_vec(), tris).expect("valid")
}

pub fn translated(mesh: &Mesh, d: [f64; 3]) -> Mesh {
    let positions = mesh
        .positions()
        .iter()
        .map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]])
        .collect();
    Mesh::new(positions, mesh.triangles().to_vec()).expect("valid")
}

/// Concatenate meshes in order, offsetting indices.
pub fn merge(meshes: &[Mesh]) -> Mesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for m in meshes {
        let base = positions.len() as u32;
        positions.extend_from_slice(m.positions());
        triangles.extend(m.triangles().iter().map(|t| t.map(|i| i + base)));
    }
    Mesh::new(positions, triangles).expect("valid")
}

/// Regular-ish tetrahedron with outward winding, 4 triangles.
pub fn tetrahedron() -> Mesh {
    Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]],
    )
    .expect("valid")
}

/// Tetrahedron with one face's winding reversed.
pub fn flipped_tetrahedron() -> Mesh {
    let t = tetrahedron();
    let mut tris = t.triangles().to_vec();
    tris[0] = [tris[0][0], tris[0][2], tris[0][1]];
    Mesh::new(t.positions().to_vec(), tris).expect("valid")
}

/// Closed UV sphere of radius 1 centred at the origin, outward winding.
/// Triangle count: `2 * segments * (rings - 1)`.
pub fn uv_sphere(rings: u32, segments: u32) -> Mesh {
    assert!(rings >= 2 && segments >= 3);
    let mut positions = vec![[0.0, 0.0, 1.0]];
    for r in 1..rings {
        let theta = std::f64::consts::PI * r as f64 / rings as f64;
        for s in 0..segments {
            let phi = 2.0 * std::f64::consts::PI * s as f64 / segments as f64;
            positions.push([
                theta.sin() * phi.cos(),
                theta.sin() * phi.sin(),
                theta.cos(),
            ]);
        }
    }
    positions.push([0.0, 0.0, -1.0]);
    let south = positions.len() as u32 - 1;
    let ring = |r: u32, s: u32| 1 + (r - 1) * segments + (s % segments);
    let mut triangles = Vec::new();
    for s in 0..segments {
        triangles.push([0, ring(1, s), ring(1, s + 1)]);
    }
    for r in 1..rings - 1 {
        for s in 0..segments {
            let (a, b, c, d) = (
                ring(r, s),
                ring(r + 1, s),
                ring(r + 1, s + 1),
                ring(r, s + 1),
            );
            triangles.push([a, b, c]);
            triangles.push([a, c, d]);
        }
    }
    for s in 0..segments {
        triangles.push([south, ring(rings - 1, s + 1), ring(rings - 1, s)]);
    }
    Mesh::new(positions, triangles).expect("valid")
}
