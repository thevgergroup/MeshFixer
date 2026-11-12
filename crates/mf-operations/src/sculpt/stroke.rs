//! The stroke engine (MF-SCULPT-001, MF-SCULPT-003): one stroke's dabs
//! applied in order to a copy-on-write edit of the input revision.
//!
//! - **Dab centres** are points on the *input* revision (face +
//!   barycentric weights), projected along the view direction onto the
//!   surface as it is mid-stroke: the nearest crossing of that line with a
//!   current face near the point, within the radius. With none, the input
//!   point itself is the centre.
//! - **Each dab** reads positions as they were before it (all of its
//!   displacements are computed, then applied), visits faces in ascending
//!   id order, and runs on one thread, so a stroke is a pure function of
//!   its input, parameters and samples, at any thread count.
//! - **Symmetry** mirrors each dab across the planes through the pivot
//!   normal to the enabled object axes. When the vertices under every
//!   mirrored dab are exact mirror images of each other, each one's
//!   displacement is combined from its mirror images' in a fixed
//!   butterfly order, so the result is mirrored **bit for bit**. On a
//!   model that isn't exactly symmetric there, each mirrored dab is
//!   evaluated on its own.
//! - **Masks**: a vertex of a frozen face never moves; with `limit_to`,
//!   only vertices of those faces move.
//!
//! The live preview ([`StrokeState::preview_mesh`]) and the committed or
//! replayed command run this same code, so they're identical.

use std::collections::{HashMap, HashSet};

use mf_geometry::{CHUNK, FaceId, Mesh, MeshEdit, Provenance, VertexId};
use mf_runtime::RunContext;

use super::brush::{BrushKind, BrushParams, DabSample};
use super::index::SculptIndex;
use super::remesh::{RegionAdjacency, RemeshStats, remesh_region};
use crate::OpError;

/// What one dab changed: the GPU face chunks and vertex groups (of
/// [`CHUNK`]) to refresh (MF-SCULPT-004).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DabDelta {
    pub face_chunks: Vec<usize>,
    pub vertex_chunks: Vec<usize>,
}

type V3 = [f64; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalized(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 0.0 && l.is_finite()).then(|| scale(a, 1.0 / l))
}

/// The positions (relative to `pivot` on mirrored axes) mirrored across
/// the axes in `mask` (bit k: axis k).
fn mirror_point(p: V3, pivot: V3, mask: u8) -> V3 {
    std::array::from_fn(|k| {
        if mask & (1 << k) != 0 {
            2.0 * pivot[k] - p[k]
        } else {
            p[k]
        }
    })
}
fn mirror_vec(d: V3, mask: u8) -> V3 {
    std::array::from_fn(|k| if mask & (1 << k) != 0 { -d[k] } else { d[k] })
}

/// Grab's captured vertex set (MF-SCULPT-005/Grab fix): the geodesic
/// region connected to the face under the first dab, within the radius,
/// applying "stay on this surface"'s own crease rule regardless of that
/// option's setting (a drag must never reach past a sharp crease into an
/// unrelated part of the model). Captured once, at stroke start, and
/// never recomputed or grown as the stroke continues: every later dab
/// moves exactly this set, from its position *as captured*, by the
/// cursor's total displacement since then — never a per-dab delta, which
/// would otherwise keep re-applying motion to a (possibly different)
/// radius-based set on every dab and run away into spikes.
struct GrabRegion {
    /// The dab centre the drag started from (the view-plane projection
    /// origin for every later dab's displacement).
    origin: V3,
    /// Each captured vertex's position at capture time and its falloff
    /// weight (by geodesic distance from the start face, fixed for the
    /// whole stroke).
    vertices: HashMap<u32, (V3, f64)>,
    /// Every face with a corner in `vertices` (captured region or its
    /// one-ring boundary), and its normal at capture time: the fixed
    /// baseline the whole stroke's inversion clamp measures against
    /// (never a dab's own prior positions — a long drag must never flip
    /// a face relative to where the stroke *started*, even if each
    /// individual dab's own step looked safe against the step before
    /// it).
    faces: Vec<([u32; 3], V3)>,
}

/// One stroke in progress. See the module docs.
pub struct StrokeState {
    /// The stroke's input revision.
    base: Mesh,
    /// Cheap to hold by value: just an `Arc` clone of the revision's BVH
    /// (`SculptIndex::from_bvh`), so a stroke can own it for as long as it
    /// runs (a live UI-thread preview included) with no borrow to thread
    /// through.
    index: SculptIndex,
    params: BrushParams,
    edit: MeshEdit,
    /// An upper bound on how far any vertex has moved in this stroke, so
    /// queries of the input revision's index still find every face near a
    /// point on the current surface.
    drift: f64,
    prev_centre: Option<V3>,
    limit: Option<HashSet<u32>>,
    frozen: HashSet<u32>,
    /// Faces created during the stroke (Task 18's remeshing), not in the
    /// index: always considered.
    created_faces: Vec<FaceId>,
    centres: Vec<V3>,
    base_faces: u32,
    /// Created faces → the input faces they came from (remeshing).
    origins: HashMap<FaceId, Vec<FaceId>>,
    remesh: RemeshStats,
    /// Grab's captured regions (`BrushKind::Drag` only), one per mirror
    /// image (`apply`'s `masks`, same order), set on the first dab and
    /// then fixed for the rest of the stroke. Empty until the first dab
    /// runs.
    grab: Vec<GrabRegion>,
}

/// A vertex's surroundings for one dab evaluation.
struct Local {
    /// Candidate faces (live, ascending id) and their current corners.
    faces: Vec<(FaceId, [u32; 3])>,
    /// Vertex → indices into `faces` of its incident candidate faces.
    incident: HashMap<u32, Vec<usize>>,
}

/// "Stay on this surface" (MF-SCULPT-005): a sharp crease stays a wall
/// the brush never crosses. Two faces sharing an edge are on the same
/// side of it when their normals are within this angle (about 60°, the
/// brief's own number): past it, the dihedral is a deliberate edge (a
/// split cap meeting the surface it came from, a hard corner), not a
/// smooth bend a brush should flow across.
const CREASE_COS: f64 = 0.5; // cos(60°)

/// Every candidate face's centroid and outward normal, indexed the same
/// as `local.faces`.
fn face_frames(local: &Local, pos: impl Fn(u32) -> V3) -> Vec<(V3, V3)> {
    local
        .faces
        .iter()
        .map(|(_, t)| {
            let [a, b, c] = t.map(&pos);
            let n = cross(sub(b, a), sub(c, a));
            let centroid = scale(add(add(a, b), c), 1.0 / 3.0);
            (centroid, normalized(n).unwrap_or([0.0, 0.0, 1.0]))
        })
        .collect()
}

/// Each candidate face's neighbours across a shared edge whose dihedral
/// stays under [`CREASE_COS`] (the faces a flood fill may step into from
/// it), by index into `local.faces`.
fn face_adjacency(local: &Local, frames: &[(V3, V3)]) -> Vec<Vec<usize>> {
    // Every live edge (its two vertices, sorted) → the face indices that
    // have it (at most 2 on a manifold mesh; more are skipped, same as a
    // non-manifold edge never being a "smooth" crossing).
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, (_, t)) in local.faces.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = if a < b { (a, b) } else { (b, a) };
            by_edge.entry(key).or_default().push(i);
        }
    }
    let mut adj = vec![Vec::new(); local.faces.len()];
    for faces in by_edge.values() {
        if let [i, j] = faces[..] {
            let smooth = dot(frames[i].1, frames[j].1) >= CREASE_COS;
            if smooth {
                adj[i].push(j);
                adj[j].push(i);
            }
        }
    }
    adj
}

/// The face in `local.faces` whose centroid is nearest `c`: where a dab
/// centred at `c` lands, for "Stay on this surface"'s flood fill to
/// start from. `None` if `local` has no faces (nothing to flood from).
fn nearest_face(c: V3, frames: &[(V3, V3)]) -> Option<usize> {
    frames
        .iter()
        .enumerate()
        .min_by(|(_, (ca, _)), (_, (cb, _))| {
            let (da, db) = (dot(sub(*ca, c), sub(*ca, c)), dot(sub(*cb, c), sub(*cb, c)));
            da.total_cmp(&db)
        })
        .map(|(i, _)| i)
}

/// Each face's geodesic distance from `c` (Dijkstra over face centroids,
/// stepping only across a smooth edge: [`face_adjacency`]), up to `max`;
/// faces past `max` or unreachable without crossing a crease are left
/// out. A standard fast-marching approximation (centroid-to-centroid
/// steps, not exact geodesics), the same kind of estimate interactive
/// sculpting tools have always used: cheap enough for every dab, and
/// correct about what it must be — it never steps across a sharp crease.
fn face_geodesic_distances(
    frames: &[(V3, V3)],
    adj: &[Vec<usize>],
    origin: usize,
    origin_point: V3,
    max: f64,
) -> HashMap<usize, f64> {
    use std::cmp::Ordering;
    #[derive(PartialEq)]
    struct Entry(f64, usize);
    impl Eq for Entry {}
    impl PartialOrd for Entry {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Entry {
        fn cmp(&self, other: &Self) -> Ordering {
            // A min-heap from `BinaryHeap` (a max-heap): reversed.
            other.0.total_cmp(&self.0)
        }
    }
    let mut dist: HashMap<usize, f64> = HashMap::new();
    let mut heap = std::collections::BinaryHeap::new();
    let d0 = dot(
        sub(frames[origin].0, origin_point),
        sub(frames[origin].0, origin_point),
    )
    .sqrt();
    dist.insert(origin, d0);
    heap.push(Entry(d0, origin));
    while let Some(Entry(d, i)) = heap.pop() {
        if dist.get(&i).is_some_and(|&best| d > best) {
            continue; // a shorter path already settled this face
        }
        if d > max {
            continue;
        }
        for &j in &adj[i] {
            let step = dot(sub(frames[j].0, frames[i].0), sub(frames[j].0, frames[i].0)).sqrt();
            let nd = d + step;
            if nd <= max && dist.get(&j).is_none_or(|&best| nd < best) {
                dist.insert(j, nd);
                heap.push(Entry(nd, j));
            }
        }
    }
    dist
}

/// Each candidate vertex's geodesic distance from `c` (the dab centre),
/// for "Stay on this surface": the *highest* of its incident candidate
/// faces' (`face_geodesic_distances`), and left out entirely if even one
/// incident face is unreached. A vertex moving would change every face
/// it's a corner of — a cap's rim vertex, shared with the curved surface
/// it was cut from, would deform the cap's own flat face too if it
/// moved, so it only ever moves when *every* face it touches is on the
/// reachable side of the crease, the same "never" semantics as a frozen
/// face's vertices (`movable`).
fn geodesic_vertex_distances(
    local: &Local,
    c: V3,
    max: f64,
    pos: impl Fn(u32) -> V3,
) -> HashMap<u32, f64> {
    let frames = face_frames(local, &pos);
    let Some(origin) = nearest_face(c, &frames) else {
        return HashMap::new();
    };
    let adj = face_adjacency(local, &frames);
    let face_dist = face_geodesic_distances(&frames, &adj, origin, c, max);
    let mut worst: HashMap<u32, f64> = HashMap::new();
    let mut blocked: HashSet<u32> = HashSet::new();
    for (i, (_, t)) in local.faces.iter().enumerate() {
        match face_dist.get(&i) {
            Some(&d) => {
                for &v in t {
                    worst.entry(v).and_modify(|best| *best = best.max(d)).or_insert(d);
                }
            }
            // This candidate face wasn't reached (too far, or across the
            // crease): every one of its corners is blocked, however close
            // another of its faces got.
            None => blocked.extend(t),
        }
    }
    for v in blocked {
        worst.remove(&v);
    }
    worst
}

impl StrokeState {
    /// A stroke on `base`, whose faces `index` holds.
    pub fn new(base: &Mesh, index: SculptIndex, params: BrushParams) -> StrokeState {
        let limit = params
            .limit_to
            .as_ref()
            .map(|faces| faces.iter().map(|f| f.0).collect());
        let frozen = params.frozen.iter().map(|f| f.0).collect();
        StrokeState {
            base: base.clone(),
            index,
            params,
            edit: base.edit(),
            drift: 0.0,
            prev_centre: None,
            limit,
            frozen,
            created_faces: Vec::new(),
            centres: Vec::new(),
            base_faces: base.face_slots(),
            origins: HashMap::new(),
            remesh: RemeshStats::default(),
            grab: Vec::new(),
        }
    }

    pub fn params(&self) -> &BrushParams {
        &self.params
    }

    /// Every dab centre applied so far, mirrored ones included.
    pub fn centres(&self) -> &[V3] {
        &self.centres
    }

    /// What dynamic detail has done so far in this stroke.
    pub fn remesh_stats(&self) -> RemeshStats {
        self.remesh
    }

    /// The mesh as stroked so far. Cheap: it shares every chunk with the
    /// stroke's edit.
    pub fn preview_mesh(&self) -> Mesh {
        self.edit.snapshot()
    }

    /// Ends the stroke: the new revision and its provenance (moved
    /// vertices, and faces created or removed by remeshing).
    pub fn finish(self) -> Result<(Mesh, Provenance), OpError> {
        self.edit.finish().map_err(|e| OpError::Failed {
            code: "invalid_mesh",
            message: e.to_string(),
        })
    }

    /// Checks a sample against the stroke's input revision.
    pub fn check_sample(&self, dab: &DabSample) -> Result<(), OpError> {
        let bad = |why: String| OpError::Failed {
            code: "bad_sample",
            message: why,
        };
        if dab.face.0 >= self.base_faces || self.base.face(dab.face).is_none() {
            return Err(bad(format!(
                "a dab is on face {}, which the model doesn't have (was it removed by an \
                 earlier edit?)",
                dab.face.0
            )));
        }
        if !dab.bary.iter().all(|b| b.is_finite()) || !(0.0..=1.0).contains(&dab.pressure) {
            return Err(bad(format!(
                "a dab's position or pressure is out of range: {:?}, {}",
                dab.bary, dab.pressure
            )));
        }
        Ok(())
    }

    fn pos(&self, v: u32) -> V3 {
        self.edit
            .position(VertexId(v))
            .expect("a live face's corner is a live vertex")
    }

    /// Live faces (with corners) that may have a corner within `r` of `p`
    /// on the current surface, ascending.
    fn gather(&self, p: V3, r: f64) -> Local {
        let mut ids = Vec::new();
        self.index
            .faces_near(p, r + self.index.face_radius() + self.drift, &mut ids);
        ids.extend(self.created_faces.iter().copied());
        ids.sort_unstable();
        ids.dedup();
        let faces: Vec<(FaceId, [u32; 3])> = ids
            .into_iter()
            .filter_map(|f| self.edit.face(f).map(|t| (f, t)))
            .collect();
        let mut incident: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, (_, t)) in faces.iter().enumerate() {
            for &v in t {
                incident.entry(v).or_default().push(i);
            }
        }
        Local { faces, incident }
    }

    fn movable(&self, v: u32, local: &Local) -> bool {
        let Some(inc) = local.incident.get(&v) else {
            return false;
        };
        let faces = || inc.iter().map(|&i| local.faces[i].0.0);
        if faces().any(|f| self.frozen.contains(&f)) {
            return false;
        }
        match &self.limit {
            None => true,
            Some(l) => faces().any(|f| l.contains(&f)),
        }
    }

    fn face_cross(&self, t: [u32; 3]) -> V3 {
        let [a, b, c] = t.map(|v| self.pos(v));
        cross(sub(b, a), sub(c, a))
    }

    /// The dab centre: the input point projected along the view direction
    /// onto the current surface (see the module docs).
    fn centre(&self, base_point: V3) -> V3 {
        let r = self.params.radius;
        let Some(d) = normalized(self.params.view_dir) else {
            return base_point;
        };
        let local = self.gather(base_point, r);
        let mut best: Option<f64> = None;
        for (_, t) in &local.faces {
            let [a, b, c] = t.map(|v| self.pos(v));
            let (e1, e2) = (sub(b, a), sub(c, a));
            let h = cross(d, e2);
            let det = dot(e1, h);
            if det.abs() < 1e-300 {
                continue;
            }
            let s = sub(base_point, a);
            let u = dot(s, h) / det;
            let q = cross(s, e1);
            let w = dot(d, q) / det;
            if u < 0.0 || w < 0.0 || u + w > 1.0 {
                continue;
            }
            let tt = dot(e2, q) / det;
            if tt.abs() <= r + self.drift && best.is_none_or(|b| tt.abs() < b.abs()) {
                best = Some(tt);
            }
        }
        best.map_or(base_point, |t| add(base_point, scale(d, t)))
    }

    /// Grab's region capture (MF-SCULPT-005/Grab fix): the vertices
    /// geodesically connected to the face under `c` within the radius,
    /// always applying "stay on this surface"'s own crease rule (a drag
    /// must never reach past a sharp crease, whatever that option is set
    /// to — this is about containing the grab to one surface, not about
    /// the brush's general falloff shape). Each vertex's falloff weight
    /// is fixed here, from its geodesic distance at capture time, and
    /// never recomputed: later dabs move this same set by the cursor's
    /// displacement, they never re-gather it.
    fn capture_grab_region(&self, c: V3) -> GrabRegion {
        let r = self.params.radius;
        let local = self.gather(c, r);
        let distances = geodesic_vertex_distances(&local, c, r, |v| self.pos(v));
        let vertices: HashMap<u32, (V3, f64)> = distances
            .into_iter()
            .filter(|&(v, d)| d < r && self.movable(v, &local))
            .map(|(v, d)| {
                let w = self.params.falloff.weight(d / r);
                (v, (self.pos(v), w))
            })
            .collect();
        // Every face the captured set could possibly rotate (a corner in
        // the set), with its normal as of right now (stroke start):
        // fixed for the whole stroke's inversion clamp.
        let faces: Vec<([u32; 3], V3)> = local
            .faces
            .iter()
            .filter(|(_, t)| t.iter().any(|v| vertices.contains_key(v)))
            .map(|(_, t)| (*t, self.face_cross(*t)))
            .collect();
        GrabRegion {
            origin: c,
            vertices,
            faces,
        }
    }

    /// Grab's per-dab delta (`BrushKind::Drag` only): each captured
    /// vertex moves from its position *as captured* toward
    /// `captured + total_disp * weight`, where `total_disp` is the
    /// cursor's displacement since the region was captured, projected
    /// onto the view plane (normal to `view`) at the hit depth, and
    /// weighted by `strength × pressure`. Absolute, not incremental: the
    /// same dab replayed twice gives the same delta, and dab N doesn't
    /// compound dab N-1's motion onto a moving base.
    fn grab_delta(&self, grab: &GrabRegion, c: V3, view: V3, pressure: f64) -> Vec<(u32, V3)> {
        let p = &self.params;
        let raw = sub(c, grab.origin);
        // The view-plane projection: drop the component along the view
        // direction, so the drag reads as the cursor's on-screen motion,
        // not a 3D offset that includes depth the user never asked to
        // move along.
        let total = match normalized(view) {
            Some(n) => sub(raw, scale(n, dot(raw, n))),
            None => raw,
        };
        let strength = p.strength * pressure;
        // No inversions: `total` is scaled by one shared factor `t`
        // (binary search) so that every captured face still faces the
        // way it did when the region was captured (stroke start), not
        // just the way it did before this one dab — a long drag must
        // never flip a face relative to where it started, even through
        // many individually-safe-looking steps. `pos_at(v, t)` is the
        // vertex's position with this dab's move scaled by `t`: moved
        // vertices come from `grab.vertices`' captured position plus the
        // scaled total, exactly as the final, unscaled delta will.
        let pos_at = |v: u32, t: f64| match grab.vertices.get(&v) {
            Some(&(captured, w)) => add(captured, scale(total, t * strength * w)),
            None => self.pos(v),
        };
        let normal_at = |t: f64, tri: [u32; 3]| {
            let [a, b, c] = tri.map(|v| pos_at(v, t));
            cross(sub(b, a), sub(c, a))
        };
        let flips = |t: f64| {
            grab.faces
                .iter()
                .any(|&(tri, n0)| dot(normal_at(t, tri), n0) <= 0.0)
        };
        let t = if flips(1.0) {
            let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if flips(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            lo
        } else {
            1.0
        };
        let mut out = Vec::with_capacity(grab.vertices.len());
        let mut verts: Vec<&u32> = grab.vertices.keys().collect();
        verts.sort_unstable();
        for &v in verts {
            if !self.is_live_vertex(v) {
                continue;
            }
            let target = pos_at(v, t);
            let delta = sub(target, self.pos(v));
            if delta != [0.0; 3] {
                out.push((v, delta));
            }
        }
        out
    }

    fn is_live_vertex(&self, v: u32) -> bool {
        self.edit.is_vertex_live(VertexId(v))
    }

    /// The displacement the brush gives each movable vertex within the
    /// radius of `c`, ascending by vertex, from current positions.
    fn evaluate(&self, c: V3, drag: V3, view: V3, pressure: f64) -> (Vec<(u32, V3)>, Local) {
        let p = &self.params;
        let r = p.radius;
        let local = self.gather(c, r);
        // "Stay on this surface": a vertex's distance follows the surface
        // from the face under the dab centre, never crossing a sharp
        // crease (`geodesic_vertex_distances`); off, straight-line
        // distance, as before this option existed.
        let geodesic = p
            .stay_on_surface
            .then(|| geodesic_vertex_distances(&local, c, r, |v| self.pos(v)));
        let mut verts: Vec<u32> = local.incident.keys().copied().collect();
        verts.sort_unstable();
        let within: Vec<(u32, f64)> = verts
            .into_iter()
            .filter_map(|v| match &geodesic {
                Some(distances) => distances.get(&v).filter(|&&d| d < r).map(|&d| (v, d)),
                None => {
                    let d = dot(sub(self.pos(v), c), sub(self.pos(v), c)).sqrt();
                    (d < r).then_some((v, d))
                }
            })
            .collect();
        // The surface under the brush: area-weighted normal and centroid
        // of the candidate faces with a corner inside the radius.
        let inside: HashSet<u32> = within.iter().map(|&(v, _)| v).collect();
        let (mut area_n, mut centroid, mut area) = ([0.0; 3], [0.0; 3], 0.0);
        for (_, t) in &local.faces {
            if t.iter().any(|v| inside.contains(v)) {
                let n = self.face_cross(*t);
                let a = dot(n, n).sqrt();
                area_n = add(area_n, n);
                let [x, y, z] = t.map(|v| self.pos(v));
                centroid = add(centroid, scale(add(add(x, y), z), a / 3.0));
                area += a;
            }
        }
        let brush_n = normalized(area_n)
            .or_else(|| normalized(scale(view, -1.0)))
            .unwrap_or([0.0, 0.0, 1.0]);
        let plane_c = if area > 0.0 {
            scale(centroid, 1.0 / area)
        } else {
            c
        };
        let k = 0.1 * r;
        let mut out = Vec::with_capacity(within.len());
        for (v, d) in within {
            if !self.movable(v, &local) {
                continue;
            }
            let w = p.strength * pressure * p.falloff.weight(d / r);
            if w == 0.0 {
                continue;
            }
            let a = w.min(1.0);
            let pv = self.pos(v);
            let inc = &local.incident[&v];
            let vn = normalized(
                inc.iter()
                    .fold([0.0; 3], |s, &i| add(s, self.face_cross(local.faces[i].1))),
            )
            .unwrap_or(brush_n);
            let tangential = |to: V3| {
                let x = sub(to, pv);
                sub(x, scale(brush_n, dot(x, brush_n)))
            };
            let delta = match p.kind {
                BrushKind::Smooth => {
                    let mut nb: Vec<u32> = inc
                        .iter()
                        .flat_map(|&i| local.faces[i].1)
                        .filter(|&u| u != v)
                        .collect();
                    nb.sort_unstable();
                    nb.dedup();
                    if nb.is_empty() {
                        continue;
                    }
                    let sum = nb.iter().fold([0.0; 3], |s, &u| add(s, self.pos(u)));
                    scale(sub(scale(sum, 1.0 / nb.len() as f64), pv), a)
                }
                BrushKind::Inflate => scale(vn, w * k),
                BrushKind::Deflate => scale(vn, -w * k),
                BrushKind::Flatten => scale(brush_n, a * dot(sub(plane_c, pv), brush_n)),
                BrushKind::Drag => scale(drag, a),
                BrushKind::Pinch => scale(tangential(c), 0.5 * a),
                BrushKind::Crease => {
                    add(scale(tangential(c), 0.5 * a), scale(brush_n, -0.5 * w * k))
                }
                BrushKind::Carve => scale(brush_n, -w * k),
            };
            if delta != [0.0; 3] {
                out.push((v, delta));
            }
        }
        (out, local)
    }

    /// Every live face with a corner in `verts`, deduplicated: the set
    /// whose orientation a batch of vertex moves could possibly flip.
    fn faces_touching(&self, verts: &HashSet<u32>, local: &[&Local]) -> Vec<[u32; 3]> {
        let mut faces: Vec<[u32; 3]> = local
            .iter()
            .flat_map(|l| l.faces.iter())
            .map(|(_, t)| *t)
            .filter(|t| t.iter().any(|v| verts.contains(v)))
            .collect();
        faces.sort_unstable();
        faces.dedup();
        faces
    }

    /// Scales every delta in `deltas` down by the same factor (binary
    /// search between 0 and 1) until no face in `faces` flips: no face's
    /// normal, recomputed with the scaled moves applied, points away
    /// from (dot ≤ 0) its normal at the deltas' current, un-moved
    /// positions. A single shared factor rather than a per-vertex clamp,
    /// so the moved region keeps its shape (just shorter), rather than
    /// warping unevenly near whichever vertex happened to clamp hardest.
    /// No-op (factor 1) if nothing would flip.
    fn clamp_to_avoid_flips(&self, deltas: &mut [(u32, V3)], faces: &[[u32; 3]]) {
        if deltas.is_empty() || faces.is_empty() {
            return;
        }
        let moved: HashMap<u32, V3> = deltas.iter().copied().collect();
        let pos_at = |v: u32, t: f64| {
            let p = self.pos(v);
            match moved.get(&v) {
                Some(&d) => add(p, scale(d, t)),
                None => p,
            }
        };
        let normal_at = |t: f64, tri: [u32; 3]| {
            let [a, b, c] = tri.map(|v| pos_at(v, t));
            cross(sub(b, a), sub(c, a))
        };
        let old: Vec<V3> = faces.iter().map(|&t| normal_at(0.0, t)).collect();
        let flips = |t: f64| {
            faces
                .iter()
                .zip(&old)
                .any(|(&t2, &n0)| dot(normal_at(t, t2), n0) <= 0.0)
        };
        if !flips(1.0) {
            return;
        }
        // Binary search the largest safe `t` in [0, 1]: `flips(0)` is
        // always false (the un-moved mesh's own faces don't flip against
        // themselves), so the search always has a safe lower bound.
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if flips(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        for (_, d) in deltas.iter_mut() {
            *d = scale(*d, lo);
        }
    }

    /// Applies one dab. Cancellable between dabs only: one dab is local
    /// work (milliseconds).
    pub fn apply(&mut self, dab: &DabSample, ctx: &RunContext) -> Result<DabDelta, OpError> {
        ctx.checkpoint()?;
        self.check_sample(dab)?;
        let tri = self.base.face(dab.face).expect("checked live");
        let base_point: V3 = {
            // On the input revision, whatever the stroke has done since.
            let base = tri.map(|v| self.base_position(v));
            std::array::from_fn(|k| {
                dab.bary[0] * base[0][k] + dab.bary[1] * base[1][k] + dab.bary[2] * base[2][k]
            })
        };
        let c = self.centre(base_point);
        let drag = self.prev_centre.map_or([0.0; 3], |p| sub(c, p));
        let view = self.params.view_dir;
        let axes: Vec<u8> = (0..3u8)
            .filter(|&k| self.params.symmetry[k as usize])
            .collect();
        // Every mirror image: bit k of a mask mirrors axis k. Identity
        // first.
        let masks: Vec<u8> = (0..(1u32 << axes.len()))
            .map(|bits| {
                axes.iter()
                    .enumerate()
                    .filter(|(i, _)| bits & (1 << i) != 0)
                    .fold(0u8, |m, (_, &k)| m | (1 << k))
            })
            .collect();
        let pivot = self.params.pivot;
        let is_grab = self.params.kind == BrushKind::Drag;
        if is_grab && self.grab.is_empty() {
            // Grab's region is captured once, from the first dab's
            // centre (every mirror image's own hit face): fixed for the
            // rest of the stroke (see `GrabRegion`'s docs).
            self.grab = masks
                .iter()
                .map(|&m| self.capture_grab_region(mirror_point(c, pivot, m)))
                .collect();
        }
        let mut evals = Vec::with_capacity(masks.len());
        for (i, &m) in masks.iter().enumerate() {
            let cm = mirror_point(c, pivot, m);
            self.centres.push(cm);
            if is_grab {
                let local = self.gather(cm, self.params.radius);
                let delta = self.grab_delta(&self.grab[i], cm, mirror_vec(view, m), dab.pressure);
                evals.push((delta, local));
            } else {
                evals.push(self.evaluate(
                    cm,
                    mirror_vec(drag, m),
                    mirror_vec(view, m),
                    dab.pressure,
                ));
            }
        }
        let mut deltas = self.combine(&masks, &axes, &evals);
        // No inversions (owner report: a large drag tore the surface):
        // clamp this dab's moves, as one shared factor, so no face
        // incident to a moved vertex flips its orientation.
        let moved_verts: HashSet<u32> = deltas.iter().map(|&(v, _)| v).collect();
        let locals: Vec<&Local> = evals.iter().map(|(_, l)| l).collect();
        let faces = self.faces_touching(&moved_verts, &locals);
        self.clamp_to_avoid_flips(&mut deltas, &faces);
        // Apply, and report what changed.
        let mut moved_max = 0.0_f64;
        let mut moved = HashSet::new();
        for &(v, d) in &deltas {
            let p = self.pos(v);
            self.edit
                .move_vertex(VertexId(v), add(p, d))
                .map_err(|e| OpError::Failed {
                    code: "invalid_mesh",
                    message: e.to_string(),
                })?;
            moved_max = moved_max.max(dot(d, d).sqrt());
            moved.insert(v);
        }
        self.drift += moved_max;
        self.prev_centre = Some(c);
        let mut remeshed: Vec<usize> = Vec::new();
        if self.params.detail > 0.0 {
            let n = masks.len();
            let centres: Vec<V3> = self.centres[self.centres.len() - n..].to_vec();
            for cm in centres {
                remeshed.extend(self.remesh_at(cm, ctx)?);
            }
        }
        let mut face_chunks: Vec<usize> = evals
            .iter()
            .flat_map(|(_, l)| l.faces.iter())
            .filter(|(_, t)| t.iter().any(|v| moved.contains(v)))
            .map(|(f, _)| f.0 as usize / CHUNK)
            .chain(remeshed)
            .collect();
        face_chunks.sort_unstable();
        face_chunks.dedup();
        let mut vertex_chunks: Vec<usize> = moved.iter().map(|&v| v as usize / CHUNK).collect();
        vertex_chunks.sort_unstable();
        vertex_chunks.dedup();
        Ok(DabDelta {
            face_chunks,
            vertex_chunks,
        })
    }

    fn base_position(&self, v: u32) -> V3 {
        self.base
            .position(VertexId(v))
            .expect("a face of the input revision has live corners")
    }

    /// Remeshes the faces wholly within the radius of `c` (and movable)
    /// toward `detail × radius`. Returns the face chunks it changed.
    fn remesh_at(&mut self, c: V3, ctx: &RunContext) -> Result<Vec<usize>, OpError> {
        let r = self.params.radius;
        let local = self.gather(c, r);
        let inside = |v: u32| {
            let p = self.pos(v);
            dot(sub(p, c), sub(p, c)) < r * r
        };
        let region: Vec<FaceId> = local
            .faces
            .iter()
            .filter(|(f, t)| {
                t.iter().all(|&v| inside(v))
                    && !self.frozen.contains(&f.0)
                    && self.limit.as_ref().is_none_or(|l| l.contains(&f.0))
            })
            .map(|(f, _)| *f)
            .collect();
        if region.is_empty() {
            return Ok(Vec::new());
        }
        let around: Vec<FaceId> = local.faces.iter().map(|(f, _)| *f).collect();
        let first_new = self.edit.face_slots();
        let mut adj = RegionAdjacency::new(
            &self.edit,
            &region,
            &around,
            self.base_faces,
            &std::mem::take(&mut self.origins),
        );
        let stats = remesh_region(&mut self.edit, &mut adj, self.params.detail * r, ctx)?;
        self.origins = adj.into_origins();
        self.remesh.splits += stats.splits;
        self.remesh.collapses += stats.collapses;
        self.remesh.flips += stats.flips;
        self.remesh.capped |= stats.capped;
        let created: Vec<FaceId> = (first_new..self.edit.face_slots())
            .map(FaceId)
            .filter(|&f| self.edit.face(f).is_some())
            .collect();
        if let Some(l) = &mut self.limit {
            // Faces made from limited faces are limited too.
            l.extend(created.iter().map(|f| f.0));
        }
        let mut chunks: Vec<usize> = region
            .iter()
            .chain(&created)
            .map(|f| f.0 as usize / CHUNK)
            .collect();
        chunks.sort_unstable();
        chunks.dedup();
        self.created_faces.extend(created);
        Ok(chunks)
    }

    /// Each vertex's total displacement from all mirror images of the dab,
    /// ascending by vertex (see the module docs on exact symmetry).
    fn combine(
        &self,
        masks: &[u8],
        axes: &[u8],
        evals: &[(Vec<(u32, V3)>, Local)],
    ) -> Vec<(u32, V3)> {
        if masks.len() == 1 {
            return evals[0].0.clone();
        }
        let pivot = self.params.pivot;
        // Every vertex any image moves.
        let mut union: Vec<u32> = evals
            .iter()
            .flat_map(|(d, _)| d.iter().map(|&(v, _)| v))
            .collect();
        union.sort_unstable();
        union.dedup();
        let at: HashMap<[u64; 3], u32> = union
            .iter()
            .map(|&v| (self.pos(v).map(f64::to_bits), v))
            .collect();
        let partner = |v: u32, axis: u8| -> Option<u32> {
            let m = mirror_point(self.pos(v), pivot, 1 << axis);
            at.get(&m.map(f64::to_bits)).copied()
        };
        // "Stay on this surface"'s geodesic flood fill walks a graph (not
        // a closed-form distance): its distances (and so its falloff
        // weights) aren't guaranteed mirror-exact the way straight-line
        // distance always is, even on an exactly symmetric model (the
        // search order can round a shared floating-point sum a last bit
        // differently from one image to another). The butterfly only
        // trusts the identity image's own numbers and mirrors them
        // algebraically, so trusting it here could silently mirror the
        // *wrong* (slightly off) displacement bit for bit instead of
        // leaving the two images' own, independently-correct ones. Every
        // mirror image is evaluated and summed on its own instead
        // (still correct; just not guaranteed bit-identical between the
        // two sides in this one case). Grab's region capture always walks
        // the same geodesic flood fill (regardless of `stay_on_surface`:
        // see `capture_grab_region`), so it has the same caveat.
        let symmetric = !self.params.stay_on_surface
            && self.params.kind != BrushKind::Drag
            && union
                .iter()
                .all(|&v| axes.iter().all(|&k| partner(v, k).is_some()));
        if symmetric {
            // Butterfly: S_i(v) = S_{i-1}(v) + M_i S_{i-1}(m_i v), from the
            // identity image's displacements. Each step is one addition of
            // a value and its exact mirror, so the result is mirrored
            // bit for bit.
            let slot: HashMap<u32, usize> =
                union.iter().enumerate().map(|(i, &v)| (v, i)).collect();
            let mut s = vec![[0.0; 3]; union.len()];
            for &(v, d) in &evals[0].0 {
                s[slot[&v]] = d;
            }
            for &k in axes {
                let prev = s.clone();
                for (i, &v) in union.iter().enumerate() {
                    let u = partner(v, k).expect("checked symmetric");
                    s[i] = add(prev[i], mirror_vec(prev[slot[&u]], 1 << k));
                }
            }
            union
                .iter()
                .zip(s)
                .filter(|(_, d)| *d != [0.0; 3])
                .map(|(&v, d)| (v, d))
                .collect()
        } else {
            let mut total: HashMap<u32, V3> = HashMap::new();
            for (d, _) in evals {
                for &(v, x) in d {
                    let e = total.entry(v).or_insert([0.0; 3]);
                    *e = add(*e, x);
                }
            }
            union
                .iter()
                .filter_map(|v| total.get(v).map(|&d| (*v, d)))
                .collect()
        }
    }
}
