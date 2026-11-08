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
use mf_geometry::{FaceId, Mesh, Provenance, VertexId};
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
    /// The selected boundary loop was closed by these faces.
    LoopClosed {
        faces: Vec<FaceId>,
    },
    KindNotTranslatable,
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

/// Face IDs surviving `steps`, and those lost on the way (both sorted).
pub fn translate_faces(ids: &[u32], steps: &[Step]) -> (Vec<u32>, Vec<u32>) {
    let mut alive: BTreeSet<u32> = ids.iter().copied().collect();
    let mut lost = BTreeSet::new();
    for step in steps {
        let gone: Vec<u32> = match step {
            Step::Forward(p) => p.removed_faces.iter().map(|f| f.0).collect(),
            Step::Backward(p) => p.created_faces.iter().map(|(f, _)| f.0).collect(),
        };
        for f in gone {
            if alive.remove(&f) {
                lost.insert(f);
            }
        }
    }
    (alive.into_iter().collect(), lost.into_iter().collect())
}

fn translate_vertices(ids: &[u32], steps: &[Step]) -> (Vec<u32>, Vec<u32>) {
    let mut alive: BTreeSet<u32> = ids.iter().copied().collect();
    let mut lost = BTreeSet::new();
    for step in steps {
        let gone: Vec<VertexId> = match step {
            Step::Forward(p) => p.removed_vertices.clone(),
            Step::Backward(p) => p.created_vertices.clone(),
        };
        for v in gone {
            if alive.remove(&v.0) {
                lost.insert(v.0);
            }
        }
    }
    (alive.into_iter().collect(), lost.into_iter().collect())
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

fn resolve_components(
    sel: &Selection,
    from_mesh: &Mesh,
    to_mesh: &Mesh,
    steps: &[Step],
    to: RevisionId,
) -> Translation {
    let from_topo = from_mesh.topology();
    let mut faces = Vec::new();
    for &c in &sel.ids {
        match from_topo.components.get(c as usize) {
            Some(comp) => faces.extend_from_slice(comp),
            None => return Translation::Failed(TranslateError::KindNotTranslatable),
        }
    }
    let (mapped, _) = translate_faces(&faces, steps);
    if mapped.is_empty() {
        return Translation::Failed(TranslateError::AllLost {
            lost: sel.ids.clone(),
        });
    }
    let to_topo = to_mesh.topology();
    let mut component_of = vec![u32::MAX; to_mesh.face_slots() as usize];
    for (i, comp) in to_topo.components.iter().enumerate() {
        for &f in comp {
            component_of[f as usize] = i as u32;
        }
    }
    let candidates: BTreeSet<u32> = mapped
        .iter()
        .filter_map(|&f| {
            component_of
                .get(f as usize)
                .copied()
                .filter(|&c| c != u32::MAX)
        })
        .collect();
    match candidates.len() {
        0 => Translation::Failed(TranslateError::AllLost {
            lost: sel.ids.clone(),
        }),
        1 => Translation::Mapped(at(
            sel,
            to,
            SelectionKind::Component,
            candidates.into_iter().collect(),
        )),
        _ => Translation::Ambiguous {
            candidates: candidates.into_iter().collect(),
        },
    }
}

fn resolve_loops(
    sel: &Selection,
    from_mesh: &Mesh,
    to_mesh: &Mesh,
    steps: &[Step],
    to: RevisionId,
) -> Translation {
    let from_topo = from_mesh.topology();
    let mut vertices = Vec::new();
    for &l in &sel.ids {
        match from_topo.boundary_loops.iter().find(|b| b.id == l) {
            Some(b) => vertices.extend_from_slice(&b.vertices),
            None => return Translation::Failed(TranslateError::KindNotTranslatable),
        }
    }
    let (alive, lost) = translate_vertices(&vertices, steps);
    let wanted: BTreeSet<u32> = alive.iter().copied().collect();
    let to_topo = to_mesh.topology();
    let candidates: Vec<u32> = to_topo
        .boundary_loops
        .iter()
        .filter(|b| b.vertices.iter().any(|v| wanted.contains(v)))
        .map(|b| b.id)
        .collect();
    match candidates.as_slice() {
        [] => {
            // The loop no longer exists: report the created faces that use
            // its vertices, i.e. the faces that closed it.
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
            Translation::Failed(TranslateError::LoopClosed { faces })
        }
        [one] => {
            let same = to_topo
                .boundary_loops
                .iter()
                .find(|b| b.id == *one)
                .is_some_and(|b| b.vertices.iter().copied().collect::<BTreeSet<_>>() == wanted);
            let mapped = at(sel, to, SelectionKind::BoundaryLoop, vec![*one]);
            if same && lost.is_empty() {
                Translation::Mapped(mapped)
            } else {
                // The loop changed shape; `lost` names removed loop vertices.
                Translation::Partial { mapped, lost }
            }
        }
        many => Translation::Ambiguous {
            candidates: many.to_vec(),
        },
    }
}
