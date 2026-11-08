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
use crate::viewport::{Click, ViewOptions, Viewport};
use crate::worker::{self, Analysis, Message};
use mf_render::{LineVertex, loop_line_vertices, pick_loop, screen_position};
use mf_selection::{Selection, SelectionKind, TranslateError, Translation};

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
    /// What the next operation targets, bound to the current revision. It is
    /// translated after every history move (MF-SEL-003).
    pub selection: Option<Selection>,
    /// Highlight lines for the selected loops, with what they were built for.
    highlight: Option<(Arc<Vec<LineVertex>>, Selection, Mesh)>,
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
            selection: None,
            highlight: None,
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
            self.after_history_move();
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
    /// The selection, if the operation's spec accepts its kind; otherwise the
    /// operation runs on the whole object (the panel says which).
    pub fn selection_for(&self, op_id: &str) -> Option<Selection> {
        let sel = self.selection.as_ref()?;
        let spec = mf_spec::builtin::get(op_id)?;
        let kind = match sel.kind {
            SelectionKind::Faces => "face_set",
            SelectionKind::Component => "component",
            SelectionKind::BoundaryLoop => "boundary_loop",
            SelectionKind::Object => return Some(sel.clone()),
        };
        spec.target
            .kinds
            .iter()
            .any(|k| k == kind)
            .then(|| sel.clone())
    }

    pub fn apply(&mut self, ctx: &egui::Context, op_id: String) {
        let Some(target) = self.active else { return };
        let selection = self.selection_for(&op_id);
        let raw_params = self
            .params_for(&op_id)
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        match self.session.prepare(Invocation {
            selection,
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
            self.after_history_move();
        }
    }

    pub fn redo(&mut self) {
        if self.busy.is_none() {
            self.session.redo();
            self.after_history_move();
        }
    }

    fn after_history_move(&mut self) {
        self.sync_active();
        self.retarget_selection();
    }

    /// Translate the selection to the current revision (MF-SEL-003). A loop
    /// that was filled clears it with a message; anything not mappable clears
    /// it with the reason. Never applied to a revision it wasn't translated to.
    fn retarget_selection(&mut self) {
        let Some(sel) = self.selection.take() else {
            return;
        };
        let current = self.session.current();
        if sel.revision == current {
            self.selection = Some(sel);
            return;
        }
        match self.session.translate(&sel, current) {
            Translation::Mapped(m) => self.selection = Some(m),
            Translation::Partial { mapped, lost } => {
                self.status = Some(format!("Selection partly lost ({} element(s))", lost.len()));
                self.selection = Some(mapped);
            }
            Translation::Failed(TranslateError::LoopClosed { faces }) => {
                let plural = if faces.len() == 1 { "" } else { "s" };
                self.status = Some(format!(
                    "Hole filled ({} face{plural}); selection cleared",
                    faces.len()
                ));
            }
            Translation::Failed(e) => self.status = Some(format!("Selection cleared: {e:?}")),
            Translation::Ambiguous { candidates } => {
                self.status = Some(format!(
                    "Selection is now ambiguous between {candidates:?}; cleared"
                ))
            }
        }
    }

    /// Select what's under a click: a boundary loop within 8 px (when the
    /// current revision's topology is ready), otherwise the clicked face's
    /// component (ray cast on a worker). Shift adds to a same-kind selection.
    pub fn click(&mut self, ctx: &egui::Context, click: Click) {
        let (Some(object), Some(mesh), Some(cam)) =
            (self.active, self.active_mesh().cloned(), self.camera)
        else {
            return;
        };
        let origin = mesh_origin(&mesh);
        if self.current_analysis().is_some()
            && let Some(id) = pick_loop(&mesh, &cam, origin, click.size, click.px, 8.0)
        {
            self.select(object, SelectionKind::BoundaryLoop, id, click.shift);
            return;
        }
        let (ray_origin, dir) = cam.ray(origin, click.size, click.px);
        worker::pick_face(&self.tx, ctx, mesh, ray_origin, dir, click.shift);
    }

    fn select(&mut self, object: ObjectId, kind: SelectionKind, id: u32, add: bool) {
        let revision = self.session.current();
        match self.selection.as_mut() {
            Some(sel)
                if add && sel.kind == kind && sel.object == object && sel.revision == revision =>
            {
                if !sel.ids.contains(&id) {
                    sel.ids.push(id);
                    sel.ids.sort_unstable();
                }
            }
            _ => {
                self.selection = Some(Selection {
                    object,
                    revision,
                    kind,
                    ids: vec![id],
                })
            }
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Where a world point appears in the window (for overlays and tests).
    pub fn screen_point(&self, world: [f64; 3]) -> Option<egui::Pos2> {
        let (rect, cam, mesh) = (self.viewport.last_rect?, self.camera?, self.active_mesh()?);
        let size = (rect.width() as f64, rect.height() as f64);
        let (x, y) = screen_position(&cam, mesh_origin(mesh), size, world)?;
        Some(egui::pos2(rect.left() + x as f32, rect.top() + y as f32))
    }

    /// Make `id` the active object. A selection belongs to one object, so
    /// switching to another clears it (its IDs mean nothing there).
    pub fn set_active(&mut self, id: ObjectId) {
        if self.active != Some(id) {
            self.camera = None;
            if self.selection.as_ref().is_some_and(|s| s.object != id) {
                self.selection = None;
                self.status = Some("Selection cleared (another object is active)".into());
            }
        }
        self.active = Some(id);
    }

    /// Loop IDs the viewport highlights: only a loop selection of the active
    /// object at the current revision.
    pub fn highlighted_loops(&self) -> Option<Vec<u32>> {
        let sel = self.selection.as_ref().filter(|s| {
            s.kind == SelectionKind::BoundaryLoop
                && Some(s.object) == self.active
                && s.revision == self.session.current()
        })?;
        Some(sel.ids.clone())
    }

    /// Highlight lines for the selected loops of the active mesh.
    fn highlight_lines(&mut self) -> Option<Arc<Vec<LineVertex>>> {
        self.highlighted_loops()?;
        let sel = self.selection.clone()?;
        let mesh = self.active_mesh()?.clone();
        // Topology is only read when the worker has already computed it.
        self.current_analysis()?;
        if let Some((lines, for_sel, for_mesh)) = &self.highlight
            && *for_sel == sel
            && Mesh::ptr_eq(for_mesh, &mesh)
        {
            return Some(lines.clone());
        }
        let origin = self
            .displayed
            .as_ref()
            .map_or(mesh_origin(&mesh), |v| v.origin);
        let lines = Arc::new(loop_line_vertices(&mesh, origin, &sel.ids));
        self.highlight = Some((lines.clone(), sel, mesh));
        Some(lines)
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
                            Some(&id) => self.set_active(id),
                            None => self.error = Some("The file contains no mesh objects".into()),
                        }
                    }
                    Err(e) => self.error = Some(format!("Import failed: {e}")),
                }
            }
            Message::Completed(result) => {
                self.busy = None;
                match (*result).and_then(|c| self.session.commit(c)) {
                    Ok(CommitOutcome::NewRevision { report }) => {
                        self.status = Some(report.to_string());
                        self.retarget_selection();
                    }
                    Ok(CommitOutcome::Report(report)) => {
                        self.status = Some(report.to_string());
                    }
                    Ok(CommitOutcome::NoChange { report }) => {
                        self.status = Some(format!("Nothing to change: {report}"));
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Message::Picked { mesh, face, add } => {
                // Ignore a pick for a revision that's no longer on screen.
                if let (Some(object), Some(current)) = (self.active, self.active_mesh())
                    && Mesh::ptr_eq(current, &mesh)
                {
                    match face {
                        Some(f) => {
                            let component = mesh
                                .topology()
                                .components
                                .iter()
                                .position(|c| c.contains(&f.0));
                            if let Some(c) = component {
                                self.select(object, SelectionKind::Component, c as u32, add);
                            }
                        }
                        None if !add => self.selection = None,
                        None => {}
                    }
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
        let (undo, redo, fit, escape) = ctx.input_mut(|i| {
            if typing {
                return (false, false, false, false);
            }
            let escape = i.key_pressed(egui::Key::Escape);
            let redo = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            );
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let fit = i.key_pressed(egui::Key::F);
            (undo, redo, fit, escape)
        });
        if escape {
            self.clear_selection();
        }
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
        let highlight = self.highlight_lines();
        let click = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                self.viewport.show(
                    ui,
                    &mut self.camera,
                    vertices.as_ref(),
                    ViewOptions {
                        show_boundaries,
                        highlight: highlight.as_ref(),
                        updating,
                        placeholder,
                    },
                )
            })
            .inner;
        if let Some(c) = click
            && self.busy.is_none()
        {
            self.click(&ctx, c);
        }

        if self.busy.is_some() || self.is_analyzing() {
            ctx.request_repaint();
        }
    }
}

/// The local origin the renderer and picking use for a mesh.
fn mesh_origin(mesh: &Mesh) -> glam::DVec3 {
    mesh.bounding_box()
        .map_or(glam::DVec3::ZERO, |b| glam::DVec3::from(b.center()))
}
