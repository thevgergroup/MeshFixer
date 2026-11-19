//! Tool sessions (ADR-0007, consolidation plan C21): every tool on the
//! rail is a [`ToolSession`] that owns its work (what Apply would run,
//! what it previews, what it does with its own results and with an undo,
//! how it drops a drag, what it paints over the view), and keeps its
//! state in itself rather than on [`MeshFixerApp`].
//!
//! The shell owns everything around the sessions, here and nowhere else:
//!
//! - the shown transform and the shown render, from the open session's
//!   [`ToolSession::preview`] ([`MeshFixerApp::shown_transform`],
//!   [`MeshFixerApp::shown_render`]);
//! - the Esc order ([`MeshFixerApp::escape`]);
//! - undo notification: every session hears about a history move
//!   ([`ToolSession::on_revision`]);
//! - refusal routing: a command's result goes to the session that issued
//!   it ([`ToolSession::on_result`]), and its refusal to that tool's
//!   panel, drawn by the tool dock ([`crate::refusal_card::show_issued`]);
//! - the lifecycle: a tool switch closes the old session
//!   ([`ToolSession::close`]) and cancels its previews.
//!
//! The session harness (`tests/session_contract.rs`) checks every tool in
//! `shell.tools()` against this contract.
//!
//! Split, Connect, Hollow and Sculpt are still thin adapters around their
//! old app fields (they move into their sessions in C23 and C24), and the
//! hinge swing preview is a generator fact read by the shell until C26.

use std::any::Any;
use std::sync::Arc;

use eframe::egui;
use mf_command::ObjectId;

use crate::MeshFixerApp;
use crate::intent::Intent;
use crate::refusal_card::Refusal;
use crate::worker::RenderMesh;

/// What a session shows in place of the document.
#[derive(Clone, Default)]
pub enum PreviewSpec {
    #[default]
    None,
    /// Objects drawn at these transforms instead of the document's (a
    /// drag, a hovered Align dot).
    Placement(Vec<(ObjectId, [f64; 12])>),
    /// An object drawn with this render instead of its own (a live
    /// stroke, Hollow's shell).
    Geometry(ObjectId, Arc<RenderMesh>),
}

impl std::fmt::Debug for PreviewSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewSpec::None => write!(f, "None"),
            PreviewSpec::Placement(p) => f.debug_tuple("Placement").field(p).finish(),
            PreviewSpec::Geometry(id, _) => f.debug_tuple("Geometry").field(id).finish(),
        }
    }
}

impl PreviewSpec {
    pub fn is_none(&self) -> bool {
        matches!(self, PreviewSpec::None)
    }
}

/// How the document's history moved under the sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMove {
    Undo,
    Redo,
    /// A jump to a History row.
    Checkout,
    /// A direct edit that made a revision without a command (a role,
    /// visibility, a group, an opening mark).
    Edit,
}

/// What became of a command a session issued.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A new revision.
    Committed,
    /// It ran and changed nothing (or only reported).
    NoChange,
    /// The operation refused it (the card in the issuer's panel says why).
    Refused(Refusal),
    /// Cancelled, or it failed outright (the error bar says why).
    Failed,
}

/// A command's result, as its issuer's session hears it.
#[derive(Debug)]
pub struct CommandResult<'a> {
    pub op_id: &'a str,
    pub outcome: Outcome,
    /// The operation's report (a commit's), when there is one.
    pub report: Option<&'a serde_json::Value>,
    /// What the session attached to the command when it issued it
    /// (ADR-0009): its to act on now, for this command only.
    pub follow_ups: &'a [crate::app::FollowUp],
}

impl CommandResult<'_> {
    pub fn committed(&self) -> bool {
        self.outcome == Outcome::Committed
    }
}

/// One open-able tool's work. State a session keeps across a close (its
/// settings: a wall thickness, a search) is declared by leaving it out of
/// [`ToolSession::held`]; anything else is dropped by
/// [`ToolSession::close`].
///
/// The methods that change the session get the app too, with the
/// session taken out of the store for the call: a session never reads
/// its own state back through the app.
pub trait ToolSession: Any {
    /// What Apply would run now, and its label (`None`: nothing to run,
    /// or the tool has no Apply).
    fn intent(&self, _app: &MeshFixerApp) -> Option<(Intent<'static>, String)> {
        None
    }

    /// What it shows in place of the document.
    fn preview(&self, _app: &MeshFixerApp) -> PreviewSpec {
        PreviewSpec::None
    }

    /// A command it issued finished: committed, refused or failed.
    fn on_result(&mut self, _app: &mut MeshFixerApp, _r: &CommandResult<'_>) {}

    /// The history moved (undo, redo, a History jump): drop whatever was
    /// bound to the revision it left.
    fn on_revision(&mut self, _app: &mut MeshFixerApp, _moved: HistoryMove) {}

    /// Esc, or Undo, mid-drag: put back what the drag changed and stop
    /// it. True if there was one.
    fn cancel_drag(&mut self, _app: &mut MeshFixerApp) -> bool {
        false
    }

    /// Its 2D overlays over the view (only while it is open).
    fn paint(&self, _app: &MeshFixerApp, _ctx: &egui::Context) {}

    /// It closed (a tool switch, Esc): drop its transient state, stop its
    /// previews. Its settings stay.
    fn close(&mut self, _app: &mut MeshFixerApp) {}

    /// The transient state it still holds (a drag, a hover, a preview, a
    /// pick), by name: empty once it is closed and once the history moves
    /// past what that state was for. Settings aren't listed.
    fn held(&self, _app: &MeshFixerApp) -> Vec<&'static str> {
        Vec::new()
    }

    /// The state it holds that is bound to a revision the document no
    /// longer shows (a preview of it, a pick on it, a note about it), by
    /// name: empty once [`ToolSession::on_revision`] has run.
    fn stale(&self, _app: &MeshFixerApp) -> Vec<&'static str> {
        Vec::new()
    }

    /// What it shows in the view beyond the objects' transforms (a cut
    /// plane, a box, connector placements), as text: equal means the
    /// view shows the same. The harness compares it across a cancelled
    /// drag.
    fn view_state(&self, _app: &MeshFixerApp) -> String {
        String::new()
    }
}

/// The session of a tool with no work of its own (Select, Inspect,
/// Export).
#[derive(Debug, Default)]
pub struct NoSession;

impl ToolSession for NoSession {}

/// A fresh [`NoSession`] (the `shell::Tool::session` default).
pub fn no_session() -> Box<dyn ToolSession> {
    Box::new(NoSession)
}

struct Slot {
    id: &'static str,
    /// `None` only while one of its methods runs.
    session: Option<Box<dyn ToolSession>>,
    /// How many history moves it was told about (the harness checks
    /// `on_revision` is called).
    revisions: u64,
    /// How many of its own commands' results it was given.
    results: u64,
}

/// Every tool's session, one per registered tool, made from its
/// registration when the app starts.
pub struct Sessions {
    slots: Vec<Slot>,
    /// Sessions closed while one of their own methods ran (a placement's
    /// result opening Transform over its tool): closed once it returns.
    closing: Vec<&'static str>,
}

impl Sessions {
    pub fn new(shell: &crate::shell::Shell) -> Sessions {
        Sessions {
            slots: shell
                .tools()
                .iter()
                .map(|t| Slot {
                    id: t.id,
                    session: Some((t.session)()),
                    revisions: 0,
                    results: 0,
                })
                .collect(),
            closing: Vec::new(),
        }
    }

    fn slot(&self, id: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    fn slot_mut(&mut self, id: &str) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    /// Tool `id`'s session (`None` while one of its own methods runs).
    pub fn get(&self, id: &str) -> Option<&dyn ToolSession> {
        self.slot(id)?.session.as_deref()
    }

    /// The session of type `T`.
    pub fn find<T: ToolSession>(&self) -> Option<&T> {
        self.slots
            .iter()
            .find_map(|s| (s.session.as_deref()? as &dyn Any).downcast_ref::<T>())
    }

    pub fn find_mut<T: ToolSession>(&mut self) -> Option<&mut T> {
        self.slots
            .iter_mut()
            .find_map(|s| (s.session.as_deref_mut()? as &mut dyn Any).downcast_mut::<T>())
    }

    fn ids(&self) -> Vec<&'static str> {
        self.slots.iter().map(|s| s.id).collect()
    }
}

impl MeshFixerApp {
    /// Tool state of type `T` (a ported tool's session). Panics if it is
    /// asked for from inside one of its own session's methods, where the
    /// session is `self`.
    pub fn tool_state<T: ToolSession>(&self) -> &T {
        self.sessions.find::<T>().unwrap_or_else(|| {
            panic!(
                "{} is read through the app inside its own session",
                std::any::type_name::<T>()
            )
        })
    }

    pub fn tool_state_mut<T: ToolSession>(&mut self) -> &mut T {
        self.sessions.find_mut::<T>().unwrap_or_else(|| {
            panic!(
                "{} is read through the app inside its own session",
                std::any::type_name::<T>()
            )
        })
    }

    /// Tool `id`'s session.
    pub fn tool_session(&self, id: &str) -> Option<&dyn ToolSession> {
        self.sessions.get(id)
    }

    /// Run `f` on tool `id`'s session, taken out of the store for the
    /// call so it can change the app too.
    fn with_session<R>(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut dyn ToolSession, &mut MeshFixerApp) -> R,
    ) -> Option<R> {
        let mut session = self.sessions.slot_mut(id)?.session.take()?;
        let r = f(session.as_mut(), self);
        let Some(slot) = self.sessions.slot_mut(id) else {
            return Some(r);
        };
        slot.session = Some(session);
        self.close_deferred(id);
        Some(r)
    }

    /// Close session `id` if it was closed while one of its own methods
    /// ran ([`Sessions::closing`]).
    fn close_deferred(&mut self, id: &str) {
        if let Some(i) = self.sessions.closing.iter().position(|c| *c == id) {
            self.sessions.closing.remove(i);
            self.with_session(id, |s, app| s.close(app));
        }
    }

    /// The open tool's session.
    fn open_session(&self) -> Option<&dyn ToolSession> {
        self.sessions.get(self.active_tool)
    }

    /// The transform object `id` is drawn at: the open session's
    /// placement preview, then a committed placement still being applied,
    /// then a generator's motion preview (the hinge swing, C26), else
    /// `None` (the document's).
    pub fn shown_transform(&self, id: ObjectId) -> Option<[f64; 12]> {
        if let Some(PreviewSpec::Placement(p)) = self.open_session().map(|s| s.preview(self))
            && let Some(&(_, t)) = p.iter().find(|(o, _)| *o == id)
        {
            return Some(t);
        }
        self.settling_placement
            .filter(|(o, _)| *o == id)
            .map(|(_, t)| t)
            .or_else(|| crate::hinge_tool::swung(self, id))
    }

    /// The render object `id` is drawn with, when it isn't its current
    /// revision's own: the open session's geometry preview, else a
    /// committed result's render still being applied (until the
    /// revision's own is ready).
    pub fn shown_render(&self, id: ObjectId) -> Option<Arc<RenderMesh>> {
        if let Some(PreviewSpec::Geometry(o, r)) = self.open_session().map(|s| s.preview(self))
            && o == id
        {
            return Some(r);
        }
        if !self.shows_current_revision()
            && let Some((object, r)) = &self.settling_render
            && *object == id
        {
            return Some(r.clone());
        }
        None
    }

    /// Keep `render` on screen for `object` until its current revision's
    /// own render is ready (a stroke just released: no flicker back to
    /// the revision before it).
    pub fn settle_render(&mut self, object: ObjectId, render: Arc<RenderMesh>) {
        self.settling_render = Some((object, render));
    }

    /// The render a committed result left on screen, if one still is.
    pub fn settling_render(&self) -> Option<ObjectId> {
        self.settling_render.as_ref().map(|(id, _)| *id)
    }

    /// Esc, in order (ui-plan.md §5): stop the running job; else cancel
    /// the open tool's drag; else go one level back inside the tool (a
    /// sub-tool's settings, a selection); else leave the tool; else clear
    /// the selection. One step per press.
    pub fn escape(&mut self) {
        if self.can_cancel() {
            self.cancel_job();
            return;
        }
        if self.cancel_tool_drag() {
            return;
        }
        let tool = self.active_tool_def();
        if (tool.back)(self) {
            return;
        }
        if self.active_tool != crate::shell::SELECT {
            self.close_rail_tool();
            return;
        }
        self.clear_selection();
    }

    /// Cancel the open tool's drag, if one is under way: nothing is
    /// committed and what it moved goes back. Returns whether there was
    /// one.
    pub fn cancel_tool_drag(&mut self) -> bool {
        let id = self.active_tool;
        self.with_session(id, |s, app| s.cancel_drag(app))
            .unwrap_or(false) // allow-swallow: a tool without a session has no drag
    }

    /// Open rail tool `id` (a `shell::Tool` id): the open one closes,
    /// keeping its work. Opening the open one does nothing; an
    /// unavailable one stays shut (its reason in the status line).
    pub fn open_rail_tool(&mut self, id: &str) {
        let Some(tool) = self.shell.tool(id).copied() else {
            return;
        };
        if let crate::shell::Availability::Unavailable(why) = (tool.availability)(self) {
            self.status = Some(why);
            return;
        }
        if tool.id != crate::shell::SELECT {
            self.first_use_hint = false;
            self.narrow_focus = crate::NarrowFocus::Tool;
        }
        if self.active_tool == tool.id {
            // The open tool's rail button takes it back to its top level.
            while (tool.back)(self) {}
            return;
        }
        let old = self.active_tool_def();
        (old.on_close)(self, crate::shell::Close::Commit);
        if self.with_session(old.id, |s, app| s.close(app)).is_none() {
            // Its own method switched tools (a placement's result opening
            // Transform): it closes once that method returns.
            self.sessions.closing.push(old.id);
        }
        self.active_tool = tool.id;
        (tool.on_open)(self);
        self.release_closed_tools_previews();
    }

    /// Open the tool that offers operation `op` (an operation spec's
    /// command: Transform for `object.set_transform`, Repair's section
    /// for a repair). False if no tool offers it.
    pub fn open_tool(&mut self, op: &str) -> bool {
        let tools: Vec<crate::shell::Tool> = self.shell.tools().to_vec();
        tools.iter().any(|t| (t.on_operation)(self, op))
    }

    /// Close the open rail tool: back to Select.
    pub fn close_rail_tool(&mut self) {
        self.open_rail_tool(crate::shell::SELECT);
    }

    /// The open tool's registration.
    pub fn active_tool_def(&self) -> crate::shell::Tool {
        *self
            .shell
            .tool(self.active_tool)
            .or_else(|| self.shell.tool(crate::shell::SELECT))
            .expect("Select is registered")
    }

    /// The tools' keys, before the app's (I8): the open tool's own keys
    /// first (they win over another tool's shortcut while it is open:
    /// Transform's W, E and R pick Move, Rotate and Scale, as in
    /// Meshmixer), then each tool's shortcut opens it. A key no tool uses
    /// is left for the rest of the app.
    pub(crate) fn tool_keys(&mut self, ctx: &egui::Context) {
        let tool = self.active_tool_def();
        let keys = (tool.keys)(self);
        let pressed = ctx.input_mut(|i| {
            keys.iter()
                .find(|(k, _)| i.consume_key(egui::Modifiers::NONE, *k))
                .map(|(_, what)| *what)
        });
        if let Some(k) = pressed {
            (tool.on_key)(self, k);
        }
        let shortcuts: Vec<(&'static str, egui::Key)> = self
            .shell
            .tools()
            .iter()
            .filter_map(|t| t.shortcut.map(|k| (t.id, k)))
            .collect();
        for (id, key) in shortcuts {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                self.open_rail_tool(id);
            }
        }
    }

    /// Tell every session the history moved.
    pub(crate) fn notify_revision(&mut self, moved: HistoryMove) {
        for id in self.sessions.ids() {
            if let Some(slot) = self.sessions.slot_mut(id) {
                slot.revisions += 1;
            }
            self.with_session(id, |s, app| s.on_revision(app, moved));
        }
    }

    /// Give `r` to the session of the tool that issued its command.
    pub(crate) fn deliver_result(&mut self, by: &'static str, r: &CommandResult<'_>) {
        if let Some(slot) = self.sessions.slot_mut(by) {
            slot.results += 1;
        }
        self.with_session(by, |s, app| s.on_result(app, r));
    }

    /// How many history moves tool `id`'s session was told about.
    pub fn revisions_seen(&self, id: &str) -> u64 {
        self.sessions.slot(id).map_or(0, |s| s.revisions)
    }

    /// How many of its own commands' results tool `id`'s session got.
    pub fn results_seen(&self, id: &str) -> u64 {
        self.sessions.slot(id).map_or(0, |s| s.results)
    }

    /// The transient state tool `id`'s session still holds.
    pub fn tool_holds(&self, id: &str) -> Vec<&'static str> {
        self.sessions
            .get(id)
            .map_or_else(Vec::new, |s| s.held(self))
    }

    /// The state tool `id`'s session holds for a revision no longer shown.
    pub fn tool_stale(&self, id: &str) -> Vec<&'static str> {
        self.sessions
            .get(id)
            .map_or_else(Vec::new, |s| s.stale(self))
    }

    /// A committed placement still drawn over the document's, if one is.
    pub fn settling_placement(&self) -> Option<(ObjectId, [f64; 12])> {
        self.settling_placement
    }

    /// What tool `id` shows in the view beyond the objects' transforms.
    pub fn tool_view_state(&self, id: &str) -> String {
        self.sessions
            .get(id)
            .map_or_else(String::new, |s| s.view_state(self))
    }

    /// What tool `id`'s session previews now.
    pub fn tool_preview(&self, id: &str) -> PreviewSpec {
        self.sessions
            .get(id)
            .map_or(PreviewSpec::None, |s| s.preview(self))
    }

    /// What the open tool's Apply would run now.
    pub fn tool_intent(&self) -> Option<(Intent<'static>, String)> {
        self.open_session()?.intent(self)
    }

    /// Run the open tool's Apply: its session's intent, issued by it.
    pub fn apply_tool(&mut self) {
        if let Some((intent, label)) = self.tool_intent() {
            self.refusal = None;
            self.run_intent(intent, label);
        }
    }

    /// The open tool's 2D overlays, then the generator overlays (the
    /// hinge swing's contacts, C26).
    pub(crate) fn paint_tools(&self, ctx: &egui::Context) {
        if let Some(s) = self.open_session() {
            s.paint(self, ctx);
        }
        crate::hinge_tool::paint(self, ctx);
    }

    /// A preview is cancelled once the tool that asked for it closes
    /// (ADR-0009), however it closed (its rail button, Esc, a tool
    /// opening over it): checked on every switch and every frame.
    pub(crate) fn release_closed_tools_previews(&mut self) {
        let open = self.active_tool;
        self.peg_preview_worker.release_unless_owner(open);
        self.hollow_worker.release_unless_owner(open);
        crate::hinge_tool::release_unless_owner(self, open);
    }

    /// How many of `tool`'s previews are still queued, running or owned
    /// by it (tests: closing a tool cancels its preview).
    pub fn previews_owned_by(&self, tool: &str) -> usize {
        usize::from(self.peg_preview_worker.owner() == Some(tool))
            + usize::from(self.hollow_worker.owner() == Some(tool))
            + crate::hinge_tool::previews_owned_by(self, tool)
            + usize::from(!self.tool_preview(tool).is_none() && self.active_tool != tool)
    }
}
