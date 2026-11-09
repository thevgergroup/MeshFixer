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
    let (positions, triangles) = cube(1.0).to_arrays();
    let tris: Vec<_> = triangles
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2 && *i != 3)
        .map(|(_, t)| *t)
        .collect();
    Mesh::new(positions, tris).expect("valid")
}

pub fn translated(mesh: &Mesh, d: [f64; 3]) -> Mesh {
    let (positions, triangles) = mesh.to_arrays();
    let positions = positions
        .iter()
        .map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]])
        .collect();
    Mesh::new(positions, triangles).expect("valid")
}

/// Concatenate meshes in order, offsetting indices.
pub fn merge(meshes: &[Mesh]) -> Mesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for m in meshes {
        let base = positions.len() as u32;
        let (p, t) = m.to_arrays();
        positions.extend_from_slice(&p);
        triangles.extend(t.iter().map(|t| t.map(|i| i + base)));
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
    let (positions, mut tris) = t.to_arrays();
    tris[0] = [tris[0][0], tris[0][2], tris[0][1]];
    Mesh::new(positions, tris).expect("valid")
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

/// Controlled damage on a UV sphere, representative of scan output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageSpec {
    pub rings: u32,
    pub segments: u32,
    /// Holes punched by deleting faces within a small cap.
    pub holes: u32,
    /// Tiny tetrahedron islands floating just off the surface.
    pub islands: u32,
    /// Face corners split onto exact-duplicate vertices (unwelded seams).
    pub unwelded: u32,
    pub seed: u64,
}

impl DamageSpec {
    /// Roughly `triangles` triangles, with damage scaled to match.
    pub fn for_triangles(triangles: usize, seed: u64) -> DamageSpec {
        // tris = 2·seg·(rings-1), with seg = 2·rings.
        let rings = ((triangles as f64 / 4.0).sqrt().round() as u32).max(8);
        DamageSpec {
            rings,
            segments: rings * 2,
            holes: 12,
            islands: 50,
            unwelded: 200,
            seed,
        }
    }
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn direction(&mut self) -> [f64; 3] {
        let z = self.unit() * 2.0 - 1.0;
        let phi = self.unit() * std::f64::consts::TAU;
        let r = (1.0 - z * z).sqrt();
        [r * phi.cos(), r * phi.sin(), z]
    }
}

/// Deterministic damaged sphere: holes, islands and unwelded corners.
pub fn damaged_sphere(spec: &DamageSpec) -> Mesh {
    let sphere = uv_sphere(spec.rings, spec.segments);
    let mut rng = SplitMix(spec.seed);
    let (mut positions, sphere_triangles) = sphere.to_arrays();
    let centroid = |t: &[u32; 3]| {
        let p = t.map(|i| positions[i as usize]);
        let c = [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0);
        let n = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt().max(1e-12);
        c.map(|x| x / n)
    };
    let holes: Vec<[f64; 3]> = (0..spec.holes).map(|_| rng.direction()).collect();
    let cos_radius = 0.12f64.cos();
    let mut triangles: Vec<[u32; 3]> = sphere_triangles
        .iter()
        .filter(|t| {
            let c = centroid(t);
            !holes
                .iter()
                .any(|h| h[0] * c[0] + h[1] * c[1] + h[2] * c[2] > cos_radius)
        })
        .copied()
        .collect();

    for _ in 0..spec.unwelded {
        if triangles.is_empty() {
            break;
        }
        let f = rng.below(triangles.len());
        let v = triangles[f][0];
        positions.push(positions[v as usize]);
        triangles[f][0] = positions.len() as u32 - 1;
    }

    let mut parts = vec![Mesh::new(positions, triangles).expect("valid")];
    let (tet_positions, tet_triangles) = tetrahedron().to_arrays();
    for _ in 0..spec.islands {
        let d = rng.direction();
        let scaled: Vec<[f64; 3]> = tet_positions
            .iter()
            .map(|p| [0, 1, 2].map(|k| p[k] * 0.01 + d[k] * 1.05))
            .collect();
        parts.push(Mesh::new(scaled, tet_triangles.clone()).expect("valid"));
    }
    merge(&parts)
}

/// Policy for tests that need a GPU adapter.
///
/// A skipped GPU test must never look like a pass. With `MF_REQUIRE_GPU=1`
/// a missing adapter fails the test. Otherwise the skip is printed and
/// appended to `MF_SKIP_LOG`, which CI reports separately from passes.
pub mod skip {
    use std::io::Write;
    use std::path::Path;

    /// Call when a GPU test can't run. Reads the policy from the environment.
    pub fn gpu_unavailable(test: &str) {
        let require = std::env::var("MF_REQUIRE_GPU").is_ok_and(|v| v == "1");
        let log = std::env::var_os("MF_SKIP_LOG");
        record_gpu_skip(test, require, log.as_deref().map(Path::new));
    }

    pub fn record_gpu_skip(test: &str, require: bool, log: Option<&Path>) {
        assert!(
            !require,
            "GPU required (MF_REQUIRE_GPU=1) but no adapter is available for {test}"
        );
        record(test, "no GPU adapter available", log);
    }

    /// Call when a fetched fixture is missing. `MF_REQUIRE_FIXTURES=1`
    /// makes that a failure.
    pub fn fixture_missing(test: &str, fixture: &Path) {
        let require = std::env::var("MF_REQUIRE_FIXTURES").is_ok_and(|v| v == "1");
        let log = std::env::var_os("MF_SKIP_LOG");
        record_fixture_skip(test, fixture, require, log.as_deref().map(Path::new));
    }

    pub fn record_fixture_skip(test: &str, fixture: &Path, require: bool, log: Option<&Path>) {
        assert!(
            !require,
            "fixture required (MF_REQUIRE_FIXTURES=1) but {} is missing for {test}; run scripts/fetch-fixtures.sh",
            fixture.display()
        );
        let reason = format!(
            "fixture {} not fetched (scripts/fetch-fixtures.sh)",
            fixture.display()
        );
        record(test, &reason, log);
    }

    fn record(test: &str, reason: &str, log: Option<&Path>) {
        eprintln!("SKIPPED: {test}: {reason}");
        if let Some(log) = log {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                // One write per line: parallel tests append to the same file, and
                // writeln! may split the name and the newline into two writes.
                .and_then(|mut f| f.write_all(format!("{test}\n").as_bytes()));
            if let Err(e) = written {
                panic!("could not record skip of {test} in {}: {e}", log.display());
            }
        }
    }
}

/// Real scans fetched by `scripts/fetch-fixtures.sh` (hash-verified, listed
/// in `fixtures/manifest.txt`, sources and licences in `fixtures/SOURCES.md`).
pub mod real {
    use std::path::PathBuf;

    /// Where fetched fixtures live (git-ignored).
    pub fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real")
    }

    /// Path of fixture `name` (its file name in `fixtures/real`), or `None`
    /// after recording a skip for `test` if it hasn't been fetched.
    pub fn path(name: &str, test: &str) -> Option<PathBuf> {
        let path = dir().join(name);
        if path.is_file() {
            Some(path)
        } else {
            super::skip::fixture_missing(test, &path);
            None
        }
    }
}

/// A flat (n+1)×(n+1) vertex grid in z=0, two CCW triangles per cell.
/// Cell (i, j) has faces `2*(j*n+i)` (lower-right) and `2*(j*n+i)+1`
/// (upper-left); vertex (i, j) is `j*(n+1)+i`.
pub fn grid(n: u32) -> Mesh {
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
    Mesh::new(positions, tris).expect("valid")
}
