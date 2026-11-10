//! Built-in parts, generated in code (D16): no asset licences needed. Each
//! is deterministic (a pure function of its id and parameters), sized in
//! millimetres, attached at its base with +z out of the target surface.

use mf_geometry::Mesh;

use crate::manifest::{Attach, PartKind, PartManifest};

/// Segments around every round built-in.
const SEGMENTS: u32 = 48;

/// A surface of revolution about z: `profile` is `(radius, z)` from bottom
/// to top. A point with radius 0 is a pole (one vertex); the profile is
/// closed at an end only if it ends at a pole.
fn revolve(profile: &[(f64, f64)], segments: u32) -> Mesh {
    let mut positions = Vec::new();
    // Per profile point: its first vertex, and whether it's a pole.
    let mut rows: Vec<(u32, bool)> = Vec::new();
    for &(r, z) in profile {
        let start = positions.len() as u32;
        if r == 0.0 {
            positions.push([0.0, 0.0, z]);
            rows.push((start, true));
        } else {
            for s in 0..segments {
                let a = std::f64::consts::TAU * s as f64 / segments as f64;
                positions.push([r * a.cos(), r * a.sin(), z]);
            }
            rows.push((start, false));
        }
    }
    let mut triangles = Vec::new();
    for k in 0..rows.len() - 1 {
        let ((a, pa), (b, pb)) = (rows[k], rows[k + 1]);
        for i in 0..segments {
            let j = (i + 1) % segments;
            match (pa, pb) {
                (false, false) => {
                    triangles.push([a + i, a + j, b + j]);
                    triangles.push([a + i, b + j, b + i]);
                }
                (true, false) => triangles.push([a, b + j, b + i]),
                (false, true) => triangles.push([a + i, a + j, b]),
                (true, true) => {}
            }
        }
    }
    Mesh::new(positions, triangles).expect("a revolved profile is a valid mesh")
}

fn arc(r: f64, z0: f64, from: f64, to: f64, steps: u32) -> Vec<(f64, f64)> {
    (0..=steps)
        .map(|i| {
            let t = from + (to - from) * i as f64 / steps as f64;
            (r * t.cos(), z0 + r * t.sin())
        })
        .map(|(x, z)| (if x.abs() < 1e-12 { 0.0 } else { x }, z))
        .collect()
}

fn sphere(r: f64) -> Mesh {
    use std::f64::consts::FRAC_PI_2;
    revolve(&arc(r, r, -FRAC_PI_2, FRAC_PI_2, 24), SEGMENTS)
}

fn capsule(r: f64, h: f64) -> Mesh {
    use std::f64::consts::FRAC_PI_2;
    let mut p = arc(r, r, -FRAC_PI_2, 0.0, 12);
    p.extend(arc(r, r + h, 0.0, FRAC_PI_2, 12));
    revolve(&p, SEGMENTS)
}

fn torus(major: f64, minor: f64) -> Mesh {
    let (nu, nv) = (SEGMENTS, 24u32);
    let mut positions = Vec::new();
    for i in 0..nu {
        let u = std::f64::consts::TAU * i as f64 / nu as f64;
        for j in 0..nv {
            let v = std::f64::consts::TAU * j as f64 / nv as f64;
            let r = major + minor * v.cos();
            positions.push([r * u.cos(), r * u.sin(), minor + minor * v.sin()]);
        }
    }
    let at = |i: u32, j: u32| (i % nu) * nv + (j % nv);
    let mut triangles = Vec::new();
    for i in 0..nu {
        for j in 0..nv {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            triangles.push([a, b, c]);
            triangles.push([a, c, d]);
        }
    }
    Mesh::new(positions, triangles).expect("a torus is a valid mesh")
}

/// A box `size` wide, its corners rounded with radius `round` (0: sharp),
/// sitting on z = 0, from a cube subdivided `n` times per edge.
fn rounded_box(size: [f64; 3], round: f64, n: u32) -> Mesh {
    use std::collections::HashMap;
    let mut index: HashMap<[i64; 3], u32> = HashMap::new();
    let mut grid: Vec<[i64; 3]> = Vec::new();
    let mut triangles = Vec::new();
    let n = n as i64;
    let mut vertex = |g: [i64; 3]| -> u32 {
        *index.entry(g).or_insert_with(|| {
            grid.push(g);
            (grid.len() - 1) as u32
        })
    };
    // Each face of the [-n, n]³ cube: axis, sign, and the two in-face axes
    // ordered so (u × v) points outward.
    for (axis, sign, u, v) in [
        (0, 1, 1, 2),
        (0, -1, 2, 1),
        (1, 1, 2, 0),
        (1, -1, 0, 2),
        (2, 1, 0, 1),
        (2, -1, 1, 0),
    ] {
        for i in 0..n {
            for j in 0..n {
                let p = |a: i64, b: i64| {
                    let mut g = [0i64; 3];
                    g[axis] = sign * n;
                    g[u] = -n + 2 * a;
                    g[v] = -n + 2 * b;
                    g
                };
                let (a, b, c, d) = (
                    vertex(p(i, j)),
                    vertex(p(i + 1, j)),
                    vertex(p(i + 1, j + 1)),
                    vertex(p(i, j + 1)),
                );
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            }
        }
    }
    let half = size.map(|s| s / 2.0);
    let inner = half.map(|h| (h - round).max(0.0));
    let positions = grid
        .iter()
        .map(|g| {
            let p: [f64; 3] = std::array::from_fn(|k| g[k] as f64 / n as f64 * half[k]);
            let q: [f64; 3] = std::array::from_fn(|k| p[k].clamp(-inner[k], inner[k]));
            let d: [f64; 3] = std::array::from_fn(|k| p[k] - q[k]);
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let out: [f64; 3] = if round > 0.0 && l > 0.0 {
                std::array::from_fn(|k| q[k] + d[k] / l * round)
            } else {
                p
            };
            [out[0], out[1], out[2] + half[2]]
        })
        .collect();
    Mesh::new(positions, triangles).expect("a subdivided box is a valid mesh")
}

/// One built-in part: its manifest and how it's made.
pub struct Builtin {
    pub manifest: PartManifest,
    /// Its generator parameters, part of its hash.
    pub params: String,
    make: fn() -> Mesh,
}

impl Builtin {
    pub fn mesh(&self) -> Mesh {
        (self.make)()
    }
}

fn manifest(id: &str, name: &str, kind: PartKind) -> PartManifest {
    PartManifest {
        id: id.into(),
        name: name.into(),
        version: 1,
        licence: "MIT OR Apache-2.0".into(),
        source: "MeshFixer built-in".into(),
        kind,
        attach: Attach {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        },
        default_scale: 1.0,
        mesh: String::new(),
    }
}

/// Every built-in, in a fixed order. Each sits on z = 0 with its attach
/// point at the origin and grows along +z, its attach normal: placed, +z
/// lines up with the target surface's outward normal ([`crate::place`]).
pub fn builtins() -> Vec<Builtin> {
    use std::f64::consts::FRAC_PI_2;
    vec![
        Builtin {
            manifest: manifest("sphere", "Sphere", PartKind::Solid),
            params: "r=5".into(),
            make: || sphere(5.0),
        },
        Builtin {
            manifest: manifest("cylinder", "Cylinder", PartKind::Solid),
            params: "r=5,h=10".into(),
            make: || {
                revolve(
                    &[(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (0.0, 10.0)],
                    SEGMENTS,
                )
            },
        },
        Builtin {
            manifest: manifest("cone", "Cone", PartKind::Solid),
            params: "r=5,h=10".into(),
            make: || revolve(&[(0.0, 0.0), (5.0, 0.0), (0.0, 10.0)], SEGMENTS),
        },
        Builtin {
            manifest: manifest("box", "Box", PartKind::Solid),
            params: "size=10,n=4".into(),
            make: || rounded_box([10.0; 3], 0.0, 4),
        },
        Builtin {
            manifest: manifest("rounded-box", "Rounded box", PartKind::Solid),
            params: "size=10,round=2,n=12".into(),
            make: || rounded_box([10.0; 3], 2.0, 12),
        },
        Builtin {
            manifest: manifest("torus", "Torus", PartKind::Solid),
            params: "R=6,r=2".into(),
            make: || torus(6.0, 2.0),
        },
        Builtin {
            manifest: manifest("capsule", "Capsule", PartKind::Solid),
            params: "r=4,h=8".into(),
            make: || capsule(4.0, 8.0),
        },
        Builtin {
            manifest: manifest("hemisphere-stamp", "Hemisphere stamp", PartKind::Open),
            params: "r=5".into(),
            make: || revolve(&arc(5.0, 0.0, 0.0, FRAC_PI_2, 12), SEGMENTS),
        },
    ]
}
