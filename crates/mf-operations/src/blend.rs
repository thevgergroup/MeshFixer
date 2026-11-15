//! The seam blend of a join (MF-MODEL-004, D13, D14, Phase 1 Task 22):
//! after a union, the band of vertices within `blend_width` (along edges)
//! of the seam is remeshed to the seam's edge length ([`remesh_band`]) and
//! faired smooth (Δ² = 0, [`fair_region`]) against the two rings around
//! it, then the result is validated (D4). Nothing outside the band moves.
//!
//! Each step's provenance is composed onto the union's, so the command's
//! provenance is still relative to the target's input and total.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use mf_geometry::{Bvh, FaceId, Mesh, Provenance, Sources, VertexId};

use crate::refusal::RefusalArg;
use crate::sculpt::remesh_band;
use crate::{FairStyle, OpError, RunContext, fair_region};

/// Builds the [`OpError::Refused`] for `code` (ADR-0010, through the one
/// registry helper), with `detail` as the `detail` argument its
/// `specs/operations/mesh.boolean.yaml` message fills in (`mesh.blend`
/// has no spec of its own: it runs only inside `mesh.boolean`'s union,
/// so its codes are declared there).
fn refuse(code: &'static str, detail: &str) -> OpError {
    crate::refusal::refuse(
        code,
        std::collections::BTreeMap::from([("detail", RefusalArg::Text(detail.to_owned()))]),
    )
}

/// `second` (from `first`'s output) composed onto `first` (from a base):
/// the provenance from the base to `second`'s output.
pub fn compose(first: &Provenance, second: &Provenance) -> Provenance {
    assert!(
        first.remap.is_none() && second.remap.is_none(),
        "compaction isn't composed"
    );
    let created1: BTreeMap<FaceId, &Sources> =
        first.created_faces.iter().map(|(f, s)| (*f, s)).collect();
    let removed2: HashSet<FaceId> = second.removed_faces.iter().copied().collect();
    let mut created: Vec<(FaceId, Sources)> = first
        .created_faces
        .iter()
        .filter(|(f, _)| !removed2.contains(f))
        .cloned()
        .collect();
    for (f, s) in &second.created_faces {
        let mut out = Sources {
            faces: Vec::new(),
            loops: s.loops.clone(),
            operands: s.operands.clone(),
        };
        for g in &s.faces {
            match created1.get(g) {
                Some(src) => {
                    out.faces.extend(src.faces.iter().copied());
                    out.loops.extend(src.loops.iter().copied());
                    out.operands.extend(src.operands.iter().copied());
                }
                None => out.faces.push(*g),
            }
        }
        out.faces.sort_unstable();
        out.faces.dedup();
        out.loops.sort_unstable();
        out.loops.dedup();
        out.operands.sort_unstable();
        out.operands.dedup();
        created.push((*f, out));
    }
    created.sort_by_key(|(f, _)| *f);
    let mut removed: Vec<FaceId> = first.removed_faces.clone();
    removed.extend(
        second
            .removed_faces
            .iter()
            .filter(|f| !created1.contains_key(f)),
    );
    removed.sort_unstable();
    removed.dedup();
    let mut modified: Vec<FaceId> = first
        .modified_faces
        .iter()
        .chain(
            second
                .modified_faces
                .iter()
                .filter(|f| !created1.contains_key(f)),
        )
        .copied()
        .filter(|f| !removed2.contains(f))
        .collect();
    modified.sort_unstable();
    modified.dedup();
    let created_v1: HashSet<VertexId> = first.created_vertices.iter().copied().collect();
    let removed_v2: HashSet<VertexId> = second.removed_vertices.iter().copied().collect();
    let mut created_v: Vec<VertexId> = first
        .created_vertices
        .iter()
        .chain(&second.created_vertices)
        .copied()
        .filter(|v| !removed_v2.contains(v))
        .collect();
    created_v.sort_unstable();
    created_v.dedup();
    let mut removed_v: Vec<VertexId> = first
        .removed_vertices
        .iter()
        .chain(
            second
                .removed_vertices
                .iter()
                .filter(|v| !created_v1.contains(v)),
        )
        .copied()
        .collect();
    removed_v.sort_unstable();
    removed_v.dedup();
    let created_set: HashSet<VertexId> = created_v.iter().copied().collect();
    let mut moved: Vec<VertexId> = first
        .moved_vertices
        .iter()
        .chain(&second.moved_vertices)
        .copied()
        .filter(|v| !created_set.contains(v) && !removed_v2.contains(v))
        .collect();
    moved.sort_unstable();
    moved.dedup();
    Provenance {
        removed_faces: removed,
        created_faces: created,
        modified_faces: modified,
        removed_vertices: removed_v,
        created_vertices: created_v,
        moved_vertices: moved,
        remap: None,
    }
}

/// The seam of a union's output: vertices shared by a face that came from
/// the tool (operand `tool`) and a face that didn't.
fn seam(mesh: &Mesh, prov: &Provenance, tool: u32, ctx: &RunContext) -> Result<Vec<u32>, OpError> {
    let tool_faces: HashSet<FaceId> = prov
        .created_faces
        .iter()
        .filter(|(_, s)| s.operands.iter().any(|&(k, _)| k == tool))
        .map(|(f, _)| *f)
        .collect();
    let tool_vertices: HashSet<u32> = tool_faces
        .iter()
        .filter_map(|&f| mesh.face(f))
        .flatten()
        .collect();
    let mut out = HashSet::new();
    let mut tick = ctx.ticker();
    for (f, t) in mesh.faces() {
        tick.tick()?;
        if !tool_faces.contains(&f) {
            out.extend(t.iter().filter(|v| tool_vertices.contains(v)));
        }
    }
    let mut out: Vec<u32> = out.into_iter().collect();
    out.sort_unstable();
    Ok(out)
}

/// The band: every vertex within `width` of the seam along edges, and the
/// faces with every corner in it, ascending.
fn band(mesh: &Mesh, seam: &[u32], width: f64, ctx: &RunContext) -> Result<Vec<FaceId>, OpError> {
    let pos = |v: u32| mesh.corner_position(v);
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &v in seam {
        let p = pos(v);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] - width);
            hi[k] = hi[k].max(p[k] + width);
        }
    }
    let inside = |p: [f64; 3]| (0..3).all(|k| p[k] >= lo[k] && p[k] <= hi[k]);
    // Edge paths are at least as long as straight lines, so the band lies
    // in the seam's box grown by `width`.
    let mut adj: HashMap<u32, Vec<(u32, f64)>> = HashMap::new();
    let mut faces = Vec::new();
    let mut tick = ctx.ticker();
    for (f, t) in mesh.faces() {
        tick.tick()?;
        if t.iter().any(|&v| inside(pos(v))) {
            faces.push((f, t));
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let d = pos(a);
                let e = pos(b);
                let l =
                    ((d[0] - e[0]).powi(2) + (d[1] - e[1]).powi(2) + (d[2] - e[2]).powi(2)).sqrt();
                adj.entry(a).or_default().push((b, l));
                adj.entry(b).or_default().push((a, l));
            }
        }
    }
    let mut dist: HashMap<u32, f64> = HashMap::new();
    let mut heap = BinaryHeap::new();
    for &v in seam {
        dist.insert(v, 0.0);
        heap.push(Reverse((0u64, v)));
    }
    while let Some(Reverse((d_bits, v))) = heap.pop() {
        let d = f64::from_bits(d_bits);
        if dist.get(&v).is_some_and(|&x| x < d) {
            continue;
        }
        for &(u, l) in adj.get(&v).map_or(&[][..], Vec::as_slice) {
            let nd = d + l;
            if nd <= width && dist.get(&u).is_none_or(|&x| nd < x) {
                dist.insert(u, nd);
                heap.push(Reverse((nd.to_bits(), u)));
            }
        }
    }
    let vertices: HashSet<u32> = dist.keys().copied().collect();
    let mut band_faces: Vec<FaceId> = faces
        .iter()
        .filter(|(_, t)| t.iter().all(|v| vertices.contains(v)))
        .map(|(f, _)| *f)
        .collect();
    band_faces.sort_unstable();
    Ok(band_faces)
}

/// Blends a union's seam (see the module docs). `tool` is the tool's
/// operand index in `prov`'s sources.
pub fn blend(
    union: Mesh,
    prov: Provenance,
    tool: u32,
    width: f64,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance), OpError> {
    ctx.progress.report("blending the seam", 0.0);
    let seam = seam(&union, &prov, tool, ctx)?;
    if seam.is_empty() {
        return Err(refuse(
            "no_overlap",
            "These objects don't touch, so there's no seam to blend. Move one onto the other.",
        ));
    }
    let band_faces = band(&union, &seam, width, ctx)?;
    if band_faces.len() >= union.face_count() {
        return Err(refuse(
            "blend_too_wide",
            "The blend would cover the whole model, leaving nothing to blend into. Use a \
             narrower blend.",
        ));
    }
    // The seam's mean edge length.
    let seam_set: HashSet<u32> = seam.iter().copied().collect();
    let (mut total, mut count) = (0.0, 0usize);
    for &f in &band_faces {
        let t = union.face(f).expect("band faces are live");
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if a < b && seam_set.contains(&a) && seam_set.contains(&b) {
                let (p, q) = (union.corner_position(a), union.corner_position(b));
                total +=
                    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt();
                count += 1;
            }
        }
    }
    if count == 0 {
        return Err(refuse(
            "blend_too_narrow",
            "The blend is narrower than the faces at the seam. Use a wider blend.",
        ));
    }
    let target_edge = total / count as f64;
    ctx.progress.report("blending the seam", 0.3);
    let (remeshed, p2) = remesh_band(&union, &band_faces, target_edge, ctx)?;
    // The band after remeshing: its surviving faces and the new ones.
    let removed: HashSet<FaceId> = p2.removed_faces.iter().copied().collect();
    let region: HashSet<FaceId> = band_faces
        .iter()
        .copied()
        .filter(|f| !removed.contains(f))
        .chain(p2.created_faces.iter().map(|(f, _)| *f))
        .filter(|&f| remeshed.face(f).is_some())
        .collect();
    let region_vertices: HashSet<u32> = region
        .iter()
        .filter_map(|&f| remeshed.face(f))
        .flatten()
        .collect();
    // Vertices on the band's rim (with a face outside it) stay put.
    let mut rim: HashSet<u32> = HashSet::new();
    let mut tick = ctx.ticker();
    for (f, t) in remeshed.faces() {
        tick.tick()?;
        if !region.contains(&f) {
            rim.extend(t.iter().filter(|v| region_vertices.contains(v)));
        }
    }
    let mut free: Vec<VertexId> = region_vertices
        .iter()
        .filter(|v| !rim.contains(v))
        .map(|&v| VertexId(v))
        .collect();
    free.sort_unstable();
    ctx.progress.report("blending the seam", 0.6);
    let (faired, p3) = match fair_region(&remeshed, &free, FairStyle::Smooth, ctx)? {
        Ok(r) => r,
        Err(e) => {
            return Err(refuse(
                "blend_did_not_converge",
                &format!("The blend couldn't be smoothed ({e}). Try a different blend width."),
            ));
        }
    };
    ctx.progress.report("checking the blend", 0.8);
    let total_prov = compose(&compose(&prov, &p2), &p3);
    let bvh = Bvh::build(&faired, ctx)?;
    let mut changed: Vec<FaceId> = region.into_iter().collect();
    changed.sort_unstable();
    // A target may be open elsewhere (a scan with holes, D15): its own
    // open edges are allowed, any new one isn't.
    let open: HashSet<(u32, u32)> = union
        .topology_with(ctx)?
        .boundary_loops
        .iter()
        .flat_map(|l| {
            let n = l.vertices.len();
            (0..n).map(move |k| {
                let (a, b) = (l.vertices[k], l.vertices[(k + 1) % n]);
                (a.min(b), a.max(b))
            })
        })
        .collect();
    let allow_open = |a: u32, b: u32| open.contains(&(a.min(b), a.max(b)));
    if let Err(defect) =
        mf_geometry::validate_solid_with(&faired, &changed, &bvh, &allow_open, ctx)?
    {
        return Err(refuse(
            "blend_self_intersects",
            &format!("The blend folds over itself ({defect}). Try a narrower blend."),
        ));
    }
    ctx.progress.report("checking the blend", 1.0);
    Ok((faired, total_prov))
}
