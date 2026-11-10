//! Local dynamic remeshing (MF-SCULPT-002, Phase 1 Task 18): detail where
//! it's needed, under the brush, and nothing outside the footprint.
//!
//! [`remesh_region`] works on a set of faces (the region) of a
//! [`MeshEdit`]:
//! - it **splits** edges longer than 4/3 of the target length at their
//!   midpoint;
//! - it **collapses** edges shorter than 4/5 of it to their midpoint, when
//!   the link condition holds, no face turns over and no edge grows past
//!   4/3;
//! - it **flips** edges where that brings vertex valences closer to 6,
//!   when the two new faces face the same way as the old ones.
//!
//! Only edges whose two faces are both in the region change, and only
//! vertices all of whose faces are in the region move or go. So every face
//! outside the region keeps its id, corners and positions bit for bit.
//! Created faces name the input faces they came from, so provenance stays
//! total. Every choice is made in a fixed order (by length, then by
//! vertex ids), so the result is deterministic.
//!
//! [`RegionAdjacency`] is built on demand from the faces the caller
//! gathered (the region plus the faces around it): no global topology is
//! rebuilt per dab. P1-M5's blend reuses [`remesh_band`] (D13).

use std::collections::{BTreeSet, HashMap, HashSet};

use mf_geometry::{FaceId, Mesh, MeshEdit, Provenance, Sources, VertexId};
use mf_runtime::{Cancelled, RunContext};

use crate::OpError;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
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
fn mid(a: V3, b: V3) -> V3 {
    std::array::from_fn(|k| (a[k] + b[k]) * 0.5)
}
fn len(a: V3, b: V3) -> f64 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}

/// What a remesh did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RemeshStats {
    pub splits: usize,
    pub collapses: usize,
    pub flips: usize,
    /// True if it stopped at its operation budget with work left.
    pub capped: bool,
}

/// The faces a remesh may change (the region) and the faces around them,
/// with vertex-to-face incidence, built from an edit on demand.
pub struct RegionAdjacency {
    /// Vertex → its live faces (region and around), ascending.
    vf: HashMap<u32, Vec<FaceId>>,
    region: BTreeSet<FaceId>,
    /// Faces created by this remesh → the input faces they came from.
    origin: HashMap<FaceId, Vec<FaceId>>,
    /// Faces of the input revision (ids below this) are their own origin.
    base_faces: u32,
}

impl RegionAdjacency {
    /// `region`: the faces that may change. `around`: every other live
    /// face that has a corner in common with a region face (more is fine).
    /// Faces of `edit` with ids at or above `base_faces` were created by
    /// the caller's edit; `origins` gives their input faces.
    pub fn new(
        edit: &MeshEdit,
        region: &[FaceId],
        around: &[FaceId],
        base_faces: u32,
        origins: &HashMap<FaceId, Vec<FaceId>>,
    ) -> RegionAdjacency {
        let mut vf: HashMap<u32, Vec<FaceId>> = HashMap::new();
        let mut all: Vec<FaceId> = region.iter().chain(around).copied().collect();
        all.sort_unstable();
        all.dedup();
        for f in all {
            if let Some(t) = edit.face(f) {
                for v in t {
                    vf.entry(v).or_default().push(f);
                }
            }
        }
        RegionAdjacency {
            vf,
            region: region
                .iter()
                .copied()
                .filter(|&f| edit.face(f).is_some())
                .collect(),
            origin: origins.clone(),
            base_faces,
        }
    }

    /// The created-face origins, to carry into the next remesh of the same
    /// edit.
    pub fn into_origins(self) -> HashMap<FaceId, Vec<FaceId>> {
        self.origin
    }

    /// The faces now in the region, ascending.
    pub fn region(&self) -> impl Iterator<Item = FaceId> + '_ {
        self.region.iter().copied()
    }

    /// Input faces a face came from.
    pub fn origin_of(&self, f: FaceId) -> Vec<FaceId> {
        if f.0 < self.base_faces {
            vec![f]
        } else {
            self.origin
                .get(&f)
                .cloned()
                .expect("every face a remesh creates has its origin recorded")
        }
    }

    fn origins(&self, faces: &[FaceId]) -> Vec<FaceId> {
        let mut o: Vec<FaceId> = faces.iter().flat_map(|&f| self.origin_of(f)).collect();
        o.sort_unstable();
        o.dedup();
        o
    }

    fn faces_of(&self, v: u32) -> &[FaceId] {
        self.vf.get(&v).map_or(&[], Vec::as_slice)
    }

    /// Faces using both `a` and `b`.
    fn edge_faces(&self, a: u32, b: u32) -> Vec<FaceId> {
        let fb = self.faces_of(b);
        self.faces_of(a)
            .iter()
            .copied()
            .filter(|f| fb.contains(f))
            .collect()
    }

    /// True if every face of `v` is in the region.
    fn free(&self, v: u32) -> bool {
        self.faces_of(v).iter().all(|f| self.region.contains(f))
    }

    fn valence(&self, v: u32) -> usize {
        self.faces_of(v).len()
    }

    fn neighbours(&self, v: u32, edit: &MeshEdit) -> BTreeSet<u32> {
        self.faces_of(v)
            .iter()
            .filter_map(|&f| edit.face(f))
            .flatten()
            .filter(|&u| u != v)
            .collect()
    }

    fn unlink(&mut self, f: FaceId, t: [u32; 3]) {
        for v in t {
            if let Some(l) = self.vf.get_mut(&v) {
                l.retain(|&g| g != f);
            }
        }
        self.region.remove(&f);
    }

    fn link(&mut self, f: FaceId, t: [u32; 3]) {
        for v in t {
            let l = self.vf.entry(v).or_default();
            if let Err(i) = l.binary_search(&f) {
                l.insert(i, f);
            }
        }
        self.region.insert(f);
    }

    /// The region's edges whose two faces are both in the region, as
    /// `(a, b)` with `a < b`, ascending.
    fn interior_edges(&self, edit: &MeshEdit) -> Vec<(u32, u32)> {
        let mut out = BTreeSet::new();
        for &f in &self.region {
            let Some(t) = edit.face(f) else { continue };
            for i in 0..3 {
                let (a, b) = (t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3]));
                let ef = self.edge_faces(a, b);
                if ef.len() == 2 && ef.iter().all(|g| self.region.contains(g)) {
                    out.insert((a, b));
                }
            }
        }
        out.into_iter().collect()
    }
}

fn pos(edit: &MeshEdit, v: u32) -> V3 {
    edit.position(VertexId(v))
        .expect("a live face's corner is a live vertex")
}

/// `t` rotated so it starts at `a`.
fn rotate_to(t: [u32; 3], a: u32) -> [u32; 3] {
    match t.iter().position(|&x| x == a) {
        Some(0) => t,
        Some(1) => [t[1], t[2], t[0]],
        _ => [t[2], t[0], t[1]],
    }
}

fn normal(edit: &MeshEdit, t: [u32; 3]) -> V3 {
    let [a, b, c] = t.map(|v| pos(edit, v));
    cross(sub(b, a), sub(c, a))
}

/// Triangle shape quality, 1 for equilateral and 0 for a sliver:
/// `4√3 · area / Σ edge²`.
fn quality(p: [V3; 3]) -> f64 {
    let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
    let area = 0.5 * dot(n, n).sqrt();
    let e2: f64 = (0..3)
        .map(|i| {
            let d = sub(p[(i + 1) % 3], p[i]);
            dot(d, d)
        })
        .sum();
    if e2 > 0.0 {
        4.0 * 3f64.sqrt() * area / e2
    } else {
        0.0
    }
}

/// No flip or collapse may leave a face worse than this.
const MIN_QUALITY: f64 = 0.1;

fn err(e: mf_geometry::GeometryError) -> Cancelled {
    // MeshEdit only fails here on a programming error (a dead face or
    // vertex), which the adjacency rules out.
    panic!("remesh made an invalid edit: {e}")
}

/// Remeshes the region of `edit` toward edges of `target_edge` (see the
/// module docs). At most `16 × region + 4096` operations per call.
pub fn remesh_region(
    edit: &mut MeshEdit,
    region: &mut RegionAdjacency,
    target_edge: f64,
    ctx: &RunContext,
) -> Result<RemeshStats, Cancelled> {
    let mut stats = RemeshStats::default();
    if !(target_edge > 0.0 && target_edge.is_finite()) {
        return Ok(stats);
    }
    let (hi, lo) = (target_edge * 4.0 / 3.0, target_edge * 4.0 / 5.0);
    let budget = 16 * region.region.len() + 4096;
    let mut ops = 0;
    for _pass in 0..8 {
        ctx.checkpoint()?;
        let before = stats;
        // Splits, longest first, until no interior edge is long.
        loop {
            let mut long: Vec<(f64, u32, u32)> = region
                .interior_edges(edit)
                .into_iter()
                .map(|(a, b)| (len(pos(edit, a), pos(edit, b)), a, b))
                .filter(|e| e.0 > hi)
                .collect();
            if long.is_empty() {
                break;
            }
            long.sort_by(|x, y| y.0.total_cmp(&x.0).then((x.1, x.2).cmp(&(y.1, y.2))));
            let done = stats.splits;
            for (_, a, b) in long {
                if ops >= budget {
                    stats.capped = true;
                    return Ok(stats);
                }
                if split(edit, region, a, b, hi)? {
                    stats.splits += 1;
                    ops += 1;
                    #[cfg(debug_assertions)]
                    debug_check(edit, region, "split");
                }
            }
            ctx.checkpoint()?;
            if stats.splits == done {
                // The rest can't be split (see `edge_pair`).
                break;
            }
        }
        ctx.checkpoint()?;
        // Collapses, shortest first.
        let mut short: Vec<(f64, u32, u32)> = region
            .interior_edges(edit)
            .into_iter()
            .map(|(a, b)| (len(pos(edit, a), pos(edit, b)), a, b))
            .filter(|e| e.0 < lo)
            .collect();
        short.sort_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
        for (_, a, b) in short {
            if ops >= budget {
                stats.capped = true;
                return Ok(stats);
            }
            if collapse(edit, region, a, b, lo, hi)? {
                stats.collapses += 1;
                ops += 1;
                #[cfg(debug_assertions)]
                debug_check(edit, region, "collapse");
            }
        }
        ctx.checkpoint()?;
        // Flips toward valence 6.
        for (a, b) in region.interior_edges(edit) {
            if ops >= budget {
                stats.capped = true;
                return Ok(stats);
            }
            if flip(edit, region, a, b, hi)? {
                stats.flips += 1;
                ops += 1;
                #[cfg(debug_assertions)]
                debug_check(edit, region, "flip");
            }
        }
        if stats == before {
            break;
        }
    }
    Ok(stats)
}

/// A face and its corners, rotated to start at a given vertex.
type OrientedFace = (FaceId, [u32; 3]);

/// The two faces of interior edge `(a, b)` as `([a, b, c], [b, a, d])`,
/// with their ids, if it still is one.
fn edge_pair(
    edit: &MeshEdit,
    region: &RegionAdjacency,
    a: u32,
    b: u32,
) -> Option<(OrientedFace, OrientedFace)> {
    let ef = region.edge_faces(a, b);
    if ef.len() != 2 || !ef.iter().all(|f| region.region.contains(f)) {
        return None;
    }
    let mut ab = None;
    let mut ba = None;
    for f in ef {
        let t = edit.face(f)?;
        debug_assert!(
            t.contains(&a) && t.contains(&b),
            "stale adjacency: face {f:?} {t:?} listed for edge {a}-{b}"
        );
        let r = rotate_to(t, a);
        if r[1] == b {
            ab = Some((f, r));
        } else {
            ba = Some((f, rotate_to(t, b)));
        }
    }
    Some((ab?, ba?))
}

#[cfg(debug_assertions)]
fn debug_check(edit: &MeshEdit, region: &RegionAdjacency, what: &str) {
    if std::env::var_os("MF_REMESH_DEBUG").is_none() {
        return;
    }
    for f in region.region() {
        let t = edit.face(f).expect("region faces are live");
        let n = normal(edit, t);
        assert!(
            dot(n, n) > 0.0 && quality(t.map(|v| pos(edit, v))) > 1e-3,
            "{what} left face {f:?} {t:?} with no area: {:?}",
            t.map(|v| pos(edit, v))
        );
        assert!(
            t[0] != t[1] && t[1] != t[2] && t[0] != t[2],
            "{what}: {t:?}"
        );
    }
}

fn add_face(
    edit: &mut MeshEdit,
    region: &mut RegionAdjacency,
    t: [u32; 3],
    from: &[FaceId],
) -> Result<FaceId, Cancelled> {
    let sources = region.origins(from);
    let f = edit
        .add_face(
            t,
            Sources {
                faces: sources.clone(),
                ..Sources::default()
            },
        )
        .map_err(err)?;
    region.origin.insert(f, sources);
    region.link(f, t);
    Ok(f)
}

fn split(
    edit: &mut MeshEdit,
    region: &mut RegionAdjacency,
    a: u32,
    b: u32,
    hi: f64,
) -> Result<bool, Cancelled> {
    let Some(((f1, [_, _, c]), (f2, [_, _, d]))) = edge_pair(edit, region, a, b) else {
        return Ok(false);
    };
    let (pa, pb) = (pos(edit, a), pos(edit, b));
    if len(pa, pb) <= hi {
        return Ok(false);
    }
    // Next to the region's fixed rim, splitting can fan out into slivers:
    // skip a split that would leave a face worse than both MIN_QUALITY
    // and the face it came from.
    let pm = mid(pa, pb);
    let (pc, pd) = (pos(edit, c), pos(edit, d));
    let parent = quality([pa, pb, pc]).min(quality([pb, pa, pd]));
    let child = [
        quality([pa, pm, pc]),
        quality([pm, pb, pc]),
        quality([pb, pm, pd]),
        quality([pm, pa, pd]),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    if child < MIN_QUALITY && child < parent {
        return Ok(false);
    }
    let m = edit.add_vertex(pm).0;
    region.unlink(f1, [a, b, c]);
    region.unlink(f2, [b, a, d]);
    edit.remove_face(f1).map_err(err)?;
    edit.remove_face(f2).map_err(err)?;
    add_face(edit, region, [a, m, c], &[f1])?;
    add_face(edit, region, [m, b, c], &[f1])?;
    add_face(edit, region, [b, m, d], &[f2])?;
    add_face(edit, region, [m, a, d], &[f2])?;
    Ok(true)
}

fn collapse(
    edit: &mut MeshEdit,
    region: &mut RegionAdjacency,
    a: u32,
    b: u32,
    lo: f64,
    hi: f64,
) -> Result<bool, Cancelled> {
    let Some(((f1, [_, _, c]), (f2, [_, _, d]))) = edge_pair(edit, region, a, b) else {
        return Ok(false);
    };
    let (pa, pb) = (pos(edit, a), pos(edit, b));
    if len(pa, pb) >= lo || !region.free(a) || !region.free(b) {
        return Ok(false);
    }
    // Link condition: the only common neighbours are the two opposite
    // corners; and they keep at least three faces.
    let common: Vec<u32> = region
        .neighbours(a, edit)
        .intersection(&region.neighbours(b, edit))
        .copied()
        .collect();
    if common.len() != 2
        || !common.contains(&c)
        || !common.contains(&d)
        || region.valence(c) <= 3
        || region.valence(d) <= 3
    {
        return Ok(false);
    }
    let p = mid(pa, pb);
    // Every surviving face of a or b, with b replaced by a and a at p:
    // none may turn over or get an edge longer than `hi`.
    let mut changed: Vec<(FaceId, [u32; 3], [u32; 3])> = Vec::new();
    let mut faces: Vec<FaceId> = region
        .faces_of(a)
        .iter()
        .chain(region.faces_of(b))
        .copied()
        .filter(|&f| f != f1 && f != f2)
        .collect();
    faces.sort_unstable();
    faces.dedup();
    for f in faces {
        let Some(t) = edit.face(f) else { continue };
        let nt = t.map(|v| if v == b { a } else { v });
        let at = |v: u32| if v == a { p } else { pos(edit, v) };
        let [x, y, z] = nt.map(at);
        let n_new = cross(sub(y, x), sub(z, x));
        if dot(n_new, normal(edit, t)) <= 0.0
            || [len(x, y), len(y, z), len(z, x)].iter().any(|&l| l > hi)
            || quality([x, y, z]) < MIN_QUALITY
        {
            return Ok(false);
        }
        changed.push((f, t, nt));
    }
    region.unlink(f1, [a, b, c]);
    region.unlink(f2, [b, a, d]);
    edit.remove_face(f1).map_err(err)?;
    edit.remove_face(f2).map_err(err)?;
    for (f, t, nt) in changed {
        if t != nt {
            region.unlink(f, t);
            edit.set_face(f, nt).map_err(err)?;
            region.link(f, nt);
        }
    }
    edit.move_vertex(VertexId(a), p).map_err(err)?;
    edit.remove_vertex(VertexId(b)).map_err(err)?;
    region.vf.remove(&b);
    Ok(true)
}

fn flip(
    edit: &mut MeshEdit,
    region: &mut RegionAdjacency,
    a: u32,
    b: u32,
    hi: f64,
) -> Result<bool, Cancelled> {
    let Some(((f1, [_, _, c]), (f2, [_, _, d]))) = edge_pair(edit, region, a, b) else {
        return Ok(false);
    };
    if c == d || !region.edge_faces(c, d).is_empty() || len(pos(edit, c), pos(edit, d)) > hi {
        return Ok(false);
    }
    let dev = |v: u32, delta: isize| (region.valence(v) as isize + delta - 6).unsigned_abs();
    let before = dev(a, 0) + dev(b, 0) + dev(c, 0) + dev(d, 0);
    let after = dev(a, -1) + dev(b, -1) + dev(c, 1) + dev(d, 1);
    if after >= before || region.valence(a) <= 3 || region.valence(b) <= 3 {
        return Ok(false);
    }
    let (t1, t2) = ([a, d, c], [d, b, c]);
    let old = {
        let (n1, n2) = (normal(edit, [a, b, c]), normal(edit, [b, a, d]));
        [n1[0] + n2[0], n1[1] + n2[1], n1[2] + n2[2]]
    };
    let (n1, n2) = (normal(edit, t1), normal(edit, t2));
    let q = |t: [u32; 3]| quality(t.map(|v| pos(edit, v)));
    let new_q = q(t1).min(q(t2));
    if dot(n1, old) <= 0.0
        || dot(n2, old) <= 0.0
        || dot(n1, n2) <= 0.0
        || new_q < MIN_QUALITY.min(q([a, b, c]).min(q([b, a, d])))
    {
        return Ok(false);
    }
    region.unlink(f1, [a, b, c]);
    region.unlink(f2, [b, a, d]);
    edit.set_face(f1, t1).map_err(err)?;
    edit.set_face(f2, t2).map_err(err)?;
    region.link(f1, t1);
    region.link(f2, t2);
    Ok(true)
}

/// Remeshes `faces` of `mesh` toward `target_edge` as a new revision (for
/// P1-M5's blend, D13). Finds the faces around the band with one pass over
/// the mesh.
pub fn remesh_band(
    mesh: &Mesh,
    faces: &[FaceId],
    target_edge: f64,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance), OpError> {
    let corners: HashSet<u32> = faces
        .iter()
        .filter_map(|&f| mesh.face(f))
        .flatten()
        .collect();
    let mut tick = ctx.ticker();
    let mut around = Vec::new();
    for (f, t) in mesh.faces() {
        tick.tick()?;
        if t.iter().any(|v| corners.contains(v)) {
            around.push(f);
        }
    }
    let mut edit = mesh.edit();
    let mut region =
        RegionAdjacency::new(&edit, faces, &around, mesh.face_slots(), &HashMap::new());
    remesh_region(&mut edit, &mut region, target_edge, ctx)?;
    edit.finish().map_err(|e| OpError::Failed {
        code: "invalid_mesh",
        message: e.to_string(),
    })
}
