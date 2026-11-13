//! Selections and their translation between revisions (MF-SEL-001…004).
//!
//! A selection is data: element IDs bound to one revision, never GUI state.
//! It is never applied to another revision without [`resolve`]. Because
//! kept elements keep their IDs across edits (ADR-0001), translating a face
//! selection only needs each step's provenance. Components and boundary
//! loops have revision-scoped indices, so they are converted to stable
//! elements at the source and re-resolved at the destination.

use std::collections::BTreeSet;

use mf_document::{ObjectId, RevisionId};
use mf_geometry::{FaceId, Mesh, Provenance};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    /// The whole object (`ids` is empty).
    Object,
    /// Face IDs.
    Faces,
    /// Component indices, as ordered by the revision's topology.
    Component,
    /// Boundary-loop IDs, as numbered by the revision's topology.
    BoundaryLoop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub object: ObjectId,
    pub revision: RevisionId,
    pub kind: SelectionKind,
    pub ids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// The revisions aren't connected in the history DAG.
    NoPath,
    /// The object doesn't exist at the destination.
    ObjectMissing,
    /// Every selected element was removed (or not yet created).
    AllLost {
        lost: Vec<u32>,
    },
    /// The selected boundary loop was closed by these faces (never empty).
    LoopClosed {
        faces: Vec<FaceId>,
    },
    /// The selected boundary loop is gone because the faces around it were
    /// removed (for example its island was deleted); nothing closed it
    /// (MF-ISSUE-036).
    LoopRemoved,
    KindNotTranslatable,
    /// These IDs don't name live elements of the selection's own revision.
    NotInSource {
        ids: Vec<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Translation {
    Mapped(Selection),
    /// Some elements survived. `lost` lists the IDs (in the source
    /// revision's numbering) that didn't.
    Partial {
        mapped: Selection,
        lost: Vec<u32>,
    },
    /// The elements survived but now belong to more than one candidate.
    Ambiguous {
        candidates: Vec<u32>,
    },
    Failed(TranslateError),
}

/// One edge of the path between two revisions, for one object.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    /// Parent → child: removed elements are lost.
    Forward(&'a Provenance),
    /// Child → parent: elements the child created are lost.
    Backward(&'a Provenance),
}

/// Follow element IDs through `steps`. Each ID is tracked as (source ID,
/// current ID) so a remap (compaction) can renumber it while `lost` still
/// reports source IDs. Returns (current IDs of survivors, lost source IDs),
/// both sorted.
fn follow(
    ids: &[u32],
    steps: &[Step],
    gone: impl Fn(&Step) -> Vec<u32>,
    remap_of: impl Fn(&mf_geometry::SlotRemap) -> Vec<Option<u32>>,
) -> (Vec<u32>, Vec<u32>) {
    let mut alive: Vec<(u32, u32)> = ids.iter().map(|&i| (i, i)).collect();
    alive.sort_unstable();
    alive.dedup();
    let mut lost = BTreeSet::new();
    for step in steps {
        let removed: BTreeSet<u32> = gone(step).into_iter().collect();
        let p = match step {
            Step::Forward(p) | Step::Backward(p) => p,
        };
        // Lists use the pre-remap numbering going forward and the
        // post-remap numbering going backward, so order matters.
        let remap = p.remap.as_ref().map(|r| {
            let forward = remap_of(r);
            match step {
                Step::Forward(_) => forward,
                Step::Backward(_) => {
                    let len = forward
                        .iter()
                        .flatten()
                        .map(|&n| n as usize + 1)
                        .max()
                        // allow-swallow: an empty remap has an empty inverse
                        .unwrap_or(0);
                    let mut inverse = vec![None; len];
                    for (old, new) in forward.iter().enumerate() {
                        if let Some(n) = new {
                            inverse[*n as usize] = Some(old as u32);
                        }
                    }
                    inverse
                }
            }
        });
        let apply_remap = |alive: &mut Vec<(u32, u32)>, lost: &mut BTreeSet<u32>| {
            if let Some(map) = &remap {
                alive.retain_mut(
                    |(src, cur)| match map.get(*cur as usize).copied().flatten() {
                        Some(n) => {
                            *cur = n;
                            true
                        }
                        None => {
                            lost.insert(*src);
                            false
                        }
                    },
                );
            }
        };
        if matches!(step, Step::Backward(_)) {
            apply_remap(&mut alive, &mut lost);
        }
        alive.retain(|(src, cur)| {
            let keep = !removed.contains(cur);
            if !keep {
                lost.insert(*src);
            }
            keep
        });
        if matches!(step, Step::Forward(_)) {
            apply_remap(&mut alive, &mut lost);
        }
    }
    let mut current: Vec<u32> = alive.into_iter().map(|(_, c)| c).collect();
    current.sort_unstable();
    (current, lost.into_iter().collect())
}

/// Face IDs surviving `steps`, and the source IDs lost on the way.
pub fn translate_faces(ids: &[u32], steps: &[Step]) -> (Vec<u32>, Vec<u32>) {
    follow(
        ids,
        steps,
        |s| match s {
            Step::Forward(p) => p.removed_faces.iter().map(|f| f.0).collect(),
            Step::Backward(p) => p.created_faces.iter().map(|(f, _)| f.0).collect(),
        },
        |r| r.faces.iter().map(|f| f.map(|f| f.0)).collect(),
    )
}

fn translate_vertices(ids: &[u32], steps: &[Step]) -> (Vec<u32>, Vec<u32>) {
    follow(
        ids,
        steps,
        |s| match s {
            Step::Forward(p) => p.removed_vertices.iter().map(|v| v.0).collect(),
            Step::Backward(p) => p.created_vertices.iter().map(|v| v.0).collect(),
        },
        |r| r.vertices.iter().map(|v| v.map(|v| v.0)).collect(),
    )
}

fn at(sel: &Selection, to: RevisionId, kind: SelectionKind, ids: Vec<u32>) -> Selection {
    Selection {
        object: sel.object,
        revision: to,
        kind,
        ids,
    }
}

fn outcome(mapped: Selection, lost: Vec<u32>) -> Translation {
    if lost.is_empty() {
        Translation::Mapped(mapped)
    } else {
        Translation::Partial { mapped, lost }
    }
}

/// Translate `sel` (on `from_mesh`) to revision `to` (with `to_mesh`) along
/// `steps`, the per-edit provenance between them in path order.
pub fn resolve(
    sel: &Selection,
    from_mesh: &Mesh,
    to_mesh: &Mesh,
    steps: &[Step],
    to: RevisionId,
) -> Translation {
    match sel.kind {
        SelectionKind::Object => {
            Translation::Mapped(at(sel, to, SelectionKind::Object, Vec::new()))
        }
        SelectionKind::Faces => {
            let missing: Vec<u32> = sel
                .ids
                .iter()
                .copied()
                .filter(|&f| !from_mesh.is_face_live(FaceId(f)))
                .collect();
            if !missing.is_empty() {
                return Translation::Failed(TranslateError::NotInSource { ids: missing });
            }
            let (mapped, lost) = translate_faces(&sel.ids, steps);
            if mapped.is_empty() {
                return Translation::Failed(TranslateError::AllLost { lost });
            }
            outcome(at(sel, to, SelectionKind::Faces, mapped), lost)
        }
        SelectionKind::Component => resolve_components(sel, from_mesh, to_mesh, steps, to),
        SelectionKind::BoundaryLoop => resolve_loops(sel, from_mesh, to_mesh, steps, to),
    }
}

/// How one selected component or loop resolved at the destination.
enum Part {
    One(u32),
    Partial(u32, Vec<u32>),
    Many(Vec<u32>),
    Gone(TranslateError),
}

/// Combine per-element results: any ambiguity makes the whole selection
/// ambiguous; elements that are gone are reported as lost; if everything
/// is gone, the first reason is returned.
fn combine(
    sel: &Selection,
    to: RevisionId,
    kind: SelectionKind,
    parts: Vec<(u32, Part)>,
) -> Translation {
    let mut mapped = BTreeSet::new();
    let mut lost = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    let mut first_failure = None;
    for (source, part) in parts {
        match part {
            Part::One(id) => {
                mapped.insert(id);
            }
            Part::Partial(id, l) => {
                mapped.insert(id);
                lost.extend(l);
            }
            Part::Many(c) => ambiguous.extend(c),
            Part::Gone(e) => {
                lost.insert(source);
                first_failure.get_or_insert(e);
            }
        }
    }
    if !ambiguous.is_empty() {
        return Translation::Ambiguous {
            candidates: ambiguous.into_iter().collect(),
        };
    }
    if mapped.is_empty() {
        return Translation::Failed(first_failure.unwrap_or(TranslateError::AllLost {
            lost: lost.into_iter().collect(),
        }));
    }
    outcome(
        at(sel, to, kind, mapped.into_iter().collect()),
        lost.into_iter().collect(),
    )
}

fn resolve_components(
    sel: &Selection,
    from_mesh: &Mesh,
    to_mesh: &Mesh,
    steps: &[Step],
    to: RevisionId,
) -> Translation {
    let from_topo = from_mesh.topology();
    let to_topo = to_mesh.topology();
    let mut component_of = vec![u32::MAX; to_mesh.face_slots() as usize];
    for (i, comp) in to_topo.components.iter().enumerate() {
        for &f in comp {
            component_of[f as usize] = i as u32;
        }
    }
    let mut parts = Vec::new();
    for &c in &sel.ids {
        let Some(faces) = from_topo.components.get(c as usize) else {
            return Translation::Failed(TranslateError::NotInSource { ids: vec![c] });
        };
        let (mapped, _) = translate_faces(faces, steps);
        let candidates: BTreeSet<u32> = mapped
            .iter()
            .filter_map(|&f| {
                component_of
                    .get(f as usize)
                    .copied()
                    .filter(|&c| c != u32::MAX)
            })
            .collect();
        let part = match candidates.len() {
            0 => Part::Gone(TranslateError::AllLost { lost: vec![c] }),
            1 => Part::One(*candidates.first().expect("exactly one candidate")),
            _ => Part::Many(candidates.into_iter().collect()),
        };
        parts.push((c, part));
    }
    combine(sel, to, SelectionKind::Component, parts)
}

/// Follow one vertex ID through `steps`; `None` if it was removed.
fn follow_vertex(v: u32, steps: &[Step]) -> Option<u32> {
    let (alive, _) = translate_vertices(&[v], steps);
    alive.first().copied()
}

fn directed_edges(vertices: &[u32]) -> Vec<(u32, u32)> {
    let n = vertices.len();
    (0..n)
        .map(|k| (vertices[k], vertices[(k + 1) % n]))
        .collect()
}

/// Loops are matched on directed boundary edges, not shared vertices: at a
/// pinch vertex several loops share a vertex but never an edge (review I1).
fn resolve_loops(
    sel: &Selection,
    from_mesh: &Mesh,
    to_mesh: &Mesh,
    steps: &[Step],
    to: RevisionId,
) -> Translation {
    let from_topo = from_mesh.topology();
    let to_topo = to_mesh.topology();
    let mut loop_of_edge: Vec<((u32, u32), u32)> = to_topo
        .boundary_loops
        .iter()
        .flat_map(|l| {
            directed_edges(&l.vertices)
                .into_iter()
                .map(move |e| (e, l.id))
        })
        .collect();
    loop_of_edge.sort_unstable();
    let edges_of = |id: u32| -> BTreeSet<(u32, u32)> {
        to_topo
            .boundary_loops
            .iter()
            .find(|l| l.id == id)
            .map(|l| directed_edges(&l.vertices).into_iter().collect())
            .expect("candidate loops come from this topology")
    };
    let mut parts = Vec::new();
    for &l in &sel.ids {
        let Some(source) = from_topo.boundary_loops.iter().find(|b| b.id == l) else {
            return Translation::Failed(TranslateError::NotInSource { ids: vec![l] });
        };
        let mut lost_vertices = Vec::new();
        let mut mapped_edges = BTreeSet::new();
        for (a, b) in directed_edges(&source.vertices) {
            match (follow_vertex(a, steps), follow_vertex(b, steps)) {
                (Some(x), Some(y)) => {
                    mapped_edges.insert((x, y));
                }
                (x, y) => {
                    if x.is_none() {
                        lost_vertices.push(a);
                    }
                    if y.is_none() {
                        lost_vertices.push(b);
                    }
                }
            }
        }
        lost_vertices.sort_unstable();
        lost_vertices.dedup();
        let candidates: BTreeSet<u32> = mapped_edges
            .iter()
            .filter_map(|e| {
                loop_of_edge
                    .binary_search_by_key(e, |(edge, _)| *edge)
                    // allow-swallow: binary_search's Err means 'not found', not a failure
                    .ok()
                    .map(|i| loop_of_edge[i].1)
            })
            .collect();
        let part = match candidates.len() {
            0 => {
                // The loop no longer exists. If created faces use its
                // surviving vertices, they closed it: report them. If none
                // do, its surroundings were removed instead (MF-ISSUE-036).
                let wanted: BTreeSet<u32> =
                    mapped_edges.iter().flat_map(|&(a, b)| [a, b]).collect();
                let mut faces: Vec<FaceId> = steps
                    .iter()
                    .filter_map(|s| match s {
                        Step::Forward(p) => Some(p),
                        Step::Backward(_) => None,
                    })
                    .flat_map(|p| p.created_faces.iter().map(|(f, _)| *f))
                    .filter(|&f| {
                        to_mesh
                            .face(f)
                            .is_some_and(|t| t.iter().any(|v| wanted.contains(v)))
                    })
                    .collect();
                faces.sort_unstable();
                faces.dedup();
                if faces.is_empty() {
                    Part::Gone(TranslateError::LoopRemoved)
                } else {
                    Part::Gone(TranslateError::LoopClosed { faces })
                }
            }
            1 => {
                let id = *candidates.first().expect("exactly one candidate");
                if edges_of(id) == mapped_edges && lost_vertices.is_empty() {
                    Part::One(id)
                } else {
                    // The loop changed shape; report the removed vertices.
                    Part::Partial(id, lost_vertices)
                }
            }
            _ => Part::Many(candidates.into_iter().collect()),
        };
        parts.push((l, part));
    }
    combine(sel, to, SelectionKind::BoundaryLoop, parts)
}
