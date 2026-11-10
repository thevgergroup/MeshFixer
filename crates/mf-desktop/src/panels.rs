//! The layout (docs/design/visual-language.md): menu bar on top, "Objects &
//! Defects" on the left, "Properties & Tools" on the right, the history
//! breadcrumb and status line at the bottom; the 3D view takes the rest.

use eframe::egui::{self, AtomExt as _, RichText};
use mf_selection::{Selection, SelectionKind};
use mf_spec::{ParamKind, ParamValue};

use crate::app::{MeshFixerApp, Tool, TreeItem};
use crate::defects::{
    Defect, HoleGroup, choice_copy, count, defect_copy, measure, param_label, thousands,
    tool_summary, unit_symbol,
};
use crate::theme;
use crate::tools::{LeftTab, ToolKind};

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

/// Panel widths (points). With the bottom bar, they leave the 3D view at
/// least 60% of a 1440×900 window.
const TREE_WIDTH: f32 = 210.0;
const PROPERTIES_WIDTH: f32 = 280.0;

pub fn menu_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::top("menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let idle = app.busy.is_none();
                if ui
                    .add_enabled(idle, egui::Button::new("Open…").shortcut_text(shortcut_text(egui::Modifiers::COMMAND, egui::Key::O)))
                    .clicked()
                {
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
                        egui::Button::new("Undo").shortcut_text(undo_keys()),
                    )
                    .clicked()
                {
                    app.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        idle && app.session.can_redo(),
                        egui::Button::new("Redo").shortcut_text(redo_keys()),
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
                ui.checkbox(&mut app.show_hole_markers, "Show hole markers")
                    .on_hover_text("A dot on each hole too small to see at this zoom");
                ui.checkbox(&mut app.show_hole_overview, "Show hole overview")
                    .on_hover_text(
                        "Tint around every hole, and a dot on each one too small to see",
                    );
                let both = app.show_tree || app.show_properties;
                if ui
                    .add(
                        egui::Button::new(if both {
                            "Hide side panels"
                        } else {
                            "Show side panels"
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
                .on_hover_text(undo_keys())
                .clicked()
            {
                app.undo();
            }
            if ui
                .add_enabled(idle && app.session.can_redo(), egui::Button::new("Redo"))
                .on_hover_text(redo_keys())
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
    let (text, spinning) = if app.is_analyzing() {
        ("Analysing…".to_owned(), true)
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
    let at = app
        .viewport
        .last_rect
        .map_or(egui::pos2(TREE_WIDTH + 16.0, 40.0), |view| {
            view.left_top() + egui::vec2(12.0, 12.0)
        });
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
                "On screen (bottom-right of the view)",
                &[
                    ("Orbit / Pan / Zoom / Select", "What a plain drag does"),
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
                        "Cancel the running job; otherwise clear the selection",
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

/// Left: "Objects & Defects". Every object; under the active one, its
/// model and each defect category with a count. Clicking a row makes it
/// the target; hovering outlines it in the view. The legend below says
/// what each colour in the view means and switches each overlay.
pub fn tree_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let narrow = ui.ctx().content_rect().width() < NARROW_WINDOW;
    let collapsed = egui::Panel::left("tree collapsed")
        .resizable(false)
        .exact_size(COLLAPSED_WIDTH);
    let expanded = egui::Panel::left("tree")
        .resizable(true)
        .default_size(if narrow {
            TREE_WIDTH_NARROW
        } else {
            TREE_WIDTH
        })
        .size_range(150.0..=TREE_WIDTH_MAX);
    let was = app.show_tree;
    let mut open = was;
    egui::Panel::show_switched(ui, &mut open, collapsed, expanded, |ui, expanded| {
        if !expanded {
            if ui
                .button("»")
                .on_hover_text("Show Objects & Defects (Cmd/Ctrl+\\ shows both panels)")
                .labelled("Show Objects & Defects")
                .clicked()
            {
                app.show_tree = true;
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.heading("Objects & Defects");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("«")
                    .on_hover_text("Hide this panel (Cmd/Ctrl+\\ hides both)")
                    .labelled("Hide Objects & Defects")
                    .clicked()
                {
                    app.show_tree = false;
                }
            });
        });
        ui.separator();
        // One tab per section once there's more than one (the Parts
        // library joins Objects in Phase 1 Task 23, I8).
        if LeftTab::ALL.len() > 1 {
            ui.horizontal(|ui| {
                for &tab in LeftTab::ALL {
                    if ui
                        .selectable_label(app.left_tab == tab, tab.label())
                        .clicked()
                    {
                        app.left_tab = tab;
                    }
                }
            });
            ui.separator();
        }
        egui::Panel::bottom("legend")
            .show_separator_line(true)
            .show(ui, |ui| legend(app, ui));
        match app.left_tab {
            LeftTab::Objects => {
                egui::ScrollArea::vertical()
                    .id_salt("tree")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Long rows are cut short ("…"), never widening the
                        // panel (and so shrinking the view, I5): tooltips
                        // have them all.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        tree(app, ui)
                    });
            }
        }
    });
    // Dragging the edge past the minimum collapses it too.
    if was && !open {
        app.show_tree = false;
    }
}

/// Below this window width (points), the side panels open narrower.
const NARROW_WINDOW: f32 = 1100.0;
/// A collapsed side panel: just its "show" button.
const COLLAPSED_WIDTH: f32 = 28.0;
const TREE_WIDTH_NARROW: f32 = 180.0;
const TREE_WIDTH_MAX: f32 = 420.0;
const PROPERTIES_WIDTH_NARROW: f32 = 240.0;
const PROPERTIES_WIDTH_MAX: f32 = 480.0;

/// The view's colours, each with its switch (View > Show defects hides
/// every defect overlay at once). Data-driven: an open interactive tool
/// supplies its own rows (I8); collapsible, so the tree (and later the
/// Parts library) gets the room on a laptop screen.
fn legend(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let defects = app.show_boundaries;
    egui::CollapsingHeader::new(RichText::new("Legend").strong())
        .id_salt("legend")
        .default_open(true)
        .show(ui, |ui| {
            for row in app.legend_rows() {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, row.color);
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
                            ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
                            ui.label(row.name).on_hover_text(row.meaning);
                        }
                    }
                });
            }
        });
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
        ui.weak(empty_state());
    }
    for (id, name, tris) in objects {
        let active = app.active == Some(id);
        let text = if active {
            RichText::new(&name).strong()
        } else {
            RichText::new(&name).color(theme::TEXT_MUTED)
        };
        let tip = format!(
            "{}{} · double-click to frame it",
            count(tris, "triangle"),
            if active {
                ""
            } else {
                " · click to work on it"
            }
        );
        tree_row(
            app,
            ui,
            &TreeItem::Object(id),
            Row {
                text,
                size: None,
                label: &name,
            },
            |ui| {
                ui.label(&tip);
            },
        );
        if app.tree_expanded() == Some(id) {
            ui.indent(("defects", id.0), |ui| defects_tree(app, ui, id));
        }
    }
}

/// What a tree row shows: its text, its size at the right (muted), and
/// its accessible label (both, as words).
struct Row<'a> {
    text: RichText,
    size: Option<&'a str>,
    label: &'a str,
}

/// A one-click action at a row's right ("Fill", "Remove",
/// "Unmark"): its text and accessible label ("Fill Hole 3").
struct RowAction {
    text: &'static str,
    label: String,
    tip: &'static str,
}

/// One row of "Objects & Defects". Every row (an object, "Model", each
/// defect, and any row added later) behaves the same way, here (M4 Task
/// 8a): click selects its item; double-click, or Enter on the focused row,
/// selects it and frames it in the view (animated); hovering highlights it
/// in the view (an outline, or a glow); `tooltip` says more.
fn tree_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    item: &TreeItem,
    row: Row<'_>,
    tooltip: impl FnOnce(&mut egui::Ui),
) {
    tree_row_with(app, ui, item, row, None, tooltip);
}

/// [`tree_row`] with a one-click action at its right; true if it was
/// clicked.
fn tree_row_with(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    item: &TreeItem,
    row: Row<'_>,
    action: Option<RowAction>,
    tooltip: impl FnOnce(&mut egui::Ui),
) -> bool {
    let Some(action) = action else {
        let width = ui.available_width();
        tree_row_sized(app, ui, item, row, width, tooltip);
        return false;
    };
    // The row, then the action, in one line exactly as wide as the panel:
    // anything wider would widen the panel and move the 3D view.
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text = ui
        .painter()
        .layout_no_wrap(action.text.to_owned(), font, theme::TEXT_MUTED)
        .size()
        .x;
    let action_width = text + 2.0 * ui.spacing().button_padding.x + 4.0;
    let spacing = ui.spacing().item_spacing.x;
    let width = ui.available_width();
    let height = ui.spacing().interact_size.y;
    let (line, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let row_width = (width - action_width - spacing).max(20.0);
    let left = egui::Rect::from_min_size(line.min, egui::vec2(row_width, height));
    let right = egui::Rect::from_min_max(egui::pos2(left.right() + spacing, line.top()), line.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
        tree_row_sized(app, ui, item, row, row_width, tooltip);
    });
    let mut acted = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(right)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            let r = ui
                .add_enabled(app.busy.is_none(), egui::Button::new(action.text).small())
                .on_hover_text(action.tip);
            let enabled = r.enabled();
            let label = action.label.clone();
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
            acted = r.clicked();
        },
    );
    acted
}

/// One row (see [`tree_row`]) `width` points wide.
fn tree_row_sized(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    item: &TreeItem,
    row: Row<'_>,
    width: f32,
    tooltip: impl FnOnce(&mut egui::Ui),
) {
    let selected = app.tree_selected(item);
    let mut button = egui::Button::selectable(selected, row.text).min_size(egui::vec2(width, 0.0));
    if let Some(size) = row.size {
        // Smaller, muted; cut short ("…") rather than wrapped when the
        // panel is narrow (the tooltip and the label have it all).
        button = button.right_text(
            RichText::new(size)
                .small()
                .color(theme::TEXT_MUTED)
                .atom_shrink(true),
        );
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
    } else if response.clicked() {
        app.tree_select(item);
    }
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
    tree_row(
        app,
        ui,
        &TreeItem::Model(object),
        Row {
            text: RichText::new("Model"),
            size: None,
            label: "Model",
        },
        |ui| {
            ui.label("The whole object: its operations are in Properties");
            ui.label(RichText::new("Double-click to frame it").color(theme::TEXT_MUTED));
        },
    );
    holes_tree(app, ui, object, &defects, units);
    // Cracks marked intentional are listed with the openings instead.
    let all_cracks: Vec<usize> = (0..defects.cracks.len()).collect();
    let cracks: Vec<usize> = match app.hole_view_of(object) {
        Some(view) => view.cracks.clone(),
        None => all_cracks,
    };
    let every = |n: usize| (0..n).collect::<Vec<usize>>();
    // (name, tooltip, its defects, the rows listed, colour)
    type Category<'a> = (&'a str, &'a str, &'a [Defect], Vec<usize>, egui::Color32);
    let categories: [Category<'_>; 3] = [
        (
            "Narrow cracks",
            "Edges that almost touch, like a hairline seam",
            &defects.cracks,
            cracks,
            theme::CRACK,
        ),
        (
            "Loose pieces",
            "Pieces not connected to the model",
            &defects.loose,
            every(defects.loose.len()),
            theme::LOOSE_PIECE,
        ),
        (
            "Tangled edges",
            "Tangled (non-manifold) edges: more than two triangles share each one",
            &defects.tangled,
            every(defects.tangled.len()),
            theme::NON_MANIFOLD,
        ),
    ];
    for (name, tip, list, rows, color) in categories {
        let label = format!("{name} ({})", rows.len());
        let text = if rows.is_empty() {
            RichText::new(label).color(theme::TEXT_MUTED)
        } else {
            RichText::new(label).color(color)
        };
        egui::CollapsingHeader::new(text)
            .id_salt((object.0, name))
            .default_open(!rows.is_empty())
            .show(ui, |ui| {
                virtual_rows(ui, rows.len(), |ui, i| {
                    defect_row(app, ui, object, &defects, &list[rows[i]], units);
                });
            })
            .header_response
            .on_hover_text(tip);
    }
}

/// The holes: next / previous, each size group with its count, group
/// action and rows (largest first), and the intentional openings.
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
    let label = format!("Holes ({})", view.listed.len());
    let text = if view.listed.is_empty() {
        RichText::new(label).color(theme::TEXT_MUTED)
    } else {
        RichText::new(label).color(theme::HOLE)
    };
    egui::CollapsingHeader::new(text)
        .id_salt((object.0, "Holes"))
        .default_open(!view.listed.is_empty())
        .open(reveal.map(|_| true))
        .show(ui, |ui| {
            if !view.listed.is_empty() {
                ui.horizontal(|ui| {
                    if ui
                        .small_button("Previous")
                        .on_hover_text("Select and frame the previous hole (Shift+N)")
                        .labelled("Previous hole")
                        .clicked()
                    {
                        app.next_hole(false);
                    }
                    if ui
                        .small_button("Next")
                        .on_hover_text("Select and frame the next hole (N)")
                        .labelled("Next hole")
                        .clicked()
                    {
                        app.next_hole(true);
                    }
                });
            }
            // Each size group's count and actions, together at the top so
            // they're in reach however many holes there are.
            for group in HoleGroup::ALL {
                let range = view.group(group);
                if range.is_empty() {
                    continue;
                }
                let n = range.len();
                let plural = if n == 1 { "hole" } else { "holes" };
                ui.add(
                    egui::Label::new(RichText::new(format!("{} · {n}", group.title())).strong())
                        .truncate(),
                );
                ui.horizontal_wrapped(|ui| {
                    if group == HoleGroup::TooLarge {
                        // Over the fill method's limit (MF-ISSUE-043): say
                        // so, and offer what works today.
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Over this fill method's {}-edge limit. Often a scan's \
                                     open base: mark it if it's meant to be open.",
                                    thousands(crate::defects::FILL_CAP_EDGES as u64)
                                ))
                                .color(theme::TEXT_MUTED),
                            )
                            .wrap(),
                        );
                    } else {
                        // N is what Apply fills: pinched holes are left out.
                        let k = view.group_fillable(group).len();
                        let fill = format!(
                            "Fill all {k} {} {}",
                            group.adjective(),
                            if k == 1 { "hole" } else { "holes" }
                        );
                        if ui
                            .add_enabled(
                                app.busy.is_none() && k > 0,
                                egui::Button::new(&fill).small(),
                            )
                            .on_hover_text("One step: one Undo brings them all back")
                            .clicked()
                        {
                            let ids = app.group_fillable(group);
                            app.fill_loops(ids);
                        }
                    }
                    let mark = format!(
                        "Mark all {n} {} {plural} as intentional openings",
                        group.adjective()
                    );
                    if ui
                        .add_enabled(
                            app.busy.is_none(),
                            egui::Button::new("Mark intentional").small(),
                        )
                        .on_hover_text("Leave them open: they're left out of counts and fills")
                        .labelled(&mark)
                        .clicked()
                    {
                        let ids: Vec<u32> = view.listed[range.clone()]
                            .iter()
                            .filter_map(|&i| defects.holes[i].selection().map(|(_, id)| id))
                            .collect();
                        app.mark_openings(&ids, true);
                    }
                });
            }
            ui.add_space(4.0);
            // Then every hole, a list per group (largest first).
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
                // Short: a collapsing header never cuts its text, so a long
                // one would widen the panel (I5).
                egui::CollapsingHeader::new(format!("{} ({})", group.short_title(), rows.len()))
                    .id_salt((object.0, "hole group", group.title()))
                    .default_open(true)
                    .open(reveal.map(|_| true))
                    .show(ui, |ui| {
                        virtual_rows_to(ui, rows.len(), reveal, |ui, k| {
                            let d = &defects.holes[rows[k]];
                            hole_row(app, ui, object, defects, d, units, false);
                        });
                    })
                    .header_response
                    .on_hover_text(group.title());
            }
        });
    let marked = view.marked.len() + view.marked_cracks.len();
    if marked > 0 {
        egui::CollapsingHeader::new(
            RichText::new(format!("Intentional openings ({marked})")).color(theme::OPENING),
        )
        .id_salt((object.0, "Intentional openings"))
        .default_open(true)
        .show(ui, |ui| {
            virtual_rows(ui, marked, |ui, k| {
                let d = match view.marked.get(k) {
                    Some(&i) => &defects.holes[i],
                    None => &defects.cracks[view.marked_cracks[k - view.marked.len()]],
                };
                hole_row(app, ui, object, defects, d, units, true);
            });
        });
    }
}

/// A hole's row: Fill at its right (when it can be filled), or Unmark
/// for an intentional opening.
fn hole_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    object: mf_command::ObjectId,
    defects: &crate::defects::Defects,
    d: &Defect,
    units: mf_io::threemf::Unit,
    marked: bool,
) {
    let copy = defects.copy(d, units);
    let fillable = defects.fillable(d);
    let action = if marked {
        Some(RowAction {
            text: "Unmark",
            label: format!("Unmark {}", copy.name),
            tip: "List it as a hole again",
        })
    } else if fillable {
        Some(RowAction {
            text: "Fill",
            label: format!("Fill {}", copy.name),
            tip: "Fill just this hole (one step)",
        })
    } else {
        None
    };
    let next = if marked {
        "Unmark it to list it again".to_owned()
    } else {
        copy.next_action.clone()
    };
    let clicked = tree_row_with(
        app,
        ui,
        &TreeItem::Defect(object, d.clone()),
        Row {
            text: RichText::new(&copy.name),
            size: Some(&copy.size),
            label: &copy.row,
        },
        action,
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
            ui.label(RichText::new("Double-click to frame it").color(theme::TEXT_MUTED));
        },
    );
    if clicked && let Some((_, id)) = d.selection() {
        if marked {
            app.mark_openings(&[id], false);
        } else {
            app.fill_loops(vec![id]);
        }
    }
}

/// Give a widget an accessible name of its own (and the name tests find
/// it by), keeping its visible text short.
trait Labelled {
    fn labelled(self, label: &str) -> Self;
}

impl Labelled for egui::Response {
    fn labelled(self, label: &str) -> Self {
        let enabled = self.enabled();
        self.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
        self
    }
}

fn defect_row(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    object: mf_command::ObjectId,
    defects: &crate::defects::Defects,
    d: &Defect,
    units: mf_io::threemf::Unit,
) {
    let copy = defects.copy(d, units);
    let action = match d {
        Defect::LoosePiece { .. } => Some(RowAction {
            text: "Remove",
            label: format!("Remove {}", copy.name),
            tip: "Remove just this loose piece (one step)",
        }),
        _ => None,
    };
    let clicked = tree_row_with(
        app,
        ui,
        &TreeItem::Defect(object, d.clone()),
        Row {
            text: RichText::new(&copy.name),
            size: Some(&copy.size),
            label: &copy.row,
        },
        action,
        |ui| {
            ui.strong(&copy.title);
            ui.label(&copy.detail);
            ui.label(&copy.verdict);
            if let Some(limit) = &copy.limit {
                ui.label(RichText::new(limit).color(theme::TEXT_MUTED));
            }
            ui.label(RichText::new(format!("Next: {}", copy.next_action)).color(theme::TEXT_MUTED));
            ui.label(RichText::new("Double-click to frame it").color(theme::TEXT_MUTED));
        },
    );
    if clicked
        && let Defect::LoosePiece {
            component,
            triangles,
            ..
        } = d
    {
        app.remove_piece(*component, *triangles);
    }
}

/// Right: "Properties & Tools", contextual to the current target: what it
/// is, the operations valid for it, their parameters, and Apply.
pub fn properties_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let narrow = ui.ctx().content_rect().width() < NARROW_WINDOW;
    let collapsed = egui::Panel::right("properties collapsed")
        .resizable(false)
        .exact_size(COLLAPSED_WIDTH);
    let expanded = egui::Panel::right("properties")
        .resizable(true)
        .default_size(if narrow {
            PROPERTIES_WIDTH_NARROW
        } else {
            PROPERTIES_WIDTH
        })
        .size_range(200.0..=PROPERTIES_WIDTH_MAX);
    let was = app.show_properties;
    let mut open = was;
    egui::Panel::show_switched(ui, &mut open, collapsed, expanded, |ui, expanded| {
        if !expanded {
            if ui
                .button("«")
                .on_hover_text("Show Properties & Tools (Cmd/Ctrl+\\ shows both panels)")
                .labelled("Show Properties & Tools")
                .clicked()
            {
                app.show_properties = true;
            }
            return;
        }
        ui.horizontal(|ui| {
            if ui
                .small_button("»")
                .on_hover_text("Hide this panel (Cmd/Ctrl+\\ hides both)")
                .labelled("Hide Properties & Tools")
                .clicked()
            {
                app.show_properties = false;
            }
            ui.heading("Properties & Tools");
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("properties")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                notice(app, ui);
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
    if was && !open {
        app.show_properties = false;
    }
}

/// The last command's result when it carried a warning, in full (I1).
fn notice(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(text) = app.notice.clone() else {
        return;
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Last result").strong());
        ui.add(egui::Label::new(&text).wrap());
        if ui
            .small_button("Dismiss")
            .labelled("Dismiss note")
            .clicked()
        {
            app.notice = None;
        }
    });
    ui.add_space(8.0);
}

/// What the target is, in plain language.
fn target_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let units = app.session.document().units();
    let analysis = app.current_analysis();
    if let Some(inspected) = app.current_inspected() {
        let copy = defect_copy(&inspected.defect, units);
        ui.label(RichText::new(&copy.title).strong().size(16.0));
        ui.label(&copy.detail);
        verdict(ui, &copy);
        if ui
            .add(primary_button(ui, &copy.next_action))
            .on_hover_text("Selects the triangles at this edge, so F frames them")
            .clicked()
        {
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
                let copy = match analysis {
                    Some(a) => a.defects.copy(&defects[0], units),
                    None => defect_copy(&defects[0], units),
                };
                ui.label(RichText::new(&copy.title).strong().size(16.0));
                ui.label(&copy.detail);
                verdict(ui, &copy);
            }
            (Some(defects), n) => {
                ui.label(RichText::new(selection_title(&sel, n)).strong().size(16.0));
                let blocked: Vec<_> = defects
                    .iter()
                    .filter(|d| {
                        !matches!(d, Defect::LoosePiece { .. })
                            && !analysis.is_some_and(|a| a.defects.fillable(d))
                    })
                    .collect();
                if let (Some(d), Some(a)) = (blocked.first(), analysis) {
                    let copy = a.defects.copy(d, units);
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
                let tangled = sel.kind == SelectionKind::Faces
                    && analysis.is_some_and(|a| {
                        sel.ids
                            .iter()
                            .all(|f| a.tangled_faces.binary_search(f).is_ok())
                    });
                if main_body {
                    ui.label(RichText::new("Main body").strong().size(16.0));
                    if let Some(a) = analysis {
                        ui.label(count(a.defects.main_body_triangles, "triangle"));
                    }
                } else {
                    ui.label(RichText::new(selection_title(&sel, n)).strong().size(16.0));
                }
                ui.label(
                    RichText::new(if tangled {
                        "These triangles meet at a tangled edge. Press F to frame them and \
                         look closer. MeshFixer can't untangle edges yet: repairing them is \
                         planned. The tools below act on the whole object."
                    } else {
                        "No operation works on this selection yet; these act on the whole object."
                    })
                    .color(theme::TEXT_MUTED),
                );
            }
        }
        let holes: Vec<u32> = match &found {
            Some(defects) if sel.kind == SelectionKind::BoundaryLoop => defects
                .iter()
                .filter_map(|d| d.selection().map(|(_, id)| id))
                .collect(),
            _ => Vec::new(),
        };
        // Primary when nothing can fill the target (C1): it's the way on.
        let refused = app.tools().is_empty();
        if !holes.is_empty() {
            let text = if holes.len() == 1 {
                crate::defects::MARK_INTENTIONAL.to_owned()
            } else {
                format!("Mark {} as intentional openings", holes.len())
            };
            let button = if refused {
                primary_button(ui, &text)
            } else {
                egui::Button::new(text)
            };
            if ui
                .add_enabled(app.busy.is_none(), button)
                .on_hover_text(
                    "It's meant to be open (a scan's base, a mouth, a socket): leave it out \
                     of defect counts and fills. Undo, or Unmark under Intentional openings, \
                     to list it again.",
                )
                .clicked()
            {
                app.mark_openings(&holes, true);
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
            "Analysing…"
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
    let (holes, cracks, marked) =
        app.hole_view()
            .map_or((a.defects.holes.len(), a.defects.cracks.len(), 0), |v| {
                (
                    v.listed.len(),
                    v.cracks.len(),
                    v.marked.len() + v.marked_cracks.len(),
                )
            });
    let problems = holes + cracks + a.defects.loose.len() + a.defects.tangled.len();
    let verdict = print_verdict(
        d,
        holes + cracks,
        marked,
        a.defects.loose.len(),
        a.defects.tangled.len(),
    );
    ui.label(RichText::new(verdict).strong());
    ui.label(if problems == 0 {
        RichText::new("No defects found").color(theme::TEXT_MUTED)
    } else if holes > 0 {
        RichText::new(format!(
            "{} to look at, listed on the left. Press N to visit the holes one by one.",
            count(problems, "defect")
        ))
        .color(theme::TEXT_MUTED)
    } else {
        RichText::new(format!(
            "{} to look at, listed on the left",
            count(problems, "defect")
        ))
        .color(theme::TEXT_MUTED)
    });
    // From the analysis (worked out on a worker), never counted here.
    if d.dead_slot_ratio > SUGGEST_COMPACT_ABOVE {
        ui.label(
            RichText::new(
                "This model has lots of unused space from earlier edits. Compact to reclaim it.",
            )
            .color(theme::TEXT_MUTED),
        );
    }
}

/// Whether the object will print, in one line, from the analysis: closed
/// and manifold, or what's left (M4 final UX review C1). `open` counts the
/// listed holes and cracks, `marked` the intentional openings.
pub fn print_verdict(
    d: &mf_geometry::Diagnostics,
    open: usize,
    marked: usize,
    loose: usize,
    tangled: usize,
) -> String {
    let left = open + marked;
    let mut parts = Vec::new();
    if left > 0 {
        let mut text = format!("Not watertight: {} left", count(left, "opening"));
        if marked > 0 {
            text.push_str(&format!(" ({marked} marked intentional)"));
        }
        parts.push(text);
    } else if !d.is_closed {
        parts.push("Not watertight".to_owned());
    }
    if tangled > 0 || !d.is_manifold {
        parts.push("Has tangled edges: may print incorrectly".to_owned());
    }
    if !parts.is_empty() {
        return parts.join(" · ");
    }
    if loose == 0 {
        "Watertight: ready to print".to_owned()
    } else {
        format!(
            "Watertight, but {} will print separately",
            count(loose, "loose piece")
        )
    }
}

/// What it means for printing, and why MeshFixer can't repair it yet (if
/// it can't), under a target's title.
fn verdict(ui: &mut egui::Ui, copy: &crate::defects::DefectCopy) {
    ui.label(&copy.verdict);
    if let Some(limit) = &copy.limit {
        ui.label(RichText::new(limit).color(theme::TEXT_MUTED));
    }
}

/// A full-width button: the target's next action when no tool applies.
fn primary_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).strong())
        .min_size(egui::vec2(ui.available_width(), 26.0))
}

/// Above this fraction of unused (tombstoned) slots, Properties suggests
/// `mesh.compact` (MF-ISSUE-020).
const SUGGEST_COMPACT_ABOVE: f64 = 0.5;

fn selection_title(sel: &Selection, n: usize) -> String {
    match sel.kind {
        SelectionKind::BoundaryLoop => format!("{} selected", count(n, "hole")),
        SelectionKind::Component => format!("{} selected", count(n, "piece")),
        SelectionKind::Faces => format!("{} selected", count(n, "triangle")),
        SelectionKind::Object => "Whole object".to_owned(),
    }
}

/// The operations valid for the target. Targeted tools are open at once;
/// whole-object tools are listed by group (`tools::CATALOGUE`, I8) and
/// open when chosen.
fn tools_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let tools = app.tools();
    if tools.is_empty() {
        return;
    }
    let targeted = tools.iter().any(|t| t.selection.is_some());
    if targeted {
        ui.label(RichText::new("Tool").strong());
        for tool in &tools {
            ui.label(RichText::new(&tool.label).size(15.0).strong());
            ui.indent(("tool", &tool.op_id), |ui| tool_body(app, ui, tool));
        }
        return;
    }
    ui.label(RichText::new("Tools for the whole object").strong());
    let mut group = None;
    for tool in &tools {
        if tool.group != group {
            group = tool.group;
            if let Some(g) = group {
                ui.add_space(4.0);
                ui.label(RichText::new(g.label()).color(theme::TEXT_MUTED).small());
            }
        }
        let open = app.tool.as_deref() == Some(&tool.op_id);
        if ui
            .add(
                egui::Button::new(&tool.label)
                    .selected(open)
                    .min_size(egui::vec2(ui.available_width(), 24.0)),
            )
            .on_hover_text(if open { "Close" } else { "Open this tool" })
            .clicked()
        {
            if open {
                app.close_tool();
            } else {
                app.open_tool(&tool.op_id);
            }
        }
        if app.tool.as_deref() == Some(&tool.op_id) {
            ui.indent(("tool", &tool.op_id), |ui| match tool.kind {
                ToolKind::OneShot => tool_body(app, ui, tool),
                ToolKind::Interactive => interactive_body(app, ui),
            });
        }
    }
}

/// The open interactive tool's panel.
fn interactive_body(app: &mut MeshFixerApp, _ui: &mut egui::Ui) {
    if let Some(t) = &app.interactive {
        match *t {}
    }
}

/// Why Refine is on and greyed out for a smooth or curvature fill.
const REFINE_FORCED: &str = "Smooth and Curvature always add points so the patch can curve";

fn tool_body(app: &mut MeshFixerApp, ui: &mut egui::Ui, tool: &Tool) {
    let Some(spec) = mf_spec::builtin::get(&tool.op_id) else {
        ui.label(format!("Unknown operation {}", tool.op_id));
        return;
    };
    if let Some(plain) = tool_summary(&tool.op_id) {
        ui.label(plain);
    }
    ui.label(
        RichText::new(&spec.summary)
            .color(theme::TEXT_MUTED)
            .small(),
    );
    let batch = tool.op_id == "mesh.fill_holes" && tool.selection.is_none();
    let values = app.params_for(&tool.op_id);
    // A fill that isn't flat always refines (MF-OP-002): it needs the
    // added points to curve.
    let refine_forced = tool.op_id == "mesh.fill_holes"
        && matches!(values.get("fill_style"), Some(ParamValue::Text(s)) if s != "flat");
    // The batch fill's size limit: a slider on its own line, as wide as
    // the panel allows (a wider grid would widen the panel).
    let limit = spec.parameters.iter().find(|p| p.name == "max_loop_edges");
    if batch && let (Some(p), Some(ParamValue::Int(v))) = (limit, values.get_mut("max_loop_edges"))
    {
        let (min, max) = match p.kind {
            ParamKind::Integer { min, max, .. } => (min.unwrap_or(3), max.unwrap_or(1000)),
            _ => (3, 1000),
        };
        ui.label(param_label(&tool.op_id, &p.name))
            .on_hover_text(&p.description);
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 110.0).max(60.0);
            ui.add(
                egui::Slider::new(v, min..=max)
                    .logarithmic(true)
                    .suffix(" edges"),
            )
            .on_hover_text(&p.description);
        });
    }
    egui::Grid::new(("params", &tool.op_id))
        .num_columns(2)
        .show(ui, |ui| {
            for p in spec
                .parameters
                .iter()
                .filter(|p| !tool.fixed.contains_key(&p.name))
                .filter(|p| !(batch && p.name == "max_loop_edges"))
                // Choices get their own block below the grid.
                .filter(|p| !matches!(p.kind, ParamKind::Choice { .. }))
            {
                let Some(value) = values.get_mut(&p.name) else {
                    continue;
                };
                let name = param_label(&tool.op_id, &p.name);
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
                    (ParamKind::Bool { .. }, ParamValue::Bool(_))
                        if refine_forced && p.name == "refine" =>
                    {
                        // Why it's greyed out: once, below the grid (a grid cell never wraps, so a
                        // line here would widen the panel and move the
                        // view when the tool opens).
                        ui.label("");
                        ui.add_enabled(false, egui::Checkbox::new(&mut true, &name))
                            .on_disabled_hover_text(&p.description);
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
    // A choice: its name, then one option per line, each with a plain
    // one-line explanation under it.
    for p in spec
        .parameters
        .iter()
        .filter(|p| !tool.fixed.contains_key(&p.name))
    {
        let (ParamKind::Choice { values, .. }, Some(ParamValue::Text(v))) =
            (&p.kind, values.get_mut(&p.name))
        else {
            continue;
        };
        ui.label(param_label(&tool.op_id, &p.name))
            .on_hover_text(&p.description);
        for value in values {
            let (label, line) = choice_copy(&tool.op_id, &p.name, value)
                // allow-swallow: a choice without plain words shows its value
                .unwrap_or((value.as_str(), ""));
            ui.radio_value(v, value.clone(), label)
                .on_hover_text(&p.description);
            if !line.is_empty() {
                ui.indent(("choice", &p.name, value), |ui| {
                    ui.label(RichText::new(line).color(theme::TEXT_MUTED).small());
                });
            }
        }
    }
    if refine_forced {
        ui.add(
            egui::Label::new(
                RichText::new(REFINE_FORCED)
                    .color(theme::TEXT_MUTED)
                    .small(),
            )
            .wrap(),
        );
    }
    if let Some(warning) = curvature_warning(app, tool) {
        ui.add(egui::Label::new(RichText::new(warning).color(theme::TEXT_MUTED)).wrap());
    }
    if batch
        && let (Some(view), Some(ParamValue::Int(limit))) = (
            app.hole_view(),
            app.op_params
                .get("mesh.fill_holes")
                .and_then(|p| p.get("max_loop_edges")),
        )
    {
        let (fill, open) = view.batch((*limit).max(0) as usize);
        let text = format!(
            "{} · {} left open",
            count(fill, "hole"),
            thousands(open as u64)
        );
        ui.label(RichText::new(&text).color(theme::TEXT_MUTED))
            .on_hover_text("Holes over the limit, too large or pinched stay open; intentional openings aren't counted");
    }
    if tool.selection.is_none()
        && let Some(p) = app.removal_preview()
    {
        let share = 100.0 * p.triangles as f64 / p.of_triangles.max(1) as f64;
        ui.label(
            RichText::new(format!(
                "Removes {} of {} ({share:.1}% of triangles)",
                thousands(p.pieces),
                count(p.of_pieces as usize, "piece")
            ))
            .color(theme::TEXT_MUTED),
        )
        .on_hover_text("Tinted in the view: the pieces Apply would remove");
    }
    // A batch fill that would fill nothing at this limit isn't offered.
    let nothing_to_fill = batch
        && match (
            app.hole_view(),
            app.op_params
                .get("mesh.fill_holes")
                .and_then(|p| p.get("max_loop_edges")),
        ) {
            (Some(view), Some(ParamValue::Int(limit))) => {
                view.batch((*limit).max(0) as usize).0 == 0
            }
            _ => false,
        };
    ui.horizontal(|ui| {
        let can_apply = app.busy.is_none() && app.active.is_some() && !nothing_to_fill;
        if ui
            .add_enabled(can_apply, egui::Button::new("Apply"))
            .on_disabled_hover_text(if nothing_to_fill {
                "Nothing to fill at this limit"
            } else {
                "Busy, or nothing open"
            })
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

/// Before Apply: curvature matching is less accurate on holes over
/// [`mf_operations::CURVATURE_ACCURATE_EDGES`] edges (MF-ISSUE-055), so a
/// curvature fill of one says so, with Smooth as the way round (I1).
fn curvature_warning(app: &MeshFixerApp, tool: &Tool) -> Option<String> {
    use mf_operations::CURVATURE_ACCURATE_EDGES as LIMIT;
    if tool.op_id != "mesh.fill_holes" {
        return None;
    }
    let curvature = app
        .op_params
        .get("mesh.fill_holes")
        .and_then(|p| p.get("fill_style"))
        .is_some_and(|v| matches!(v, ParamValue::Text(s) if s == "curvature"));
    if !curvature {
        return None;
    }
    let advice = "Smooth is recommended";
    match &tool.selection {
        Some(sel) => {
            let a = app.current_analysis()?;
            let edges: Vec<usize> = sel
                .ids
                .iter()
                .filter_map(|&id| match a.defects.find(sel.kind, id)? {
                    Defect::Hole { edges, .. } => Some(*edges),
                    _ => None,
                })
                .filter(|&e| e > LIMIT)
                .collect();
            match (edges.as_slice(), sel.ids.len()) {
                ([], _) => None,
                ([e], 1) => Some(format!(
                    "Less accurate on holes over {LIMIT} edges (this one has {e}): {advice}."
                )),
                (big, _) => Some(format!(
                    "Less accurate on holes over {LIMIT} edges ({} of these {}): {advice}.",
                    big.len(),
                    if big.len() == 1 { "is" } else { "are" }
                )),
            }
        }
        None => {
            let view = app.hole_view()?;
            let limit = match app
                .op_params
                .get("mesh.fill_holes")
                .and_then(|p| p.get("max_loop_edges"))
            {
                Some(ParamValue::Int(v)) => (*v).max(0) as usize,
                _ => return None,
            };
            let (upto_limit, _) = view.batch(limit);
            let (accurate, _) = view.batch(LIMIT);
            let big = upto_limit.saturating_sub(accurate);
            (big > 0).then(|| {
                format!(
                    "Less accurate on holes over {LIMIT} edges ({} of these {}): {advice}.",
                    thousands(big as u64),
                    if big == 1 { "is" } else { "are" }
                )
            })
        }
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
                    row(
                        "Tangled edges (non-manifold)",
                        d.non_manifold_edges.to_string(),
                    );
                    row(
                        "Pinched points (non-manifold vertices)",
                        d.non_manifold_vertices.to_string(),
                    );
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
                        "Unused space",
                        format!("{:.0}% of slots", d.dead_slot_ratio * 100.0),
                    );
                    row(
                        "Volume",
                        d.signed_volume
                            .map_or("— (not closed)".into(), |v| format!("{v:.4}")),
                    );
                });
            if !app.warnings.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "Import warnings ({})",
                    thousands(app.warnings.len() as u64)
                ))
                .id_salt("import warnings")
                .show(ui, |ui| {
                    for w in grouped_warnings(&app.warnings) {
                        ui.add(egui::Label::new(RichText::new(w).small()).wrap());
                    }
                });
            }
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
