//! The pluggable shell (docs/design/ui-plan.md §5). Every built-in tool,
//! command, panel, overlay, inspector section, context action and
//! settings page registers here exactly as a plugin would: the rail, the
//! tool panel, the menus, the Browser's right-click menu, the Overlays
//! popover and Settings are all drawn from these registries, never from a
//! hand-written list.
//!
//! Contributions carry message ids (`crate::i18n`), not display text.
//! Behaviour is plain function pointers on the app, so a contribution is
//! data the shell can list, check and route to.
//!
//! What still lives outside the contract (tracked in docs/issues.md, plan
//! step 5): the built-in overlays' *drawing* is the core view-layer code
//! (`MeshFixerApp::view_layers`); their registry entries carry the id,
//! order, legend entry and switch.

use eframe::egui;

use crate::MeshFixerApp;
use crate::app::TreeItem;
use crate::tools::{LegendRow, ToolKey};
use crate::viewport::ToolPointer;

/// The rail's groups, in order (ui-plan.md §3), separated by thin rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Select, Transform.
    Arrange,
    /// Split, Combine, Make solid, Sculpt.
    Shape,
    /// Parts, Connectors.
    Add,
    /// Repair, Inspect.
    Fix,
    /// Export, Print prep.
    Output,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Arrange,
        Category::Shape,
        Category::Add,
        Category::Fix,
        Category::Output,
    ];

    /// Its name (a message id).
    pub fn name(self) -> &'static str {
        match self {
            Category::Arrange => "cat.arrange",
            Category::Shape => "cat.shape",
            Category::Add => "cat.add",
            Category::Fix => "cat.fix",
            Category::Output => "cat.output",
        }
    }
}

/// Whether a tool can open now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Greyed out on the rail, with why as its tooltip.
    Unavailable(String),
}

/// What the open tool did with a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Consumed,
    Pass,
}

/// How a tool closes: its work kept, or put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    Commit,
    Cancel,
}

/// The view's pointer, for the open tool: its drag (if it claimed the
/// press), where the pointer is, and the modifiers held.
#[derive(Debug, Clone, Copy)]
pub struct ToolInput {
    pub pointer: Option<ToolPointer>,
    pub hover: Option<(f64, f64)>,
    pub modifiers: egui::Modifiers,
}

/// A tool's test of whether a press at a point of the view is its own.
pub type Claims = Box<dyn Fn((f64, f64)) -> bool>;

/// A tool: an entry on the rail with its panel, its keys and its share of
/// the view's input.
#[derive(Clone, Copy)]
pub struct Tool {
    pub id: &'static str,
    /// Its name (a message id).
    pub name: &'static str,
    /// Its icon (an `icons` glyph). Required: a tool without one is
    /// rejected at registration, so every tool has a visual icon.
    pub icon: &'static str,
    pub category: Category,
    /// Its place within the category.
    pub order: u16,
    /// The key that opens it (no modifiers).
    pub shortcut: Option<egui::Key>,
    /// One line of help (a message id): the rail's tooltip and the
    /// panel's header.
    pub help: &'static str,
    pub availability: fn(&MeshFixerApp) -> Availability,
    /// Its panel in the tool dock (`None`: no panel, the dock closes;
    /// Select's summary is the inspector card instead).
    pub panel: Option<fn(&mut MeshFixerApp, &mut egui::Ui)>,
    /// What presses in the view are the tool's, ahead of the camera and
    /// picking: a test of a point (points from the view's top-left), made
    /// from this frame's state (`None`: it claims none).
    pub claims: fn(&MeshFixerApp) -> Option<Claims>,
    pub viewport_input: fn(&mut MeshFixerApp, &ToolInput) -> Input,
    /// The keys it takes while open (no modifiers).
    pub keys: fn(&MeshFixerApp) -> &'static [(egui::Key, ToolKey)],
    pub on_key: fn(&mut MeshFixerApp, ToolKey),
    /// Its own legend rows while open (`None`: the registered overlays).
    pub overlays: fn(&MeshFixerApp) -> Option<Vec<LegendRow>>,
    /// True if, while it's open, the defect overlays are hidden so its
    /// own colours mean one thing.
    pub hides_defects: fn(&MeshFixerApp) -> bool,
    pub on_open: fn(&mut MeshFixerApp),
    pub on_close: fn(&mut MeshFixerApp, Close),
    /// One level back inside the tool (Esc): from a sub-tool's settings or
    /// a selection back to the tool's list. True if it went back.
    pub back: fn(&mut MeshFixerApp) -> bool,
}

impl Tool {
    /// A tool with no panel, input, keys or overlays of its own; set the
    /// rest with struct update syntax (`Tool { panel: …, ..Tool::new(…) }`).
    pub fn new(
        id: &'static str,
        name: &'static str,
        icon: &'static str,
        category: Category,
        order: u16,
    ) -> Tool {
        Tool {
            id,
            name,
            icon,
            category,
            order,
            shortcut: None,
            help: name,
            availability: |_| Availability::Available,
            panel: None,
            claims: |_| None,
            viewport_input: |_, _| Input::Pass,
            keys: |_| &[],
            on_key: |_, _| {},
            overlays: |_| None,
            hides_defects: |_| false,
            on_open: |_| {},
            on_close: |_, _| {},
            back: |_| false,
        }
    }
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("id", &self.id).finish()
    }
}

/// What a command does.
#[derive(Clone)]
pub enum Action {
    Run(fn(&mut MeshFixerApp)),
    /// An operation spec (MF-SPEC), run the way the app offers it: its
    /// tool opens on the current target.
    Operation(String),
}

/// A command: menus, the keymap and (plan step 4) ⌘K list these.
#[derive(Clone)]
pub struct Command {
    pub id: String,
    /// Its title: a message id, or (an operation spec's) plain words.
    pub title: Title,
    pub keywords: &'static [&'static str],
    pub shortcut: Option<egui::KeyboardShortcut>,
    pub enabled: fn(&MeshFixerApp) -> bool,
    pub action: Action,
}

/// A title: a message id, or words from elsewhere (an operation spec's
/// plain name, which has its own catalogue).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Title {
    Message(&'static str),
    Plain(String),
}

impl Title {
    pub fn text(&self) -> String {
        match self {
            Title::Message(id) => crate::i18n::tr(id, &[]),
            Title::Plain(s) => s.clone(),
        }
    }
}

/// Where a panel docks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dock {
    Left,
    Right,
    Bottom,
}

/// A docked panel (the Browser, History; plugin panels later). Panels in
/// one dock are tabs.
#[derive(Clone, Copy)]
pub struct Panel {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub dock: Dock,
    pub min_size: f32,
    pub ui: fn(&mut MeshFixerApp, &mut egui::Ui),
}

/// An overlay in the 3D view: its place in the draw order, its legend
/// entry (colour, name, meaning) and its switch.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub id: &'static str,
    pub z_order: u16,
    pub legend: LegendRow,
}

/// A section of the inspector card (no tool open).
#[derive(Clone, Copy)]
pub struct InspectorSection {
    pub id: &'static str,
    pub order: u16,
    pub ui: fn(&mut MeshFixerApp, &mut egui::Ui),
}

/// An action in the right-click menu on a Browser row (and later on the
/// model under the cursor).
#[derive(Clone, Copy)]
pub struct ContextAction {
    pub id: &'static str,
    pub applies_to: fn(&MeshFixerApp, &TreeItem) -> bool,
    /// Its title for this target ("Fill Hole 3"): also its accessible
    /// label.
    pub title: fn(&MeshFixerApp, &TreeItem) -> String,
    pub run: fn(&mut MeshFixerApp, &TreeItem),
}

/// A page of Settings.
#[derive(Clone, Copy)]
pub struct SettingsPage {
    pub id: &'static str,
    pub title: &'static str,
    pub ui: fn(&mut MeshFixerApp, &mut egui::Ui),
}

/// Why a contribution was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    /// A tool without an icon: every tool has a visual icon.
    MissingIcon(&'static str),
    /// An icon that isn't one icon-font glyph.
    UnknownIcon(&'static str),
    /// Two contributions of a kind with one id.
    DuplicateId(String),
    /// A name or title that isn't in the message catalogue.
    UnknownMessage(&'static str),
}

/// Every contribution, in registration order (each list is kept sorted
/// the way it is shown).
#[derive(Default)]
pub struct Shell {
    tools: Vec<Tool>,
    commands: Vec<Command>,
    panels: Vec<Panel>,
    overlays: Vec<Overlay>,
    inspector: Vec<InspectorSection>,
    context_actions: Vec<ContextAction>,
    settings: Vec<SettingsPage>,
}

/// True if `id` is in the message catalogue.
fn known_message(id: &str) -> bool {
    crate::i18n::has(id)
}

impl Shell {
    pub fn register_tool(&mut self, tool: Tool) -> Result<(), RegisterError> {
        let mut chars = tool.icon.chars();
        match (chars.next(), chars.next()) {
            (None, _) => return Err(RegisterError::MissingIcon(tool.id)),
            // Phosphor's glyphs are in the Private Use Area.
            (Some(c), None) if ('\u{E000}'..='\u{F8FF}').contains(&c) => {}
            _ => return Err(RegisterError::UnknownIcon(tool.id)),
        }
        for id in [tool.name, tool.help] {
            if !known_message(id) {
                return Err(RegisterError::UnknownMessage(id));
            }
        }
        if self.tools.iter().any(|t| t.id == tool.id) {
            return Err(RegisterError::DuplicateId(tool.id.to_owned()));
        }
        self.tools.push(tool);
        self.tools.sort_by_key(|t| (t.category, t.order));
        Ok(())
    }

    pub fn register_command(&mut self, command: Command) -> Result<(), RegisterError> {
        if let Title::Message(id) = command.title
            && !known_message(id)
        {
            return Err(RegisterError::UnknownMessage(id));
        }
        if self.commands.iter().any(|c| c.id == command.id) {
            return Err(RegisterError::DuplicateId(command.id));
        }
        self.commands.push(command);
        Ok(())
    }

    pub fn register_panel(&mut self, panel: Panel) -> Result<(), RegisterError> {
        if !known_message(panel.title) {
            return Err(RegisterError::UnknownMessage(panel.title));
        }
        if self.panels.iter().any(|p| p.id == panel.id) {
            return Err(RegisterError::DuplicateId(panel.id.to_owned()));
        }
        self.panels.push(panel);
        Ok(())
    }

    pub fn register_overlay(&mut self, overlay: Overlay) -> Result<(), RegisterError> {
        if self.overlays.iter().any(|o| o.id == overlay.id) {
            return Err(RegisterError::DuplicateId(overlay.id.to_owned()));
        }
        self.overlays.push(overlay);
        self.overlays.sort_by_key(|o| o.z_order);
        Ok(())
    }

    pub fn register_inspector_section(
        &mut self,
        section: InspectorSection,
    ) -> Result<(), RegisterError> {
        if self.inspector.iter().any(|s| s.id == section.id) {
            return Err(RegisterError::DuplicateId(section.id.to_owned()));
        }
        self.inspector.push(section);
        self.inspector.sort_by_key(|s| s.order);
        Ok(())
    }

    pub fn register_context_action(&mut self, action: ContextAction) -> Result<(), RegisterError> {
        if self.context_actions.iter().any(|a| a.id == action.id) {
            return Err(RegisterError::DuplicateId(action.id.to_owned()));
        }
        self.context_actions.push(action);
        Ok(())
    }

    pub fn register_settings_page(&mut self, page: SettingsPage) -> Result<(), RegisterError> {
        if !known_message(page.title) {
            return Err(RegisterError::UnknownMessage(page.title));
        }
        if self.settings.iter().any(|p| p.id == page.id) {
            return Err(RegisterError::DuplicateId(page.id.to_owned()));
        }
        self.settings.push(page);
        Ok(())
    }

    /// Every tool, in rail order (by category, then order).
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn tool(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn command(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn panels(&self, dock: Dock) -> impl Iterator<Item = &Panel> {
        self.panels.iter().filter(move |p| p.dock == dock)
    }

    pub fn overlays(&self) -> &[Overlay] {
        &self.overlays
    }

    pub fn inspector_sections(&self) -> &[InspectorSection] {
        &self.inspector
    }

    pub fn context_actions(&self) -> &[ContextAction] {
        &self.context_actions
    }

    pub fn settings_pages(&self) -> &[SettingsPage] {
        &self.settings
    }
}

/// The built-in shell: every built-in contribution, registered through the
/// same calls a plugin makes. Panics if one is refused: that is a bug in a
/// built-in, caught by any test that starts the app.
pub fn builtin() -> Shell {
    let mut shell = Shell::default();
    crate::tool_panels::register(&mut shell).expect("the built-in tools register");
    crate::shape_tools::register(&mut shell).expect("the Shape tools register");
    crate::panels::register(&mut shell).expect("the built-in panels register");
    register_commands(&mut shell).expect("the built-in commands register");
    shell
}

/// The app's commands, then one per tool ("open it") and one per
/// operation spec.
fn register_commands(shell: &mut Shell) -> Result<(), RegisterError> {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let cmd = |id: &str,
               title: &'static str,
               shortcut: Option<KeyboardShortcut>,
               enabled: fn(&MeshFixerApp) -> bool,
               run: fn(&mut MeshFixerApp)| Command {
        id: id.to_owned(),
        title: Title::Message(title),
        keywords: &[],
        shortcut,
        enabled,
        action: Action::Run(run),
    };
    let idle: fn(&MeshFixerApp) -> bool = |a| a.busy.is_none();
    for c in [
        cmd(
            "file.open",
            "cmd.open",
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::O)),
            idle,
            MeshFixerApp::open_dialog,
        ),
        cmd(
            "file.export",
            "cmd.export",
            None,
            |a| a.busy.is_none() && a.active_mesh().is_some(),
            MeshFixerApp::export_dialog,
        ),
        cmd(
            "file.save_log",
            "cmd.save_log",
            None,
            idle,
            MeshFixerApp::save_log_dialog,
        ),
        cmd(
            "file.replay",
            "cmd.replay",
            None,
            idle,
            MeshFixerApp::replay_dialog,
        ),
        cmd(
            "edit.undo",
            "cmd.undo",
            Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
            |a| a.busy.is_none() && a.session.can_undo(),
            MeshFixerApp::undo,
        ),
        cmd(
            "edit.redo",
            "cmd.redo",
            Some(KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Z,
            )),
            |a| a.busy.is_none() && a.session.can_redo(),
            MeshFixerApp::redo,
        ),
        cmd(
            "view.frame",
            "cmd.frame",
            Some(KeyboardShortcut::new(Modifiers::NONE, Key::F)),
            |_| true,
            MeshFixerApp::frame_target,
        ),
        cmd(
            "view.reset",
            "cmd.reset_view",
            Some(KeyboardShortcut::new(Modifiers::NONE, Key::Home)),
            |_| true,
            MeshFixerApp::fit_camera,
        ),
        cmd(
            "view.overlays",
            "view.overlays",
            None,
            |_| true,
            |a| a.show_overlays = !a.show_overlays,
        ),
    ] {
        shell.register_command(c)?;
    }
    let tools: Vec<(&'static str, &'static str, Option<Key>)> = shell
        .tools()
        .iter()
        .map(|t| (t.id, t.name, t.shortcut))
        .collect();
    for (id, name, key) in tools {
        shell.register_command(Command {
            id: format!("tool.{id}"),
            title: Title::Message(name),
            keywords: &[],
            shortcut: key.map(|k| KeyboardShortcut::new(Modifiers::NONE, k)),
            enabled: |_| true,
            action: Action::Operation(format!("tool:{id}")),
        })?;
    }
    // Every operation spec becomes a command (MF-SPEC).
    for spec in mf_spec::builtin::specs() {
        let title =
            crate::defects::operation_label(&spec.id, None).unwrap_or_else(|| spec.id.clone());
        shell.register_command(Command {
            id: format!("op.{}", spec.id),
            title: Title::Plain(title),
            keywords: &[],
            shortcut: None,
            enabled: |a| a.busy.is_none() && a.active_mesh().is_some(),
            action: Action::Operation(spec.id.clone()),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool without an icon is refused at registration (ui-plan.md §5),
    /// and so is one whose "icon" is text.
    #[test]
    fn a_tool_without_an_icon_is_rejected() {
        let mut shell = Shell::default();
        let t = Tool::new("x", "tool.select.name", "", Category::Arrange, 0);
        assert_eq!(shell.register_tool(t), Err(RegisterError::MissingIcon("x")));
        let t = Tool::new("x", "tool.select.name", "X", Category::Arrange, 0);
        assert_eq!(shell.register_tool(t), Err(RegisterError::UnknownIcon("x")));
        let t = Tool::new(
            "x",
            "no.such.name",
            crate::icons::SELECT,
            Category::Arrange,
            0,
        );
        assert_eq!(
            shell.register_tool(t),
            Err(RegisterError::UnknownMessage("no.such.name"))
        );
        let t = Tool::new(
            "x",
            "tool.select.name",
            crate::icons::SELECT,
            Category::Arrange,
            0,
        );
        assert_eq!(shell.register_tool(t), Ok(()));
        assert_eq!(
            shell.register_tool(t),
            Err(RegisterError::DuplicateId("x".into()))
        );
    }

    /// The built-ins register through the contracts: the rail lists the
    /// built tools in category order, each with an icon; every operation
    /// spec is a command; every tool is a command too.
    #[test]
    fn the_builtin_shell_registers_every_contribution() {
        let shell = builtin();
        let ids: Vec<&str> = shell.tools().iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [
                "select",
                "transform",
                "split",
                "combine",
                "make_solid",
                "repair",
                "inspect",
                "export"
            ]
        );
        assert!(
            shell
                .tools()
                .windows(2)
                .all(|w| w[0].category <= w[1].category)
        );
        for spec in mf_spec::builtin::specs() {
            assert!(
                shell.command(&format!("op.{}", spec.id)).is_some(),
                "{}",
                spec.id
            );
        }
        for t in shell.tools() {
            assert!(
                shell.command(&format!("tool.{}", t.id)).is_some(),
                "{}",
                t.id
            );
        }
        assert!(shell.panels(Dock::Right).count() >= 2);
        assert!(!shell.overlays().is_empty());
        assert!(!shell.inspector_sections().is_empty());
        assert!(!shell.context_actions().is_empty());
        assert!(!shell.settings_pages().is_empty());
    }
}
