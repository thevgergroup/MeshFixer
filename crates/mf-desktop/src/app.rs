use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use mf_command::{CommitOutcome, Invocation, ObjectId, Session};
use mf_geometry::{Diagnostics, Mesh};
use mf_io::Format;
use mf_render::{MeshVertices, OrbitCamera};
use mf_spec::ParamValue;

use crate::viewport::Viewport;
use crate::worker::{self, Message};

/// Analysis results for one mesh revision (identified by pointer, not hash,
/// so the UI thread never does O(n) work to recognize a revision).
pub struct Analysis {
    pub mesh: Mesh,
    pub diagnostics: Diagnostics,
    pub vertices: Arc<MeshVertices>,
}

pub struct MeshFixerApp {
    pub session: Session,
    pub active: Option<ObjectId>,
    pub camera: Option<OrbitCamera>,
    pub analysis: Option<Analysis>,
    /// Mesh currently being analyzed, if any.
    analyzing: Option<Mesh>,
    /// Label of the running import/operation/export. Blocks new commands.
    pub busy: Option<String>,
    pub selected_op: String,
    pub op_params: BTreeMap<String, BTreeMap<String, ParamValue>>,
    pub show_boundaries: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    pub viewport: Viewport,
    tx: Sender<Message>,
    rx: Receiver<Message>,
}

impl MeshFixerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("MeshFixer needs the wgpu renderer, but it is unavailable")?;

        let (tx, rx) = channel();
        let selected_op = mf_spec::builtin::specs()
            .iter()
            .find(|s| !s.is_read_only())
            .map(|s| s.id.clone())
            .unwrap_or_default();
        let mut app = MeshFixerApp {
            session: Session::new(),
            active: None,
            camera: None,
            analysis: None,
            analyzing: None,
            busy: None,
            selected_op,
            op_params: BTreeMap::new(),
            show_boundaries: true,
            error: None,
            status: None,
            viewport: Viewport::new(render_state.clone()),
            tx,
            rx,
        };
        if let Some(path) = initial {
            app.open(&cc.egui_ctx, path);
        }
        Ok(app)
    }

    pub fn active_mesh(&self) -> Option<&Mesh> {
        self.active
            .and_then(|id| self.session.document().object(id))
            .map(|o| &o.mesh)
    }

    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.busy = Some(format!("Importing {}", path.display()));
        worker::import(&self.tx, ctx, path);
    }

    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Meshes", &["stl", "obj"])
            .pick_file()
        {
            self.open(ctx, path);
        }
    }

    pub fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some(mesh) = self.active_mesh().cloned() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Binary STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .save_file()
        else {
            return;
        };
        match Format::from_path(&path) {
            Some(format) => {
                self.busy = Some(format!("Exporting {}", path.display()));
                worker::export(&self.tx, ctx, mesh, path, format);
            }
            None => self.error = Some(format!("Unsupported export format: {}", path.display())),
        }
    }

    /// Current parameter values for an operation, initialized from spec defaults.
    pub fn params_for(&mut self, op_id: &str) -> &mut BTreeMap<String, ParamValue> {
        self.op_params.entry(op_id.to_owned()).or_insert_with(|| {
            mf_spec::builtin::get(op_id)
                .map(|s| {
                    s.parameters
                        .iter()
                        .map(|p| (p.name.clone(), p.kind.default_value()))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// Validate on the UI thread, run on a worker, commit when it returns.
    pub fn apply(&mut self, ctx: &egui::Context, op_id: String) {
        let Some(target) = self.active else { return };
        let raw_params = self
            .params_for(&op_id)
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        match self.session.prepare(Invocation {
            op_id: op_id.clone(),
            target,
            raw_params,
        }) {
            Ok(prepared) => {
                self.busy = Some(format!("Running {op_id}"));
                worker::run(&self.tx, ctx, prepared);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn undo(&mut self) {
        if self.busy.is_none() {
            self.session.undo();
            self.sync_active();
        }
    }

    pub fn redo(&mut self) {
        if self.busy.is_none() {
            self.session.redo();
            self.sync_active();
        }
    }

    /// Keep `active` pointing at an object that exists in the current document.
    fn sync_active(&mut self) {
        let doc = self.session.document();
        if self.active.and_then(|id| doc.object(id)).is_none() {
            self.active = doc.objects().last().map(|o| o.id);
            self.camera = None;
        }
    }

    fn handle(&mut self, msg: Message) {
        match msg {
            Message::Imported(result, name) => {
                self.busy = None;
                match result {
                    Ok(imported) => {
                        for w in &imported.warnings {
                            self.status = Some(format!("Warning: {w}"));
                        }
                        let id = self.session.import(imported, name);
                        self.active = Some(id);
                        self.camera = None;
                    }
                    Err(e) => self.error = Some(format!("Import failed: {e}")),
                }
            }
            Message::Completed(result) => {
                self.busy = None;
                match result.and_then(|c| self.session.commit(c)) {
                    Ok(CommitOutcome::NewRevision { report })
                    | Ok(CommitOutcome::Report(report)) => {
                        self.status = Some(report.to_string());
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Message::Analyzed {
                mesh,
                diagnostics,
                vertices,
            } => {
                if self
                    .analyzing
                    .as_ref()
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
                {
                    self.analyzing = None;
                }
                self.analysis = Some(Analysis {
                    mesh,
                    diagnostics,
                    vertices,
                });
            }
            Message::Exported(result) => {
                self.busy = None;
                match result {
                    Ok(path) => self.status = Some(format!("Exported {}", path.display())),
                    Err(e) => self.error = Some(format!("Export failed: {e}")),
                }
            }
        }
    }

    /// Start analysis when the active mesh revision changes.
    fn refresh_analysis(&mut self, ctx: &egui::Context) {
        let Some(mesh) = self.active_mesh().cloned() else {
            self.analysis = None;
            return;
        };
        let current = self
            .analysis
            .as_ref()
            .is_some_and(|a| Mesh::ptr_eq(&a.mesh, &mesh));
        let pending = self
            .analyzing
            .as_ref()
            .is_some_and(|m| Mesh::ptr_eq(m, &mesh));
        if !current && !pending {
            self.analyzing = Some(mesh.clone());
            worker::analyze(&self.tx, ctx, mesh);
        }
        if self.camera.is_none()
            && let Some(bb) = self.active_mesh().and_then(|m| m.bounding_box())
        {
            self.camera = Some(OrbitCamera::fit(&bb));
        }
    }

    pub fn fit_camera(&mut self) {
        self.camera = self
            .active_mesh()
            .and_then(|m| m.bounding_box())
            .map(|bb| OrbitCamera::fit(&bb));
    }

    pub fn is_analyzing(&self) -> bool {
        self.analyzing.is_some()
    }
}

impl eframe::App for MeshFixerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.refresh_analysis(&ctx);

        let (undo, redo, fit) = ctx.input_mut(|i| {
            let redo = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            );
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let fit = i.key_pressed(egui::Key::F);
            (undo, redo, fit)
        });
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if fit && !ctx.egui_wants_keyboard_input() {
            self.fit_camera();
        }

        crate::panels::menu_bar(self, ui);
        crate::panels::error_banner(self, ui);
        crate::panels::status_bar(self, ui);
        crate::panels::side_panel(self, ui);

        let vertices = self
            .analysis
            .as_ref()
            .filter(|a| {
                self.active_mesh().is_some_and(|m| Mesh::ptr_eq(m, &a.mesh)) || self.is_analyzing()
            })
            .map(|a| a.vertices.clone());
        let show_boundaries = self.show_boundaries;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.viewport
                .show(ui, &mut self.camera, vertices.as_ref(), show_boundaries);
        });

        if self.busy.is_some() || self.is_analyzing() {
            ctx.request_repaint();
        }
    }
}
