//! Defects in plain language (docs/design/visual-language.md).
//!
//! [`classify`] sorts a revision's problems into the tree's categories (on
//! the analysis worker: it is O(n), MF-RT-001); [`defect_copy`] is the one
//! place their words come from, for the tree, Properties and tooltips.

use mf_geometry::{Diagnostics, Mesh};
use mf_io::threemf::Unit;
use mf_selection::SelectionKind;

/// Above this many edges a hole is a "large opening".
pub const LARGE_HOLE_EDGES: usize = 200;

/// One defect of the active object, as listed in the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Defect {
    Hole {
        number: usize,
        loop_id: u32,
        edges: usize,
        perimeter: Option<f64>,
    },
    OverCap {
        number: usize,
        loop_id: u32,
        edges: usize,
        perimeter: Option<f64>,
    },
    Crack {
        loop_id: u32,
        edges: usize,
    },
    LoosePiece {
        component: u32,
        triangles: usize,
    },
    Tangled {
        edge: [u32; 2],
        faces: Vec<u32>,
    },
}

/// Every defect of one mesh revision, by tree category, plus what the
/// panels show about the object. Built on the analysis worker so the UI
/// thread only reads it (MF-RT-001).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Defects {
    /// Holes, including ones too large to fill (`OverCap`).
    pub holes: Vec<Defect>,
    pub cracks: Vec<Defect>,
    /// Loose pieces: `loose[i]` is component `i + 1`.
    pub loose: Vec<Defect>,
    pub tangled: Vec<Defect>,
    /// Triangles in the model's main body (the largest component).
    pub main_body_triangles: usize,
    /// The object's size along the world axes, after its transform
    /// (document units); `None` for an empty mesh.
    pub world_size: Option<[f64; 3]>,
    /// (loop id, is a crack, index in `holes` or `cracks`), sorted by id.
    loop_index: Vec<(u32, bool, usize)>,
}

/// The words for one defect: what it is, what it measures, what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefectCopy {
    pub title: String,
    pub detail: String,
    pub next_action: String,
}

impl Defect {
    /// What selecting this defect selects, if it maps to a selection kind
    /// (a tangled edge has none: its next action selects its faces).
    pub fn selection(&self) -> Option<(SelectionKind, u32)> {
        match self {
            Defect::Hole { loop_id, .. }
            | Defect::OverCap { loop_id, .. }
            | Defect::Crack { loop_id, .. } => Some((SelectionKind::BoundaryLoop, *loop_id)),
            Defect::LoosePiece { component, .. } => Some((SelectionKind::Component, *component)),
            Defect::Tangled { .. } => None,
        }
    }
}

impl Defects {
    /// The defect a selected loop or component is, if any. O(log n).
    pub fn find(&self, kind: SelectionKind, id: u32) -> Option<&Defect> {
        match kind {
            SelectionKind::BoundaryLoop => {
                let i = self
                    .loop_index
                    .binary_search_by_key(&id, |(l, ..)| *l)
                    // allow-swallow: a loop missing from the index is not a defect (None is the answer)
                    .ok()?;
                let (_, crack, at) = self.loop_index[i];
                if crack {
                    self.cracks.get(at)
                } else {
                    self.holes.get(at)
                }
            }
            SelectionKind::Component => self.loose.get((id as usize).checked_sub(1)?),
            SelectionKind::Faces | SelectionKind::Object => None,
        }
    }
}

/// Sort `mesh`'s defects into the tree's categories. O(n log n); run it on a
/// worker. Measurements (perimeters, size) are in document units after the
/// object's `transform` (3MF row order). `fill_cap` is the largest hole the
/// fill operation can take; bigger ones are [`Defect::OverCap`]. A loop is a
/// narrow crack when its vector area is below the degenerate-area threshold
/// `inspect` and fill_holes use (1e-12 × bounding-box diagonal², in object
/// space like theirs): its edges almost touch.
pub fn classify(
    mesh: &Mesh,
    diagnostics: &Diagnostics,
    transform: &[f64; 12],
    fill_cap: usize,
) -> Defects {
    let topo = mesh.topology();
    let model = mf_render::model_matrix(transform);
    let linear = glam::DMat3::from_mat4(model);
    let diag = diagnostics.bounding_box.map_or(0.0, |b| b.diagonal());
    let min_area = 1e-12 * diag * diag;
    // Perimeter after the transform; `None` if it overflowed.
    let perimeter = |vertices: &[u32]| {
        let p = |v: u32| glam::DVec3::from(mesh.corner_position(v));
        let n = vertices.len();
        let total: f64 = (0..n)
            .map(|k| (linear * (p(vertices[(k + 1) % n]) - p(vertices[k]))).length())
            .sum();
        total.is_finite().then_some(total)
    };

    let mut out = Defects::default();
    for l in &topo.boundary_loops {
        if vector_area(mesh, &l.vertices) < min_area {
            out.loop_index.push((l.id, true, out.cracks.len()));
            out.cracks.push(Defect::Crack {
                loop_id: l.id,
                edges: l.edge_count,
            });
            continue;
        }
        let number = out.holes.len() + 1;
        let (loop_id, edges, perimeter) = (l.id, l.edge_count, perimeter(&l.vertices));
        out.loop_index.push((l.id, false, out.holes.len()));
        out.holes.push(if edges > fill_cap {
            Defect::OverCap {
                number,
                loop_id,
                edges,
                perimeter,
            }
        } else {
            Defect::Hole {
                number,
                loop_id,
                edges,
                perimeter,
            }
        });
    }
    out.loop_index.sort_unstable_by_key(|(id, ..)| *id);
    // Components are ordered largest first: the first is the model.
    out.main_body_triangles = topo.components.first().map_or(0, Vec::len);
    out.loose = topo
        .components
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, c)| Defect::LoosePiece {
            component: i as u32,
            triangles: c.len(),
        })
        .collect();
    if !topo.non_manifold_edges.is_empty() {
        let mut edges = topo.non_manifold_edges.clone();
        edges.sort_unstable();
        let mut faces: Vec<Vec<u32>> = vec![Vec::new(); edges.len()];
        for (f, t) in mesh.faces() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if let Ok(i) = edges.binary_search(&[a.min(b), a.max(b)]) {
                    faces[i].push(f.0);
                }
            }
        }
        out.tangled = edges
            .into_iter()
            .zip(faces)
            .map(|(edge, faces)| Defect::Tangled { edge, faces })
            .collect();
    }
    out.world_size = world_size(mesh, &model);
    out
}

/// Size along the world axes of every vertex placed by `model`.
fn world_size(mesh: &Mesh, model: &glam::DMat4) -> Option<[f64; 3]> {
    let mut points = mesh
        .vertices()
        .map(|(_, p)| (*model * glam::DVec3::from(p).extend(1.0)).truncate());
    let first = points.next()?;
    let (lo, hi) = points.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
    Some((hi - lo).to_array())
}

/// |½ Σ pᵢ × pᵢ₊₁| about the first vertex: ~0 for a slit.
fn vector_area(mesh: &Mesh, loop_vertices: &[u32]) -> f64 {
    let p = |v: u32| glam::DVec3::from(mesh.corner_position(v));
    let Some(&first) = loop_vertices.first() else {
        return 0.0;
    };
    let o = p(first);
    let mut sum = glam::DVec3::ZERO;
    for w in loop_vertices.windows(2) {
        sum += (p(w[0]) - o).cross(p(w[1]) - o);
    }
    (sum * 0.5).length()
}

/// The defect's title, measurements and next action, in plain language.
/// Measurements are in the document's `units`.
pub fn defect_copy(defect: &Defect, units: Unit) -> DefectCopy {
    let copy = |title: String, detail: String, next: &str| DefectCopy {
        title,
        detail,
        next_action: next.to_owned(),
    };
    match defect {
        Defect::Hole {
            number,
            edges,
            perimeter,
            ..
        } => {
            let size = if *edges > LARGE_HOLE_EDGES {
                "large"
            } else {
                "small"
            };
            copy(
                format!("Hole {number}: {size} opening"),
                hole_detail(*edges, *perimeter, units),
                "Fill hole",
            )
        }
        Defect::OverCap {
            edges, perimeter, ..
        } => copy(
            "Very large opening".into(),
            hole_detail(*edges, *perimeter, units),
            "Needs the large-hole method (not yet available)",
        ),
        Defect::Crack { .. } => copy(
            "Narrow crack".into(),
            "Edges almost touch".into(),
            "Needs edge stitching (not yet available)",
        ),
        Defect::LoosePiece { triangles, .. } => copy(
            "Loose piece".into(),
            format!(
                "{}, not connected to the model",
                count(*triangles, "triangle")
            ),
            "Remove piece",
        ),
        Defect::Tangled { .. } => copy(
            "Tangled edge".into(),
            "More than two faces share this edge".into(),
            "Select the faces involved",
        ),
    }
}

fn hole_detail(edges: usize, perimeter: Option<f64>, units: Unit) -> String {
    let edges = count(edges, "boundary edge");
    match perimeter {
        Some(p) => format!("{edges} · {} {} perimeter", measure(p), unit_symbol(units)),
        None => format!("{edges} · perimeter too large to measure"),
    }
}

/// "1 triangle", "12 triangles".
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// A length to three significant figures (whole numbers from 100 up).
pub fn measure(v: f64) -> String {
    let a = v.abs();
    let decimals = if !a.is_finite() || a == 0.0 || a >= 100.0 {
        0
    } else {
        (2 - a.log10().floor() as i32).max(0) as usize
    };
    format!("{v:.decimals$}")
}

pub fn unit_symbol(units: Unit) -> &'static str {
    match units {
        Unit::Micron => "µm",
        Unit::Millimeter => "mm",
        Unit::Centimeter => "cm",
        Unit::Inch => "in",
        Unit::Foot => "ft",
        Unit::Meter => "m",
    }
}

/// The plain-language name of running `op_id` on a target of `kind` with
/// `n` elements (`None`: the whole object). Tools in Properties and steps in
/// the history breadcrumb use the same words.
pub fn operation_label(op_id: &str, target: Option<(SelectionKind, usize)>) -> Option<String> {
    let many = |n: usize, one: &str, plural: &str| {
        if n == 1 {
            one.to_owned()
        } else {
            format!("{plural} ({n})")
        }
    };
    Some(match (op_id, target) {
        ("mesh.fill_holes", Some((SelectionKind::BoundaryLoop, n))) => {
            many(n, "Fill hole", "Fill holes")
        }
        ("mesh.fill_holes", _) => "Fill all holes".into(),
        ("mesh.remove_small_components", Some((SelectionKind::Component, n))) => {
            many(n, "Remove piece", "Remove pieces")
        }
        ("mesh.remove_small_components", _) => "Remove loose pieces".into(),
        ("mesh.inspect", _) => "Inspect".into(),
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

    /// A 6×6 unit grid with the lower-right triangle of cell (1, 1) removed:
    /// its open rim (24 edges) and a 3-edge hole of perimeter 2 + √2.
    fn holed_grid() -> Mesh {
        let n = 6;
        let mut e = mf_fixtures::grid(n).edit();
        e.remove_face(mf_geometry::FaceId(2 * (n + 1))).unwrap();
        e.finish().unwrap().0
    }

    fn hole_perimeter(d: &Defects, edges: usize) -> f64 {
        d.holes
            .iter()
            .find_map(|h| match h {
                Defect::Hole {
                    edges: e,
                    perimeter,
                    ..
                } if *e == edges => *perimeter,
                _ => None,
            })
            .unwrap()
    }

    /// Measurements are in document units *after* the object's transform
    /// (fix round 1, I3): a 3MF object scaled ×2 has holes twice as long.
    #[test]
    fn measurements_follow_the_object_transform() {
        let mesh = holed_grid();
        let diagnostics = mf_geometry::inspect(&mesh);
        let plain = classify(&mesh, &diagnostics, &IDENTITY, 1000);
        assert!((hole_perimeter(&plain, 3) - (2.0 + 2f64.sqrt())).abs() < 1e-9);

        // Scale x by 2, y by 3 (column-vector linear part, 3MF row order).
        let scaled = [2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 1.0, 5.0, 0.0, 0.0];
        let d = classify(&mesh, &diagnostics, &scaled, 1000);
        // Legs 2 and 3, hypotenuse √13.
        let expected = 2.0 + 3.0 + 13f64.sqrt();
        assert!(
            (hole_perimeter(&d, 3) - expected).abs() < 1e-9,
            "{} vs {expected}",
            hole_perimeter(&d, 3)
        );
        // The rim: 6 cells × (2 + 3) × 2 sides.
        assert!((hole_perimeter(&d, 24) - 60.0).abs() < 1e-9);
        // The object's size is measured in world axes too.
        let size = d.world_size.unwrap();
        assert!(
            (size[0] - 12.0).abs() < 1e-9 && (size[1] - 18.0).abs() < 1e-9 && size[2].abs() < 1e-9,
            "{size:?}"
        );
    }

    /// The panel's per-frame work is O(1) per row (fix round 1, I4): the
    /// main body's size is computed on the worker, and a selected loop or
    /// component finds its defect without scanning the lists.
    #[test]
    fn main_body_and_lookups_are_precomputed() {
        let mesh = mf_fixtures::merge(&[
            holed_grid(),
            mf_fixtures::translated(&mf_fixtures::cube(1.0), [20.0, 0.0, 0.0]),
        ]);
        let d = classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000);
        assert_eq!(d.main_body_triangles, 71);
        assert_eq!(d.loose.len(), 1);
        for h in d.holes.iter().chain(&d.loose) {
            let (kind, id) = h.selection().unwrap();
            assert_eq!(d.find(kind, id), Some(h));
        }
        assert_eq!(
            d.find(SelectionKind::Component, 0),
            None,
            "the main body isn't a defect"
        );
        assert_eq!(d.find(SelectionKind::BoundaryLoop, 999), None);
    }

    fn copy(d: Defect) -> (String, String, String) {
        let c = defect_copy(&d, Unit::Millimeter);
        (c.title, c.detail, c.next_action)
    }

    fn s(a: &str, b: &str, c: &str) -> (String, String, String) {
        (a.into(), b.into(), c.into())
    }

    #[test]
    fn defect_copy_matches_the_table() {
        assert_eq!(
            copy(Defect::Hole {
                number: 3,
                loop_id: 7,
                edges: 42,
                perimeter: Some(12.4),
            }),
            s(
                "Hole 3: small opening",
                "42 boundary edges · 12.4 mm perimeter",
                "Fill hole"
            )
        );
        // Above 200 edges it's a large opening.
        assert_eq!(
            copy(Defect::Hole {
                number: 1,
                loop_id: 0,
                edges: 201,
                perimeter: Some(1234.5),
            })
            .0,
            "Hole 1: large opening"
        );
        assert_eq!(
            copy(Defect::Hole {
                number: 1,
                loop_id: 0,
                edges: 200,
                perimeter: Some(1.0),
            })
            .0,
            "Hole 1: small opening"
        );
        assert_eq!(
            copy(Defect::Crack {
                loop_id: 2,
                edges: 8
            }),
            s(
                "Narrow crack",
                "Edges almost touch",
                "Needs edge stitching (not yet available)"
            )
        );
        assert_eq!(
            copy(Defect::LoosePiece {
                component: 4,
                triangles: 12
            }),
            s(
                "Loose piece",
                "12 triangles, not connected to the model",
                "Remove piece"
            )
        );
        assert_eq!(
            copy(Defect::Tangled {
                edge: [1, 2],
                faces: vec![0, 1, 2]
            }),
            s(
                "Tangled edge",
                "More than two faces share this edge",
                "Select the faces involved"
            )
        );
        assert_eq!(
            copy(Defect::OverCap {
                number: 2,
                loop_id: 5,
                edges: 1500,
                perimeter: Some(88.0),
            }),
            s(
                "Very large opening",
                "1500 boundary edges · 88.0 mm perimeter",
                "Needs the large-hole method (not yet available)"
            )
        );
        // Measurements use the document's units.
        assert_eq!(
            defect_copy(
                &Defect::Hole {
                    number: 1,
                    loop_id: 0,
                    edges: 3,
                    perimeter: Some(0.5),
                },
                Unit::Inch
            )
            .detail,
            "3 boundary edges · 0.500 in perimeter"
        );
    }
}
