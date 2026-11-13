//! Deterministic seeded scenes for the use-case acceptance scenarios
//! (Phase 1 Task 24a, D24): `octopus`, `head` and `hollow_box`. Each is a
//! closed, manifold solid built by radially displacing a UV sphere, so
//! manifoldness is free (the surface stays a topological sphere) and the
//! shapes are a pure, seeded function of their inputs.

use mf_geometry::Mesh;

use crate::uv_sphere;

/// A small deterministic pseudo-random generator (splitmix64), so bumps
/// are reproducible without pulling in a `rand` dependency.
struct Seeded(u64);

impl Seeded {
    fn new(seed: u64) -> Seeded {
        Seeded(seed)
    }

    /// Next value in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }
}

/// A smooth radial bump of `strength` centred at direction `centre`
/// (unit vector) with angular half-width `width` (radians), evaluated at
/// unit direction `dir`: `strength * smoothstep` of the angular distance.
fn bump(dir: [f64; 3], centre: [f64; 3], width: f64, strength: f64) -> f64 {
    let dot = (dir[0] * centre[0] + dir[1] * centre[1] + dir[2] * centre[2]).clamp(-1.0, 1.0);
    let angle = dot.acos();
    if angle >= width {
        return 0.0;
    }
    let t = 1.0 - angle / width;
    let s = t * t * (3.0 - 2.0 * t); // smoothstep
    strength * s
}

/// A UV sphere with every vertex's radius replaced by `radius(direction)`,
/// `direction` being the unit vector from the origin to the original
/// point (so `radius` is a function of latitude/longitude only; the base
/// sphere's genus-0 topology, and so its manifoldness, is unchanged).
fn radial_mesh(rings: u32, segments: u32, radius: impl Fn([f64; 3]) -> f64) -> Mesh {
    let base = uv_sphere(rings, segments);
    let (positions, triangles) = base.to_arrays();
    let positions = positions
        .iter()
        .map(|&d| {
            let r = radius(d);
            [d[0] * r, d[1] * r, d[2] * r]
        })
        .collect();
    Mesh::new(positions, triangles).expect("a radially displaced sphere stays a valid solid")
}

/// Unit vector for a point at height `z` in `[-1, 1]` (pole to pole) and
/// azimuth `phi` (radians).
fn dir(z: f64, phi: f64) -> [f64; 3] {
    let z = z.clamp(-1.0, 1.0);
    let s = (1.0 - z * z).max(0.0).sqrt();
    [s * phi.cos(), s * phi.sin(), z]
}

/// The octopus "Rocktopus" fixture (UC2, D24): a body (top, +z), a
/// narrower neck below it, and eight tentacles splayed out and down from
/// the bottom. Seeded (`seed`), closed and manifold. Centred near the
/// origin with the neck roughly at z = 0, sized a few tens of mm, so
/// `mesh.split_plane` through z = 0 with `plane_normal = [0, 0, 1]` cuts
/// through the neck and keeps the body "above".
pub fn octopus(seed: u64) -> Mesh {
    let mut rng = Seeded::new(seed);
    // Per-tentacle azimuth and a little seeded jitter in length/droop, so
    // the shape is reproducible but not perfectly symmetric.
    let tentacles: Vec<(f64, f64, f64)> = (0..8)
        .map(|i| {
            let phi = std::f64::consts::TAU * i as f64 / 8.0 + rng.range(-0.05, 0.05);
            let length = rng.range(0.9, 1.15);
            let spread = rng.range(0.85, 1.0);
            (phi, length, spread)
        })
        .collect();
    radial_mesh(40, 64, move |d| {
        let z = d[2]; // -1 (south pole) to +1 (north pole)
        // A radial profile purely as a function of height, built as
        // control points joined by smoothstep interpolation (so it is
        // continuous and has an actual local minimum at the neck,
        // instead of a max of independent bumps, which only ever grows).
        // (z, radius) from the bottom (tentacle splay) to the top (body
        // crown): the neck (z = 0) is the narrowest point, well below
        // both the body above it and the splay below it.
        let points: [(f64, f64); 5] = [
            (-1.0, 4.0),   // south pole
            (-0.55, 17.0), // tentacle splay, wide
            (0.0, 4.5),    // neck: narrower than either neighbour
            (0.65, 21.0),  // body, wide
            (1.0, 4.0),    // north pole
        ];
        let smoothstep = |t: f64| {
            let t = t.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let mut profile = points[0].1;
        for w in points.windows(2) {
            let ((z0, r0), (z1, r1)) = (w[0], w[1]);
            if z >= z0 && z <= z1 {
                let t = smoothstep((z - z0) / (z1 - z0));
                profile = r0 + (r1 - r0) * t;
            }
        }
        // Tentacles: eight lobes reaching toward -z and outward, added on
        // top of the profile only in the lower hemisphere, each centred
        // on its own azimuth so the tentacles separate around the ring.
        let mut tentacle = 0.0f64;
        if z < -0.1 {
            let phi = d[1].atan2(d[0]);
            let gaussian = |centre: f64, width: f64| {
                let t = (z - centre) / width;
                (-0.5 * t * t).exp()
            };
            for &(centre_phi, length, spread) in &tentacles {
                let mut da = (phi - centre_phi).rem_euclid(std::f64::consts::TAU);
                if da > std::f64::consts::PI {
                    da -= std::f64::consts::TAU;
                }
                let angular = (-0.5 * (da / (0.22 * spread)).powi(2)).exp();
                let reach = gaussian(-0.75, 0.35) * length;
                tentacle = tentacle.max(10.0 * angular * reach);
            }
        }
        (profile + tentacle).max(4.0)
    })
}

/// The replacement head (UC2, D24): an ellipsoid (taller along z) with a
/// brow ridge and two jaw bumps, closed and manifold. Its base (the −z
/// pole region) is the attach side that overlaps the octopus's neck.
pub fn head(seed: u64) -> Mesh {
    let mut rng = Seeded::new(seed);
    let brow_strength = rng.range(2.0, 3.0);
    let jaw_offset = rng.range(0.35, 0.45);
    radial_mesh(32, 48, move |d| {
        // Ellipsoid: wider in x/y than tall, so it reads as a head.
        let ellipsoid_r = {
            let (rx, ry, rz) = (9.0, 8.0, 11.0);
            let n = ((d[0] / rx).powi(2) + (d[1] / ry).powi(2) + (d[2] / rz).powi(2)).sqrt();
            if n <= 1e-9 { rx } else { 1.0 / n }
        };
        let brow = brow_strength * bump(d, dir(0.4, std::f64::consts::FRAC_PI_2), 0.4, 1.0);
        let jaw_l = 1.5 * bump(d, dir(-jaw_offset, 0.5), 0.3, 1.0);
        let jaw_r = 1.5 * bump(d, dir(-jaw_offset, -0.5), 0.3, 1.0);
        ellipsoid_r + brow + jaw_l + jaw_r
    })
}

/// A hollow box with wall thickness `wall` (mm), outer size 40×30×20 mm,
/// open at the bottom (UC3): a 6-sided shell. Built directly (not a
/// boolean) so this crate stays free of `mf-boolean`.
pub fn hollow_box(wall: f64) -> Mesh {
    let (ox, oy, oz): (f64, f64, f64) = (40.0, 30.0, 20.0);
    let w = wall.max(0.1).min(ox.min(oy).min(oz) / 2.0 - 0.1);
    let (ix0, iy0) = (w, w);
    let (ix1, iy1) = (ox - w, oy - w);
    let iz1 = oz - w; // inner ceiling; open at z = 0 (no inner floor)

    // Outer box, no bottom (z = 0) face: sides and top only.
    let outer = [
        [0.0, 0.0, 0.0],
        [ox, 0.0, 0.0],
        [ox, oy, 0.0],
        [0.0, oy, 0.0],
        [0.0, 0.0, oz],
        [ox, 0.0, oz],
        [ox, oy, oz],
        [0.0, oy, oz],
    ];
    // Inner cavity, no bottom either (open through to the outside at z =
    // 0), ceiling at iz1.
    let inner = [
        [ix0, iy0, 0.0],
        [ix1, iy0, 0.0],
        [ix1, iy1, 0.0],
        [ix0, iy1, 0.0],
        [ix0, iy0, iz1],
        [ix1, iy0, iz1],
        [ix1, iy1, iz1],
        [ix0, iy1, iz1],
    ];
    let mut positions: Vec<[f64; 3]> = Vec::new();
    positions.extend_from_slice(&outer);
    let inner_base = positions.len() as u32;
    positions.extend_from_slice(&inner);

    let mut tris: Vec<[u32; 3]> = Vec::new();
    // Outer walls (4 sides) + top, outward winding.
    let outer_side = |a: u32, b: u32, c: u32, d: u32, tris: &mut Vec<[u32; 3]>| {
        tris.push([a, b, c]);
        tris.push([a, c, d]);
    };
    outer_side(0, 1, 5, 4, &mut tris); // -y
    outer_side(1, 2, 6, 5, &mut tris); // +x
    outer_side(2, 3, 7, 6, &mut tris); // +y
    outer_side(3, 0, 4, 7, &mut tris); // -x
    tris.push([4, 5, 6]);
    tris.push([4, 6, 7]); // top (+z)

    // Inner walls (facing inward, so winding reversed relative to the
    // outer shell) + inner ceiling (facing down, into the cavity).
    let inner_side = |a: u32, b: u32, c: u32, d: u32, tris: &mut Vec<[u32; 3]>| {
        tris.push([inner_base + a, inner_base + c, inner_base + b]);
        tris.push([inner_base + a, inner_base + d, inner_base + c]);
    };
    inner_side(0, 1, 5, 4, &mut tris);
    inner_side(1, 2, 6, 5, &mut tris);
    inner_side(2, 3, 7, 6, &mut tris);
    inner_side(3, 0, 4, 7, &mut tris);
    tris.push([inner_base + 4, inner_base + 6, inner_base + 5]);
    tris.push([inner_base + 4, inner_base + 7, inner_base + 6]);

    // The bottom rim: a ring of quads joining the outer and inner bottom
    // loops (z = 0), closing the shell's open underside into one
    // boundary-free (watertight) wall of thickness `w`. The rim faces
    // downward (-z, out of the shell's solid material), so each quad
    // must traverse the outer edge in the opposite direction from the
    // outer wall face that shares it (oa -> ob there, so ob -> oa here).
    let rim = |oa: u32, ob: u32, ia: u32, ib: u32, tris: &mut Vec<[u32; 3]>| {
        tris.push([ob, oa, inner_base + ia]);
        tris.push([ob, inner_base + ia, inner_base + ib]);
    };
    rim(0, 1, 0, 1, &mut tris);
    rim(1, 2, 1, 2, &mut tris);
    rim(2, 3, 2, 3, &mut tris);
    rim(3, 0, 3, 0, &mut tris);

    Mesh::new(positions, tris).expect("hollow box shell is a valid solid")
}
