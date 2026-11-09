//! Background work. Every O(n) step (import, operations, analysis, export)
//! runs on a worker thread and reports back over a channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use glam::DMat4;
use mf_command::{CommandError, Completed, ObjectId, Prepared};
use mf_geometry::{Diagnostics, Mesh};

/// Analysis results for one mesh revision.
pub struct Analysis {
    pub diagnostics: Diagnostics,
    pub vertices: Arc<MeshVertices>,
}
use mf_io::{Format, ImportedModel, IoError};
use mf_render::MeshVertices;
use mf_selection::SelectionKind;

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
    /// What a click picked (see [`pick`]), for the revision it was made on.
    Picked {
        object: ObjectId,
        mesh: Mesh,
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

/// Diagnostics and render vertices for one revision (O(n), so off the UI thread).
pub fn analyze(mesh: Mesh) -> Message {
    let analysis = Analysis {
        diagnostics: mf_geometry::inspect(&mesh),
        vertices: Arc::new(MeshVertices::build(&mesh)),
    };
    Message::Analyzed { mesh, analysis }
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

/// Ray-cast pick on a worker (O(n) over faces, across every object).
/// What a click picked: a visible boundary loop near the cursor on any
/// object (ties go to the first target), else the nearest face in world
/// space across every object. All O(n) work (topology, ray casts) happens
/// here, never on the UI thread (MF-RT-001). `active` (and its mesh, which
/// must be one of `targets`) is reported when the click hits nothing, so a
/// miss still clears the right object's selection.
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
        let loop_hit = targets.iter().find_map(|t| {
            mf_render::pick_loop(&t.mesh, &t.model, &cam, origin, size, px, 8.0)
                .map(|id| (t, (SelectionKind::BoundaryLoop, id)))
        });
        let hit = loop_hit.or_else(|| {
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
                .map(|(t, picked, _)| (t, picked))
        });
        match hit {
            Some((target, picked)) => Message::Picked {
                object: target.object,
                mesh: target.mesh.clone(),
                picked: Some(picked),
                add,
            },
            None => Message::Picked {
                object: active,
                mesh: targets
                    .iter()
                    .find(|t| t.object == active)
                    .map_or_else(Mesh::empty, |t| t.mesh.clone()),
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
