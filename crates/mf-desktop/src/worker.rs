//! Background work. Every O(n) step (import, operations, analysis, export)
//! runs on a worker thread and reports back over a channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use mf_command::{CommandError, Completed, Prepared};
use mf_geometry::{Diagnostics, Mesh};

/// Analysis results for one mesh revision.
pub struct Analysis {
    pub diagnostics: Diagnostics,
    pub vertices: Arc<MeshVertices>,
}
use mf_io::{Format, Imported, IoError};
use mf_render::MeshVertices;

pub enum Message {
    Imported(Result<Imported, IoError>, String),
    Completed(Result<Completed, CommandError>),
    Analyzed { mesh: Mesh, analysis: Analysis },
    Exported(Result<PathBuf, IoError>),
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
        let _ = tx.send(f());
        ctx.request_repaint();
    });
}

pub fn import(tx: &Sender<Message>, ctx: &eframe::egui::Context, path: PathBuf) {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("object")
        .to_owned();
    spawn(tx, ctx, move || {
        Message::Imported(mf_io::read_path(&path), name)
    });
}

pub fn run(tx: &Sender<Message>, ctx: &eframe::egui::Context, prepared: Prepared) {
    spawn(tx, ctx, move || Message::Completed(prepared.run()));
}

/// Diagnostics and render vertices for one revision (O(n), so off the UI thread).
pub fn analyze(mesh: Mesh) -> Message {
    let analysis = Analysis {
        diagnostics: mf_geometry::inspect(&mesh),
        vertices: Arc::new(MeshVertices::build(&mesh)),
    };
    Message::Analyzed { mesh, analysis }
}

pub fn export(
    tx: &Sender<Message>,
    ctx: &eframe::egui::Context,
    mesh: Mesh,
    path: PathBuf,
    format: Format,
) {
    spawn(tx, ctx, move || {
        Message::Exported(mf_io::write_path(&mesh, &path, format).map(|()| path))
    });
}
