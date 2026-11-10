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

/// One stroke in progress. See the module docs.
pub struct StrokeState<'a> {
    /// The stroke's input revision.
    base: Mesh,
    index: &'a SculptIndex,
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
}

/// A vertex's surroundings for one dab evaluation.
struct Local {
    /// Candidate faces (live, ascending id) and their current corners.
    faces: Vec<(FaceId, [u32; 3])>,
    /// Vertex → indices into `faces` of its incident candidate faces.
    incident: HashMap<u32, Vec<usize>>,
}

impl<'a> StrokeState<'a> {
    /// A stroke on `base`, whose faces `index` holds.
    pub fn new(base: &Mesh, index: &'a SculptIndex, params: BrushParams) -> StrokeState<'a> {
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

    /// The displacement the brush gives each movable vertex within the
    /// radius of `c`, ascending by vertex, from current positions.
    fn evaluate(&self, c: V3, drag: V3, view: V3, pressure: f64) -> (Vec<(u32, V3)>, Local) {
        let p = &self.params;
        let r = p.radius;
        let local = self.gather(c, r);
        let mut verts: Vec<u32> = local.incident.keys().copied().collect();
        verts.sort_unstable();
        let within: Vec<(u32, f64)> = verts
            .into_iter()
            .filter_map(|v| {
                let d = dot(sub(self.pos(v), c), sub(self.pos(v), c)).sqrt();
                (d < r).then_some((v, d))
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
        let mut evals = Vec::with_capacity(masks.len());
        for &m in &masks {
            let cm = mirror_point(c, pivot, m);
            self.centres.push(cm);
            evals.push(self.evaluate(cm, mirror_vec(drag, m), mirror_vec(view, m), dab.pressure));
        }
        let deltas = self.combine(&masks, &axes, &evals);
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
        let symmetric = union
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
