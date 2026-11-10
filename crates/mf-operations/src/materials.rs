//! Carrying per-face materials through every operation (MF-ISSUE-091, UI
//! plan §11B), from provenance alone, so no operation has to know about
//! them:
//!
//! - a **created face with source faces** takes the majority material of
//!   its sources: faces of the object's input (`Sources::faces`) or of any
//!   operand (`Sources::operands`; booleans, joins and connectors keep each
//!   part's own material). An operand's palette entries are merged into
//!   the output's by equality;
//! - a **created face with no source faces** (a hole fill, which names only
//!   the loop) takes, per connected patch of such faces, the majority
//!   material of the faces around the patch;
//! - kept faces keep theirs (the edit shares their storage).
//!
//! Ties go to the lowest palette index; "no material" loses every tie.
//! Faces an operation gave a material itself keep it. Deterministic, so
//! replay reproduces the result, and the hashes include it.

use std::collections::{BTreeMap, HashMap};

use mf_geometry::{FaceId, Material, Mesh, NO_MATERIAL, Provenance};
use mf_runtime::{Cancelled, RunContext};

/// The material `face` of `mesh` prints with, as a palette entry.
fn effective(mesh: &Mesh, face: FaceId) -> Option<&Material> {
    mesh.face_material(face)
        .and_then(|m| mesh.materials().get(m as usize))
}

/// The index of `m` in `palette`, appending it if new.
fn index_of(palette: &mut Vec<Material>, m: &Material) -> u32 {
    match palette.iter().position(|p| p == m) {
        Some(i) => i as u32,
        None => {
            palette.push(m.clone());
            (palette.len() - 1) as u32
        }
    }
}

/// The most common value; ties to the lowest (so [`NO_MATERIAL`] loses).
fn majority(votes: impl IntoIterator<Item = u32>) -> Option<u32> {
    let mut count: BTreeMap<u32, usize> = BTreeMap::new();
    for v in votes {
        *count.entry(v).or_default() += 1;
    }
    let best = count.values().copied().max()?;
    count.into_iter().find(|&(_, n)| n == best).map(|(v, _)| v)
}

/// `mesh` (an operation's output) with materials inherited through
/// `provenance` (see the module docs). `own` is the mesh `Sources::faces`
/// name (the changed object's input; operand 0's for a created object);
/// `operands` are every operand's input, by index.
pub fn inherit(
    mesh: Mesh,
    provenance: &Provenance,
    own: Option<&Mesh>,
    operands: &[&Mesh],
    ctx: &RunContext,
) -> Result<Mesh, Cancelled> {
    let any = mesh.has_materials()
        || own.is_some_and(Mesh::has_materials)
        || operands.iter().any(|m| m.has_materials());
    if !any || provenance.created_faces.is_empty() {
        return Ok(mesh);
    }
    let mut palette: Vec<Material> = mesh.materials().to_vec();
    let mut set: Vec<(FaceId, u32)> = Vec::new();
    let mut sourceless: Vec<FaceId> = Vec::new();
    let mut tick = ctx.ticker();
    for (f, sources) in &provenance.created_faces {
        tick.tick()?;
        if mesh.face(*f).is_none() || mesh.face_material_override(*f) != NO_MATERIAL {
            continue;
        }
        let mut votes = Vec::new();
        for &g in &sources.faces {
            if let Some(m) = own.and_then(|o| effective(o, g)) {
                votes.push(index_of(&mut palette, m));
            } else {
                votes.push(NO_MATERIAL);
            }
        }
        for &(k, g) in &sources.operands {
            match operands.get(k as usize).and_then(|o| effective(o, g)) {
                Some(m) => votes.push(index_of(&mut palette, m)),
                None => votes.push(NO_MATERIAL),
            }
        }
        match majority(votes) {
            Some(m) if m != NO_MATERIAL => set.push((*f, m)),
            Some(_) => {}
            None => sourceless.push(*f),
        }
    }
    let mut out = if set.is_empty() {
        mesh
    } else {
        mesh.with_face_materials(palette.clone(), &set)
    };
    if !sourceless.is_empty() {
        let fill = patch_majority(&out, &sourceless, ctx)?;
        if !fill.is_empty() {
            out = out.with_face_materials(palette, &fill);
        }
    }
    Ok(out)
}

/// For each connected patch of `faces` (sharing edges), the majority
/// material of the other faces sharing an edge with it.
fn patch_majority(
    mesh: &Mesh,
    faces: &[FaceId],
    ctx: &RunContext,
) -> Result<Vec<(FaceId, u32)>, Cancelled> {
    let inside: std::collections::HashSet<FaceId> = faces.iter().copied().collect();
    // Edge → faces, for the patch's edges and every face touching them.
    let corners: std::collections::HashSet<u32> = faces
        .iter()
        .filter_map(|&f| mesh.face(f))
        .flatten()
        .collect();
    let mut edges: HashMap<(u32, u32), Vec<FaceId>> = HashMap::new();
    let mut tick = ctx.ticker();
    for (f, t) in mesh.faces() {
        tick.tick()?;
        if !t.iter().any(|v| corners.contains(v)) {
            continue;
        }
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            edges.entry((a.min(b), a.max(b))).or_default().push(f);
        }
    }
    let neighbours = |f: FaceId| -> Vec<FaceId> {
        let Some(t) = mesh.face(f) else {
            return Vec::new();
        };
        let mut n: Vec<FaceId> = (0..3)
            .flat_map(|i| {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                edges[&(a.min(b), a.max(b))].iter().copied()
            })
            .filter(|&g| g != f)
            .collect();
        n.sort_unstable();
        n.dedup();
        n
    };
    let mut sorted: Vec<FaceId> = faces.to_vec();
    sorted.sort_unstable();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &start in &sorted {
        if !seen.insert(start) {
            continue;
        }
        let mut patch = vec![start];
        let mut votes = Vec::new();
        let mut i = 0;
        while i < patch.len() {
            for g in neighbours(patch[i]) {
                if inside.contains(&g) {
                    if seen.insert(g) {
                        patch.push(g);
                    }
                } else {
                    votes.push(mesh.face_material(g).unwrap_or(NO_MATERIAL));
                }
            }
            i += 1;
        }
        if let Some(m) = majority(votes)
            && m != NO_MATERIAL
        {
            out.extend(patch.into_iter().map(|f| (f, m)));
        }
    }
    Ok(out)
}
