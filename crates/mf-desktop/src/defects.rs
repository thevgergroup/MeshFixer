//! Defects in plain language (docs/design/visual-language.md).
//!
//! [`classify`] sorts a revision's problems into the tree's categories (on
//! the analysis worker: it is O(n), MF-RT-001); [`defect_copy`] is the one
//! place their words come from, for the tree, Properties and tooltips.

use glam::{DMat3, DMat4, DVec3};
use mf_geometry::{Aabb, Diagnostics, Mesh};
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
        /// Its largest dimension along the world axes, after the object's
        /// transform (document units); `None` if it overflowed.
        size: Option<f64>,
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
    /// Where each defect is, for framing it: `places.holes[i]` is
    /// `holes[i]`'s, and so on.
    places: Places,
}

/// Where each defect of [`Defects`] is, list by list.
#[derive(Debug, Clone, Default, PartialEq)]
struct Places {
    holes: Vec<Option<Place>>,
    cracks: Vec<Option<Place>>,
    loose: Vec<Option<Place>>,
    tangled: Vec<Option<Place>>,
}

/// Where a defect is, in world space (after the object's transform):
/// what framing it needs, computed with the analysis (MF-RT-001).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub bounds: Aabb,
    /// The way its faces face (unit), if they mostly face one way: a hole's
    /// surrounding faces, a piece that isn't closed. `None` for a closed
    /// piece, whose faces face every way.
    pub facing: Option<DVec3>,
}

/// The words for one defect: what it is, what it measures, what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefectCopy {
    pub title: String,
    /// The tree row: a short name ("Hole 3", "Loose piece")…
    pub name: String,
    /// …and its size ("42 edges · 12.4 mm", "12 triangles · 0.400 mm").
    pub size: String,
    /// Both, as the row's accessible label: "Hole 3 · 42 edges · 12.4 mm".
    pub row: String,
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

    /// Where `defect` (one of these) is. O(log n).
    pub fn place(&self, defect: &Defect) -> Option<Place> {
        let p = &self.places;
        match defect {
            Defect::Hole { loop_id, .. }
            | Defect::OverCap { loop_id, .. }
            | Defect::Crack { loop_id, .. } => {
                let i = self
                    .loop_index
                    .binary_search_by_key(loop_id, |(l, ..)| *l)
                    // allow-swallow: a loop missing from the index isn't one of these defects (None is the answer)
                    .ok()?;
                let (_, crack, at) = self.loop_index[i];
                if crack { &p.cracks } else { &p.holes }.get(at).copied()?
            }
            Defect::LoosePiece { component, .. } => p
                .loose
                .get((*component as usize).checked_sub(1)?)
                .copied()?,
            Defect::Tangled { edge, .. } => {
                // Listed in edge order.
                let i = self
                    .tangled
                    .binary_search_by(|d| match d {
                        Defect::Tangled { edge: e, .. } => e.cmp(edge),
                        _ => std::cmp::Ordering::Less,
                    })
                    // allow-swallow: an edge that isn't listed isn't one of these defects (None is the answer)
                    .ok()?;
                p.tangled.get(i).copied()?
            }
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
    let linear = DMat3::from_mat4(model);
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

    let world = |v: u32| (model * DVec3::from(mesh.corner_position(v)).extend(1.0)).truncate();
    let mut out = Defects::default();
    // Each loop's place, by its index in `topo.boundary_loops`; facing
    // filled in by the face pass below.
    let mut loop_places: Vec<Option<Place>> = topo
        .boundary_loops
        .iter()
        .map(|l| {
            bounds_of(l.vertices.iter().map(|&v| world(v))).map(|bounds| Place {
                bounds,
                facing: None,
            })
        })
        .collect();
    // A mirroring transform turns faces inside out in world space: undo
    // that, so outward stays outward.
    let flip = if linear.determinant() < 0.0 {
        -1.0
    } else {
        1.0
    };
    let cofactor = cofactor(&linear) * flip;
    loop_facing(
        mesh,
        &topo.boundary_loops,
        &world,
        &cofactor,
        flip,
        &mut loop_places,
    );
    for (l, place) in topo.boundary_loops.iter().zip(loop_places) {
        if vector_area(mesh, &l.vertices) < min_area {
            out.places.cracks.push(place);
            out.loop_index.push((l.id, true, out.cracks.len()));
            out.cracks.push(Defect::Crack {
                loop_id: l.id,
                edges: l.edge_count,
            });
            continue;
        }
        out.places.holes.push(place);
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
    for (i, c) in topo.components.iter().enumerate().skip(1) {
        let place = faces_place(mesh, c, &world, &cofactor);
        out.places.loose.push(place);
        out.loose.push(Defect::LoosePiece {
            component: i as u32,
            triangles: c.len(),
            size: place.and_then(|p| {
                let d = (0..3)
                    .map(|k| p.bounds.max[k] - p.bounds.min[k])
                    .fold(0.0, f64::max);
                d.is_finite().then_some(d)
            }),
        });
    }
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
        out.places.tangled = edges
            .iter()
            .zip(&faces)
            .map(|(edge, faces)| {
                let facing = faces_place(mesh, faces, &world, &cofactor).and_then(|p| p.facing);
                bounds_of(edge.iter().map(|&v| world(v))).map(|bounds| Place { bounds, facing })
            })
            .collect();
        out.tangled = edges
            .into_iter()
            .zip(faces)
            .map(|(edge, faces)| Defect::Tangled { edge, faces })
            .collect();
    }
    out.world_size = world_size(mesh, &model);
    out
}

/// The bounding box of `points`; `None` if there are none or one isn't
/// finite.
fn bounds_of(points: impl Iterator<Item = DVec3>) -> Option<Aabb> {
    let mut out: Option<(DVec3, DVec3)> = None;
    for p in points {
        if !p.is_finite() {
            return None;
        }
        out = Some(out.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
    }
    out.map(|(lo, hi)| Aabb {
        min: lo.to_array(),
        max: hi.to_array(),
    })
}

/// The matrix taking an object-space area vector (a face's cross product)
/// to world space under `linear`: its cofactor matrix, det · L⁻ᵀ, which
/// stays exact for non-uniform scales (and is defined even when `linear`
/// is singular). Under a mirror it points inward: the caller flips it.
fn cofactor(linear: &DMat3) -> DMat3 {
    let [a, b, c] = [linear.x_axis, linear.y_axis, linear.z_axis];
    DMat3::from_cols(b.cross(c), c.cross(a), a.cross(b))
}

/// Accumulates world-space area vectors: the way they mostly face, if they
/// do (faces that face every way, like a closed piece's, cancel out).
#[derive(Debug, Clone, Copy, Default)]
struct Facing {
    sum: DVec3,
    area: f64,
}

impl Facing {
    fn add(&mut self, n: DVec3) {
        self.sum += n;
        self.area += n.length();
    }

    fn direction(&self) -> Option<DVec3> {
        (self.area > 0.0 && self.sum.length() > 0.2 * self.area && self.sum.is_finite())
            .then(|| self.sum.normalize())
    }
}

/// A face's area vector (object space).
fn area_vector(mesh: &Mesh, t: [u32; 3]) -> DVec3 {
    let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
    (b - a).cross(c - a)
}

/// The place of the live faces among `faces`: their corners' bounds and
/// the way they face. O(faces).
fn faces_place(
    mesh: &Mesh,
    faces: &[u32],
    world: &impl Fn(u32) -> DVec3,
    cofactor: &DMat3,
) -> Option<Place> {
    let mut facing = Facing::default();
    let live: Vec<[u32; 3]> = faces
        .iter()
        .filter_map(|&f| mesh.face(mf_geometry::FaceId(f)))
        .collect();
    for &t in &live {
        facing.add(*cofactor * area_vector(mesh, t));
    }
    bounds_of(live.iter().flatten().map(|&v| world(v))).map(|bounds| Place {
        bounds,
        facing: facing.direction(),
    })
}

/// Fills in which way each loop of `loops` faces, from the faces along
/// its edges (each boundary edge has one). An opening faces the way the
/// face that would close it would (its "cap": the loop's area vector,
/// wound against those faces), so a hole on the far side of a closed
/// surface faces away from it; a loop with no real cap (a slit, a twisted
/// loop) faces the way its faces mostly do. One pass over the faces,
/// testing only those with a corner on a boundary (a bitmap): O(n) with a
/// small constant, on the analysis worker.
fn loop_facing(
    mesh: &Mesh,
    loops: &[mf_geometry::BoundaryLoop],
    world: &impl Fn(u32) -> DVec3,
    cofactor: &DMat3,
    flip: f64,
    places: &mut [Option<Place>],
) {
    if loops.is_empty() {
        return;
    }
    let mut on_boundary = vec![false; mesh.vertex_slots() as usize];
    // (undirected edge, loop index), sorted.
    let mut edges: Vec<([u32; 2], usize)> = Vec::new();
    let mut perimeter = vec![0.0; loops.len()];
    let origin: Vec<DVec3> = loops
        .iter()
        .map(|l| l.vertices.first().map_or(DVec3::ZERO, |&v| world(v)))
        .collect();
    for (i, l) in loops.iter().enumerate() {
        let n = l.vertices.len();
        for k in 0..n {
            let (a, b) = (l.vertices[k], l.vertices[(k + 1) % n]);
            on_boundary[a as usize] = true;
            edges.push(([a.min(b), a.max(b)], i));
            perimeter[i] += (world(b) - world(a)).length();
        }
    }
    edges.sort_unstable();
    let mut faces = vec![Facing::default(); loops.len()];
    let mut cap = vec![DVec3::ZERO; loops.len()];
    for (_, t) in mesh.faces() {
        if !t.iter().any(|&v| on_boundary[v as usize]) {
            continue;
        }
        let n = *cofactor * area_vector(mesh, t);
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = [a.min(b), a.max(b)];
            let from = edges.partition_point(|(e, _)| *e < key);
            for (_, i) in edges[from..].iter().take_while(|(e, _)| *e == key) {
                faces[*i].add(n);
                // The cap runs this edge the other way (b → a).
                let o = origin[*i];
                cap[*i] += 0.5 * flip * (world(b) - o).cross(world(a) - o);
            }
        }
    }
    for (i, place) in places.iter_mut().enumerate() {
        let Some(p) = place else { continue };
        // A flat round opening's cap is as big as its perimeter allows
        // (P² / 4π); a slit's is next to nothing.
        let round = perimeter[i] * perimeter[i] / (4.0 * std::f64::consts::PI);
        let c = cap[i];
        p.facing = if c.is_finite() && round > 0.0 && c.length() > 0.1 * round {
            Some(c.normalize())
        } else {
            faces[i].direction()
        };
    }
}

/// Size along the world axes of every vertex placed by `model`.
fn world_size(mesh: &Mesh, model: &DMat4) -> Option<[f64; 3]> {
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

/// The defect's words, in plain language: its title, its tree row (a short
/// name and its size, so a speck is recognisable before you zoom to it),
/// its measurements and next action. Measurements are in the document's
/// `units`, after the object's transform.
pub fn defect_copy(defect: &Defect, units: Unit) -> DefectCopy {
    let u = unit_symbol(units);
    let copy = |title: String, name: String, size: String, detail: String, next: &str| DefectCopy {
        row: format!("{name} · {size}"),
        title,
        name,
        size,
        detail,
        next_action: next.to_owned(),
    };
    // "42 edges · 12.4 mm" (the perimeter).
    let around = |edges: usize, perimeter: Option<f64>| match perimeter {
        Some(p) => format!("{} · {} {u}", count(edges, "edge"), measure(p)),
        None => count(edges, "edge"),
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
                format!("Hole {number}"),
                around(*edges, *perimeter),
                hole_detail(*edges, *perimeter, units),
                "Fill hole",
            )
        }
        Defect::OverCap {
            edges, perimeter, ..
        } => copy(
            "Very large opening".into(),
            "Very large opening".into(),
            around(*edges, *perimeter),
            hole_detail(*edges, *perimeter, units),
            "Needs the large-hole method (not yet available)",
        ),
        Defect::Crack { edges, .. } => copy(
            "Narrow crack".into(),
            "Narrow crack".into(),
            count(*edges, "edge"),
            format!("Edges almost touch · {}", count(*edges, "edge")),
            "Needs edge stitching (not yet available)",
        ),
        Defect::LoosePiece {
            component,
            triangles,
            size,
        } => {
            let triangles = count(*triangles, "triangle");
            let (row_size, detail) = match size {
                Some(d) => (
                    format!("{triangles} · {} {u}", measure(*d)),
                    format!(
                        "{triangles}, {} {u} across, not connected to the model",
                        measure(*d)
                    ),
                ),
                None => (
                    triangles.clone(),
                    format!("{triangles}, not connected to the model"),
                ),
            };
            copy(
                "Loose piece".into(),
                // Numbered like holes: `loose[i]` is component i + 1.
                format!("Piece {component}"),
                row_size,
                detail,
                "Remove piece",
            )
        }
        Defect::Tangled { faces, .. } => copy(
            "Tangled edge".into(),
            "Tangled edge".into(),
            count(faces.len(), "face"),
            format!(
                "More than two faces share this edge · {}",
                count(faces.len(), "face")
            ),
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
        ("mesh.compact", _) => "Compact".into(),
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

    fn row(d: Defect) -> String {
        defect_copy(&d, Unit::Millimeter).row
    }

    /// Rows give each defect's size, so a speck is recognisable before
    /// you zoom to it (Task 8a).
    #[test]
    fn rows_name_each_defect_with_its_size() {
        assert_eq!(
            row(Defect::Hole {
                number: 3,
                loop_id: 7,
                edges: 42,
                perimeter: Some(12.4),
            }),
            "Hole 3 · 42 edges · 12.4 mm"
        );
        assert_eq!(
            row(Defect::OverCap {
                number: 2,
                loop_id: 5,
                edges: 1500,
                perimeter: None,
            }),
            "Very large opening · 1500 edges"
        );
        assert_eq!(
            row(Defect::Crack {
                loop_id: 2,
                edges: 8
            }),
            "Narrow crack · 8 edges"
        );
        assert_eq!(
            row(Defect::LoosePiece {
                component: 4,
                triangles: 1,
                size: Some(0.0213),
            }),
            "Piece 4 · 1 triangle · 0.0213 mm"
        );
        assert_eq!(
            row(Defect::Tangled {
                edge: [1, 2],
                faces: vec![0, 1, 2]
            }),
            "Tangled edge · 3 faces"
        );
        let c = defect_copy(
            &Defect::Hole {
                number: 1,
                loop_id: 0,
                edges: 3,
                perimeter: Some(0.5),
            },
            Unit::Inch,
        );
        assert_eq!(
            (c.name.as_str(), c.size.as_str()),
            ("Hole 1", "3 edges · 0.500 in")
        );
    }

    /// Each defect's place (for framing it) is in world space, measured on
    /// the worker: bounds after the transform, and which way it faces.
    #[test]
    fn places_are_in_world_space_and_face_outwards() {
        // An open box (top missing: the hole faces +z) and a loose
        // one-triangle speck in the xy plane, scaled ×2 and moved.
        let open_box = mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0);
        let speck = Mesh::new(
            vec![[20.0, 0.0, 0.0], [21.0, 0.0, 0.0], [20.0, 0.5, 0.0]],
            vec![[0, 1, 2]],
        )
        .unwrap();
        let mesh = mf_fixtures::merge(&[open_box, speck]);
        let t = [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 1.0, 2.0, 3.0];
        let d = classify(&mesh, &mf_geometry::inspect(&mesh), &t, 1000);
        let hole = d.place(&d.holes[0]).unwrap();
        assert_eq!(hole.bounds.min, [1.0, 2.0, 23.0]);
        assert_eq!(hole.bounds.max, [21.0, 22.0, 23.0]);
        let up = hole.facing.unwrap();
        assert!(up.z > 0.99, "the hole faces out of the box: {up}");
        let piece = d.place(&d.loose[0]).unwrap();
        assert_eq!(piece.bounds.min, [41.0, 2.0, 3.0]);
        assert_eq!(piece.bounds.max, [43.0, 3.0, 3.0]);
        assert!(piece.facing.unwrap().z.abs() > 0.99);
        assert!(matches!(
            d.loose[0],
            Defect::LoosePiece { size: Some(s), .. } if (s - 2.0).abs() < 1e-12
        ));
        // A closed piece faces every way.
        let cube = mf_fixtures::translated(&mf_fixtures::cube(1.0), [20.0, 0.0, 0.0]);
        let mesh = mf_fixtures::merge(&[holed_grid(), cube]);
        let d = classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000);
        assert_eq!(d.place(&d.loose[0]).unwrap().facing, None);
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
                "Edges almost touch · 8 edges",
                "Needs edge stitching (not yet available)"
            )
        );
        assert_eq!(
            copy(Defect::LoosePiece {
                component: 4,
                triangles: 12,
                size: Some(0.42),
            }),
            s(
                "Loose piece",
                "12 triangles, 0.420 mm across, not connected to the model",
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
                "More than two faces share this edge · 3 faces",
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
