//! The layout (docs/design/visual-language.md): menu bar on top, "Objects &
//! Defects" on the left, "Properties & Tools" on the right, the history
//! breadcrumb and status line at the bottom; the 3D view takes the rest.

use eframe::egui::{self, RichText};
use mf_selection::{Selection, SelectionKind};
use mf_spec::{ParamKind, ParamValue};

use crate::app::{MeshFixerApp, Tool};
use crate::defects::{Defect, count, defect_copy, measure, unit_symbol};
use crate::theme;
use crate::worker::OverlayTarget;

/// Panel widths (points). With the bottom bar, they leave the 3D view at
/// least 60% of a 1440×900 window.
const TREE_WIDTH: f32 = 210.0;
const PROPERTIES_WIDTH: f32 = 280.0;
/// Rows listed per defect category before "…and N more".
const MAX_ROWS: usize = 200;

pub fn menu_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::top("menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let idle = app.busy.is_none();
                if ui.add_enabled(idle, egui::Button::new("Open…")).clicked() {
                    app.open_dialog();
                    ui.close();
                }
                let can_export = idle && app.active_mesh().is_some();
                if ui
                    .add_enabled(can_export, egui::Button::new("Export…"))
                    .clicked()
                {
                    app.export_dialog();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(idle, egui::Button::new("Save Command Log…"))
                    .clicked()
                {
                    app.save_log_dialog();
                    ui.close();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Replay Command Log…"))
                    .clicked()
                {
                    app.replay_dialog();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                let idle = app.busy.is_none();
                if ui
                    .add_enabled(
                        idle && app.session.can_undo(),
                        egui::Button::new("Undo").shortcut_text("Cmd+Z"),
                    )
                    .clicked()
                {
                    app.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        idle && app.session.can_redo(),
                        egui::Button::new("Redo").shortcut_text("Shift+Cmd+Z"),
                    )
                    .clicked()
                {
                    app.redo();
                    ui.close();
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
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut app.show_boundaries, "Show defects");
                ui.checkbox(&mut app.shown.recent_repairs, "Highlight recent repairs");
                ui.separator();
                if ui
                    .add(egui::Button::new("Frame selection").shortcut_text("F"))
                    .clicked()
                {
                    app.frame_target();
                    ui.close();
                }
                if ui
                    .add(egui::Button::new("Reset view").shortcut_text("Home"))
                    .clicked()
                {
                    app.fit_camera();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
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
        });
    });
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

/// Width kept for the status line at the right of the bottom bar.
const STATUS_WIDTH: f32 = 360.0;

/// Bottom bar: Undo/Redo, the history breadcrumb (click a step to jump to
/// it; the current one is highlighted, steps redo would take are greyed),
/// any other branches, and the status line.
pub fn history_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("history").show(ui, |ui| {
        ui.horizontal(|ui| {
            let idle = app.busy.is_none();
            if ui
                .add_enabled(idle && app.session.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("Cmd+Z")
                .clicked()
            {
                app.undo();
            }
            if ui
                .add_enabled(idle && app.session.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("Shift+Cmd+Z")
                .clicked()
            {
                app.redo();
            }
            ui.separator();
            let crumbs_width = (ui.available_width() - STATUS_WIDTH).max(120.0);
            let mut clicked = None;
            ui.allocate_ui(egui::vec2(crumbs_width, ui.available_height()), |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("breadcrumb")
                    .stick_to_right(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let current = app.session.current();
                            let crumbs = app.breadcrumb();
                            if crumbs.is_empty() {
                                ui.weak("No history yet");
                            }
                            for (i, (id, label, reached)) in crumbs.into_iter().enumerate() {
                                if i > 0 {
                                    ui.weak("›");
                                }
                                let text = if reached {
                                    RichText::new(&label)
                                } else {
                                    RichText::new(&label).color(theme::TEXT_MUTED).italics()
                                };
                                if ui
                                    .add_enabled(
                                        idle,
                                        egui::Button::selectable(id == current, text),
                                    )
                                    .on_hover_text("Jump to this step")
                                    .clicked()
                                {
                                    clicked = Some(id);
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
                    });
            });
            if let Some(id) = clicked {
                app.checkout(id);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                status(app, ui);
            });
        });
    });
}

/// The status line: what's running (with its stage, percentage and a
/// Cancel button), or the last result, or "Ready". Laid out right to left.
fn status(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if let Some(busy) = app.busy.clone() {
        job_status(app, ui, &busy);
        return;
    }
    let (text, spinning) = if app.is_analyzing() {
        ("Analyzing…".to_owned(), true)
    } else if let Some(status) = &app.status {
        (status.clone(), false)
    } else {
        ("Ready".to_owned(), false)
    };
    ui.add(egui::Label::new(RichText::new(&text).color(theme::TEXT_MUTED)).truncate())
        .on_hover_text(&text);
    if spinning {
        ui.spinner();
    }
}

/// Width of the job's progress bar in the status line.
const PROGRESS_WIDTH: f32 = 90.0;

/// A running job: Cancel (or "Cancelling…"), a progress bar, and "<job> ·
/// <stage> <n>%" (MF-RT-003). Jobs that report no progress get a spinner.
fn job_status(app: &mut MeshFixerApp, ui: &mut egui::Ui, busy: &str) {
    if app.can_cancel() {
        if ui
            .button("Cancel")
            .on_hover_text("Stop it (Esc); the document stays as it is")
            .clicked()
        {
            app.cancel_job();
        }
    } else if app.is_cancelling() {
        ui.weak("Cancelling…");
    }
    let progress = app.job_progress().filter(|(stage, _)| !stage.is_empty());
    let text = match progress {
        Some((stage, fraction)) => {
            ui.add(
                egui::ProgressBar::new(fraction as f32)
                    .desired_width(PROGRESS_WIDTH)
                    .fill(theme::PROGRESS),
            );
            format!("{busy} · {} {:.0}%", stage_label(stage), fraction * 100.0)
        }
        None => {
            ui.spinner();
            busy.to_owned()
        }
    };
    ui.add(egui::Label::new(RichText::new(&text).color(theme::TEXT_MUTED)).truncate())
        .on_hover_text(&text);
}

/// A progress stage in plain words.
fn stage_label(stage: &str) -> String {
    match stage {
        "reading" => "reading the file".into(),
        "welding" => "joining corners".into(),
        "topology" => "finding edges".into(),
        "inspecting" => "checking".into(),
        "triangulating" => "filling".into(),
        "refining" => "smoothing the patches".into(),
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
pub fn controls_window(app: &mut MeshFixerApp, ctx: &egui::Context) {
    let mut open = app.show_controls;
    egui::Window::new("Controls")
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
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
                "On screen (bottom-right of the view)",
                &[
                    ("Orbit / Pan / Zoom", "What a plain drag does"),
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
                    ("Home or Cmd/Ctrl+0", "Reset the view"),
                    (
                        "Esc",
                        "Cancel the running job; otherwise clear the selection",
                    ),
                    ("Cmd+Z / Shift+Cmd+Z", "Undo / Redo"),
                ],
            );
        });
    app.show_controls = open;
}

/// Left: "Objects & Defects". Every object; under the active one, its
/// model and each defect category with a count. Clicking a row makes it
/// the target; hovering outlines it in the view. The legend below says
/// what each colour in the view means and switches each overlay.
pub fn tree_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::left("tree")
        .resizable(true)
        .default_size(TREE_WIDTH)
        .min_size(170.0)
        .show(ui, |ui| {
            ui.heading("Objects & Defects");
            ui.separator();
            egui::Panel::bottom("legend")
                .show_separator_line(true)
                .show(ui, |ui| legend(app, ui));
            egui::ScrollArea::vertical()
                .id_salt("tree")
                .auto_shrink([false, false])
                .show(ui, |ui| tree(app, ui));
        });
}

/// The view's colours, each with its switch (View > Show defects hides
/// every defect overlay at once).
fn legend(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.label(RichText::new("Legend").strong());
    let defects = app.show_boundaries;
    let shown = &mut app.shown;
    let rows: [(&mut bool, &str, egui::Color32, &str, bool); 7] = [
        (
            &mut shown.selection,
            "Selection",
            theme::SELECTED,
            "The current target: outlined, or tinted",
            true,
        ),
        (
            &mut shown.hover,
            "Hover",
            theme::HOVER,
            "What the pointer is over in the tree",
            true,
        ),
        (
            &mut shown.holes,
            "Holes",
            theme::HOLE,
            "Open boundaries, outlined",
            defects,
        ),
        (
            &mut shown.cracks,
            "Narrow cracks",
            theme::CRACK,
            "Boundaries whose edges almost touch",
            defects,
        ),
        (
            &mut shown.loose_pieces,
            "Loose pieces",
            theme::LOOSE_PIECE,
            "Pieces too small to keep (under Remove loose pieces' size limit), tinted; click one to select it",
            defects,
        ),
        (
            &mut shown.non_manifold,
            "Non-manifold edges",
            theme::NON_MANIFOLD,
            "Edges shared by more than two faces, and the faces at them; click one to select its faces",
            defects,
        ),
        (
            &mut shown.recent_repairs,
            "Recent repairs",
            theme::RECENT_REPAIR,
            "Faces made by the last 3 commands, tinted",
            true,
        ),
    ];
    for (on, name, color, meaning, enabled) in rows {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color);
            ui.add_enabled(enabled, egui::Checkbox::new(on, name))
                .on_hover_text(meaning)
                .on_disabled_hover_text("Hidden: View > Show defects is off");
        });
    }
}

fn tree(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let objects: Vec<_> = app
        .session
        .document()
        .objects()
        .iter()
        .map(|o| {
            (
                o.id,
                o.name.clone(),
                o.mesh().map_or(0, |m| m.triangle_count()),
            )
        })
        .collect();
    if objects.is_empty() {
        ui.weak("Open a mesh with File > Open");
    }
    for (id, name, tris) in objects {
        let active = app.active == Some(id);
        let text = if active {
            RichText::new(&name).strong()
        } else {
            RichText::new(&name).color(theme::TEXT_MUTED)
        };
        let row = ui.selectable_label(false, text).on_hover_text(format!(
            "{}{}",
            count(tris, "triangle"),
            if active {
                ""
            } else {
                " · click to work on it"
            }
        ));
        if row.clicked() {
            app.set_active(id);
        }
        if active {
            ui.indent(("defects", id.0), |ui| defects_tree(app, ui));
        }
    }
}

fn defects_tree(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(object) = app.active else { return };
    let Some(analysis) = app.current_analysis() else {
        if app.analysis_failed_for_active() {
            ui.weak("Couldn't analyse this object");
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Analyzing…");
            });
        }
        return;
    };
    let defects = analysis.defects.clone();
    let units = app.session.document().units();
    let nothing_targeted = app.selection.is_none() && app.current_inspected().is_none();
    if ui
        .selectable_label(nothing_targeted, "Model")
        .on_hover_text("The whole object: its operations are in Properties")
        .clicked()
    {
        app.clear_selection();
    }
    let categories = [
        ("Holes", &defects.holes, theme::HOLE),
        ("Narrow cracks", &defects.cracks, theme::CRACK),
        ("Loose pieces", &defects.loose, theme::LOOSE_PIECE),
        ("Non-manifold edges", &defects.tangled, theme::NON_MANIFOLD),
    ];
    for (name, list, color) in categories {
        let label = format!("{name} ({})", list.len());
        let text = if list.is_empty() {
            RichText::new(label).color(theme::TEXT_MUTED)
        } else {
            RichText::new(label).color(color)
        };
        egui::CollapsingHeader::new(text)
            .id_salt((object.0, name))
            .default_open(!list.is_empty())
            .show(ui, |ui| {
                for d in list.iter().take(MAX_ROWS) {
                    // A row scrolled out of sight only takes its space:
                    // laying out hundreds of new rows in the frame a big
                    // analysis arrives costs milliseconds (MF-RT-001).
                    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                    if ui.is_rect_visible(egui::Rect::from_min_size(ui.cursor().min, size)) {
                        defect_row(app, ui, object, d, units);
                    } else {
                        ui.allocate_space(size);
                    }
                }
                if list.len() > MAX_ROWS {
                    ui.weak(format!("…and {} more", list.len() - MAX_ROWS));
                }
            });
    }
}

fn defect_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    object: mf_command::ObjectId,
    d: &Defect,
    units: mf_io::threemf::Unit,
) {
    let copy = defect_copy(d, units);
    let selected = match (d.selection(), app.current_selection()) {
        (Some((kind, id)), Some(sel)) => sel.kind == kind && sel.ids.contains(&id),
        (None, _) => app.current_inspected().is_some_and(|i| i.defect == *d),
        _ => false,
    };
    let row = ui
        .selectable_label(selected, &copy.title)
        .on_hover_ui(|ui| {
            ui.strong(&copy.title);
            ui.label(&copy.detail);
            ui.label(RichText::new(format!("Next: {}", copy.next_action)).color(theme::TEXT_MUTED));
        });
    if row.hovered() {
        app.hover = Some((object, overlay_target(d)));
    }
    if row.clicked() {
        app.select_defect(d);
    }
}

fn overlay_target(d: &Defect) -> OverlayTarget {
    match d {
        Defect::Hole { loop_id, .. }
        | Defect::OverCap { loop_id, .. }
        | Defect::Crack { loop_id, .. } => OverlayTarget::Loops(vec![*loop_id]),
        Defect::LoosePiece { component, .. } => OverlayTarget::Components(vec![*component]),
        Defect::Tangled { edge, .. } => OverlayTarget::Edges(vec![*edge]),
    }
}

/// Right: "Properties & Tools", contextual to the current target: what it
/// is, the operations valid for it, their parameters, and Apply.
pub fn properties_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::right("properties")
        .resizable(true)
        .default_size(PROPERTIES_WIDTH)
        .min_size(220.0)
        .show(ui, |ui| {
            ui.heading("Properties & Tools");
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("properties")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if app.active_mesh().is_none() {
                        ui.weak("Nothing open yet.");
                        return;
                    }
                    target_section(app, ui);
                    ui.add_space(8.0);
                    tools_section(app, ui);
                    ui.add_space(8.0);
                    details_section(app, ui);
                });
        });
}

/// What the target is, in plain language.
fn target_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let units = app.session.document().units();
    let analysis = app.current_analysis();
    if let Some(inspected) = app.current_inspected() {
        let copy = defect_copy(&inspected.defect, units);
        ui.label(RichText::new(&copy.title).strong().size(16.0));
        ui.label(&copy.detail);
        if ui.button(&copy.next_action).clicked() {
            app.select_tangled_faces();
        }
        return;
    }
    if let Some(sel) = app.current_selection().cloned() {
        let found: Option<Vec<Defect>> = analysis.and_then(|a| {
            sel.ids
                .iter()
                .map(|&id| a.defects.find(sel.kind, id).cloned())
                .collect()
        });
        match (&found, sel.ids.len()) {
            (Some(defects), 1) => {
                let copy = defect_copy(&defects[0], units);
                ui.label(RichText::new(&copy.title).strong().size(16.0));
                ui.label(&copy.detail);
                if app.tools().is_empty() {
                    ui.label(RichText::new(&copy.next_action).color(theme::TEXT_MUTED));
                }
            }
            (Some(defects), n) => {
                ui.label(RichText::new(selection_title(&sel, n)).strong().size(16.0));
                let blocked: Vec<_> = defects
                    .iter()
                    .filter(|d| !matches!(d, Defect::Hole { .. } | Defect::LoosePiece { .. }))
                    .collect();
                if let Some(d) = blocked.first() {
                    let copy = defect_copy(d, units);
                    ui.label(
                        RichText::new(format!(
                            "Includes a {}: {}",
                            copy.title.to_lowercase(),
                            copy.next_action
                        ))
                        .color(theme::TEXT_MUTED),
                    );
                }
            }
            (None, n) => {
                let main_body = sel.kind == SelectionKind::Component && sel.ids == [0];
                if main_body {
                    ui.label(RichText::new("Main body").strong().size(16.0));
                    if let Some(a) = analysis {
                        ui.label(count(a.defects.main_body_triangles, "triangle"));
                    }
                } else {
                    ui.label(RichText::new(selection_title(&sel, n)).strong().size(16.0));
                }
                ui.label(
                    RichText::new(
                        "No operation works on this selection yet; these act on the whole object.",
                    )
                    .color(theme::TEXT_MUTED),
                );
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Clear selection").on_hover_text("Esc").clicked() {
                app.clear_selection();
            }
            ui.weak("Shift-click adds");
        });
        return;
    }
    // Nothing targeted: the whole object.
    let Some(id) = app.active else { return };
    let Some(object) = app.session.document().object(id) else {
        return;
    };
    ui.label(RichText::new(&object.name).strong().size(16.0));
    let Some(a) = analysis else {
        ui.weak(if app.analysis_failed_for_active() {
            "Couldn't analyse this object"
        } else {
            "Analyzing…"
        });
        return;
    };
    let d = &a.diagnostics;
    ui.label(count(d.triangle_count, "triangle"));
    // Size along the world axes, after the object's transform.
    if let Some(size) = a.defects.world_size {
        let u = unit_symbol(units);
        ui.label(format!(
            "{} × {} × {} {u}",
            measure(size[0]),
            measure(size[1]),
            measure(size[2])
        ));
    }
    let problems = a.defects.holes.len()
        + a.defects.cracks.len()
        + a.defects.loose.len()
        + a.defects.tangled.len();
    ui.label(if problems == 0 {
        RichText::new("No defects found").color(theme::TEXT_MUTED)
    } else {
        RichText::new(format!(
            "{} to look at (see the tree)",
            count(problems, "defect")
        ))
        .color(theme::TEXT_MUTED)
    });
}

fn selection_title(sel: &Selection, n: usize) -> String {
    match sel.kind {
        SelectionKind::BoundaryLoop => format!("{} selected", count(n, "opening")),
        SelectionKind::Component => format!("{} selected", count(n, "piece")),
        SelectionKind::Faces => format!("{} selected", count(n, "face")),
        SelectionKind::Object => "Whole object".to_owned(),
    }
}

/// The operations valid for the target. Targeted tools are open at once;
/// whole-object tools open when chosen.
fn tools_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let tools = app.tools();
    if tools.is_empty() {
        return;
    }
    let targeted = tools.iter().any(|t| t.selection.is_some());
    ui.label(
        RichText::new(if targeted {
            "Tool"
        } else {
            "Tools for the whole object"
        })
        .strong(),
    );
    for tool in &tools {
        let open = targeted || app.tool.as_deref() == Some(&tool.op_id);
        if targeted {
            ui.label(RichText::new(&tool.label).size(15.0).strong());
        } else if ui
            .add(
                egui::Button::new(&tool.label)
                    .selected(open)
                    .min_size(egui::vec2(ui.available_width(), 24.0)),
            )
            .on_hover_text(if open { "Close" } else { "Open this tool" })
            .clicked()
        {
            app.tool = if open { None } else { Some(tool.op_id.clone()) };
        }
        if open {
            ui.indent(("tool", &tool.op_id), |ui| tool_body(app, ui, tool));
        }
    }
}

fn tool_body(app: &mut MeshFixerApp, ui: &mut egui::Ui, tool: &Tool) {
    let Some(spec) = mf_spec::builtin::get(&tool.op_id) else {
        ui.label(format!("Unknown operation {}", tool.op_id));
        return;
    };
    ui.label(RichText::new(&spec.summary).color(theme::TEXT_MUTED));
    let values = app.params_for(&tool.op_id);
    egui::Grid::new(("params", &tool.op_id))
        .num_columns(2)
        .show(ui, |ui| {
            for p in spec
                .parameters
                .iter()
                .filter(|p| !tool.fixed.contains_key(&p.name))
            {
                let Some(value) = values.get_mut(&p.name) else {
                    continue;
                };
                let name = plain_name(&p.name);
                match (&p.kind, value) {
                    (ParamKind::Integer { min, max, .. }, ParamValue::Int(v)) => {
                        ui.label(&name).on_hover_text(&p.description);
                        let range = min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX);
                        ui.add(egui::DragValue::new(v).range(range))
                            .on_hover_text(&p.description);
                    }
                    (ParamKind::Float { min, max, .. }, ParamValue::Float(v)) => {
                        ui.label(&name).on_hover_text(&p.description);
                        let range = min.unwrap_or(f64::MIN)..=max.unwrap_or(f64::MAX);
                        let suffix = p
                            .unit
                            .as_deref()
                            .map(|u| format!(" {u}"))
                            // allow-swallow: no unit means no suffix
                            .unwrap_or_default();
                        ui.add(
                            egui::DragValue::new(v)
                                .range(range)
                                .speed(0.01)
                                .suffix(suffix),
                        )
                        .on_hover_text(&p.description);
                    }
                    (ParamKind::Bool { .. }, ParamValue::Bool(v)) => {
                        ui.label("");
                        ui.checkbox(v, &name).on_hover_text(&p.description);
                    }
                    _ => {
                        ui.label(&name);
                        ui.label("type mismatch");
                    }
                }
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        let can_apply = app.busy.is_none() && app.active.is_some();
        if ui
            .add_enabled(can_apply, egui::Button::new("Apply"))
            .clicked()
        {
            app.apply(tool);
        }
        let editable = spec
            .parameters
            .iter()
            .any(|p| !tool.fixed.contains_key(&p.name));
        if editable && ui.button("Defaults").clicked() {
            app.op_params.remove(&tool.op_id);
        }
    });
}

/// "max_loop_edges" → "Max loop edges".
fn plain_name(id: &str) -> String {
    let spaced = id.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

/// Every number `inspect` reports, folded away under "Details".
fn details_section(app: &MeshFixerApp, ui: &mut egui::Ui) {
    let Some(analysis) = app.current_analysis() else {
        return;
    };
    let d = &analysis.diagnostics;
    let flag = |ok: bool| if ok { "yes" } else { "no" };
    egui::CollapsingHeader::new("Details")
        .id_salt("details")
        .show(ui, |ui| {
            egui::Grid::new("diag")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(k);
                        ui.label(v);
                        ui.end_row();
                    };
                    row("Triangles", d.triangle_count.to_string());
                    row("Vertices", d.vertex_count.to_string());
                    row("Components", d.components.to_string());
                    row("Closed", flag(d.is_closed).into());
                    row("Manifold", flag(d.is_manifold).into());
                    row("Boundary loops", d.boundary_loops.len().to_string());
                    row("Boundary edges", d.boundary_edges.to_string());
                    row(
                        "Untraced boundary edges",
                        d.untraced_boundary_edges.to_string(),
                    );
                    row("Non-manifold edges", d.non_manifold_edges.to_string());
                    row("Non-manifold vertices", d.non_manifold_vertices.to_string());
                    row("Degenerate triangles", d.degenerate_triangles.to_string());
                    row("Duplicate vertices", d.duplicate_vertices_exact.to_string());
                    row(
                        "Inconsistent edges",
                        d.inconsistent_orientation_edges.to_string(),
                    );
                    row(
                        "Surface area",
                        d.surface_area
                            .map_or("overflow (coordinates too large)".into(), |a| {
                                format!("{a:.4}")
                            }),
                    );
                    row(
                        "Near-duplicate vertices",
                        d.duplicate_vertices_tolerance.to_string(),
                    );
                    row("Duplicate faces", d.duplicate_faces.to_string());
                    row(
                        "Volume",
                        d.signed_volume
                            .map_or("— (not closed)".into(), |v| format!("{v:.4}")),
                    );
                });
        });
}
