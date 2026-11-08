use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use mf_command::{CommitOutcome, Invocation, ObjectId, Session};
use mf_geometry::Mesh;
use mf_io::Format;
use mf_render::{MeshVertices, OrbitCamera};
use mf_spec::ParamValue;

use crate::analysis::{AnalysisCache, LatestWorker};
use crate::viewport::Viewport;
use crate::worker::{self, Analysis, Message};

pub struct MeshFixerApp {
    pub session: Session,
    pub active: Option<ObjectId>,
    pub camera: Option<OrbitCamera>,
    /// Analyses keyed by revision identity (pointer, so recognizing a
    /// revision is O(1) on the UI thread).
    analyses: AnalysisCache<Analysis>,
    analyzer: LatestWorker<Mesh>,
    /// Last revision submitted for analysis.
    requested: Option<Mesh>,
    /// Vertices currently drawn. May belong to the previous revision while
    /// the new one is analyzed; the viewport says so.
    displayed: Option<Arc<MeshVertices>>,
    /// Label of the running import/operation/export. Blocks new commands.
    pub busy: Option<String>,
    pub selected_op: String,
    pub op_params: BTreeMap<String, BTreeMap<String, ParamValue>>,
    pub show_boundaries: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    /// Every warning from the latest import (MF-ISSUE-004), not just the last.
    pub warnings: Vec<String>,
    /// Lines of the last replay report, shown in a window.
    pub replay_report: Option<Vec<String>>,
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

        let (tx, rx) = channel::<Message>();
        let selected_op = mf_spec::builtin::specs()
            .iter()
            .find(|s| !s.is_read_only())
            .map(|s| s.id.clone())
            // allow-swallow: no mutating operation registered means no default selection (empty id)
            .unwrap_or_default();
        let mut app = MeshFixerApp {
            session: Session::new(),
            active: None,
            camera: None,
            analyses: AnalysisCache::new(4),
            analyzer: {
                let tx = tx.clone();
                let ctx = cc.egui_ctx.clone();
                LatestWorker::new(worker::analyze, move |msg| {
                    // The receiver only disappears when the app is closing.
                    // allow-swallow: the receiver only disappears while the app is closing
                    let _ = tx.send(msg);
                    ctx.request_repaint();
                })
            },
            requested: None,
            displayed: None,
            busy: None,
            selected_op,
            op_params: BTreeMap::new(),
            show_boundaries: true,
            error: None,
            status: None,
            warnings: Vec::new(),
            replay_report: None,
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
            .and_then(|o| o.mesh())
    }

    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.busy = Some(format!("Importing {}", path.display()));
        worker::import(&self.tx, ctx, path);
    }

    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Meshes", &["stl", "obj", "3mf"])
            .pick_file()
        {
            self.open(ctx, path);
        }
    }

    /// Write the command log for the current revision path.
    pub fn save_log_to(&mut self, dir: &std::path::Path) -> Result<(), String> {
        self.session.save_log(dir).map_err(|e| e.to_string())
    }

    pub fn save_log_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("session.mfx.log")
            .save_file()
        else {
            return;
        };
        match self.save_log_to(&path) {
            Ok(()) => self.status = Some(format!("Saved command log {}", path.display())),
            Err(e) => self.error = Some(format!("Saving command log failed: {e}")),
        }
    }

    pub fn replay_dialog(&mut self, ctx: &egui::Context) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.busy = Some(format!("Replaying {}", dir.display()));
            worker::replay(&self.tx, ctx, dir);
        }
    }

    /// Jump to any revision in the DAG, including an abandoned branch.
    pub fn checkout(&mut self, id: mf_command::RevisionId) {
        if self.busy.is_none() && self.session.checkout(id) {
            self.sync_active();
        }
    }

    pub fn export_dialog(&mut self, ctx: &egui::Context) {
        let Some(mesh) = self.active_mesh().cloned() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("3MF (all objects)", &["3mf"])
            .add_filter("Binary STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .save_file()
        else {
            return;
        };
        match Format::from_path(&path) {
            Some(Format::ThreeMf) => match self.document_model() {
                Ok(model) => {
                    self.busy = Some(format!("Exporting {}", path.display()));
                    worker::export(&self.tx, ctx, worker::ExportJob::Model(model), path);
                }
                Err(e) => self.error = Some(e),
            },
            Some(format) => {
                self.busy = Some(format!("Exporting {}", path.display()));
                worker::export(&self.tx, ctx, worker::ExportJob::Mesh(mesh, format), path);
            }
            None => self.error = Some(format!("Unsupported export format: {}", path.display())),
        }
    }

    /// The whole document as a 3MF model (every object, transforms, units).
    pub fn document_model(&self) -> Result<mf_io::threemf::Model, String> {
        let doc = self.session.document();
        let objects = doc
            .objects()
            .iter()
            .map(|o| {
                let mesh = o
                    .mesh()
                    .ok_or_else(|| format!("object '{}' is not a mesh", o.name))?;
                Ok(mf_io::threemf::ModelObject {
                    name: o.name.clone(),
                    mesh: mesh.clone(),
                    transform: o.transform,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(mf_io::threemf::Model {
            unit: doc.units(),
            objects,
        })
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
                // allow-swallow: an operation id with no spec has no parameters (empty map)
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
            selection: None,
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
            Message::Imported(result) => {
                self.busy = None;
                match result {
                    Ok(model) => {
                        let (ids, warnings) = self.session.import_model(model);
                        self.warnings = warnings;
                        if !self.warnings.is_empty() {
                            self.status =
                                Some(format!("{} import warning(s)", self.warnings.len()));
                        }
                        match ids.first() {
                            Some(&id) => {
                                self.active = Some(id);
                                self.camera = None;
                            }
                            None => self.error = Some("The file contains no mesh objects".into()),
                        }
                    }
                    Err(e) => self.error = Some(format!("Import failed: {e}")),
                }
            }
            Message::Completed(result) => {
                self.busy = None;
                match (*result).and_then(|c| self.session.commit(c)) {
                    Ok(CommitOutcome::NewRevision { report })
                    | Ok(CommitOutcome::Report(report)) => {
                        self.status = Some(report.to_string());
                    }
                    Ok(CommitOutcome::NoChange { report }) => {
                        self.status = Some(format!("Nothing to change: {report}"));
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Message::Analyzed { mesh, analysis } => {
                if self
                    .requested
                    .as_ref()
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
                {
                    self.requested = None;
                }
                self.analyses.insert(mesh, analysis);
            }
            Message::Replayed(result) => {
                self.busy = None;
                match result {
                    Ok(lines) => self.replay_report = Some(lines),
                    Err(e) => self.error = Some(format!("Replay failed: {e}")),
                }
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

    /// Analysis for exactly the active revision, if it's ready.
    pub fn current_analysis(&self) -> Option<&Analysis> {
        self.active_mesh().and_then(|m| self.analyses.get(m))
    }

    /// Request analysis when the active revision changes.
    fn refresh_analysis(&mut self) {
        let Some(mesh) = self.active_mesh().cloned() else {
            self.displayed = None;
            return;
        };
        if let Some(a) = self.analyses.get(&mesh) {
            self.displayed = Some(a.vertices.clone());
        } else if !self
            .requested
            .as_ref()
            .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
        {
            self.requested = Some(mesh.clone());
            self.analyzer.submit(mesh);
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

    /// True while the active revision's analysis isn't ready.
    pub fn is_analyzing(&self) -> bool {
        self.active_mesh().is_some() && self.current_analysis().is_none()
    }
}

impl eframe::App for MeshFixerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.refresh_analysis();

        // Shortcuts go to a focused text field first (MF-ISSUE-009).
        let typing = ctx.memory(|m| m.focused().is_some());
        let (undo, redo, fit) = ctx.input_mut(|i| {
            if typing {
                return (false, false, false);
            }
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
        crate::panels::replay_window(self, &ctx);

        let vertices = self.displayed.clone();
        let placeholder = match self.active_mesh() {
            Some(m) if m.is_empty() => "Empty mesh (0 triangles)",
            Some(_) => "Analyzing…",
            None => "Open a mesh with File > Open (STL or OBJ)",
        };
        let show_boundaries = self.show_boundaries;
        let updating = self.is_analyzing();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.viewport.show(
                ui,
                &mut self.camera,
                vertices.as_ref(),
                show_boundaries,
                updating,
                placeholder,
            );
        });

        if self.busy.is_some() || self.is_analyzing() {
            ctx.request_repaint();
        }
    }
}
