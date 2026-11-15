//! Defects in plain language (docs/design/visual-language.md).
//!
//! [`classify`] sorts a revision's problems into the tree's categories (on
//! the analysis worker: it is O(n), MF-RT-001); [`defect_copy`] is the one
//! place their words come from, for the tree, Properties and tooltips.

use glam::{DMat3, DMat4, DVec3};
use mf_geometry::{Aabb, Diagnostics, Mesh};
use mf_io::threemf::Unit;
use mf_selection::SelectionKind;

/// The largest hole the fill operation closes today: bigger ones are
/// [`Defect::OverCap`] (MF-ISSUE-043). The desktop reads the real cap from
/// the operation's spec; this is only for the words.
pub const FILL_CAP_EDGES: usize = 1000;

/// The label every refusal's reason starts with: the limit is the fill
/// method's, not the model's (owner rule: algorithm limits are not product
/// limits).
pub const METHOD_LIMIT: &str = "Limit of this fill method";

/// The next action for a defect MeshFixer can't repair yet but that may be
/// meant to be there (a scan's open base, a socket, a seam).
pub const MARK_INTENTIONAL: &str = "Mark as intentional opening";

/// The next action for a tangled edge: select the faces at it, to inspect.
pub const SELECT_ITS_FACES: &str = "Select its faces";

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
    /// Every piece's size, indexed so "Remove loose pieces" can preview
    /// any thresholds in O(log n) (MF-ISSUE-052).
    pub pieces: mf_operations::ComponentSizes,
    /// Holes that pass through a vertex twice (a pinched point): fill_holes
    /// can't close them yet (MF-ISSUE-051). Loop ids, sorted.
    pub pinched: Vec<u32>,
    /// The object's hole numbering after this revision (see
    /// [`classify_numbered`]).
    pub numbers: std::sync::Arc<HoleNumbers>,
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
    /// What it means for printing, in plain words.
    pub verdict: String,
    /// Why MeshFixer can't repair it today, starting "Limit of this fill
    /// method" for a hole or crack ([`METHOD_LIMIT`]); `None` when it can.
    pub limit: Option<String>,
    /// What to do next, a button that exists today.
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
    /// [`defect_copy`], knowing which holes are pinched (fill_holes refuses
    /// them, MF-ISSUE-051), so the tree and Properties say the same. O(log n).
    pub fn copy(&self, defect: &Defect, units: Unit) -> DefectCopy {
        let pinched = matches!(defect, Defect::Hole { loop_id, .. }
            if self.pinched.binary_search(loop_id).is_ok());
        defect_copy_with(defect, pinched, units)
    }

    /// True if `defect` is a hole fill_holes can close (not over the cap,
    /// not pinched, not a crack). O(log n).
    pub fn fillable(&self, defect: &Defect) -> bool {
        matches!(defect, Defect::Hole { loop_id, .. }
            if self.pinched.binary_search(loop_id).is_err())
    }

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
    classify_numbered(
        mesh,
        diagnostics,
        transform,
        fill_cap,
        &HoleNumbers::default(),
    )
}

/// As [`classify`], numbering holes stably (addendum C): a hole keeps the
/// number `numbers` (the object's numbering so far) gave it, and a new
/// one gets the next number, in list order. Holes are listed largest
/// first (by edge count), so a first analysis numbers them that way. The
/// numbering it ends with is [`Defects::numbers`].
pub fn classify_numbered(
    mesh: &Mesh,
    diagnostics: &Diagnostics,
    transform: &[f64; 12],
    fill_cap: usize,
    numbers: &HoleNumbers,
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
    let mut keys: Vec<HoleKey> = Vec::new();
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
        if !unique_vertices(&l.vertices) {
            out.pinched.push(l.id);
        }
        // Numbered below, once sorted.
        let number = 0;
        let (loop_id, edges, perimeter) = (l.id, l.edge_count, perimeter(&l.vertices));
        keys.push(hole_key(mesh, &l.vertices, edges));
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
    number_holes(&mut out, keys, numbers);
    out.pinched.sort_unstable();
    out.loop_index.sort_unstable_by_key(|(id, ..)| *id);
    // Components are ordered largest first: the first is the model.
    out.main_body_triangles = topo.components.first().map_or(0, Vec::len);
    out.pieces = mf_operations::ComponentSizes::new(
        &topo.components.iter().map(Vec::len).collect::<Vec<_>>(),
    );
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

/// `previous` (this mesh's defects measured at transform `from`), measured
/// again at `to`: which holes, cracks, pieces and tangled edges there are
/// depends only on the mesh, so only the measurements change (perimeters,
/// sizes, where each defect is). A pure move shifts every place and keeps
/// every size: O(defects). Any other change (rotate, scale) measures again
/// with [`classify_numbered`] (no inspection, topology or GPU work).
pub fn remeasured(
    previous: &Defects,
    mesh: &Mesh,
    diagnostics: &Diagnostics,
    from: &[f64; 12],
    to: &[f64; 12],
    fill_cap: usize,
    numbers: &HoleNumbers,
) -> Defects {
    if from[..9] != to[..9] {
        return classify_numbered(mesh, diagnostics, to, fill_cap, numbers);
    }
    let d = [to[9] - from[9], to[10] - from[10], to[11] - from[11]];
    let mut out = previous.clone();
    let places = &mut out.places;
    for p in places
        .holes
        .iter_mut()
        .chain(places.cracks.iter_mut())
        .chain(places.loose.iter_mut())
        .chain(places.tangled.iter_mut())
        .flatten()
    {
        for (k, dk) in d.iter().enumerate() {
            p.bounds.min[k] += dk;
            p.bounds.max[k] += dk;
        }
    }
    out
}

/// What identifies a hole across revisions: its lexicographically smallest
/// corner (exact bits) and edge count. Kept elements keep their positions
/// (ADR-0001), compaction included, so a hole another command didn't
/// touch has the same key in every revision.
pub type HoleKey = ([u64; 3], usize);

fn hole_key(mesh: &Mesh, vertices: &[u32], edges: usize) -> HoleKey {
    let lowest = vertices
        .iter()
        .map(|&v| mesh.corner_position(v))
        .min_by(|a, b| {
            a[0].total_cmp(&b[0])
                .then(a[1].total_cmp(&b[1]))
                .then(a[2].total_cmp(&b[2]))
        })
        .map_or([0; 3], |p| p.map(f64::to_bits));
    (lowest, edges)
}

/// An object's hole numbers so far (addendum C): numbers only ever get
/// added, so a hole is "Hole 3" in every revision it's open in, and
/// undoing a fill brings its hole back under its old number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoleNumbers {
    by_key: std::collections::HashMap<HoleKey, usize>,
    next: usize,
}

impl HoleNumbers {
    /// Numbers handed out so far.
    pub fn issued(&self) -> usize {
        self.next
    }
}

/// Sorts `out.holes` (and their places) largest first, numbers them from
/// `numbers`, and indexes them. `keys[i]` is `out.holes[i]`'s key.
fn number_holes(out: &mut Defects, keys: Vec<HoleKey>, numbers: &HoleNumbers) {
    let mut next = numbers.clone();
    let mut entries: Vec<(Defect, Option<Place>, HoleKey, Option<usize>)> =
        std::mem::take(&mut out.holes)
            .into_iter()
            .zip(std::mem::take(&mut out.places.holes))
            .zip(keys)
            .map(|((d, p), k)| {
                let n = next.by_key.get(&k).copied();
                (d, p, k, n)
            })
            .collect();
    let edges = |d: &Defect| match d {
        Defect::Hole { edges, .. } | Defect::OverCap { edges, .. } => *edges,
        _ => 0,
    };
    // Largest first; among equals, numbered ones in number order, then new
    // ones by key (deterministic).
    entries.sort_by(|a, b| {
        edges(&b.0)
            .cmp(&edges(&a.0))
            .then(a.3.unwrap_or(usize::MAX).cmp(&b.3.unwrap_or(usize::MAX)))
            .then(a.2.cmp(&b.2))
    });
    let mut used = std::collections::HashSet::new();
    for (d, place, key, known) in entries {
        // Two holes with one key (a pinched pair) don't share a number.
        let n = match known.filter(|n| used.insert(*n)) {
            Some(n) => n,
            None => {
                next.next += 1;
                used.insert(next.next);
                next.by_key.insert(key, next.next);
                next.next
            }
        };
        let d = match d {
            Defect::Hole {
                loop_id,
                edges,
                perimeter,
                ..
            } => Defect::Hole {
                number: n,
                loop_id,
                edges,
                perimeter,
            },
            Defect::OverCap {
                loop_id,
                edges,
                perimeter,
                ..
            } => Defect::OverCap {
                number: n,
                loop_id,
                edges,
                perimeter,
            },
            other => other,
        };
        if let Some((_, loop_id)) = d.selection() {
            out.loop_index.push((loop_id, false, out.holes.len()));
        }
        out.holes.push(d);
        out.places.holes.push(place);
    }
    out.numbers = std::sync::Arc::new(next);
}

fn unique_vertices(vertices: &[u32]) -> bool {
    let mut v = vertices.to_vec();
    v.sort_unstable();
    v.windows(2).all(|w| w[0] != w[1])
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

/// The size groups the browser sorts holes into (M4 Task 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleGroup {
    /// Over 1000 edges: over the fill method's limit (MF-ISSUE-043).
    TooLarge,
    /// 201–1000 edges.
    Large,
    /// 31–200 edges.
    Medium,
    /// 9–30 edges.
    Small,
    /// Up to 8 edges.
    Tiny,
}

impl HoleGroup {
    /// Largest first: the order holes are listed in.
    pub const ALL: [HoleGroup; 5] = [
        HoleGroup::TooLarge,
        HoleGroup::Large,
        HoleGroup::Medium,
        HoleGroup::Small,
        HoleGroup::Tiny,
    ];

    pub fn of(edges: usize) -> HoleGroup {
        match edges {
            0..=8 => HoleGroup::Tiny,
            9..=30 => HoleGroup::Small,
            31..=200 => HoleGroup::Medium,
            201..=1000 => HoleGroup::Large,
            _ => HoleGroup::TooLarge,
        }
    }

    /// Its tree header: "Tiny holes (up to 8 edges)".
    pub fn title(self) -> &'static str {
        match self {
            HoleGroup::TooLarge => "Very large holes (over 1000 edges)",
            HoleGroup::Large => "Large holes (201–1000 edges)",
            HoleGroup::Medium => "Medium holes (31–200 edges)",
            HoleGroup::Small => "Small holes (9–30 edges)",
            HoleGroup::Tiny => "Tiny holes (up to 8 edges)",
        }
    }

    /// Its title without the sizes: "Tiny holes" (where space is short;
    /// the sizes go in its tooltip).
    pub fn short_title(self) -> &'static str {
        match self {
            HoleGroup::TooLarge => "Very large holes",
            HoleGroup::Large => "Large holes",
            HoleGroup::Medium => "Medium holes",
            HoleGroup::Small => "Small holes",
            HoleGroup::Tiny => "Tiny holes",
        }
    }

    /// The word for its group action: "Fill all 312 tiny holes".
    pub fn adjective(self) -> &'static str {
        match self {
            HoleGroup::TooLarge => "very large",
            HoleGroup::Large => "large",
            HoleGroup::Medium => "medium",
            HoleGroup::Small => "small",
            HoleGroup::Tiny => "tiny",
        }
    }
}

/// How the browser lists one revision's holes, given the intentional
/// openings marked on it: built on a worker (O(holes)), read every frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoleView {
    /// The marks it was made for (loop ids, sorted).
    pub openings: Vec<u32>,
    /// Indices into [`Defects::holes`] of the holes listed (every hole not
    /// marked), largest first: the tree's, N's and the groups' order.
    pub listed: Vec<usize>,
    /// Indices into [`Defects::holes`] of the marked openings.
    pub marked: Vec<usize>,
    /// Indices into [`Defects::cracks`] of the cracks listed (not marked).
    pub cracks: Vec<usize>,
    /// Indices into [`Defects::cracks`] of the cracks marked intentional.
    pub marked_cracks: Vec<usize>,
    /// Each [`HoleGroup`]'s range of `listed` (in `HoleGroup::ALL` order).
    groups: [std::ops::Range<usize>; 5],
    /// (edges, loop id) of every listed hole fill_holes can close, fewest
    /// edges first: the batch fill's candidates.
    pub fillable: Vec<(usize, u32)>,
    /// Loop id → position in `listed`.
    position: std::collections::HashMap<u32, usize>,
    /// Each group's holes fill_holes can close (loop ids, in list order):
    /// what its "Fill all N" fills, and so its N.
    fillable_in: [Vec<u32>; 5],
}

impl HoleView {
    /// `group`'s holes: a range of [`HoleView::listed`].
    pub fn group(&self, group: HoleGroup) -> std::ops::Range<usize> {
        let i = HoleGroup::ALL
            .iter()
            .position(|g| *g == group)
            .expect("ALL lists every group");
        self.groups[i].clone()
    }

    /// The loop ids `group`'s "Fill all N" fills (pinched holes and holes
    /// over the fill cap are listed but left out).
    pub fn group_fillable(&self, group: HoleGroup) -> &[u32] {
        let i = HoleGroup::ALL
            .iter()
            .position(|g| *g == group)
            .expect("ALL lists every group");
        &self.fillable_in[i]
    }

    /// Where loop `id` is in [`HoleView::listed`], if it's listed. O(1).
    pub fn position(&self, id: u32) -> Option<usize> {
        self.position.get(&id).copied()
    }

    /// Holes a batch fill up to `max_edges` edges closes, and the listed
    /// ones it leaves open. O(log n).
    pub fn batch(&self, max_edges: usize) -> (usize, usize) {
        let k = self.fillable.partition_point(|(e, _)| *e <= max_edges);
        (k, self.listed.len() - k)
    }
}

/// How `defects`' holes and cracks are listed with `openings` (loop ids)
/// marked as intentional. O(holes + cracks); run it on a worker.
pub fn hole_view(defects: &Defects, openings: &[u32]) -> HoleView {
    let mut openings = openings.to_vec();
    openings.sort_unstable();
    openings.dedup();
    let mut view = HoleView {
        openings,
        ..HoleView::default()
    };
    for (i, d) in defects.holes.iter().enumerate() {
        let Some((_, id)) = d.selection() else {
            continue;
        };
        if view.openings.binary_search(&id).is_ok() {
            view.marked.push(i);
        } else {
            view.position.insert(id, view.listed.len());
            view.listed.push(i);
            if let Defect::Hole { edges, .. } = d
                && defects.pinched.binary_search(&id).is_err()
            {
                view.fillable.push((*edges, id));
                let g = HoleGroup::ALL
                    .iter()
                    .position(|g| *g == HoleGroup::of(*edges))
                    .expect("ALL lists every group");
                view.fillable_in[g].push(id);
            }
        }
    }
    for (i, d) in defects.cracks.iter().enumerate() {
        let marked = d
            .selection()
            .is_some_and(|(_, id)| view.openings.binary_search(&id).is_ok());
        if marked {
            view.marked_cracks.push(i);
        } else {
            view.cracks.push(i);
        }
    }
    view.fillable.sort_unstable();
    // Holes are sorted largest first, so each group is a run of `listed`.
    let edges = |i: usize| match &defects.holes[i] {
        Defect::Hole { edges, .. } | Defect::OverCap { edges, .. } => *edges,
        _ => 0,
    };
    let mut start = 0;
    for (g, group) in HoleGroup::ALL.iter().enumerate() {
        let end = start
            + view.listed[start..]
                .iter()
                .take_while(|&&i| HoleGroup::of(edges(i)) == *group)
                .count();
        view.groups[g] = start..end;
        start = end;
    }
    view
}

/// A parameter's name in Properties: "max_loop_edges" → "Max loop edges"
/// (the batch fill calls its edge limit "Fill holes up to").
pub fn param_label(op_id: &str, param: &str) -> String {
    if (op_id, param) == ("mesh.fill_holes", "max_loop_edges") {
        return "Fill holes up to".into();
    }
    let spaced = param.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

/// A choice parameter's option in plain words: its name in Properties and
/// one line saying what it does (MF-OP-002 fill styles).
pub fn choice_copy(op_id: &str, param: &str, value: &str) -> Option<(&'static str, &'static str)> {
    Some(match (op_id, param, value) {
        ("mesh.fill_holes", "fill_style", "flat") => (
            "Flat",
            "Closes the hole with a flat patch. Best on flat faces and sharp edges.",
        ),
        ("mesh.fill_holes", "fill_style", "smooth") => (
            "Smooth (matches slope)",
            "Bends the patch to meet the surface around it without a crease.",
        ),
        ("mesh.fill_holes", "fill_style", "curvature") => (
            "Curvature-matched",
            "Also follows how the surface curves. Best on round, organic shapes.",
        ),
        _ => return None,
    })
}

/// What a tool does, in plain words first (the spec's engineering
/// summary follows it in Properties).
pub fn tool_summary(op_id: &str) -> Option<&'static str> {
    Some(match op_id {
        "mesh.fill_holes" => "Closes holes with new triangles matched to the mesh around them.",
        "mesh.remove_small_components" => {
            "Deletes small pieces that aren't connected to the model, such as scan specks."
        }
        "mesh.compact" => "Frees the space earlier edits left unused. The shape doesn't change.",
        _ => return None,
    })
}

/// The defect's words, in plain language: its title, its tree row (a short
/// name and its size, so a speck is recognisable before you zoom to it),
/// its measurements, what it means for printing, why MeshFixer can't fix
/// it yet (if it can't) and the next action. Measurements are in the
/// document's `units`, after the object's transform. A pinched hole needs
/// the analysis: see [`Defects::copy`].
pub fn defect_copy(defect: &Defect, units: Unit) -> DefectCopy {
    defect_copy_with(defect, false, units)
}

/// [`defect_copy`], for a hole that is `pinched` (passes through one point
/// twice, so fill_holes refuses it) or not.
pub fn defect_copy_with(defect: &Defect, pinched: bool, units: Unit) -> DefectCopy {
    let u = unit_symbol(units);
    let copy = |title: String,
                name: String,
                size: String,
                detail: String,
                verdict: &str,
                limit: Option<String>,
                next: &str| DefectCopy {
        row: format!("{name} · {size}"),
        title,
        name,
        size,
        detail,
        verdict: verdict.to_owned(),
        limit,
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
        } if pinched => copy(
            format!(
                "Hole {number}: {} opening",
                HoleGroup::of(*edges).adjective()
            ),
            format!("Hole {number}"),
            around(*edges, *perimeter),
            hole_detail(*edges, *perimeter, units),
            "An opening that pinches to a single point. The model isn't closed, so it won't \
             print as a solid until it's closed.",
            Some(format!(
                "{METHOD_LIMIT}: this hole passes through one point twice, so it can't be \
                 filled yet. Filling holes like this is planned."
            )),
            MARK_INTENTIONAL,
        ),
        Defect::Hole {
            number,
            edges,
            perimeter,
            ..
        } => copy(
            format!(
                "Hole {number}: {} opening",
                HoleGroup::of(*edges).adjective()
            ),
            format!("Hole {number}"),
            around(*edges, *perimeter),
            hole_detail(*edges, *perimeter, units),
            "An opening in the surface. The model isn't closed, so it won't print as a solid \
             until it's filled.",
            None,
            "Fill hole",
        ),
        Defect::OverCap {
            number,
            edges,
            perimeter,
            ..
        } => copy(
            format!("Hole {number}: very large opening"),
            format!("Hole {number}"),
            around(*edges, *perimeter),
            hole_detail(*edges, *perimeter, units),
            "A large opening, often the open base or cut edge of a scan. It won't print \
             closed as it is. If it's meant to be open (a base, a socket), mark it so.",
            Some(format!(
                "{METHOD_LIMIT}: it fills holes of up to {} edges, and this one has {}. \
                 Filling larger holes is planned.",
                thousands(FILL_CAP_EDGES as u64),
                thousands(*edges as u64)
            )),
            MARK_INTENTIONAL,
        ),
        Defect::Crack { edges, .. } => copy(
            "Narrow crack".into(),
            "Narrow crack".into(),
            count(*edges, "edge"),
            format!("Edges almost touch · {}", count(*edges, "edge")),
            "Two edges almost touch, like a hairline seam. Usually harmless for printing: \
             slicers close a gap this thin.",
            Some(format!(
                "{METHOD_LIMIT}: a crack has no area to fill, so it needs its edges joined \
                 instead. Joining them is planned."
            )),
            MARK_INTENTIONAL,
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
                "It prints as a separate bit, or not at all if it's tiny. Usually scan debris.",
                None,
                "Remove piece",
            )
        }
        Defect::Tangled { faces, .. } => copy(
            "Tangled edge".into(),
            "Tangled edge".into(),
            count(faces.len(), "triangle"),
            format!(
                "More than two triangles share this edge (a non-manifold edge) · {}",
                count(faces.len(), "triangle")
            ),
            "The surface folds onto itself here. Slicers may print this spot wrongly.",
            Some(
                "MeshFixer can't untangle edges yet: repairing them is planned. Select its \
                 faces to see where they are."
                    .into(),
            ),
            SELECT_ITS_FACES,
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

/// A count with thousands separators: "1,050".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 triangle", "12 triangles", "1,050 triangles".
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", thousands(n as u64))
    }
}

/// A length to three significant figures (whole numbers from 100 up):
/// the one formatter every panel's caption uses, now `mf_units`'s own
/// unitless core (`significant_figures`, consolidation plan C11) rather
/// than a second copy of it — the same rule the engine's own refusal
/// messages use via `mf_spec::format_length` (D20: ops have no `Unit` in
/// scope), so "at least 0.000" never reaches a person, in a panel or in
/// a refusal. Callers combine this with [`unit_symbol`] themselves
/// because `v` here is already a document-unit value and `units` the
/// document's own unit — composing "{measure(v)} {unit_symbol(units)}"
/// is the same physical length [`mf_units::format_length`] would show,
/// just without its automatic unit-stepping (callers that want that,
/// such as an area that could round to "0" in the document's unit, call
/// `mf_units::format_length`/`format_area`/`format_volume` directly).
pub fn measure(v: f64) -> String {
    mf_units::significant_figures(v, 3)
}

/// Inspect's surface area row: `area` (document units²) through the one
/// formatter, with its unit ("1.20 mm²", never a bare "0.0000").
pub fn area_text(area: Option<f64>, units: Unit) -> String {
    area.map_or("too large".into(), |a| mf_units::format_area(a, units))
}

/// Inspect's volume row, as [`area_text`] ("8.00 cm³").
pub fn volume_text(volume: Option<f64>, units: Unit) -> String {
    volume.map_or("— (not closed)".into(), |v| {
        mf_units::format_volume(v, units)
    })
}

/// `unit`'s display symbol ("mm", "m", …), delegating to `mf_units`
/// (consolidation plan C11) so there is one table of unit symbols, not
/// a second copy kept here.
pub fn unit_symbol(units: Unit) -> &'static str {
    units.symbol()
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

    /// A move only shifts where each defect is; a rotation measures again.
    /// Either way the result is what a fresh analysis at the new
    /// transform finds.
    #[test]
    fn remeasuring_after_a_move_or_turn_matches_a_fresh_analysis() {
        // Every kind: holes, a crack, a tangled edge (the plate's fin) and
        // a loose piece (a tetrahedron above the plate).
        let piece = mf_fixtures::translated(
            &mf_fixtures::scaled(&mf_fixtures::tetrahedron(), 4.0),
            [10.0, 10.0, 8.0],
        );
        let mesh = mf_fixtures::merge(&[mf_fixtures::plate_with_defects(), piece]);
        let diagnostics = mf_geometry::inspect(&mesh);
        let numbers = HoleNumbers::default();
        let before = classify_numbered(&mesh, &diagnostics, &IDENTITY, 1000, &numbers);
        assert!(
            !before.holes.is_empty()
                && !before.cracks.is_empty()
                && !before.loose.is_empty()
                && !before.tangled.is_empty(),
            "every kind of defect is here"
        );
        let mut moved = IDENTITY;
        moved[9] = 4.0;
        moved[11] = -2.5;
        let mut turned = moved;
        (turned[0], turned[1], turned[3], turned[4]) = (0.0, 1.0, -1.0, 0.0);
        let close = |a: &[Option<Place>], b: &[Option<Place>]| {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                let (a, b) = (a.unwrap(), b.unwrap());
                for k in 0..3 {
                    assert!((a.bounds.min[k] - b.bounds.min[k]).abs() < 1e-9);
                    assert!((a.bounds.max[k] - b.bounds.max[k]).abs() < 1e-9);
                }
            }
        };
        for to in [moved, turned] {
            let again = remeasured(&before, &mesh, &diagnostics, &IDENTITY, &to, 1000, &numbers);
            let fresh = classify_numbered(&mesh, &diagnostics, &to, 1000, &numbers);
            assert_eq!(again.holes, fresh.holes);
            assert_eq!(again.cracks, fresh.cracks);
            assert_eq!(again.tangled, fresh.tangled);
            assert_eq!(again.loose.len(), fresh.loose.len());
            for (a, b) in again.loose.iter().zip(&fresh.loose) {
                match (a, b) {
                    (
                        Defect::LoosePiece {
                            component: c1,
                            triangles: t1,
                            size: s1,
                        },
                        Defect::LoosePiece {
                            component: c2,
                            triangles: t2,
                            size: s2,
                        },
                    ) => {
                        assert_eq!((c1, t1), (c2, t2));
                        assert!((s1.unwrap() - s2.unwrap()).abs() < 1e-9);
                    }
                    other => panic!("not pieces: {other:?}"),
                }
            }
            assert_eq!(again.world_size, fresh.world_size);
            close(&again.places.holes, &fresh.places.holes);
            close(&again.places.cracks, &fresh.places.cracks);
            close(&again.places.loose, &fresh.places.loose);
            close(&again.places.tangled, &fresh.places.tangled);
        }
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
            "Hole 2 · 1,500 edges"
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
            "Tangled edge · 3 triangles"
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

    /// The browser's size groups (M4 Task 12): tiny ≤ 8, small ≤ 30,
    /// medium ≤ 200, large ≤ 1000, too large above.
    #[test]
    fn hole_groups_split_at_the_stated_sizes() {
        for (edges, group) in [
            (3, HoleGroup::Tiny),
            (8, HoleGroup::Tiny),
            (9, HoleGroup::Small),
            (30, HoleGroup::Small),
            (31, HoleGroup::Medium),
            (200, HoleGroup::Medium),
            (201, HoleGroup::Large),
            (1000, HoleGroup::Large),
            (1001, HoleGroup::TooLarge),
        ] {
            assert_eq!(HoleGroup::of(edges), group, "{edges} edges");
        }
    }

    /// Holes are listed largest first and numbered that way on a first
    /// analysis; a later analysis keeps the numbers (addendum C).
    #[test]
    fn holes_are_numbered_largest_first_and_keep_their_numbers() {
        let mesh = holed_grid();
        let d = classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000);
        let numbers: Vec<(usize, usize)> = d
            .holes
            .iter()
            .map(|h| match h {
                Defect::Hole { number, edges, .. } => (*number, *edges),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(numbers, vec![(1, 24), (2, 3)]);
        let again = classify_numbered(
            &mesh,
            &mf_geometry::inspect(&mesh),
            &IDENTITY,
            1000,
            &d.numbers,
        );
        assert_eq!(again.holes, d.holes);
        assert_eq!(again.numbers.issued(), 2);
        let view = hole_view(&d, &[]);
        assert_eq!(view.listed, vec![0, 1]);
        assert_eq!(view.group(HoleGroup::Tiny), 1..2);
        assert_eq!(view.batch(3), (1, 1));
    }

    /// M4 final UX review C1: a narrow crack can be marked intentional too,
    /// so the defect count can reach zero.
    #[test]
    fn a_crack_marked_intentional_is_not_listed() {
        let plate = mf_fixtures::plate_with_defects();
        let d = classify(&plate, &mf_geometry::inspect(&plate), &IDENTITY, 1000);
        assert_eq!(d.cracks.len(), 1);
        assert_eq!(hole_view(&d, &[]).cracks, vec![0]);
        let (_, id) = d.cracks[0].selection().unwrap();
        let view = hole_view(&d, &[id]);
        assert!(view.cracks.is_empty());
        assert_eq!(view.marked_cracks, vec![0]);
        assert_eq!(view.listed.len(), d.holes.len(), "holes are unaffected");
    }

    /// A group's "Fill all N" counts only the holes Apply fills: a pinched
    /// hole is listed but left out (fix round 1, P3).
    #[test]
    fn a_groups_fill_count_leaves_out_pinched_holes() {
        let mesh = holed_grid();
        let mut d = classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000);
        let (_, small) = d.holes[1].selection().unwrap();
        d.pinched = vec![small];
        let view = hole_view(&d, &[]);
        assert_eq!(view.group(HoleGroup::Tiny).len(), 1, "still listed");
        assert!(view.group_fillable(HoleGroup::Tiny).is_empty());
        let view = hole_view(
            &classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000),
            &[],
        );
        assert_eq!(view.group_fillable(HoleGroup::Tiny), &[small]);
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
                "Hole 3: medium opening",
                "42 boundary edges · 12.4 mm perimeter",
                "Fill hole"
            )
        );
        assert_eq!(
            copy(Defect::Crack {
                loop_id: 2,
                edges: 8
            }),
            s(
                "Narrow crack",
                "Edges almost touch · 8 edges",
                "Mark as intentional opening"
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
                "More than two triangles share this edge (a non-manifold edge) · 3 triangles",
                "Select its faces"
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
                "Hole 2: very large opening",
                "1,500 boundary edges · 88.0 mm perimeter",
                "Mark as intentional opening"
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

    fn one_of_each() -> Vec<Defect> {
        vec![
            Defect::Hole {
                number: 1,
                loop_id: 0,
                edges: 12,
                perimeter: Some(3.0),
            },
            Defect::OverCap {
                number: 2,
                loop_id: 1,
                edges: 1525,
                perimeter: Some(300.0),
            },
            Defect::Crack {
                loop_id: 2,
                edges: 6,
            },
            Defect::LoosePiece {
                component: 1,
                triangles: 4,
                size: Some(0.5),
            },
            Defect::Tangled {
                edge: [1, 2],
                faces: vec![0, 1, 2],
            },
        ]
    }

    /// M4 final UX review C1: every defect says what it means for
    /// printing; the ones MeshFixer can't repair yet say why, as a limit of
    /// the method (never "the current method" or "not yet available"), and
    /// name a next action that exists today.
    #[test]
    fn every_defect_has_a_verdict_and_refusals_say_why_and_what_next() {
        for d in one_of_each() {
            for pinched in [false, true] {
                let c = defect_copy_with(&d, pinched, Unit::Millimeter);
                let all = format!("{} {} {:?} {}", c.title, c.verdict, c.limit, c.next_action);
                assert!(!c.verdict.is_empty(), "{d:?}");
                for banned in ["current method", "not yet available", "Slit", "welding"] {
                    assert!(!all.contains(banned), "{banned:?} in {all}");
                }
                let refused = match d {
                    Defect::Hole { .. } => pinched,
                    Defect::OverCap { .. } | Defect::Crack { .. } | Defect::Tangled { .. } => true,
                    Defect::LoosePiece { .. } => false,
                };
                assert_eq!(c.limit.is_some(), refused, "{d:?} pinched {pinched}");
                if refused && !matches!(d, Defect::Tangled { .. }) {
                    assert!(
                        c.limit.as_deref().unwrap().starts_with(METHOD_LIMIT),
                        "{:?}",
                        c.limit
                    );
                    assert_eq!(c.next_action, MARK_INTENTIONAL, "{d:?}");
                }
            }
        }
        let over = defect_copy(&one_of_each()[1], Unit::Millimeter);
        assert!(over.verdict.contains("base"), "{}", over.verdict);
        assert!(
            over.limit.as_deref().unwrap().contains("1,000")
                && over.limit.unwrap().contains("1,525")
        );
        let tangled = defect_copy(&one_of_each()[4], Unit::Millimeter);
        assert_eq!(tangled.next_action, SELECT_ITS_FACES);
        assert!(tangled.verdict.contains("print"));
    }

    /// M4 final UX review I4: a hole's title uses its size group's word,
    /// so the tree's "Medium holes" lists "medium" openings.
    #[test]
    fn hole_titles_agree_with_their_size_groups() {
        for edges in [3, 8, 9, 30, 31, 200, 201, 1000, 1001, 5000] {
            let group = HoleGroup::of(edges);
            let d = if edges > FILL_CAP_EDGES {
                Defect::OverCap {
                    number: 1,
                    loop_id: 0,
                    edges,
                    perimeter: None,
                }
            } else {
                Defect::Hole {
                    number: 1,
                    loop_id: 0,
                    edges,
                    perimeter: None,
                }
            };
            let title = defect_copy(&d, Unit::Millimeter).title;
            assert!(
                title.ends_with(&format!(": {} opening", group.adjective())),
                "{edges} edges: {title}"
            );
            assert!(
                group.title().to_lowercase().starts_with(group.adjective()),
                "{}",
                group.title()
            );
        }
    }

    /// M4 final UX review I7: a pinched hole isn't fillable, and its words
    /// (through the analysis) say why and what to do instead.
    #[test]
    fn a_pinched_hole_says_it_cant_be_filled() {
        let mesh = holed_grid();
        let mut d = classify(&mesh, &mf_geometry::inspect(&mesh), &IDENTITY, 1000);
        // (Real pinched loops come from scans; marked as the analysis would.)
        let (_, small) = d.holes[1].selection().unwrap();
        d.pinched = vec![small];
        let hole = d
            .holes
            .iter()
            .find(|h| h.selection().is_some_and(|(_, id)| id == d.pinched[0]))
            .unwrap();
        assert!(!d.fillable(hole));
        let c = d.copy(hole, Unit::Millimeter);
        assert_eq!(c.next_action, MARK_INTENTIONAL);
        assert!(c.limit.unwrap().contains("passes through one point twice"));
        let rim = d.holes.iter().find(|h| *h != hole).unwrap();
        assert!(d.fillable(rim));
        assert_eq!(d.copy(rim, Unit::Millimeter).next_action, "Fill hole");
    }
}
