use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use mf_command::{CommitOutcome, Invocation, ObjectId, Session};
use mf_geometry::{Aabb, Mesh};
use mf_io::Format;
use mf_render::{MeshVertices, OrbitCamera, model_matrix};
use mf_spec::ParamValue;

use crate::analysis::{AnalysisCache, LatestWorker};
use crate::viewport::{Click, ObjectView, ViewOptions, Viewport};
use crate::worker::{self, Analysis, Message, PickTarget};
use mf_render::{LineVertex, loop_line_vertices, screen_position};
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
    /// Vertices currently drawn for the active object. May belong to the
    /// previous revision while the new one is analyzed; the viewport says so.
    displayed: Option<Arc<MeshVertices>>,
    /// Render-only vertices for every object that isn't active
    /// (MF-ISSUE-042): the viewport still draws them, dimmed, at their
    /// transform. Keyed by object id; the `Mesh` says which revision the
    /// vertices belong to, so a stale build for a since-changed object is
    /// never shown.
    object_vertices: BTreeMap<ObjectId, (Mesh, Arc<MeshVertices>)>,
    /// Object ids with a build in flight, and the mesh it was requested for
    /// (so a second change before the first build returns still gets
    /// re-requested).
    requested_vertices: BTreeMap<ObjectId, Mesh>,
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
            object_vertices: BTreeMap::new(),
            requested_vertices: BTreeMap::new(),
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

    /// Select what's under a click: a boundary loop within 8 px of any
    /// object (when its topology is ready), otherwise the nearest clicked
    /// face's component across every object (ray cast on a worker). A hit
    /// makes its object active (MF-ISSUE-042); shift adds to a same-kind,
    /// same-object selection.
    pub fn click(&mut self, ctx: &egui::Context, click: Click) {
        let (Some(active), Some(cam)) = (self.active, self.camera) else {
            return;
        };
        let targets: Vec<PickTarget> = self
            .session
            .document()
            .objects()
            .iter()
            .filter_map(|o| {
                Some(PickTarget {
                    object: o.id,
                    mesh: o.mesh()?.clone(),
                    model: model_matrix(&o.transform),
                })
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        worker::pick(
            &self.tx,
            ctx,
            targets,
            active,
            cam,
            cam.target,
            click.size,
            click.px,
            click.shift,
        );
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
        let (rect, cam) = (self.viewport.last_rect?, self.camera?);
        let size = (rect.width() as f64, rect.height() as f64);
        let (x, y) = screen_position(&cam, cam.target, size, world)?;
        Some(egui::pos2(rect.left() + x as f32, rect.top() + y as f32))
    }

    /// Make `id` the active object. A selection belongs to one object, so
    /// switching to another clears it (its IDs mean nothing there). The
    /// camera is shared by the whole document (MF-ISSUE-042) and keeps
    /// framing every object, so activating another one doesn't move it.
    pub fn set_active(&mut self, id: ObjectId) {
        if self.active != Some(id) && self.selection.as_ref().is_some_and(|s| s.object != id) {
            self.selection = None;
            self.status = Some("Selection cleared (another object is active)".into());
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
            Message::Picked {
                object,
                mesh,
                picked,
                add,
            } => {
                // Ignore a pick for a revision that's no longer on screen.
                let current = self
                    .session
                    .document()
                    .object(object)
                    .and_then(|o| o.mesh());
                if current.is_some_and(|m| Mesh::ptr_eq(m, &mesh)) {
                    match picked {
                        Some((kind, id)) => {
                            // A hit on another object makes it active
                            // (MF-ISSUE-042); `set_active` clears a
                            // selection that belongs to a different object.
                            self.set_active(object);
                            self.select(object, kind, id, add);
                        }
                        None if !add => self.selection = None,
                        None => {}
                    }
                }
            }
            Message::ObjectVertices {
                object,
                mesh,
                vertices,
            } => {
                if self
                    .requested_vertices
                    .get(&object)
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
                {
                    self.requested_vertices.remove(&object);
                }
                self.object_vertices.insert(object, (mesh, vertices));
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
            && let Some(bb) = self.document_world_bounds()
        {
            self.camera = Some(OrbitCamera::fit(&bb));
        }
    }

    /// Request render vertices for every object that isn't active
    /// (MF-ISSUE-042): the viewport draws them dimmed at their transform,
    /// but they never need the active object's full diagnostics.
    fn refresh_object_vertices(&mut self, ctx: &egui::Context) {
        let stale: Vec<(ObjectId, Mesh)> = self
            .session
            .document()
            .objects()
            .iter()
            .filter(|o| Some(o.id) != self.active)
            .filter_map(|o| {
                let mesh = o.mesh()?;
                let have = self
                    .object_vertices
                    .get(&o.id)
                    .is_some_and(|(m, _)| Mesh::ptr_eq(m, mesh));
                let requested = self
                    .requested_vertices
                    .get(&o.id)
                    .is_some_and(|m| Mesh::ptr_eq(m, mesh));
                (!have && !requested).then(|| (o.id, mesh.clone()))
            })
            .collect();
        for (id, mesh) in stale {
            self.requested_vertices.insert(id, mesh.clone());
            worker::build_vertices(&self.tx, ctx, id, mesh);
        }
        // Drop vertices and in-flight requests for objects no longer in the document.
        let doc = self.session.document();
        self.object_vertices
            .retain(|id, _| doc.object(*id).is_some());
        self.requested_vertices
            .retain(|id, _| doc.object(*id).is_some());
    }

    /// Every object the viewport should draw this frame: the active one
    /// (its vertices come from the full analysis, so they can lag one
    /// revision behind while it's running) plus every other object whose
    /// render vertices are ready (MF-ISSUE-042).
    fn object_views(&self) -> Vec<ObjectView> {
        let doc = self.session.document();
        doc.objects()
            .iter()
            .filter_map(|o| {
                let active = Some(o.id) == self.active;
                let vertices = if active {
                    self.displayed.clone()?
                } else {
                    self.object_vertices.get(&o.id)?.1.clone()
                };
                Some(ObjectView {
                    id: o.id,
                    vertices,
                    transform: o.transform,
                    active,
                })
            })
            .collect()
    }

    /// Union of every object's bounding box, at its transform
    /// (MF-ISSUE-042): what the camera fits.
    fn document_world_bounds(&self) -> Option<Aabb> {
        self.session
            .document()
            .objects()
            .iter()
            .filter_map(|o| Some((o.mesh()?.bounding_box()?, o.transform)))
            .map(|(bb, t)| transformed_bounds(&bb, &model_matrix(&t)))
            .reduce(|a, b| union_bounds(&a, &b))
    }

    pub fn fit_camera(&mut self) {
        self.camera = self.document_world_bounds().map(|bb| OrbitCamera::fit(&bb));
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
        self.refresh_object_vertices(&ctx);

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

        let objects = self.object_views();
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
                    &objects,
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

/// The axis-aligned box containing every corner of `bb` after `model`
/// (MF-ISSUE-042): a rotation or mirror can move the AABB's own min/max off
/// the transformed shape's extremes, so every corner is transformed.
fn transformed_bounds(bb: &Aabb, model: &glam::DMat4) -> Aabb {
    let corners = (0u8..8).map(|i| {
        let pick = |lo: f64, hi: f64, bit: u8| if i & bit == 0 { lo } else { hi };
        glam::DVec3::new(
            pick(bb.min[0], bb.max[0], 1),
            pick(bb.min[1], bb.max[1], 2),
            pick(bb.min[2], bb.max[2], 4),
        )
    });
    let mut out: Option<Aabb> = None;
    for c in corners {
        let p = (*model * glam::DVec4::from((c, 1.0))).truncate().to_array();
        out = Some(match out {
            None => Aabb { min: p, max: p },
            Some(mut acc) => {
                for (k, &v) in p.iter().enumerate() {
                    acc.min[k] = acc.min[k].min(v);
                    acc.max[k] = acc.max[k].max(v);
                }
                acc
            }
        });
    }
    // `bb` is never empty for a non-empty mesh's bounding box, so there's
    // always at least one corner.
    out.expect("a bounding box has 8 corners")
}

fn union_bounds(a: &Aabb, b: &Aabb) -> Aabb {
    let mut out = *a;
    for k in 0..3 {
        out.min[k] = out.min[k].min(b.min[k]);
        out.max[k] = out.max[k].max(b.max[k]);
    }
    out
}
