//! Threads, "what's on the packet" presets (Phase 1, connector `thread`
//! v1): the user picks a metric coarse size (M1…M12) and a length, for
//! example `M3 x 12`; pitch and profile come from the standard
//! ([`COARSE_PITCH`]) and are never shown unless "Advanced" is opened.
//!
//! **Simplification (documented, not hidden):** the generated geometry
//! is not a modelled helix. A printed FDM thread rarely benefits from
//! one at this scale (the nozzle can't resolve a sub-millimetre pitch
//! cleanly, and a tapped or heat-set metal thread doesn't need a
//! modelled helix in the print at all) — `bolt_hex`/`bolt_socket`'s
//! shaft and `nut`'s bore are plain cylinders at the standard's major
//! diameter, which is exactly what a tap-drill hole
//! (`threaded_hole`) or a bought bolt's clearance hole
//! (`clearance_hole`) need anyway. A real helical thread is future
//! work (tracked as an issue) once there's a concrete need for one
//! (printing directly onto plastic without a tap or an insert).
//!
//! Every hole-shaped part (`threaded_hole`, `clearance_hole`,
//! `nut_trap`) is sized from `fit_clearance` (the Fit class, design
//! intent) plus `machine_tolerance` (the Machine profile's hole
//! undersize compensation), both recorded on the connector's own
//! parameters (D20) rather than assumed.

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};
use std::sync::OnceLock;

use crate::connector::{Connector, ConnectorError, ConnectorPart, ConnectorParts, JoinKind, Side};

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/thread.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/thread.yaml is a valid spec")
    })
}

const SEGMENTS: u32 = 32;

/// Metric coarse major diameter (mm) and pitch (mm) for each size on
/// the packet (owner requirements 2026-11-11). The pitch isn't used by
/// this simplified (non-helical) geometry yet, but is kept here as the
/// one place the standard's numbers live, ready for a real helix and
/// for "Advanced" to show it.
pub const COARSE_PITCH: &[(&str, f64, f64)] = &[
    ("M1", 1.0, 0.25),
    ("M1.2", 1.2, 0.25),
    ("M1.6", 1.6, 0.35),
    ("M2", 2.0, 0.4),
    ("M2.5", 2.5, 0.45),
    ("M3", 3.0, 0.5),
    ("M4", 4.0, 0.7),
    ("M5", 5.0, 0.8),
    ("M6", 6.0, 1.0),
    ("M8", 8.0, 1.25),
    ("M10", 10.0, 1.5),
    ("M12", 12.0, 1.75),
];

/// The major diameter and pitch for a size on the packet (`"M3"`).
pub fn lookup(size: &str) -> Option<(f64, f64)> {
    COARSE_PITCH
        .iter()
        .find(|(s, _, _)| *s == size)
        .map(|(_, d, p)| (*d, *p))
}

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

/// A regular polygon of `n` sides, circumradius `r`, flat-up (a vertex
/// at angle 0).
fn polygon(n: u32, r: f64) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A round profile (approximating the standard's circumradius with a
/// chordal error well under print resolution).
fn round(r: f64) -> Vec<[f64; 2]> {
    polygon(SEGMENTS, r)
}

/// A prism of `profile` (CCW, in x,y) from `z0` to `z1`, around the z
/// axis (unlike `clasp`/`flap`'s x- or y-swept prisms: threads stand
/// upright for printing, like `builtin::revolve`).
fn prism_z(profile: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = profile.len() as u32;
    let mut positions = Vec::with_capacity(2 * profile.len() + 2);
    for &z in &[z0, z1] {
        positions.extend(profile.iter().map(|p| [p[0], p[1], z]));
    }
    let mut triangles = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (i, j);
        let (c, d) = (n + j, n + i);
        // CCW (x,y) profile swept along z: outward normal faces away
        // from the axis for [a,b,c] and [a,c,d] (same derivation as
        // `clasp::extrude_y`'s (x,z)-swept-along-y case: z plays the
        // role of the "up" axis there did).
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    // End caps: a centre-fan (the profiles here are always convex:
    // polygons and circles), bottom flipped (outward -z).
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    triangles.extend(fan_with_centre(0, n, bottom_centre, true));
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z1]);
    triangles.extend(fan_with_centre(n, n, top_centre, false));
    Mesh::new(positions, triangles).expect("a prism with a convex profile is a valid mesh")
}

fn fan_with_centre(base: u32, n: u32, centre: u32, flip: bool) -> Vec<[u32; 3]> {
    (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            if flip {
                [centre, base + j, base + i]
            } else {
                [centre, base + i, base + j]
            }
        })
        .collect()
}

/// A bolt with a hex (6-sided) or socket (round boss) head and a plain
/// shaft (the simplified, non-helical thread, major diameter `d`) of
/// `length`.
fn bolt(d: f64, length: f64, hex_head: bool) -> Mesh {
    let head_height = 0.7 * d;
    let head_profile = if hex_head {
        let across_flats = 1.7 * d;
        let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
        polygon(6, circumradius)
    } else {
        round(0.75 * d)
    };
    // One watertight solid, not a shaft and a head merged at a shared
    // z = 0 plane: the shaft's round cross-section is a strict subset
    // of the (larger) head's footprint there, so two separately-closed
    // solids sharing that plane would overlap in area, which this
    // codebase's exact validator refuses (the same lesson as
    // `flap::bored_paddle`'s knuckle-and-wing tab). `loft` connects the
    // shaft's top ring directly to the head's bottom ring.
    loft(&round(d / 2.0), -length, &head_profile, 0.0, head_height)
}

/// A solid from a profile at `z0` lofted to a different profile at
/// `z1`, both CCW and convex, closed by a fan cap at each end: the
/// transition band (where the two profiles may have different point
/// counts) is triangulated by connecting each point to its nearest
/// point on the other ring, the same coarse-but-always-simple approach
/// as [`SolidWithBore::build`]'s annulus caps.
fn loft(p0: &[[f64; 2]], z0: f64, p1: &[[f64; 2]], z1: f64, z1_top: f64) -> Mesh {
    let (n0, n1) = (p0.len() as u32, p1.len() as u32);
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let base0 = positions.len() as u32;
    positions.extend(p0.iter().map(|p| [p[0], p[1], z0]));
    let ring0_top = positions.len() as u32;
    positions.extend(p0.iter().map(|p| [p[0], p[1], z1]));
    let base1 = positions.len() as u32;
    positions.extend(p1.iter().map(|p| [p[0], p[1], z1]));
    let top1 = positions.len() as u32;
    positions.extend(p1.iter().map(|p| [p[0], p[1], z1_top]));

    // The shaft's own side wall (z0 to z1, same profile both rings).
    for i in 0..n0 {
        let j = (i + 1) % n0;
        triangles.push([base0 + i, base0 + j, ring0_top + j]);
        triangles.push([base0 + i, ring0_top + j, ring0_top + i]);
    }
    // The transition band at z1: ring0_top (p0's shape) to base1 (p1's
    // shape), each ring0_top point joined to its nearest base1 point.
    let nearest = |p: [f64; 2]| -> u32 {
        (0..n1)
            .min_by(|&a, &b| {
                let d = |i: u32| {
                    let q = p1[i as usize];
                    (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
                };
                d(a).total_cmp(&d(b))
            })
            // allow-swallow: n1 = p1.len() is a round()/polygon() profile, never empty.
            .unwrap_or(0)
    };
    for i in 0..n0 {
        let j = (i + 1) % n0;
        let (ti, tj) = (nearest(p0[i as usize]), nearest(p0[j as usize]));
        triangles.push([ring0_top + i, ring0_top + j, base1 + ti]);
        if ti != tj {
            triangles.push([ring0_top + j, base1 + tj, base1 + ti]);
        }
    }
    // The head's own side wall (z1 to z1_top, p1's shape).
    for i in 0..n1 {
        let j = (i + 1) % n1;
        triangles.push([base1 + i, base1 + j, top1 + j]);
        triangles.push([base1 + i, top1 + j, top1 + i]);
    }
    // End caps.
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    triangles.extend(fan_with_centre(base0, n0, bottom_centre, true));
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, z1_top]);
    triangles.extend(fan_with_centre(top1, n1, top_centre, false));
    Mesh::new(positions, triangles).expect("a loft between two convex profiles is a valid mesh")
}

/// A hex nut: a hex prism bored through at `bore_d`.
fn nut(d: f64, bore_d: f64) -> SolidWithBore {
    let height = 0.8 * d;
    let across_flats = 1.7 * d;
    let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
    SolidWithBore {
        outer: polygon(6, circumradius),
        bore_r: bore_d / 2.0,
        z0: 0.0,
        z1: height,
    }
}

/// An outer profile with a round bore through it, built as one
/// watertight solid (an annulus with a possibly non-round outer ring:
/// same construction idea as `flap::bored_paddle`, generalized to any
/// convex outer polygon with a round bore, both sampled so the end
/// caps connect simply).
struct SolidWithBore {
    outer: Vec<[f64; 2]>,
    bore_r: f64,
    z0: f64,
    z1: f64,
}

impl SolidWithBore {
    fn build(&self) -> Mesh {
        let n = self.outer.len() as u32;
        let inner = round(self.bore_r);
        let in_n = inner.len() as u32;
        let mut positions = Vec::new();
        let mut triangles = Vec::new();
        let outer_base = wall_z(
            &self.outer,
            self.z0,
            self.z1,
            &mut positions,
            &mut triangles,
        );
        let inner_base = positions.len() as u32;
        let inner_start = triangles.len();
        wall_z(&inner, self.z0, self.z1, &mut positions, &mut triangles);
        for t in &mut triangles[inner_start..] {
            t.swap(1, 2);
        }
        // End caps: an annulus between the outer polygon and the round
        // bore, which don't have the same point count (a hex outer has
        // 6, the bore has SEGMENTS): each bore-ring point connects to
        // its nearest outer-ring point (the reverse of the bolt's
        // `loft`, which goes from the fine ring to the coarse one;
        // here the fine ring is the inner one), fanning the gaps where
        // several inner points share the same nearest outer point.
        let nearest_outer = |p: [f64; 2]| -> u32 {
            (0..n)
                .min_by(|&a, &b| {
                    let d = |i: u32| {
                        let q = self.outer[i as usize];
                        (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
                    };
                    d(a).total_cmp(&d(b))
                })
                // allow-swallow: n = self.outer.len() is a round()/polygon() profile, never empty.
                .unwrap_or(0)
        };
        let nearest: Vec<u32> = inner.iter().map(|&p| nearest_outer(p)).collect();
        for (bottom, outer_off, inner_off) in [(true, 0u32, 0u32), (false, n, in_n)] {
            let oa = outer_base + outer_off;
            let ia = inner_base + inner_off;
            for i in 0..in_n {
                let j = (i + 1) % in_n;
                let (a, b) = (ia + i, ia + j);
                let c = oa + nearest[i as usize];
                if bottom {
                    triangles.push([a, b, c]);
                } else {
                    triangles.push([a, c, b]);
                }
                if nearest[i as usize] != nearest[j as usize] {
                    let d = oa + nearest[j as usize];
                    if bottom {
                        triangles.push([b, d, c]);
                    } else {
                        triangles.push([b, c, d]);
                    }
                }
            }
        }
        Mesh::new(positions, triangles)
            .expect("a bored convex solid with a coarse annulus cap is a valid mesh")
    }
}

/// A ring's wall, swept along z from `z0` to `z1` (profile in x,y, CCW).
fn wall_z(
    ring: &[[f64; 2]],
    z0: f64,
    z1: f64,
    positions: &mut Vec<[f64; 3]>,
    triangles: &mut Vec<[u32; 3]>,
) -> u32 {
    let n = ring.len() as u32;
    let base = positions.len() as u32;
    for &z in &[z0, z1] {
        positions.extend(ring.iter().map(|p| [p[0], p[1], z]));
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (base + i, base + j);
        let (c, d) = (base + n + j, base + n + i);
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    base
}

pub struct Thread;

impl Connector for Thread {
    fn id(&self) -> &'static str {
        "thread"
    }

    fn version(&self) -> u32 {
        1
    }

    fn spec(&self) -> &'static OperationSpec {
        spec()
    }

    fn generate(
        &self,
        params: &Params,
        _ctx: &RunContext,
    ) -> Result<ConnectorParts, ConnectorError> {
        let size = params
            .get_choice("size")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let length = params
            .get_float("length")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let shape = params
            .get_choice("shape")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let fit_clearance = params
            .get_float("fit_clearance")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let machine_tolerance = params
            .get_float("machine_tolerance")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;

        let (major_d, _pitch) = lookup(size).ok_or_else(|| {
            refuse(
                "unknown_thread_size",
                format!("'{size}' isn't a metric coarse size on the packet (M1 to M12)"),
            )
        })?;
        // The resolved gap (D20: fit class clearance plus machine
        // tolerance), diametral: a hole is widened by this much beyond
        // the bolt's own major diameter.
        let hole_clearance = fit_clearance + machine_tolerance;

        let (role, mesh, side, join): (&'static str, Mesh, Side, JoinKind) = match shape {
            "bolt_hex" => (
                "bolt",
                bolt(major_d, length, true),
                Side::Neither,
                JoinKind::Separate,
            ),
            "bolt_socket" => (
                "bolt",
                bolt(major_d, length, false),
                Side::Neither,
                JoinKind::Separate,
            ),
            "nut" => {
                let bore = major_d + hole_clearance.max(0.0);
                (
                    "nut",
                    nut(major_d, bore).build(),
                    Side::Neither,
                    JoinKind::Separate,
                )
            }
            "threaded_hole" => {
                // A tap-drill hole: a plain bore a little under the
                // major diameter, widened only by the machine's own
                // hole-undersize compensation (never by a positive fit
                // clearance — tapping cuts its own thread, it isn't a
                // sliding fit).
                let d = (major_d - 0.1 * major_d + machine_tolerance).max(0.1);
                (
                    "threaded_hole",
                    prism_z(&round(d / 2.0), -length, 1e-3),
                    Side::A,
                    JoinKind::Subtract,
                )
            }
            "clearance_hole" => {
                let d = major_d + hole_clearance.max(0.0);
                (
                    "clearance_hole",
                    prism_z(&round(d / 2.0), -length, 1e-3),
                    Side::A,
                    JoinKind::Subtract,
                )
            }
            "nut_trap" => {
                let across_flats = 1.7 * major_d + hole_clearance.max(0.0);
                let circumradius = across_flats / 2.0 / (std::f64::consts::PI / 6.0).cos();
                let trap_depth = 0.8 * major_d + hole_clearance.max(0.0);
                let bolt_hole_d = major_d + hole_clearance.max(0.0);
                // One watertight tool (see `bolt`'s own comment on
                // `loft`): the round through-hole widens into the hex
                // trap, not two solids merged at a shared plane.
                (
                    "nut_trap",
                    loft(
                        &round(bolt_hole_d / 2.0),
                        -length,
                        &polygon(6, circumradius),
                        -trap_depth,
                        1e-3,
                    ),
                    Side::A,
                    JoinKind::Subtract,
                )
            }
            other => {
                return Err(refuse(
                    "unknown_shape",
                    format!("'{other}' isn't a thread shape"),
                ));
            }
        };

        Ok(ConnectorParts {
            parts: vec![ConnectorPart {
                role,
                mesh,
                side,
                join,
            }],
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use mf_geometry::{Bvh, FaceId};

    fn assert_closed_solid(mesh: &Mesh, label: &str) {
        let ctx = RunContext::none();
        let bvh = Bvh::build(mesh, &ctx).unwrap();
        let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
            .unwrap()
            .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
    }

    #[test]
    fn every_coarse_size_has_the_packets_pitch() {
        let want = [
            ("M1", 0.25),
            ("M1.2", 0.25),
            ("M1.6", 0.35),
            ("M2", 0.4),
            ("M2.5", 0.45),
            ("M3", 0.5),
            ("M4", 0.7),
            ("M5", 0.8),
            ("M6", 1.0),
            ("M8", 1.25),
            ("M10", 1.5),
            ("M12", 1.75),
        ];
        for (size, pitch) in want {
            let (_, p) = lookup(size).unwrap_or_else(|| panic!("no entry for {size}"));
            assert_eq!(p, pitch, "{size}");
        }
    }

    #[test]
    fn bolt_and_nut_are_closed_solids() {
        let b = bolt(3.0, 12.0, true);
        assert_closed_solid(&b, "bolt_hex");
        let s = bolt(3.0, 12.0, false);
        assert_closed_solid(&s, "bolt_socket");
        let n = nut(3.0, 3.4).build();
        assert_closed_solid(&n, "nut");
    }
}
