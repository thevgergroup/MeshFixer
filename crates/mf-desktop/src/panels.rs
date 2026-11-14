//! The frame around the 3D view (docs/design/ui-plan.md §3): the menu bar,
//! the status bar, the right dock (the Browser and History, registered as
//! `shell` panels), the Overlays popover, Settings, and the app's windows.
//! The rail, the tool dock and the tools' panels are in `tool_panels.rs`.

use eframe::egui::{self, AtomExt as _, RichText};

use crate::app::{MeshFixerApp, NarrowFocus, TreeItem};
use crate::defects::{Defect, HoleGroup, count, defect_copy, thousands};
use crate::shell::{Dock, Overlay, Panel, RegisterError, Shell};
use crate::tools::Switch;
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

/// The right dock's panels.
pub const BROWSER: &str = "browser";
pub const HISTORY: &str = "history";

/// The Browser's width (points), and its range. Widened from 248 (fix
/// round 2026-11-13, owner screenshot: object names ellipsised far too
/// early, "cr…", "pr…", even with plenty of width to spare) — the
/// column grid `object_row`/`group_entry` reserve for the hover-only
/// icons (hide, lock, Solid/Hole, delete, reorder:
/// `HOVER_ACTIONS_WIDTH`, 114pt) is a fixed cost regardless of the
/// panel's own width, so at the old 248pt default it alone ate over
/// a third of the name's own space; at 320 the name gets roughly as
/// much room back as it lost, without touching how (or how reliably)
/// those icons' own hover and click detection works — unlike the row's
/// name button itself, raising the panel's width can't introduce a new
/// way for a click to land on the wrong control.
const BROWSER_WIDTH: f32 = 320.0;
const BROWSER_WIDTH_MAX: f32 = 420.0;

/// The built-in panels and overlays.
pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_panel(Panel {
        id: BROWSER,
        title: "browser.tab",
        icon: icons::BROWSER,
        dock: Dock::Right,
        min_size: 200.0,
        ui: browser,
    })?;
    shell.register_panel(Panel {
        id: HISTORY,
        title: "history.tab",
        icon: icons::HISTORY,
        dock: Dock::Right,
        min_size: 200.0,
        ui: history,
    })?;
    for (i, row) in crate::tools::default_legend().into_iter().enumerate() {
        let id = match row.switch {
            Some(Switch::Selection) => "selection",
            Some(Switch::Hover) => "hover",
            Some(Switch::Holes) => "holes",
            Some(Switch::HoleMarkers) => "hole_markers",
            Some(Switch::Cracks) => "cracks",
            Some(Switch::LoosePieces) => "loose_pieces",
            Some(Switch::Tangled) => "tangled",
            Some(Switch::RecentRepairs) => "recent_repairs",
            None => "openings",
        };
        shell.register_overlay(Overlay {
            id,
            z_order: i as u16 * 10,
            legend: row,
        })?;
    }
    Ok(())
}

/// What an empty document says, in the tree and the 3D view.
pub fn empty_state() -> String {
    format!(
        "Open a mesh ({}) or drop a file here",
        shortcut_text(egui::Modifiers::COMMAND, egui::Key::O)
    )
}

/// A shortcut as this platform writes it: "Cmd+Z" on macOS, "Ctrl+Z"
/// elsewhere (names, not symbols: the bundled fonts have no ⌘).
pub fn shortcut_text(modifiers: egui::Modifiers, key: egui::Key) -> String {
    shortcut_text_for(modifiers, key, cfg!(target_os = "macos"))
}

fn shortcut_text_for(modifiers: egui::Modifiers, key: egui::Key, mac: bool) -> String {
    let mods = egui::ModifierNames::NAMES.format(&modifiers, mac);
    if mods.is_empty() {
        key.name().to_owned()
    } else {
        format!("{mods}+{}", key.name())
    }
}

/// The menu bar: File, Edit, View, Tools and Help, one level deep, and
/// Settings at the right.
pub fn menu_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::top("menu")
        .frame(chrome_frame())
        .show_separator_line(false)
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(t!("menu.file"), |ui| {
                    for id in ["file.open", "file.export"] {
                        command_item(app, ui, id);
                    }
                    ui.separator();
                    for id in ["file.save_log", "file.replay"] {
                        command_item(app, ui, id);
                    }
                });
                ui.menu_button(t!("menu.edit"), |ui| {
                    for id in ["edit.undo", "edit.redo"] {
                        command_item(app, ui, id);
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            app.selection.is_some() || app.inspected.is_some(),
                            egui::Button::new("Clear selection").shortcut_text("Esc"),
                        )
                        .clicked()
                    {
                        app.clear_selection();
                        ui.close();
                    }
                });
                ui.menu_button(t!("menu.view"), |ui| {
                    command_item(app, ui, "view.overlays");
                    ui.separator();
                    // The shading presets (UI plan §4): one level, as radios.
                    for (preset, label) in [
                        (mf_render::ShadingPreset::Clay, t!("view.clay")),
                        (mf_render::ShadingPreset::Studio, t!("view.studio")),
                        (mf_render::ShadingPreset::XRay, t!("view.xray")),
                    ] {
                        ui.radio_value(&mut app.shading.preset, preset, label);
                    }
                    ui.checkbox(&mut app.shading.wireframe, t!("view.wireframe"));
                    ui.separator();
                    // Object colour (UI plan §11): each object a distinct
                    // muted shade, or every object the one clay colour.
                    for (on, label) in [
                        (true, t!("view.object_colors")),
                        (false, t!("view.clay_objects")),
                    ] {
                        ui.radio_value(&mut app.object_colors, on, label);
                    }
                    ui.separator();
                    ui.checkbox(&mut app.show_boundaries, "Show defects");
                    ui.checkbox(&mut app.shown.recent_repairs, "Highlight recent repairs");
                    ui.checkbox(&mut app.show_hole_markers, "Show hole markers")
                        .on_hover_text("A dot on each hole too small to see at this zoom");
                    ui.checkbox(&mut app.show_hole_overview, "Show hole overview")
                        .on_hover_text(
                            "Tint around every hole, and a dot on each one too small to see",
                        );
                    if ui
                        .add(
                            egui::Button::new(if app.side_panels_hidden {
                                "Show side panels"
                            } else {
                                "Hide side panels"
                            })
                            .shortcut_text(shortcut_text(
                                egui::Modifiers::COMMAND,
                                egui::Key::Backslash,
                            )),
                        )
                        .clicked()
                    {
                        app.toggle_side_panels();
                        ui.close();
                    }
                    ui.checkbox(&mut app.show_metrics, "Metrics")
                        .on_hover_text("Frame times, memory, GPU buffers and threads");
                    ui.separator();
                    for id in ["view.frame", "view.reset"] {
                        command_item(app, ui, id);
                    }
                    ui.separator();
                    let mut pseudo = crate::i18n::pseudo();
                    if ui
                        .checkbox(&mut pseudo, t!("view.pseudo"))
                        .on_hover_text(t!("settings.dev.pseudo.tip"))
                        .changed()
                    {
                        crate::i18n::set_pseudo(pseudo);
                    }
                });
                ui.menu_button(t!("menu.tools"), |ui| {
                    let tools: Vec<crate::shell::Tool> = app.shell.tools().to_vec();
                    for tool in tools {
                        let enabled =
                            (tool.availability)(app) == crate::shell::Availability::Available;
                        let name = crate::i18n::tr(tool.name, &[]);
                        let mut b = egui::Button::new((
                            RichText::new(tool.icon).font(icons::font(theme::ICON_SIZE)),
                            name.clone(),
                        ));
                        if let Some(k) = tool.shortcut {
                            b = b.shortcut_text(k.name());
                        }
                        let r = ui.add_enabled(enabled, b);
                        let label = format!("{name} tool");
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
                        if r.clicked() {
                            if tool.id == crate::tool_panels::TRANSFORM {
                                app.open_transform();
                            } else {
                                app.open_rail_tool(tool.id);
                            }
                            ui.close();
                        }
                    }
                });
                ui.menu_button(t!("menu.help"), |ui| {
                    if ui.button("Controls…").clicked() {
                        app.show_controls = true;
                        ui.close();
                    }
                    ui.separator();
                    // MF-RT-004: the thread budget, shown in diagnostics.
                    let pools = app.pool_sizes();
                    ui.weak(format!(
                        "Background work: {} of {} cores ({}): {} for the running job, {} for analysis and display",
                        pools.budget.threads,
                        pools.budget.cores,
                        pools.budget.reason,
                        pools.jobs,
                        pools.background
                    ));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = t!("menu.settings");
                    if widgets::icon_button(ui, icons::SETTINGS, &label, None, 22.0)
                        .on_hover_text(&label)
                        .clicked()
                    {
                        app.show_settings = !app.show_settings;
                    }
                });
            });
        });
}

/// A menu item for registered command `id`: its title and shortcut, run
/// when chosen.
fn command_item(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: &str) {
    let Some(c) = app.shell.command(id).cloned() else {
        return;
    };
    let mut b = egui::Button::new(c.title.text());
    if let Some(s) = c.shortcut {
        b = b.shortcut_text(ui.ctx().format_shortcut(&s));
    }
    if ui.add_enabled((c.enabled)(app), b).clicked() {
        app.run_command(&c);
        ui.close();
    }
}

pub fn error_banner(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(message) = app.error.clone() else {
        return;
    };
    egui::Panel::top("error")
        .frame(egui::Frame::new().fill(theme::ERROR_FILL).inner_margin(6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::WHITE, format!("Error: {message}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Dismiss").clicked() {
                        app.error = None;
                    }
                });
            });
        });
}

/// The frame of the bars around the view (menu, bottom bar, rail).
fn chrome_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::RAIL)
        .inner_margin(egui::Margin::symmetric(
            theme::SPACE_S as i8,
            theme::SPACE_XS as i8,
        ))
}

/// The status bar (one line): Undo and Redo, the message with a running
/// job's progress and Cancel inline, and the active object's stats at the
/// right.
pub fn status_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("status")
        .frame(chrome_frame())
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let idle = app.busy.is_none();
                let can_undo = idle && app.session.can_undo();
                let can_redo = idle && app.session.can_redo();
                if ui
                    .add_enabled_ui(can_undo, |ui| {
                        widgets::icon_button(ui, icons::UNDO, "Undo", None, 24.0)
                    })
                    .inner
                    .on_hover_text(format!("Undo ({})", undo_keys()))
                    .clicked()
                {
                    app.undo();
                }
                if ui
                    .add_enabled_ui(can_redo, |ui| {
                        widgets::icon_button(ui, icons::REDO, "Redo", None, 24.0)
                    })
                    .inner
                    .on_hover_text(format!("Redo ({})", redo_keys()))
                    .clicked()
                {
                    app.redo();
                }
                ui.add_space(theme::SPACE_S);
                ui.spacing_mut().item_spacing.x = theme::SPACE_S;
                let stats = stats_text(app);
                let stats_width = stats.as_ref().map_or(0.0, |s| {
                    ui.painter()
                        .layout_no_wrap(
                            s.clone(),
                            egui::FontId::proportional(theme::TYPE_CAPTION),
                            theme::TEXT_MUTED,
                        )
                        .size()
                        .x
                        + theme::SPACE_M
                });
                let message_width = (ui.available_width() - stats_width).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(message_width, ui.available_height()),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| status(app, ui),
                );
                if let Some(stats) = stats {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(stats)
                                .size(theme::TYPE_CAPTION)
                                .color(theme::TEXT_MUTED),
                        )
                        .on_hover_text(t!("status.stats.tip"));
                    });
                }
            });
        });
}

/// The active object's stats: triangles, and its size.
fn stats_text(app: &MeshFixerApp) -> Option<String> {
    let a = app.current_analysis()?;
    let tris = count(a.diagnostics.triangle_count, "triangle");
    let size = a.defects.world_size?;
    let u = crate::defects::unit_symbol(app.session.document().units());
    let m = crate::defects::measure;
    Some(t!(
        "status.stats",
        tris = tris,
        size = format!("{} × {} × {} {u}", m(size[0]), m(size[1]), m(size[2]))
    ))
}

/// The status message: what's running (its stage, percentage, a progress
/// bar and Cancel), or the last result, or "Ready"; the import's warnings
/// after it.
fn status(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if let Some(busy) = app.busy.clone() {
        job_status(app, ui, &busy);
        return;
    }
    let (text, spinning) = if app.is_analyzing() {
        ("Analysing…".to_owned(), true)
    } else if let Some(status) = &app.status {
        (status.clone(), false)
    } else {
        (t!("status.ready"), false)
    };
    if spinning {
        ui.spinner();
    }
    // The last import's warnings, readable whatever the status says (I2).
    if !app.warnings.is_empty() {
        let text = count(app.warnings.len(), "import warning");
        if ui
            .small_button(&text)
            .on_hover_text("Show what the import warned about")
            .clicked()
        {
            app.show_warnings = !app.show_warnings;
        }
    }
    ui.add(egui::Label::new(RichText::new(&text).color(theme::TEXT)).truncate())
        .on_hover_text(&text);
}

/// Width of the job's progress bar in the status line.
const PROGRESS_WIDTH: f32 = 120.0;

/// A running job: "<job> · <stage> <n>%" (MF-RT-003), a progress bar and
/// Cancel (or "Cancelling…"). Jobs that report no progress get a spinner.
fn job_status(app: &mut MeshFixerApp, ui: &mut egui::Ui, busy: &str) {
    let progress = app.job_progress().filter(|(stage, _)| !stage.is_empty());
    let text = match progress {
        Some((stage, fraction)) => {
            format!("{busy} · {} {:.0}%", stage_label(stage), fraction * 100.0)
        }
        None => {
            ui.spinner();
            busy.to_owned()
        }
    };
    // A fixed width for the words, so the bar and Cancel stay put while
    // the percentage counts up.
    let width = (ui.available_width() - PROGRESS_WIDTH - 100.0).clamp(120.0, 420.0);
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.available_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(egui::Label::new(RichText::new(&text).color(theme::TEXT)).truncate())
                .on_hover_text(&text);
        },
    );
    if let Some((_, fraction)) = progress {
        ui.add(
            egui::ProgressBar::new(fraction as f32)
                .desired_width(PROGRESS_WIDTH)
                .desired_height(6.0)
                .fill(theme::PROGRESS),
        );
    }
    if app.can_cancel() {
        if widgets::button(ui, Kind::Secondary, None, "Cancel", 0.0)
            .on_hover_text("Stop it (Esc); the document stays as it is")
            .clicked()
        {
            app.cancel_job();
        }
    } else if app.is_cancelling() {
        ui.weak("Cancelling…");
    }
}

/// A progress stage in plain words.
fn stage_label(stage: &str) -> String {
    match stage {
        "reading" => "reading the file".into(),
        "welding" => "joining corners".into(),
        "topology" => "finding edges".into(),
        "inspecting" => "checking".into(),
        "triangulating" => "filling".into(),
        "refining" => "adding detail to the patches".into(),
        "fairing" => "shaping the patches to the surface".into(),
        "removing" => "removing pieces".into(),
        mf_io::WRITING => "writing the file".into(),
        other => other.replace('_', " "),
    }
}

fn has_branches(app: &MeshFixerApp) -> bool {
    app.session.revisions().iter().any(|n| n.children.len() > 1)
}

/// The full revision DAG (MF-HIST-001): every branch, the current path in
/// normal text, abandoned branches greyed. Returns a clicked revision.
fn history_tree(app: &MeshFixerApp, ui: &mut egui::Ui) -> Option<mf_command::RevisionId> {
    let current = app.session.current();
    let mut on_path = std::collections::BTreeSet::new();
    let mut cur = Some(current);
    while let Some(r) = cur {
        on_path.insert(r);
        cur = app.session.node(r).and_then(|n| n.parent);
    }
    let mut clicked = None;
    let mut stack = vec![(mf_command::RevisionId(0), 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(node) = app.session.node(id) else {
            continue;
        };
        let mut text = RichText::new(format!("{} · {}", id.0, node.label));
        if !on_path.contains(&id) {
            text = text.color(theme::TEXT_MUTED).italics();
        }
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            if ui.selectable_label(id == current, text).clicked() {
                clicked = Some(id);
            }
        });
        for &c in node.children.iter().rev() {
            let child_depth = if node.children.len() > 1 {
                depth + 1
            } else {
                depth
            };
            stack.push((c, child_depth));
        }
    }
    clicked
}

/// Apply's question before a large removal (MF-ISSUE-052): plain words,
/// and a choice. Esc keeps the pieces.
pub fn confirm_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    let Some(message) = app.confirmation().map(str::to_owned) else {
        return;
    };
    let mut answer = None;
    // Modal: Apply and the tree behind it don't answer until it's answered.
    egui::Modal::new(egui::Id::unique("confirm removal")).show(ctx, |ui| {
        ui.set_max_width(360.0);
        ui.heading("Remove these pieces?");
        ui.label(&message);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Remove them").clicked() {
                answer = Some(true);
            }
            if ui
                .button("Keep them")
                .on_hover_text("Esc. To remove fewer, lower Max relative size or Max triangles.")
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    // Esc keeps them; a stray click outside the question doesn't answer it.
    if answer.is_none()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        answer = Some(false);
    }
    if let Some(go) = answer {
        app.confirm(go);
    }
}

/// The last replay's per-command verdicts.
pub fn replay_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    let Some(lines) = app.replay_report.clone() else {
        return;
    };
    let mut open = true;
    egui::Window::new("Replay report")
        .open(&mut open)
        .show(ctx, |ui| {
            for line in &lines {
                ui.label(line);
            }
        });
    if !open {
        app.replay_report = None;
    }
}

/// Help > Controls.
/// Identical warnings once each, with how many times they came, in the
/// order they first came: "528 × degenerate face skipped" (I2).
pub fn grouped_warnings(warnings: &[String]) -> Vec<String> {
    let mut groups: Vec<(&str, usize)> = Vec::new();
    let mut at: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for w in warnings {
        match at.get(w.as_str()) {
            Some(&i) => groups[i].1 += 1,
            None => {
                at.insert(w, groups.len());
                groups.push((w, 1));
            }
        }
    }
    groups
        .into_iter()
        .map(|(w, n)| {
            if n == 1 {
                w.to_owned()
            } else {
                format!("{} × {w}", thousands(n as u64))
            }
        })
        .collect()
}

/// The last import's warnings, grouped (the status bar's "N import
/// warnings" opens it).
pub fn warnings_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if !app.show_warnings || app.warnings.is_empty() {
        return;
    }
    let mut open = true;
    egui::Window::new("Import warnings")
        .open(&mut open)
        .collapsible(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for w in grouped_warnings(&app.warnings) {
                        ui.add(egui::Label::new(w).wrap());
                    }
                });
        });
    app.show_warnings = open;
}

/// The tooltip beside the pointer when it rests on a defect in the 3D
/// view (MF-RENDER-004): what it is and its size, in the same words as
/// its tree row (a hole's boundary edges and perimeter, a loose piece's
/// triangles, a tangled edge's description).
pub fn view_hover_tooltip(app: &MeshFixerApp, ctx: &egui::Context) {
    let (Some(hover), Some(at)) = (app.view_hover(), app.view_pointer()) else {
        return;
    };
    let units = app.session.document().units();
    let copy = match app.current_analysis() {
        Some(a) => a.defects.copy(&hover.defect, units),
        None => defect_copy(&hover.defect, units),
    };
    egui::Area::new(egui::Id::unique("view hover tooltip"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(at + egui::vec2(16.0, 16.0))
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.strong(&copy.title);
                ui.label(&copy.detail);
                ui.label(
                    RichText::new(format!("Click to select · Next: {}", copy.next_action))
                        .color(theme::TEXT_MUTED),
                );
            });
        });
}

/// View ▸ Metrics (MF-RT-005): plain-language labels with the engineering
/// numbers, from the app's cached snapshot (refreshed twice a second, so
/// drawing this is O(1) in the mesh's size).
pub fn metrics_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    use crate::metrics::{format_bytes as bytes, format_duration};
    if !app.show_metrics {
        return;
    }
    // The app repaints on demand: without this an idle app's panel would
    // freeze until the next input.
    ctx.request_repaint_after(crate::metrics::REFRESH_EVERY);
    let mut open = true;
    let pools = app.pool_sizes();
    let snapshot = app.metrics_snapshot().cloned();
    // Over the 3D view's top-left corner, clear of the side panels (the
    // view's rect from its last frame; before any, just right of the tree).
    let at = app.viewport.last_rect.map_or(
        egui::pos2(crate::tool_panels::RAIL_WIDTH + 16.0, 40.0),
        |view| view.right_top() + egui::vec2(-400.0, 12.0),
    );
    egui::Window::new("Metrics")
        .open(&mut open)
        .resizable(false)
        .collapsible(true)
        .default_width(380.0)
        .default_pos(at)
        .show(ctx, |ui| {
            let Some(m) = snapshot else {
                ui.label("Collecting…");
                return;
            };
            let muted = |ui: &mut egui::Ui, text: String| {
                ui.label(RichText::new(text).color(theme::TEXT_MUTED));
            };
            let stats = |s: &mf_metrics::DurationStats| {
                format!(
                    "p50 {:.1} ms · p95 {:.1} ms · p99 {:.1} ms",
                    s.p50_ms, s.p95_ms, s.p99_ms
                )
            };

            ui.strong("Frame times");
            let f = &m.frames;
            // What a frame costs leads; the redraw rate follows, said to be
            // one (the app repaints on demand, so idle gaps aren't slowness).
            ui.label(match &f.cpu {
                Some(c) => format!("CPU per frame: {}", stats(c)),
                None => "CPU per frame: no frames finished yet".into(),
            });
            ui.label(f.gpu.describe());
            ui.label(match (&f.continuous, f.fps) {
                (Some(c), Some(fps)) => format!(
                    "Redraw rate: {fps:.0} fps over {} back-to-back frames ({} idle gaps left \
                     out); the view only redraws when something changes, so this is not a \
                     speed figure",
                    c.frames, f.idle_gaps
                ),
                _ => "Redraw rate: idle; the view only redraws when something changes".into(),
            });
            muted(
                ui,
                format!(
                    "Over the last {} frames; GPU time comes from timestamp queries, \
                     which Metal often can't give for heavy frames",
                    f.cpu.as_ref().map_or(0, |c| c.frames)
                ),
            );
            ui.add_space(6.0);

            ui.strong("Operations");
            if m.jobs.is_empty() {
                muted(ui, "No operations yet".into());
            }
            for job in m.jobs.iter().rev() {
                ui.label(format!(
                    "{}: {}{}",
                    job.label,
                    format_duration(job.duration),
                    job.peak_above_start
                        .map_or(String::new(), |p| format!(" (heap peak +{})", bytes(p)))
                ));
            }
            ui.add_space(6.0);

            ui.strong("Memory");
            let mem = &m.memory;
            ui.label(format!("Mesh storage: {}", bytes(mem.mesh_storage_bytes)));
            ui.label(match mem.topology_bytes {
                Some(b) => format!("Cached topology: {}", bytes(b)),
                None => "Cached topology: not computed yet".into(),
            });
            ui.label(match mem.scratch_peak_bytes {
                Some(b) => format!("Last operation's peak: {} above its start", bytes(b)),
                None if m.jobs.is_empty() => "Last operation's peak: no operation yet".into(),
                None => "Last operation's peak: not counted (no counting allocator)".into(),
            });
            ui.label(match m.peak_heap_bytes {
                Some(b) => format!("Peak heap: {} since start", bytes(b)),
                None => "Peak heap: not counted (no counting allocator)".into(),
            });
            ui.label(match &mem.gpu {
                Some(g) => format!(
                    "GPU buffers: geometry {} · face states {} · outlines {} · view textures {}",
                    bytes(g.geometry),
                    bytes(g.state),
                    bytes(g.lines),
                    bytes(g.textures)
                ),
                None => "GPU buffers: none yet".into(),
            });
            ui.label(format!(
                "Uploaded by the last change: {} ({} of geometry since start)",
                bytes(m.last_upload_bytes),
                bytes(m.geometry_uploaded_bytes)
            ));
            const RSS: &str = "Resident memory (RSS, excludes swapped/compressed pages)";
            ui.label(match (&mem.process_rss_bytes, &mem.rss_error) {
                (Some(b), _) => format!("{RSS}: {}", bytes(*b)),
                (None, Some(e)) => format!("{RSS}: unavailable ({e})"),
                (None, None) => format!("{RSS}: measuring…"),
            });
            if let Some(b) = mem.phys_footprint_bytes {
                ui.label(format!(
                    "Memory footprint: {} (Activity Monitor's \"Memory\": compressed and \
                     swapped pages and GPU buffers included)",
                    bytes(b)
                ));
            }
            muted(
                ui,
                "Resident and footprint come from the OS; the figures above them are \
                 counted. Under memory pressure resident can be far below what the app holds"
                    .into(),
            );
            ui.add_space(6.0);

            ui.strong("Threads");
            ui.label(format!(
                "Thread budget: {} of {} cores ({}): {} for the running job, {} for analysis and display",
                pools.budget.threads,
                pools.budget.cores,
                pools.budget.reason,
                pools.jobs,
                pools.background
            ));
        });
    app.show_metrics = open;
}

fn undo_keys() -> String {
    shortcut_text(egui::Modifiers::COMMAND, egui::Key::Z)
}

fn redo_keys() -> String {
    shortcut_text(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::Z,
    )
}

pub fn controls_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    let mut open = app.show_controls;
    egui::Window::new("Controls")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            let reset = format!(
                "Home or {}",
                shortcut_text(egui::Modifiers::COMMAND, egui::Key::Num0)
            );
            let undo = format!("{} / {}", undo_keys(), redo_keys());
            let open = shortcut_text(egui::Modifiers::COMMAND, egui::Key::O);
            let group = |ui: &mut egui::Ui, title: &str, rows: &[(&str, &str)]| {
                ui.strong(title);
                egui::Grid::new(title).num_columns(2).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(*k);
                        ui.label(RichText::new(*v).color(theme::TEXT_MUTED));
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
            };
            group(
                ui,
                "Mouse",
                &[
                    ("Left-drag", "Orbit (or Pan / Zoom: the view's mode switch)"),
                    ("Right- or middle-drag, or Shift + drag", "Pan"),
                    ("Option/Alt + drag up/down", "Zoom"),
                    ("Wheel", "Zoom to the cursor"),
                    ("Click", "Select a hole or piece (Shift adds)"),
                ],
            );
            group(
                ui,
                "Trackpad",
                &[
                    ("Two-finger drag", "Orbit"),
                    ("Shift + two-finger drag", "Pan"),
                    ("Pinch", "Zoom"),
                ],
            );
            group(
                ui,
                "On screen (bottom centre of the view)",
                &[
                    ("Orbit / Pan / Zoom / Box select", "What a plain drag does"),
                    ("− / +", "Zoom out / in (hold to repeat)"),
                    ("Frame / Reset", "Frame the selection / reset the view"),
                ],
            );
            group(
                ui,
                "Shortcuts",
                &[
                    ("+ or = / −", "Zoom in / out"),
                    ("F", "Frame the selection (or everything)"),
                    (&reset, "Reset the view"),
                    (
                        "Esc",
                        "Cancel the running job; else a gizmo drag; else leave the tool; else \
                         clear the selection",
                    ),
                    (
                        "Q / T / R / I",
                        "Tools: Select / Transform / Repair / Inspect",
                    ),
                    ("S / C / M", "Shape tools: Split / Combine / Make solid"),
                    (
                        "Shift-click",
                        "Select another object too (in the view or the Browser)",
                    ),
                    ("H", "Switch the selected objects between Solid and Hole"),
                    (
                        "T, then W / E / R",
                        "Transform the object: Move / Rotate / Scale (Cmd/Ctrl while dragging \
                         turns snapping off)",
                    ),
                    (&undo, "Undo / Redo"),
                    ("N / Shift+N", "Select and frame the next / previous hole"),
                    (
                        "B, then drag",
                        "Select the visible holes in a box (Shift adds)",
                    ),
                    (&open, "Open a mesh (or drop a file on the window)"),
                ],
            );
        });
    app.show_controls = open;
}

/// True if the Browser is open now: the user's choice, else open unless
/// the document is one object with nothing to fix (ui-plan.md §3); on a
/// laptop-size window, not while a tool panel has the room.
pub fn browser_shown(app: &MeshFixerApp) -> bool {
    if app.side_panels_hidden {
        return false;
    }
    app.browser_open.unwrap_or_else(|| browser_default(app))
}

fn browser_default(app: &MeshFixerApp) -> bool {
    let objects = app.session.document().objects().len();
    // Several objects, or one the analysis couldn't read (the Browser
    // says why).
    if objects > 1 || app.analysis_failed_for_active() {
        return true;
    }
    app.current_analysis().is_some_and(|a| {
        !(a.defects.holes.is_empty()
            && a.defects.cracks.is_empty()
            && a.defects.loose.is_empty()
            && a.defects.tangled.is_empty())
    })
}

/// Open or close the Browser (its button, or the strip's). On a laptop
/// window, opening it gives it the tool panel's room.
pub fn set_browser(app: &mut MeshFixerApp, open: bool) {
    app.browser_open = Some(open);
    if open {
        app.side_panels_hidden = false;
        app.narrow_focus = NarrowFocus::Browser;
    } else {
        app.narrow_focus = NarrowFocus::Tool;
    }
}

/// The right dock: the Browser and History as tabs (registered panels),
/// collapsible to a strip of their icons. On a laptop-size window it gives
/// way to the tool panel; on a compact one it is a drawer over the view.
pub fn right_dock(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if app.side_panels_hidden {
        return;
    }
    // The default is decided once, when the first analysis is in (or
    // fails), so repairing everything doesn't fold the Browser away.
    if app.browser_open.is_none()
        && (app.current_analysis().is_some()
            || app.analysis_failed_for_active()
            || app.session.document().objects().len() > 1)
    {
        app.browser_open = Some(browser_default(app));
    }
    let width = ui.ctx().content_rect().width();
    let narrow = width < crate::tool_panels::NARROW_WINDOW;
    let compact = width < crate::tool_panels::COMPACT_WINDOW;
    let tool_open = crate::tool_panels::tool_panel_shown(app);
    let open =
        browser_shown(app) && !(narrow && tool_open && app.narrow_focus == NarrowFocus::Tool);
    let panels: Vec<Panel> = app.shell.panels(Dock::Right).copied().collect();
    if !open {
        egui::Panel::right("right dock strip")
            .resizable(false)
            .exact_size(crate::tool_panels::STRIP_WIDTH)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(4))
            .show(ui, |ui| {
                let show = t!("browser.show");
                for p in &panels {
                    let title = crate::i18n::tr(p.title, &[]);
                    let label = if p.id == BROWSER {
                        show.clone()
                    } else {
                        format!("Show {title}")
                    };
                    if widgets::icon_button(ui, p.icon, &label, None, 28.0)
                        .on_hover_text(&title)
                        .clicked()
                    {
                        app.right_tab = p.id;
                        set_browser(app, true);
                    }
                }
            });
        return;
    }
    let body = |app: &mut MeshFixerApp, ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for p in &panels {
                let title = crate::i18n::tr(p.title, &[]);
                let on = app.right_tab == p.id;
                if widgets::tab(ui, p.icon, &title, on).clicked() {
                    app.right_tab = p.id;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = t!("browser.hide");
                if widgets::icon_button(ui, icons::PANEL, &label, None, 24.0)
                    .on_hover_text(t!("browser.tip"))
                    .clicked()
                {
                    set_browser(app, false);
                }
            });
        });
        ui.add_space(theme::SPACE_XS);
        ui.separator();
        let panel = panels
            .iter()
            .find(|p| p.id == app.right_tab)
            .or(panels.first())
            .copied();
        if let Some(p) = panel {
            (p.ui)(app, ui);
        }
    };
    if compact {
        // A drawer over the view's right edge.
        let Some(view) = app.viewport.last_rect else {
            return;
        };
        let rect = egui::Rect::from_min_max(
            egui::pos2(view.right() - BROWSER_WIDTH, view.top()),
            view.right_bottom(),
        );
        egui::Area::new(egui::Id::unique("browser drawer"))
            .order(egui::Order::Middle)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(egui::Stroke::new(1.0, theme::SEPARATOR))
                    .inner_margin(theme::SPACE_S)
                    .show(ui, |ui| {
                        ui.set_min_size(
                            rect.size() - egui::vec2(2.0 * theme::SPACE_S, 2.0 * theme::SPACE_S),
                        );
                        ui.set_max_width(rect.width() - 2.0 * theme::SPACE_S);
                        body(app, ui);
                    });
            });
        return;
    }
    egui::Panel::right("right dock")
        .resizable(true)
        .default_size(BROWSER_WIDTH)
        .size_range(200.0..=BROWSER_WIDTH_MAX)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(theme::SPACE_S as i8)),
        )
        .show(ui, |ui| body(app, ui));
}

/// The Browser: every object; under the active one, its model and what
/// the analysis found, grouped with counts as badges. A row's actions are
/// in its right-click menu (registered context actions) and in Repair;
/// an object row also shows a Solid/Hole toggle at its right end, but
/// only on hover, so rows stay clean otherwise.
fn browser(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .id_salt("tree")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Long rows are cut short ("…"), never widening the panel (and
            // so shrinking the view, I5): tooltips have them all.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.spacing_mut().item_spacing.y = 0.0;
            tree(app, ui)
        });
}

/// History: every step of the current path (click one to go back to it;
/// the current one is highlighted, steps Redo would take are greyed),
/// any other branches, and named checkpoints as labelled markers
/// (right-click a row, "Name this point…"; clicking the marker checks
/// out that revision, same as clicking the row itself — Task "Project
/// organisation" step 3).
fn history(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let idle = app.busy.is_none();
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("history")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            let current = app.session.current();
            let rows = app.grouped_breadcrumb();
            if rows.is_empty() {
                widgets::caption(ui, t!("history.empty"));
            }
            for row in rows {
                match row {
                    crate::app::BreadcrumbRow::Run {
                        ids,
                        label,
                        reached,
                    } => {
                        history_run_row(app, ui, &ids, &label, reached, &mut clicked);
                    }
                    crate::app::BreadcrumbRow::Single {
                        id,
                        label,
                        details,
                        reached,
                    } => {
                        history_step_row(
                            app,
                            ui,
                            idle,
                            current,
                            id,
                            label,
                            details,
                            reached,
                            &mut clicked,
                        );
                    }
                    crate::app::BreadcrumbRow::CollapseControl { .. } => {
                        if widgets::icon_button(
                            ui,
                            icons::CARET_DOWN,
                            "Collapse this run",
                            None,
                            theme::ROW_HEIGHT,
                        )
                        .on_hover_text("Collapse back into one row")
                        .clicked()
                        {
                            app.expand_run(None);
                        }
                    }
                }
            }
            if has_branches(app) {
                ui.menu_button("Branches…", |ui| {
                    if let Some(id) = history_tree(app, ui) {
                        clicked = Some(id);
                        ui.close();
                    }
                });
            }
        });
    if let Some(id) = clicked {
        app.checkout(id);
    }
}

/// One plain step of the History panel (not part of a collapsed run):
/// its label (a named checkpoint shows as "[name]" instead), a
/// right-click menu to name or rename its checkpoint, and a click
/// that checks it out.
#[allow(clippy::too_many_arguments)]
fn history_step_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    idle: bool,
    current: mf_command::RevisionId,
    id: mf_command::RevisionId,
    label: String,
    details: String,
    reached: bool,
    clicked: &mut Option<mf_command::RevisionId>,
) {
    if app
        .checkpoint_naming
        .as_ref()
        .is_some_and(|(r, _)| *r == id)
    {
        checkpoint_name_row(app, ui, id);
        return;
    }
    let checkpoint = app.session.checkpoint_name(id).map(str::to_owned);
    let shown = match &checkpoint {
        Some(name) => format!("[{name}]"),
        None => label,
    };
    let text = if reached {
        RichText::new(&shown)
    } else {
        RichText::new(&shown).color(theme::TEXT_MUTED).italics()
    };
    let w = ui.available_width();
    let tip = if checkpoint.is_some() {
        format!("Named checkpoint · {}", t!("history.tip"))
    } else {
        t!("history.tip")
    };
    let response = ui.add_enabled(
        idle,
        egui::Button::selectable(id == current, text).min_size(egui::vec2(w, theme::ROW_HEIGHT)),
    );
    response.context_menu(|ui| {
        let title = if checkpoint.is_some() {
            "Rename this point…"
        } else {
            "Name this point…"
        };
        if ui.button(title).clicked() {
            // allow-swallow: an unnamed point starts with an empty name field
            app.checkpoint_naming = Some((id, checkpoint.clone().unwrap_or_default()));
            ui.close();
        }
        if checkpoint.is_some() && ui.button("Remove checkpoint name").clicked() {
            app.name_checkpoint(id, String::new());
            ui.close();
        }
    });
    if response.on_hover_text(tip).clicked() {
        *clicked = Some(id);
    }
    // Params are hidden by default, behind this expandable Details
    // section (owner 2026-11-13): the label alone is the common case, and
    // a face-id list or a 12-number transform is never shown raw.
    if !details.is_empty() {
        ui.indent(("history-details", id), |ui| {
            egui::CollapsingHeader::new("Details")
                .id_salt(("history-details-header", id))
                .default_open(false)
                .show(ui, |ui| {
                    for line in details.split('\n') {
                        widgets::caption(ui, line);
                    }
                });
        });
    }
}

/// A collapsed run of consecutive small steps on the same object
/// ("Sculpted head (24 strokes)", Task "Project organisation" step 3):
/// clicking it checks out its last revision, same as an uncollapsed
/// run's last row would; its own caret expands it into its individual
/// rows.
fn history_run_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    ids: &[mf_command::RevisionId],
    label: &str,
    reached: bool,
    clicked: &mut Option<mf_command::RevisionId>,
) {
    let idle = app.busy.is_none();
    let current = app.session.current();
    let last = *ids.last().expect("a run is never empty");
    let w = ui.available_width();
    let text = if reached {
        RichText::new(label)
    } else {
        RichText::new(label).color(theme::TEXT_MUTED).italics()
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.scope(|ui| {
            ui.set_width(18.0);
            ui.set_height(theme::ROW_HEIGHT);
            if widgets::icon_button(ui, icons::CARET_RIGHT, "Expand this run", None, 18.0)
                .on_hover_text("Show each step")
                .clicked()
            {
                app.expand_run(Some(ids[0]));
            }
        });
        ui.scope(|ui| {
            ui.set_width(w - 18.0);
            let response = ui.add_enabled(
                idle,
                egui::Button::selectable(ids.contains(&current), text)
                    .min_size(egui::vec2(w - 18.0, theme::ROW_HEIGHT)),
            );
            if response
                .on_hover_text(format!(
                    "{} · click to go to the last of them",
                    t!("history.tip")
                ))
                .clicked()
            {
                *clicked = Some(last);
            }
        });
    });
}

/// A history row while its checkpoint name is being edited: a text field
/// pre-filled with its current name (empty for a new one), committing
/// on Enter or losing focus and cancelling on Esc.
fn checkpoint_name_row(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: mf_command::RevisionId) {
    let mut text = app
        .checkpoint_naming
        .as_ref()
        .filter(|(r, _)| *r == id)
        .map_or_else(String::new, |(_, t)| t.clone());
    let text_id = egui::Id::unique(("checkpoint name field", id));
    let r = ui.add_sized(
        egui::vec2(ui.available_width(), theme::ROW_HEIGHT),
        egui::TextEdit::singleline(&mut text)
            .id(text_id)
            .hint_text("Checkpoint name"),
    );
    if r.changed() {
        app.checkpoint_naming = Some((id, text.clone()));
    }
    let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let committed = r.lost_focus() && !escaped;
    if !r.has_focus() && !committed {
        ui.memory_mut(|m| m.request_focus(text_id));
    }
    if committed {
        app.checkpoint_naming = None;
        app.name_checkpoint(id, text);
    } else if escaped {
        app.checkpoint_naming = None;
    }
}

/// View ▸ Overlays: a compact popover at the view's top-right listing
/// what each colour means, in two titled groups: the model's own colours
/// ("Model") and the overlay meanings with their switches ("Overlays";
/// the open tool's own rows while it supplies them). View > Show defects
/// hides every defect overlay at once.
pub fn overlays_popover(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if !app.show_overlays {
        return;
    }
    let Some(view) = app.viewport.last_rect else {
        return;
    };
    let mut close = false;
    egui::Area::new(egui::Id::unique("overlays popover"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(view.right_top() + egui::vec2(-theme::SPACE_M, theme::SPACE_M))
        .constrain_to(view)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::CARD)
                .stroke(egui::Stroke::new(1.0, theme::SEPARATOR))
                .corner_radius(theme::RADIUS)
                .inner_margin(theme::SPACE_M)
                .show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.horizontal(|ui| {
                        group_heading(ui, &t!("overlays.model"), "Model colours");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::CLOSE, "Close overlays", None, 22.0)
                                .clicked()
                            {
                                close = true;
                            }
                        });
                    });
                    swatch_row(
                        ui,
                        theme::SURFACE,
                        &t!("overlays.model.surface"),
                        &t!("overlays.model.surface.tip"),
                    );
                    swatch_row(
                        ui,
                        theme::BACK_FACE,
                        &t!("overlays.model.back"),
                        &t!("overlays.model.back.tip"),
                    );
                    ui.add_space(theme::SPACE_S);
                    group_heading(ui, &t!("overlays.overlays"), "Overlay colours");
                    let defects = app.show_boundaries;
                    for row in app.legend_rows() {
                        ui.horizontal(|ui| {
                            swatch(ui, row.color);
                            match row.switch {
                                Some(switch) => {
                                    let enabled = defects || !switch.is_defect();
                                    ui.add_enabled(
                                        enabled,
                                        egui::Checkbox::new(app.switch_mut(switch), row.name),
                                    )
                                    .on_hover_text(row.meaning)
                                    .on_disabled_hover_text("Hidden: View > Show defects is off");
                                }
                                None => {
                                    ui.add_space(
                                        ui.spacing().icon_width + ui.spacing().icon_spacing,
                                    );
                                    ui.label(row.name).on_hover_text(row.meaning);
                                }
                            }
                        });
                    }
                    ui.add_space(theme::SPACE_S);
                    ui.checkbox(
                        &mut app.show_holes_through_solids,
                        "Show holes through solids",
                    )
                    .on_hover_text(
                        "A buried or backside hole shows as a faint ghost through the solid",
                    );
                });
        });
    if close {
        app.show_overlays = false;
    }
}

/// A group's heading in the Overlays popover: its visible title, and an
/// accessible name of its own (the nav bar's button is "Overlays").
fn group_heading(ui: &mut egui::Ui, title: &str, label: &str) {
    let r = ui.label(
        RichText::new(title)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    let label = label.to_owned();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
}

fn swatch(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}

fn swatch_row(ui: &mut egui::Ui, color: egui::Color32, name: &str, tip: &str) {
    ui.horizontal(|ui| {
        swatch(ui, color);
        ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing - 6.0);
        ui.label(name).on_hover_text(tip);
    });
}

/// Settings: a page per registered settings page.
pub fn settings_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    egui::Window::new(t!("settings.title"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .show(ctx, |ui| {
            let pages: Vec<crate::shell::SettingsPage> = app.shell.settings_pages().to_vec();
            for page in pages {
                ui.label(
                    RichText::new(crate::i18n::tr(page.title, &[]))
                        .size(theme::TYPE_SECTION)
                        .color(theme::TEXT_STRONG),
                );
                (page.ui)(app, ui);
                ui.add_space(theme::SPACE_S);
            }
        });
    app.show_settings = open;
}

/// Search/filter field at the top of the Browser (project organisation
/// brief, step 2): filters rows by name, case-insensitively. A group
/// that doesn't match by its own name still shows if any descendant does
/// (so filtering never hides a match inside a closed-looking folder).
fn browser_search(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::SPACE_XS;
        ui.label(RichText::new(icons::INSPECT).font(icons::font(theme::ICON_SIZE - 4.0)));
        ui.add_sized(
            egui::vec2(ui.available_width(), 22.0),
            egui::TextEdit::singleline(&mut app.browser_search)
                .hint_text("Search")
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(theme::SPACE_XS);
}

/// `name` matches the Browser's search field (always true when it's
/// empty).
fn matches_search(app: &MeshFixerApp, name: &str) -> bool {
    app.browser_search.is_empty()
        || name
            .to_lowercase()
            .contains(&app.browser_search.to_lowercase())
}

/// True if `entry` (or, for a group, any of its descendants) matches the
/// search field.
fn entry_matches_search(app: &MeshFixerApp, entry: mf_command::Entry) -> bool {
    match entry {
        mf_command::Entry::Object(id) => app
            .session
            .document()
            .object(id)
            .is_some_and(|o| matches_search(app, &o.name)),
        mf_command::Entry::Group(g) => {
            let org = app.session.document().organisation();
            let Some(group) = org.group(g) else {
                return false;
            };
            matches_search(app, &group.name)
                || group.order.iter().any(|&e| entry_matches_search(app, e))
        }
    }
}

fn tree(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    browser_search(app, ui);
    let empty = app.session.document().objects().is_empty();
    if empty {
        ui.add_space(theme::SPACE_S);
        widgets::caption(ui, t!("browser.empty"));
        return;
    }
    let root = app.session.document().organisation().root_order().to_vec();
    entries_tree(app, ui, &root, None);
}

/// One level of the Browser tree: every entry of `entries` (the root's,
/// or one group's `order`). `into` is the group these entries belong to
/// (`None`: the root), for each row's Move up/down and Move into/out of
/// group context actions (project organisation brief, step 2: reordering
/// and moving between groups).
fn entries_tree(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    entries: &[mf_command::Entry],
    into: Option<mf_command::GroupId>,
) {
    let shown: Vec<mf_command::Entry> = entries
        .iter()
        .copied()
        .filter(|&e| entry_matches_search(app, e))
        .collect();
    let n = shown.len();
    for (shown_index, &entry) in shown.iter().enumerate() {
        // The real index in `entries` (not `shown`), so Move up/down
        // target the unfiltered order the document actually keeps.
        // allow-swallow: shown is filtered from entries, so every entry is found
        let index = entries.iter().position(|&e| e == entry).unwrap_or(0);
        let reorder = Reorder {
            into,
            index,
            at_start: shown_index == 0,
            at_end: shown_index + 1 == n,
        };
        match entry {
            mf_command::Entry::Object(id) => object_entry(app, ui, id, reorder),
            mf_command::Entry::Group(g) => group_entry(app, ui, g, reorder),
        }
    }
}

/// Reserves exactly `size` of space for a hover-only control, painted or
/// not, so showing or hiding it on hover never changes how much room the
/// row around it takes (owner 2026-11-12: the hover buttons used to push
/// and shift the row's content sideways). `draw` gets a child `Ui` capped
/// to `size`, so even a control whose natural size would otherwise differ
/// (a real `menu_button`'s padding, say) can't widen the slot.
fn fixed_slot(ui: &mut egui::Ui, size: egui::Vec2, draw: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        draw(&mut child);
    }
}

/// A fixed-width reorder trigger (a caret that opens the Move up/down/out
/// menu): painted like [`widgets::icon_button`] rather than a real
/// `menu_button`, whose padding would otherwise make it wider than its
/// [`MOVE_MENU_WIDTH`] slot and shift the row when it appears on hover.
fn reorder_trigger(
    ui: &mut egui::Ui,
    label: &str,
    size: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let button = egui::Button::new(RichText::new(icons::CARET_DOWN).font(icons::font(12.0)))
        .frame(false)
        .min_size(egui::vec2(size, size));
    let (response, _) =
        egui::containers::menu::MenuButton::from_button(button).ui(ui, add_contents);
    let owned = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &owned));
    response.on_hover_text(label)
}

/// Where a row sits among its siblings, for its Move up/down actions:
/// the group it's in (`None`: the root), its index there, and whether
/// it's first or last among the rows currently shown (so the actions
/// that would do nothing are left off the menu).
#[derive(Debug, Clone, Copy)]
struct Reorder {
    into: Option<mf_command::GroupId>,
    index: usize,
    at_start: bool,
    at_end: bool,
}

/// "Move up" / "Move down" (reorder within the same group or the root)
/// and, inside a group, "Move out of group" (to just after the group, at
/// the root): the Browser row's right-click menu (project organisation
/// brief, step 2). Plain, explicit actions rather than a pointer drag,
/// so reordering is as reliable and accessible as every other action
/// here.
fn reorder_actions(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    entry: mf_command::Entry,
    r: Reorder,
) {
    if !r.at_start
        && ui
            .add_enabled(app.busy.is_none(), egui::Button::new("Move up"))
            .clicked()
    {
        app.move_entry(entry, r.into, r.index.saturating_sub(1));
        ui.close();
    }
    if !r.at_end
        && ui
            .add_enabled(app.busy.is_none(), egui::Button::new("Move down"))
            .clicked()
    {
        app.move_entry(entry, r.into, r.index + 2);
        ui.close();
    }
    if let Some(g) = r.into
        && ui
            .add_enabled(app.busy.is_none(), egui::Button::new("Move out of group"))
            .clicked()
    {
        let outer = app
            .session
            .document()
            .organisation()
            .parent_of(mf_command::Entry::Group(g));
        let end = match outer {
            Some(o) => app
                .session
                .document()
                .organisation()
                .group(o)
                .map_or(0, |p| p.order.len()),
            None => app.session.document().organisation().root_order().len(),
        };
        app.move_entry(entry, outer, end);
        ui.close();
    }
}

fn object_entry(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    id: mf_command::ObjectId,
    reorder: Reorder,
) {
    let active = app.active == Some(id);
    let hole = app.is_hole(id);
    let Some(name) = app.session.document().object(id).map(|o| o.name.clone()) else {
        return;
    };
    let tris = app
        .session
        .document()
        .object(id)
        .and_then(|o| o.mesh())
        .map_or(0, |m| m.triangle_count());
    // The accessible label says the role too ("peg (hole)").
    let label = if hole {
        t!("role.row_hole", name = name)
    } else {
        name.clone()
    };
    let text = if active {
        RichText::new(&name).color(theme::TEXT_STRONG)
    } else {
        RichText::new(&name).color(theme::TEXT_MUTED)
    };
    let tip = format!(
        "{}{} · Shift-click to select it too · double-click to frame it · F2 to rename · \
         right-click for more; the reorder menu moves it",
        count(tris, "triangle"),
        if active {
            ""
        } else {
            " · click to work on it"
        }
    );
    object_row(app, ui, id, &name, text, &label, &tip, reorder);
    if app.tree_expanded() == Some(id) {
        ui.indent(("defects", id.0), |ui| defects_tree(app, ui, id));
    }
}

/// A group (folder) row: its own icon and name, a swatch-free hide/lock
/// pair acting on every member, and its own members indented underneath
/// (always expanded; collapsing is left for later). Clicking the row
/// selects every member (project organisation brief, step 2).
fn group_entry(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    g: mf_command::GroupId,
    reorder: Reorder,
) {
    let Some(group) = app.session.document().organisation().group(g).cloned() else {
        return;
    };
    let members = app.session.document().organisation().members(g);
    let any_hidden = members.iter().any(|&id| app.is_hidden(id));
    let any_locked = members.iter().any(|&id| app.is_locked(id));
    let full_width = ui.available_width();
    let row_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(full_width, theme::ROW_HEIGHT));
    let row_hovered = ui.rect_contains_pointer(row_rect);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // A group has no collapse chevron yet either (it's always
        // expanded): the slot stays, empty, for the same grid as an
        // object row's and a defect category's.
        fixed_slot(ui, egui::vec2(CHEVRON_WIDTH, theme::ROW_HEIGHT), |_| {});
        fixed_slot(ui, egui::vec2(SWATCH_WIDTH, theme::ROW_HEIGHT), |_| {});
        ui.scope(|ui| {
            ui.set_width(
                (full_width - CHEVRON_WIDTH - SWATCH_WIDTH - HOVER_ACTIONS_WIDTH).max(0.0),
            );
            let selected = app.tree_selected(&TreeItem::Group(g));
            tree_row(
                app,
                ui,
                &TreeItem::Group(g),
                Row {
                    icon: Some((icons::GROUP, theme::TEXT_MUTED)),
                    text: if selected {
                        RichText::new(&group.name).color(theme::TEXT_STRONG)
                    } else {
                        RichText::new(&group.name).color(theme::TEXT_MUTED)
                    },
                    size: None,
                    label: &group.name,
                },
                |ui| {
                    ui.label(format!(
                        "{} · click to select every member",
                        count(members.len(), "object")
                    ));
                },
            );
        });
        let hide_label = format!(
            "{} group {}",
            if any_hidden { "Show" } else { "Hide" },
            group.name
        );
        fixed_slot(ui, egui::vec2(HIDE_TOGGLE_WIDTH, theme::ROW_HEIGHT), |ui| {
            if (row_hovered || any_hidden)
                && widgets::icon_button(
                    ui,
                    if any_hidden {
                        icons::HIDDEN
                    } else {
                        icons::SHOWN
                    },
                    &hide_label,
                    None,
                    HIDE_TOGGLE_WIDTH,
                )
                .on_hover_text(if any_hidden { "Show" } else { "Hide" })
                .clicked()
            {
                app.set_hidden(&members, !any_hidden);
            }
        });
        let lock_label = format!(
            "{} group {}",
            if any_locked { "Unlock" } else { "Lock" },
            group.name
        );
        fixed_slot(ui, egui::vec2(LOCK_TOGGLE_WIDTH, theme::ROW_HEIGHT), |ui| {
            if (row_hovered || any_locked)
                && widgets::icon_button(
                    ui,
                    if any_locked {
                        icons::LOCKED
                    } else {
                        icons::UNLOCKED
                    },
                    &lock_label,
                    None,
                    LOCK_TOGGLE_WIDTH,
                )
                .on_hover_text(if any_locked { "Unlock" } else { "Lock" })
                .clicked()
            {
                app.set_locked(&members, !any_locked);
            }
        });
        // No role toggle or delete button for a group: the space still
        // sits there, so the move menu lines up with every object row's.
        fixed_slot(
            ui,
            egui::vec2(ROLE_TOGGLE_WIDTH + DELETE_BUTTON_WIDTH, theme::ROW_HEIGHT),
            |_| {},
        );
        let group_name = group.name.clone();
        fixed_slot(ui, egui::vec2(MOVE_MENU_WIDTH, theme::ROW_HEIGHT), |ui| {
            if row_hovered {
                reorder_trigger(
                    ui,
                    &format!("Reorder group {group_name}"),
                    MOVE_MENU_WIDTH,
                    |ui| reorder_actions(app, ui, mf_command::Entry::Group(g), reorder),
                );
            }
        });
    });
    ui.indent(("group", g.0), |ui| {
        entries_tree(app, ui, &group.order, Some(g));
    });
}

/// The width `object_row` reserves for its Solid/Hole toggle.
const ROLE_TOGGLE_WIDTH: f32 = 26.0;
/// The width it reserves for its Delete (trash) button, right of the
/// role toggle (owner 2026-11-12: "Delete a split part... using both
/// the Backspace key and the trash button").
const DELETE_BUTTON_WIDTH: f32 = 26.0;
/// The width it reserves for its colour swatch (project organisation
/// brief, step 2): always shown, so the viewport's per-object colour has
/// a legend right on the row. Shared with `widgets::disclosure`'s icon
/// slot (`theme::ROW_SWATCH_WIDTH`), so a row's icon or swatch starts at
/// the same x whatever kind of row it is.
const SWATCH_WIDTH: f32 = theme::ROW_SWATCH_WIDTH;
/// The width it reserves for its show/hide toggle, left of the lock
/// toggle: both hover-only, like the role and delete buttons.
const HIDE_TOGGLE_WIDTH: f32 = 22.0;
/// The width it reserves for its lock toggle.
const LOCK_TOGGLE_WIDTH: f32 = 22.0;
/// The width it reserves for its reorder menu (Move up/down/out of
/// group: project organisation brief, step 2).
const MOVE_MENU_WIDTH: f32 = 18.0;
/// The disclosure chevron slot before the swatch (`theme::ROW_CHEVRON_WIDTH`):
/// empty for a row with no chevron, like an object's or a group's, so
/// every row's swatch or icon still lines up with a defect category's.
const CHEVRON_WIDTH: f32 = theme::ROW_CHEVRON_WIDTH;
/// The total width every row reserves, right of its name, for the hover
/// actions (hide, lock, Solid/Hole, delete, reorder): always this much,
/// painted or not, so hovering never changes a row's width (project
/// organisation brief; fix round 2026-11-12).
const HOVER_ACTIONS_WIDTH: f32 = HIDE_TOGGLE_WIDTH
    + LOCK_TOGGLE_WIDTH
    + ROLE_TOGGLE_WIDTH
    + DELETE_BUTTON_WIDTH
    + MOVE_MENU_WIDTH;

/// `id`'s row while it is being renamed (F2, or the "Rename…" context
/// menu item): a text field at the row's height, pre-filled with its
/// current name, committing on Enter or losing focus
/// (`MeshFixerApp::rename_object`) and cancelling on Esc.
fn rename_row(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: mf_command::ObjectId) {
    let mut text = app
        .renaming
        .as_ref()
        .filter(|(o, _)| *o == id)
        .map_or_else(String::new, |(_, t)| t.clone());
    let text_id = egui::Id::unique(("browser rename field", id));
    ui.set_height(theme::ROW_HEIGHT);
    let r = ui.add_sized(
        egui::vec2(ui.available_width(), theme::ROW_HEIGHT),
        egui::TextEdit::singleline(&mut text).id(text_id),
    );
    if r.changed() {
        app.renaming = Some((id, text.clone()));
    }
    let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let committed = r.lost_focus() && !escaped;
    if !r.has_focus() && !committed {
        ui.memory_mut(|m| m.request_focus(text_id));
    }
    if committed {
        app.renaming = None;
        app.rename_object(id, text);
    } else if escaped {
        app.renaming = None;
    }
}

/// An object row (the tree row plus, at its right end, a Solid/Hole
/// toggle): the toggle only shows while the row (or the toggle itself) is
/// hovered, so rows stay clean otherwise. One click is one undo step
/// ([`MeshFixerApp::toggle_role`]), the same change H and the Browser's
/// menu make.
#[allow(clippy::too_many_arguments)]
fn object_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    id: mf_command::ObjectId,
    name: &str,
    text: RichText,
    label: &str,
    tip: &str,
    reorder: Reorder,
) {
    let hole = app.is_hole(id);
    let full_width = ui.available_width();
    let row_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(full_width, theme::ROW_HEIGHT));
    let row_hovered = ui.rect_contains_pointer(row_rect);
    let renaming = app.renaming.as_ref().is_some_and(|(o, _)| *o == id);
    let hidden = app.is_hidden(id);
    let locked = app.is_locked(id);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // An object has no disclosure chevron of its own (its defects
        // open with the active object, not a click here): the slot is
        // still reserved, empty, so its swatch lines up under a group's
        // or a defect category's icon at the same depth.
        fixed_slot(ui, egui::vec2(CHEVRON_WIDTH, theme::ROW_HEIGHT), |_| {});
        fixed_slot(ui, egui::vec2(SWATCH_WIDTH, theme::ROW_HEIGHT), |ui| {
            ui.centered_and_justified(|ui| match app.object_swatch(id) {
                Some(c) => swatch(ui, c),
                None => {
                    ui.allocate_space(egui::vec2(10.0, 10.0));
                }
            });
        });
        ui.scope(|ui| {
            ui.set_width(
                (full_width - CHEVRON_WIDTH - SWATCH_WIDTH - HOVER_ACTIONS_WIDTH).max(0.0),
            );
            if renaming {
                rename_row(app, ui, id);
            } else {
                tree_row(
                    app,
                    ui,
                    &TreeItem::Object(id),
                    Row {
                        // A Hole's row says so with its own icon.
                        icon: Some(if hole {
                            (icons::HOLE_ROLE, theme::TEXT_MUTED)
                        } else {
                            (icons::OBJECT, theme::TEXT_MUTED)
                        }),
                        text,
                        size: None,
                        label,
                    },
                    |ui| {
                        ui.label(tip);
                    },
                );
            }
        });
        let hide_label = format!("{} {name}", if hidden { "Show" } else { "Hide" });
        fixed_slot(ui, egui::vec2(HIDE_TOGGLE_WIDTH, theme::ROW_HEIGHT), |ui| {
            // Hidden stays visible even when the row isn't hovered, so a
            // hidden object's row still says so at a glance.
            if (row_hovered || hidden)
                && widgets::icon_button(
                    ui,
                    if hidden { icons::HIDDEN } else { icons::SHOWN },
                    &hide_label,
                    None,
                    HIDE_TOGGLE_WIDTH,
                )
                .on_hover_text(if hidden { "Show" } else { "Hide" })
                .clicked()
            {
                app.set_hidden(&[id], !hidden);
            }
        });
        let lock_label = format!("{} {name}", if locked { "Unlock" } else { "Lock" });
        fixed_slot(ui, egui::vec2(LOCK_TOGGLE_WIDTH, theme::ROW_HEIGHT), |ui| {
            if (row_hovered || locked)
                && widgets::icon_button(
                    ui,
                    if locked {
                        icons::LOCKED
                    } else {
                        icons::UNLOCKED
                    },
                    &lock_label,
                    None,
                    LOCK_TOGGLE_WIDTH,
                )
                .on_hover_text(if locked { "Unlock" } else { "Lock" })
                .clicked()
            {
                app.set_locked(&[id], !locked);
            }
        });
        let action = if hole {
            t!("role.make_solid")
        } else {
            t!("role.make_hole")
        };
        let toggle_label = format!("{name}: {action}");
        fixed_slot(ui, egui::vec2(ROLE_TOGGLE_WIDTH, theme::ROW_HEIGHT), |ui| {
            if row_hovered
                && widgets::icon_button(
                    ui,
                    if hole {
                        icons::OBJECT
                    } else {
                        icons::HOLE_ROLE
                    },
                    &toggle_label,
                    None,
                    ROLE_TOGGLE_WIDTH,
                )
                .on_hover_text(format!("{action} (H)"))
                .clicked()
            {
                app.toggle_role(&[id]);
            }
        });
        let delete_label = format!("Delete {name}");
        fixed_slot(
            ui,
            egui::vec2(DELETE_BUTTON_WIDTH, theme::ROW_HEIGHT),
            |ui| {
                if row_hovered
                    && widgets::icon_button(
                        ui,
                        icons::TRASH,
                        &delete_label,
                        None,
                        DELETE_BUTTON_WIDTH,
                    )
                    .on_hover_text("Delete (Backspace)")
                    .clicked()
                {
                    app.delete_object(id);
                }
            },
        );
        fixed_slot(ui, egui::vec2(MOVE_MENU_WIDTH, theme::ROW_HEIGHT), |ui| {
            if row_hovered {
                reorder_trigger(ui, &format!("Reorder {name}"), MOVE_MENU_WIDTH, |ui| {
                    reorder_actions(app, ui, mf_command::Entry::Object(id), reorder);
                });
            }
        });
    });
}

/// What a tree row shows: an icon, its text, its size at the right
/// (muted), and its accessible label (both, as words).
struct Row<'a> {
    icon: Option<(&'static str, egui::Color32)>,
    text: RichText,
    size: Option<&'a str>,
    label: &'a str,
}

/// One row of the Browser. Every row (an object, "Model", each defect,
/// and any row added later) behaves the same way, here (M4 Task 8a):
/// click selects its item; double-click, or Enter on the focused row,
/// selects it and frames it in the view (animated); hovering highlights it
/// in the view (an outline, or a glow); right-click lists its registered
/// context actions; `tooltip` says more. No buttons inside rows.
fn tree_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    item: &TreeItem,
    row: Row<'_>,
    tooltip: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let width = ui.available_width();
    let selected = app.tree_selected(item);
    let mut text = egui::text::LayoutJob::default();
    if let Some((icon, color)) = row.icon {
        text.append(
            icon,
            0.0,
            egui::TextFormat::simple(icons::font(theme::ICON_SIZE - 2.0), color),
        );
    }
    let mut button = egui::Button::selectable(
        selected,
        (text.atom_shrink(false), row.text.atom_shrink(true)),
    )
    .min_size(egui::vec2(width, theme::ROW_HEIGHT));
    // Always a trailing grow atom, size or not: with none, `right_text`
    // (which pushes one before its own atoms) never runs, so without this
    // the button's content centres in the full row width instead of
    // hugging the left (owner 2026-11-12: object and group rows, which
    // have no size, showed centred or oddly indented).
    if let Some(size) = row.size {
        // Smaller, muted; cut short ("…") rather than wrapped when the
        // panel is narrow (the tooltip and the label have it all). The
        // name atom is already the one `shrink` atom (only one is
        // allowed); the grow atom `right_text` pushes ahead of this one
        // is what keeps the size pinned right without it needing to
        // shrink itself.
        button = button.right_text(
            RichText::new(size)
                .size(theme::TYPE_CAPTION)
                .color(theme::TEXT_MUTED),
        );
    } else {
        button = button.right_text(egui::Atom::grow());
    }
    // Its id is its item's, wherever it is in the tree. One line, always.
    let response = ui
        .push_id(item_key(item), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.set_max_width(width);
            ui.add(button)
        })
        .inner;
    let label = row.label.to_owned();
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    let actions: Vec<crate::shell::ContextAction> = app
        .shell
        .context_actions()
        .iter()
        .filter(|a| (a.applies_to)(app, item))
        .copied()
        .collect();
    let titles: Vec<String> = actions.iter().map(|a| (a.title)(app, item)).collect();
    let mut chosen = None;
    response.context_menu(|ui| {
        for (i, title) in titles.iter().enumerate() {
            if ui
                .add_enabled(app.busy.is_none(), egui::Button::new(title))
                .clicked()
            {
                chosen = Some(i);
                ui.close();
            }
        }
    });
    if let Some(i) = chosen {
        (actions[i].run)(app, item);
    }
    let response = response.on_hover_ui(tooltip);
    if response.hovered() {
        app.tree_hover(item);
    }
    // Enter (or Space) on a focused row is a click too: Enter also frames.
    let enter = response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    // egui's double-click is the pointer's: it only counts here if the
    // first click was on this row too. A click on another row just before
    // makes egui count the second click on this one as a triple-click:
    // that's this row's double-click too.
    let last_click = egui::Id::unique("tree row last clicked");
    let twice = (response.double_clicked() || response.triple_clicked())
        && ui.data(|d| d.get_temp::<egui::Id>(last_click)) == Some(response.id);
    if response.clicked() {
        ui.data_mut(|d| d.insert_temp(last_click, response.id));
    }
    if twice || (response.clicked() && enter) {
        app.tree_find(item);
    } else if response.clicked() && ui.input(|i| i.modifiers.shift) {
        app.tree_select_add(item);
    } else if response.clicked() {
        app.tree_select(item);
    }
    response
}

/// Rows `0..n` of a list, each `draw(ui, i)` one row high, laid out only
/// where the scroll area shows them: the rest is empty space of the right
/// height, so every row is reachable by scrolling and a frame costs
/// O(rows shown), whatever `n` (MF-RT-001).
fn virtual_rows(ui: &mut egui::Ui, n: usize, draw: impl FnMut(&mut egui::Ui, usize)) {
    virtual_rows_to(ui, n, None, draw);
}

/// [`virtual_rows`], scrolling row `reveal` (if any) into view: its place
/// is known from its index, laid out or not.
fn virtual_rows_to(
    ui: &mut egui::Ui,
    n: usize,
    reveal: Option<usize>,
    mut draw: impl FnMut(&mut egui::Ui, usize),
) {
    let row = ui.spacing().interact_size.y;
    let pitch = row + ui.spacing().item_spacing.y;
    let top = ui.cursor().min.y;
    if let Some(k) = reveal.filter(|&k| k < n) {
        let at = egui::Rect::from_min_size(
            egui::pos2(ui.cursor().min.x, top + k as f32 * pitch),
            egui::vec2(ui.available_width(), row),
        );
        ui.scroll_to_rect(at, Some(egui::Align::Center));
    }
    let clip = ui.clip_rect();
    let first = (((clip.top() - top) / pitch).floor().max(0.0) as usize).min(n);
    let last = ((((clip.bottom() - top) / pitch).ceil().max(0.0) as usize) + 1).min(n);
    let width = ui.available_width();
    let gap = |ui: &mut egui::Ui, rows: usize| {
        if rows > 0 {
            ui.allocate_exact_size(
                egui::vec2(width, rows as f32 * pitch - ui.spacing().item_spacing.y),
                egui::Sense::hover(),
            );
        }
    };
    gap(ui, first);
    for i in first..last.max(first) {
        let start = ui.cursor().min.y;
        draw(ui, i);
        // Keep every row exactly one pitch, whatever it drew.
        let used = ui.cursor().min.y - start;
        if used < pitch {
            ui.add_space(pitch - used - ui.spacing().item_spacing.y);
        }
    }
    gap(ui, n.saturating_sub(last.max(first)));
}

/// A tree item's identity, for its row's widget id.
fn item_key(item: &TreeItem) -> (u8, u64, [u32; 2]) {
    match item {
        TreeItem::Object(id) => (0, id.0, [0; 2]),
        TreeItem::Model(id) => (1, id.0, [0; 2]),
        TreeItem::Defect(id, d) => match d {
            Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => {
                (2, id.0, [*loop_id, 0])
            }
            Defect::Crack { loop_id, .. } => (3, id.0, [*loop_id, 0]),
            Defect::LoosePiece { component, .. } => (4, id.0, [*component, 0]),
            Defect::Tangled { edge, .. } => (5, id.0, *edge),
        },
        TreeItem::Group(g) => (6, g.0, [0; 2]),
    }
}

fn defects_tree(app: &mut MeshFixerApp, ui: &mut egui::Ui, object: mf_command::ObjectId) {
    let Some(analysis) = app.analysis_of(object) else {
        if app.active == Some(object) && app.analysis_failed_for_active() {
            ui.weak("Couldn't analyse this object");
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Analysing…");
            });
        }
        return;
    };
    let defects = analysis.defects.clone();
    let units = app.session.document().units();
    // Same chevron slot as "Holes (3)" below it (empty: "Model" doesn't
    // fold away), so its icon starts at the same x.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        fixed_slot(ui, egui::vec2(CHEVRON_WIDTH, theme::ROW_HEIGHT), |_| {});
        tree_row(
            app,
            ui,
            &TreeItem::Model(object),
            Row {
                icon: Some((icons::MODEL, theme::TEXT_MUTED)),
                text: RichText::new("Model"),
                size: None,
                label: "Model",
            },
            |ui| {
                ui.label("The whole object: its tools are on the rail");
                ui.label(RichText::new("Double-click to frame it").color(theme::TEXT_MUTED));
            },
        );
    });
    holes_tree(app, ui, object, &defects, units);
    // Cracks marked intentional are listed with the openings instead.
    let all_cracks: Vec<usize> = (0..defects.cracks.len()).collect();
    let cracks: Vec<usize> = match app.hole_view_of(object) {
        Some(view) => view.cracks.clone(),
        None => all_cracks,
    };
    let every = |n: usize| (0..n).collect::<Vec<usize>>();
    // (name, icon, tooltip, its defects, the rows listed, colour)
    type Category<'a> = (
        &'a str,
        &'static str,
        &'a str,
        &'a [Defect],
        Vec<usize>,
        egui::Color32,
    );
    let categories: [Category<'_>; 3] = [
        (
            "Narrow cracks",
            icons::CRACK,
            "Edges that almost touch, like a hairline seam",
            &defects.cracks,
            cracks,
            theme::CRACK,
        ),
        (
            "Loose pieces",
            icons::LOOSE_PIECE,
            "Pieces not connected to the model",
            &defects.loose,
            every(defects.loose.len()),
            theme::LOOSE_PIECE,
        ),
        (
            "Tangled edges",
            icons::TANGLED,
            "Tangled (non-manifold) edges: more than two triangles share each one",
            &defects.tangled,
            every(defects.tangled.len()),
            theme::NON_MANIFOLD,
        ),
    ];
    for (name, icon, tip, list, rows, color) in categories {
        let label = format!("{name} ({})", rows.len());
        widgets::disclosure(
            ui,
            (object.0, name),
            widgets::Disclosure {
                label: &label,
                icon,
                icon_color: if rows.is_empty() {
                    theme::TEXT_FAINT
                } else {
                    color
                },
                name,
                name_color: if rows.is_empty() {
                    theme::TEXT_MUTED
                } else {
                    theme::TEXT
                },
                count: Some(thousands(rows.len() as u64)),
                default_open: !rows.is_empty(),
                force_open: None,
                tip,
            },
            |ui| {
                virtual_rows(ui, rows.len(), |ui, i| {
                    defect_row(app, ui, object, &defects, &list[rows[i]], units, false);
                });
            },
        );
    }
}

/// The holes: each size group (largest first) with its rows, and the
/// intentional openings. Their actions are in Repair and each row's
/// right-click menu.
fn holes_tree(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    object: mf_command::ObjectId,
    defects: &std::sync::Arc<crate::defects::Defects>,
    units: mf_io::threemf::Unit,
) {
    let reveal = app.take_reveal();
    let Some(view) = app.hole_view_of(object).cloned() else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.weak("Listing holes…");
        });
        return;
    };
    let n = view.listed.len();
    let label = format!("Holes ({n})");
    widgets::disclosure(
        ui,
        (object.0, "Holes"),
        widgets::Disclosure {
            label: &label,
            icon: icons::HOLE,
            icon_color: if n == 0 {
                theme::TEXT_FAINT
            } else {
                theme::HOLE
            },
            name: "Holes",
            name_color: if n == 0 {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            },
            count: Some(thousands(n as u64)),
            default_open: n > 0,
            force_open: reveal.map(|_| true),
            tip: "Open boundaries. Fill them in Repair, or right-click one",
        },
        |ui| {
            for group in HoleGroup::ALL {
                let range = view.group(group);
                if range.is_empty() {
                    continue;
                }
                let rows = &view.listed[range.clone()];
                // The row N / Shift+N just selected, if it's in this group.
                let reveal = reveal
                    .and_then(|id| view.position(id))
                    .filter(|p| range.contains(p))
                    .map(|p| p - range.start);
                let label = format!("{} ({})", group.short_title(), rows.len());
                let name = group.short_title();
                widgets::disclosure(
                    ui,
                    (object.0, "hole group", group.title()),
                    widgets::Disclosure {
                        label: &label,
                        icon: icons::HOLE,
                        icon_color: theme::HOLE.gamma_multiply(0.7),
                        name,
                        name_color: theme::TEXT_MUTED,
                        count: Some(thousands(rows.len() as u64)),
                        default_open: true,
                        force_open: reveal.map(|_| true),
                        tip: group.title(),
                    },
                    |ui| {
                        virtual_rows_to(ui, rows.len(), reveal, |ui, k| {
                            let d = &defects.holes[rows[k]];
                            defect_row(app, ui, object, defects, d, units, false);
                        });
                    },
                );
            }
        },
    );
    let marked = view.marked.len() + view.marked_cracks.len();
    if marked > 0 {
        let label = format!("Intentional openings ({marked})");
        widgets::disclosure(
            ui,
            (object.0, "Intentional openings"),
            widgets::Disclosure {
                label: &label,
                icon: icons::OPENING,
                icon_color: theme::OPENING,
                name: "Intentional openings",
                name_color: theme::TEXT,
                count: Some(thousands(marked as u64)),
                default_open: true,
                force_open: None,
                tip: "Openings marked as meant to be open: left out of counts and fills",
            },
            |ui| {
                virtual_rows(ui, marked, |ui, k| {
                    let d = match view.marked.get(k) {
                        Some(&i) => &defects.holes[i],
                        None => &defects.cracks[view.marked_cracks[k - view.marked.len()]],
                    };
                    defect_row(app, ui, object, defects, d, units, true);
                });
            },
        );
    }
}

/// A defect's row: its name and size, no buttons. Its tooltip says what
/// it is and what to do; right-click lists its actions.
fn defect_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    object: mf_command::ObjectId,
    defects: &crate::defects::Defects,
    d: &Defect,
    units: mf_io::threemf::Unit,
    marked: bool,
) {
    let copy = defects.copy(d, units);
    let next = if marked {
        "Unmark it (right-click) to list it again".to_owned()
    } else {
        copy.next_action.clone()
    };
    // The same chevron-then-swatch slots as its disclosure header's row,
    // empty (a leaf defect has neither), so its name starts at the same
    // x as "Holes (3)" above it, not 20 pt to the left of it.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        fixed_slot(ui, egui::vec2(CHEVRON_WIDTH, theme::ROW_HEIGHT), |_| {});
        fixed_slot(ui, egui::vec2(SWATCH_WIDTH, theme::ROW_HEIGHT), |_| {});
        tree_row(
            app,
            ui,
            &TreeItem::Defect(object, d.clone()),
            Row {
                icon: None,
                text: RichText::new(&copy.name),
                size: Some(&copy.size),
                label: &copy.row,
            },
            |ui| {
                ui.strong(&copy.title);
                ui.label(&copy.detail);
                if !marked {
                    ui.label(&copy.verdict);
                    if let Some(limit) = &copy.limit {
                        ui.label(RichText::new(limit).color(theme::TEXT_MUTED));
                    }
                }
                ui.label(RichText::new(format!("Next: {next}")).color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new("Double-click to frame it · right-click for actions")
                        .color(theme::TEXT_MUTED),
                );
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shortcut text names the platform's keys (fix round 1, P4): Cmd on
    /// macOS, Ctrl elsewhere, in the platform's order.
    #[test]
    fn shortcut_text_is_platform_correct() {
        use egui::{Key, Modifiers};
        let undo = Modifiers::COMMAND;
        let redo = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(shortcut_text_for(undo, Key::Z, true), "Cmd+Z");
        assert_eq!(shortcut_text_for(undo, Key::Z, false), "Ctrl+Z");
        assert_eq!(shortcut_text_for(redo, Key::Z, true), "Shift+Cmd+Z");
        assert_eq!(shortcut_text_for(redo, Key::Z, false), "Ctrl+Shift+Z");
        assert_eq!(shortcut_text_for(Modifiers::NONE, Key::F, true), "F");
    }
}
