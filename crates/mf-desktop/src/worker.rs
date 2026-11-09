//! Background work. Every O(n) step (import, operations, analysis, export)
//! runs on a worker thread and reports back over a channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use glam::DMat4;
use mf_command::{CommandError, Completed, ObjectId, Prepared};
use mf_geometry::{Aabb, Diagnostics, Mesh};
use mf_io::{Format, ImportedModel, IoError};
use mf_render::{
    LineVertex, MeshVertices, SurfaceVertex, edge_line_vertices, face_surface_vertices,
    loop_line_vertices,
};
use mf_selection::SelectionKind;

use crate::defects::{Defect, Defects};

/// Analysis results for one mesh revision.
pub struct Analysis {
    pub diagnostics: Diagnostics,
    pub vertices: Arc<MeshVertices>,
    /// The tree's defect categories (docs/design/visual-language.md).
    pub defects: Defects,
    /// Defect overlays in the 3D view, relative to `vertices.origin`.
    pub layers: DefectLayers,
}

/// Vertex data for the defects the 3D view colours (theme.rs): cracks and
/// tangled edges as lines, loose pieces as faces. Holes are the mesh's own
/// boundary outlines.
pub struct DefectLayers {
    pub cracks: Arc<Vec<LineVertex>>,
    pub tangled: Arc<Vec<LineVertex>>,
    pub loose: Arc<Vec<SurfaceVertex>>,
}

/// What an overlay outlines or tints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayTarget {
    Loops(Vec<u32>),
    Components(Vec<u32>),
    Faces(Vec<u32>),
    Edges(Vec<[u32; 2]>),
}

/// What an overlay is for (each purpose has one slot in the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OverlayPurpose {
    Selection,
    Hover,
    RecentRepair,
}

/// An overlay's vertices (relative to the mesh's local origin), bounds and
/// outward direction (object space; `None` when the faces around it face
/// every way, like a closed piece).
pub struct Overlay {
    pub lines: Arc<Vec<LineVertex>>,
    pub faces: Arc<Vec<SurfaceVertex>>,
    pub bounds: Option<Aabb>,
    pub normal: Option<glam::DVec3>,
}

pub enum Message {
    Imported(Result<ImportedModel, IoError>),
    Completed(Box<Result<Completed, CommandError>>),
    Analyzed {
        mesh: Mesh,
        analysis: Analysis,
    },
    /// Render vertices for an inactive object (MF-ISSUE-042). The active
    /// object's vertices come from `Analyzed` instead.
    ObjectVertices {
        object: ObjectId,
        mesh: Mesh,
        vertices: Arc<MeshVertices>,
    },
    Exported(Result<PathBuf, IoError>),
    Replayed(Result<Vec<String>, String>),
    /// An overlay built for `target` on `object`'s revision `mesh`.
    Overlay {
        purpose: OverlayPurpose,
        object: ObjectId,
        mesh: Mesh,
        target: OverlayTarget,
        overlay: Overlay,
    },
    /// What a click picked (see [`pick`]), for the revision it was made on.
    /// `mesh` is `None` only when the click's `active` object had no mesh to
    /// pick against in the first place (for example a non-mesh geometry
    /// kind); a miss against a real mesh still carries it, so a plain click
    /// that hits nothing can clear that object's selection.
    Picked {
        object: ObjectId,
        mesh: Option<Mesh>,
        picked: Option<(SelectionKind, u32)>,
        add: bool,
    },
}

fn spawn(
    tx: &Sender<Message>,
    ctx: &eframe::egui::Context,
    f: impl FnOnce() -> Message + Send + 'static,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The receiver only disappears when the app is closing.
        // allow-swallow: the receiver only disappears while the app is closing
        let _ = tx.send(f());
        ctx.request_repaint();
    });
}

pub fn import(tx: &Sender<Message>, ctx: &eframe::egui::Context, path: PathBuf) {
    spawn(tx, ctx, move || Message::Imported(mf_io::read_model(&path)));
}

pub fn run(tx: &Sender<Message>, ctx: &eframe::egui::Context, prepared: Prepared) {
    spawn(tx, ctx, move || {
        Message::Completed(Box::new(prepared.run()))
    });
}

/// The largest hole the fill operation can take (its `max_loop_edges` limit).
fn fill_cap() -> usize {
    let spec = mf_spec::builtin::get("mesh.fill_holes").expect("fill_holes is built in");
    spec.parameters
        .iter()
        .find_map(|p| match (&p.name[..], &p.kind) {
            ("max_loop_edges", mf_spec::ParamKind::Integer { max: Some(max), .. }) => {
                Some(*max as usize)
            }
            _ => None,
        })
        .expect("fill_holes declares a maximum max_loop_edges")
}

/// Diagnostics, defects and render vertices for one revision (O(n), so off
/// the UI thread).
pub fn analyze(mesh: Mesh) -> Message {
    let diagnostics = mf_geometry::inspect(&mesh);
    let vertices = Arc::new(MeshVertices::build(&mesh));
    let defects = crate::defects::classify(&mesh, &diagnostics, fill_cap());
    let origin = vertices.origin;
    let crack_ids: Vec<u32> = defects
        .cracks
        .iter()
        .filter_map(|d| d.selection().map(|(_, id)| id))
        .collect();
    let tangled: Vec<[u32; 2]> = defects
        .tangled
        .iter()
        .filter_map(|d| match d {
            Defect::Tangled { edge, .. } => Some(*edge),
            _ => None,
        })
        .collect();
    let loose: Vec<u32> = mesh
        .topology()
        .components
        .iter()
        .skip(1)
        .flatten()
        .copied()
        .collect();
    let layers = DefectLayers {
        cracks: Arc::new(loop_line_vertices(&mesh, origin, &crack_ids)),
        tangled: Arc::new(edge_line_vertices(&mesh, origin, &tangled)),
        loose: Arc::new(face_surface_vertices(&mesh, origin, &loose)),
    };
    let analysis = Analysis {
        diagnostics,
        vertices,
        defects,
        layers,
    };
    Message::Analyzed { mesh, analysis }
}

/// Build an overlay on a worker: O(target) to O(n) (a large component, or
/// finding a loop's vertices), so never on the UI thread (MF-RT-001).
pub fn build_overlay(
    tx: &Sender<Message>,
    ctx: &eframe::egui::Context,
    purpose: OverlayPurpose,
    object: ObjectId,
    mesh: Mesh,
    target: OverlayTarget,
) {
    spawn(tx, ctx, move || {
        let origin = crate::app::mesh_origin(&mesh);
        let topo = mesh.topology();
        let (mut lines, mut faces, mut points) = (Vec::new(), Vec::new(), Vec::new());
        // The faces whose normals say which way the target faces.
        let around: Vec<u32> = match &target {
            OverlayTarget::Loops(ids) => {
                lines = loop_line_vertices(&mesh, origin, ids);
                let mut edges = Vec::new();
                for l in topo.boundary_loops.iter().filter(|l| ids.contains(&l.id)) {
                    points.extend(l.vertices.iter().copied());
                    let n = l.vertices.len();
                    edges.extend((0..n).map(|k| (l.vertices[k], l.vertices[(k + 1) % n])));
                }
                faces_on_edges(&mesh, edges)
            }
            OverlayTarget::Components(ids) => {
                let ids: Vec<u32> = ids
                    .iter()
                    .filter_map(|&c| topo.components.get(c as usize))
                    .flatten()
                    .copied()
                    .collect();
                faces = face_surface_vertices(&mesh, origin, &ids);
                points.extend(face_corners(&mesh, &ids));
                ids
            }
            OverlayTarget::Faces(ids) => {
                faces = face_surface_vertices(&mesh, origin, ids);
                points.extend(face_corners(&mesh, ids));
                ids.clone()
            }
            OverlayTarget::Edges(edges) => {
                lines = edge_line_vertices(&mesh, origin, edges);
                points.extend(edges.iter().flatten().copied());
                faces_on_edges(&mesh, edges.iter().map(|e| (e[0], e[1])).collect())
            }
        };
        let normal = facing(&mesh, &around);
        let bounds =
            points
                .iter()
                .map(|&v| mesh.corner_position(v))
                .fold(None, |acc: Option<Aabb>, p| {
                    Some(match acc {
                        None => Aabb { min: p, max: p },
                        Some(b) => Aabb {
                            min: [0, 1, 2].map(|k| b.min[k].min(p[k])),
                            max: [0, 1, 2].map(|k| b.max[k].max(p[k])),
                        },
                    })
                });
        Message::Overlay {
            purpose,
            object,
            mesh: mesh.clone(),
            target,
            overlay: Overlay {
                lines: Arc::new(lines),
                faces: Arc::new(faces),
                bounds,
                normal,
            },
        }
    });
}

/// Live faces with an edge among `edges` (either direction). O(n).
fn faces_on_edges(mesh: &Mesh, edges: Vec<(u32, u32)>) -> Vec<u32> {
    let mut keys: Vec<(u32, u32)> = edges
        .into_iter()
        .map(|(a, b)| (a.min(b), a.max(b)))
        .collect();
    keys.sort_unstable();
    mesh.faces()
        .filter(|(_, t)| {
            (0..3).any(|k| {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                keys.binary_search(&(a.min(b), a.max(b))).is_ok()
            })
        })
        .map(|(f, _)| f.0)
        .collect()
}

/// The area-weighted mean normal of `faces`, if they mostly face one way.
fn facing(mesh: &Mesh, faces: &[u32]) -> Option<glam::DVec3> {
    let mut sum = glam::DVec3::ZERO;
    let mut area = 0.0;
    for &f in faces {
        let Some(t) = mesh.face(mf_geometry::FaceId(f)) else {
            continue;
        };
        let [a, b, c] = t.map(|v| glam::DVec3::from(mesh.corner_position(v)));
        let n = (b - a).cross(c - a);
        sum += n;
        area += n.length();
    }
    // Faces that face every way (a closed piece) cancel out.
    (area > 0.0 && sum.length() > 0.2 * area).then(|| sum.normalize())
}

/// The corner vertices of the live faces among `faces`.
fn face_corners<'a>(mesh: &'a Mesh, faces: &'a [u32]) -> impl Iterator<Item = u32> + 'a {
    faces
        .iter()
        .filter_map(|&f| mesh.face(mf_geometry::FaceId(f)))
        .flatten()
}

/// Render vertices only, for an object that isn't active (MF-ISSUE-042): it
/// still needs to be drawn at its transform, but not analysed.
pub fn build_vertices(
    tx: &Sender<Message>,
    ctx: &eframe::egui::Context,
    object: ObjectId,
    mesh: Mesh,
) {
    spawn(tx, ctx, move || {
        let vertices = Arc::new(MeshVertices::build(&mesh));
        Message::ObjectVertices {
            object,
            mesh,
            vertices,
        }
    });
}

/// Replay a command log off the UI thread (it re-imports source files).
pub fn replay(tx: &Sender<Message>, ctx: &eframe::egui::Context, dir: PathBuf) {
    spawn(tx, ctx, move || {
        Message::Replayed(
            mf_command::replay(&dir)
                .map(|report| {
                    let mut lines: Vec<String> = report
                        .results
                        .iter()
                        .map(|(id, v)| format!("command {}: {v:?}", id.0))
                        .collect();
                    lines.push(if report.all_verified() {
                        format!("All {} command(s) verified.", report.results.len())
                    } else {
                        "Replay did NOT reproduce the log.".into()
                    });
                    lines
                })
                .map_err(|e| e.to_string()),
        )
    });
}

/// One object a click can land on: its mesh and object-to-world transform
/// (MF-ISSUE-042).
pub struct PickTarget {
    pub object: ObjectId,
    pub mesh: Mesh,
    pub model: DMat4,
}

/// What `pick_targets` found: which target (if any) and what on it.
struct PickHit<'a> {
    target: &'a PickTarget,
    picked: (SelectionKind, u32),
}

/// Ray-cast pick across every object (O(n) over faces, O(objects) ray casts
/// per candidate). A boundary loop wins over a face, matching single-object
/// picking; among loop edges within tolerance on any object, the nearest
/// one on screen wins, and it must be visible past *every* object's faces
/// (not just its own mesh's), so a click on a front object's face can't
/// select a hidden loop on an object behind it (MF-ISSUE-042). Pure
/// function (no I/O), so it's directly testable.
fn pick_targets<'a>(
    targets: &'a [PickTarget],
    cam: &mf_render::OrbitCamera,
    origin: glam::DVec3,
    size: (f64, f64),
    px: (f64, f64),
) -> Option<PickHit<'a>> {
    let eye = cam.eye();
    let mut candidates: Vec<(&PickTarget, mf_render::LoopEdgeCandidate)> = targets
        .iter()
        .flat_map(|t| {
            mf_render::loop_edge_candidates(&t.mesh, &t.model, cam, origin, size, px, 8.0)
                .into_iter()
                .map(move |c| (t, c))
        })
        .collect();
    candidates.sort_by(|(_, a), (_, b)| a.screen_distance.total_cmp(&b.screen_distance));
    let loop_hit = candidates.into_iter().find(|(_, c)| {
        !targets.iter().any(|other| {
            mf_render::edge_occluded(&other.mesh, &other.model, eye, c.point, c.endpoints)
        })
    });
    if let Some((target, c)) = loop_hit {
        return Some(PickHit {
            target,
            picked: (SelectionKind::BoundaryLoop, c.loop_id),
        });
    }

    let (ray_origin, dir) = cam.ray(origin, size, px);
    targets
        .iter()
        .filter_map(|t| {
            let (f, dist) = mf_render::ray_pick(&t.mesh, &t.model, ray_origin, dir)?;
            let component = t
                .mesh
                .topology()
                .components
                .iter()
                .position(|c| c.contains(&f.0))?;
            Some((t, (SelectionKind::Component, component as u32), dist))
        })
        .min_by(|(.., a), (.., b)| a.total_cmp(b))
        .map(|(target, picked, _)| PickHit { target, picked })
}

/// Ray-cast pick on a worker (O(n) over faces, across every object). All
/// O(n) work (topology, ray casts) happens here, never on the UI thread
/// (MF-RT-001). `active` is reported when the click hits nothing, so a
/// miss still clears the right object's selection; its mesh comes from
/// `targets` if it has one there (an active object with no mesh, such as a
/// non-mesh geometry kind, reports no mesh rather than a fabricated one).
#[allow(clippy::too_many_arguments)]
pub fn pick(
    tx: &Sender<Message>,
    ctx: &eframe::egui::Context,
    targets: Vec<PickTarget>,
    active: ObjectId,
    cam: mf_render::OrbitCamera,
    origin: glam::DVec3,
    size: (f64, f64),
    px: (f64, f64),
    add: bool,
) {
    spawn(tx, ctx, move || {
        match pick_targets(&targets, &cam, origin, size, px) {
            Some(hit) => Message::Picked {
                object: hit.target.object,
                mesh: Some(hit.target.mesh.clone()),
                picked: Some(hit.picked),
                add,
            },
            None => Message::Picked {
                object: active,
                mesh: targets
                    .iter()
                    .find(|t| t.object == active)
                    .map(|t| t.mesh.clone()),
                picked: None,
                add,
            },
        }
    });
}

/// What to export: one mesh, or a whole 3MF model.
pub enum ExportJob {
    Mesh(Mesh, Format),
    Model(mf_io::threemf::Model),
}

pub fn export(tx: &Sender<Message>, ctx: &eframe::egui::Context, job: ExportJob, path: PathBuf) {
    spawn(tx, ctx, move || {
        let result = match &job {
            ExportJob::Mesh(mesh, format) => mf_io::write_path(mesh, &path, *format),
            ExportJob::Model(model) => mf_io::write_model_path(model, &path),
        };
        Message::Exported(result.map(|()| path))
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use glam::DVec3;
    use mf_fixtures::{cube, cube_with_missing_face};
    use mf_render::{OrbitCamera, screen_position};

    /// Looking almost straight down (a tiny tilt off vertical to avoid the
    /// view-matrix singularity), framing both objects below.
    fn overhead_camera(target: DVec3, distance: f64) -> OrbitCamera {
        OrbitCamera {
            target,
            distance,
            yaw: 0.0,
            pitch: 89f64.to_radians(),
            fov_y: 40f64.to_radians(),
            radius: distance,
        }
    }

    /// A click near a loop edge that another object's solid face hides
    /// must not select that loop (MF-ISSUE-042 review I3): the occlusion
    /// check must cover every target, not just the loop's own mesh.
    #[test]
    fn loop_hidden_by_another_objects_face_is_not_picked() {
        // Back object: a cube missing its top face, open at z=1 over x,y in [0,1].
        let back = cube_with_missing_face();
        let back_target = PickTarget {
            object: ObjectId(1),
            mesh: back.clone(),
            model: DMat4::IDENTITY,
        };
        // Front object: a solid cube directly above, z in [2,3], same XY
        // footprint, so viewed from overhead it covers the hole below it.
        let front = cube(1.0);
        let front_target = PickTarget {
            object: ObjectId(2),
            mesh: front,
            model: DMat4::from_translation(DVec3::new(0.0, 0.0, 2.0)),
        };
        let targets = vec![back_target, front_target];

        let cam = overhead_camera(DVec3::new(0.5, 0.5, 1.5), 20.0);
        let origin = DVec3::ZERO;
        let size = (800.0, 600.0);

        let topo = back.topology();
        let hole = &topo.boundary_loops[0];
        let a = DVec3::from(back.corner_position(hole.vertices[0]));
        let b = DVec3::from(back.corner_position(hole.vertices[1]));
        let mid = screen_position(&cam, origin, size, ((a + b) * 0.5).to_array())
            .expect("the hole's edge projects on screen");

        let hit = pick_targets(&targets, &cam, origin, size, mid);
        match hit {
            Some(h) if h.picked.0 == SelectionKind::BoundaryLoop => {
                panic!(
                    "picked loop {:?} on object {:?}, but the front cube's face hides it",
                    h.picked, h.target.object
                );
            }
            _ => {}
        }
    }

    /// Without another object in front, the same click does find the loop:
    /// confirms the geometry above is set up to actually land on the hole's
    /// edge, so the occlusion test is about occlusion, not a missed click.
    #[test]
    fn same_loop_is_picked_when_nothing_hides_it() {
        let back = cube_with_missing_face();
        let targets = vec![PickTarget {
            object: ObjectId(1),
            mesh: back.clone(),
            model: DMat4::IDENTITY,
        }];
        let cam = overhead_camera(DVec3::new(0.5, 0.5, 1.5), 20.0);
        let origin = DVec3::ZERO;
        let size = (800.0, 600.0);
        let topo = back.topology();
        let hole = &topo.boundary_loops[0];
        let a = DVec3::from(back.corner_position(hole.vertices[0]));
        let b = DVec3::from(back.corner_position(hole.vertices[1]));
        let mid = screen_position(&cam, origin, size, ((a + b) * 0.5).to_array()).unwrap();

        let hit = pick_targets(&targets, &cam, origin, size, mid);
        assert!(
            matches!(hit, Some(h) if h.picked == (SelectionKind::BoundaryLoop, hole.id)),
            "expected the hole's loop with nothing in front of it"
        );
    }
}
