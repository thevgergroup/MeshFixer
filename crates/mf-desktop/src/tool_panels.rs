//! The rail, the tool dock and the built-in tools (docs/design/ui-plan.md
//! §3, §5, §7): Select, Transform, Repair, Inspect and Export, registered
//! through `shell` exactly as a plugin's tools would be, with their
//! panels; the inspector card (no tool open); the Browser's right-click
//! actions; the first-use hint; and the Developer settings page.

use eframe::egui::{self, RichText};
use mf_command::ObjectId;
use mf_selection::{Selection, SelectionKind};
use mf_spec::{ParamKind, ParamValue};
use mf_units::Unit;

use crate::app::{MeshFixerApp, NarrowFocus, Tool, TreeItem};
use crate::defects::{
    Defect, HoleGroup, choice_copy, count, defect_copy, measure, param_label, thousands,
    tool_summary, unit_symbol,
};
use crate::shell::{
    Availability, Category, Claims, ContextAction, Input, InspectorSection, RegisterError,
    SettingsPage, Shell,
};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme, tp};

pub const SELECT: &str = "select";
pub const TRANSFORM: &str = crate::transform_tool::ID;
pub const REPAIR: &str = "repair";
pub const INSPECT: &str = "inspect";
pub const EXPORT: &str = "export";

/// Below this window width (points) the tool panel and the Browser can't
/// both be open (ui-plan.md §3, "Laptop size").
pub const NARROW_WINDOW: f32 = 1360.0;
/// Below this width the rail drops its labels and the Browser becomes a
/// drawer over the view.
pub const COMPACT_WINDOW: f32 = 1100.0;
/// The rail's width with labels, and with icons only.
pub const RAIL_WIDTH: f32 = 64.0;
pub const RAIL_WIDTH_COMPACT: f32 = 48.0;
/// The tool dock's default width and range.
const DOCK_WIDTH: f32 = 280.0;
const DOCK_WIDTH_NARROW: f32 = 256.0;
/// A collapsed dock: a strip with its icon.
pub const STRIP_WIDTH: f32 = 36.0;

fn needs_object(app: &MeshFixerApp) -> Availability {
    if app.active_mesh().is_some() {
        Availability::Available
    } else {
        Availability::Unavailable(crate::panels::empty_state())
    }
}

/// The built-in tools, inspector sections, context actions and settings
/// page.
pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    use crate::shell::Tool as T;
    shell.register_tool(T {
        shortcut: Some(egui::Key::Q),
        help: "tool.select.help",
        ..T::new(
            SELECT,
            "tool.select.name",
            icons::SELECT,
            Category::Arrange,
            0,
        )
    })?;
    shell.register_tool(T {
        shortcut: Some(egui::Key::T),
        help: "tool.transform.help",
        availability: needs_object,
        panel: Some(transform_panel),
        claims: |app| {
            app.transform_grab().map(|(g, cam, view)| {
                Box::new(move |px: (f64, f64)| g.hit(&cam, view, px).is_some()) as Claims
            })
        },
        viewport_input: |app, input| {
            app.transform_pointer(input.pointer, input.hover, input.modifiers);
            Input::Consumed
        },
        keys: |app| crate::tools::tool_keys(app.interactive.as_ref()),
        on_key: |app, key| match key {
            crate::tools::ToolKey::Mode(mode) => app.set_transform_mode(mode),
            // Transform's own keys never include a brush shortcut or
            // Connect's delete (those are other tools' `keys` tables);
            // unreachable here, but `ToolKey` is one enum shared across
            // tools, so every tool's `on_key` must match it exhaustively.
            crate::tools::ToolKey::Brush(_) | crate::tools::ToolKey::DeleteSelection => {}
        },
        overlays: |_| Some(crate::tools::transform_legend()),
        hides_defects: |_| true,
        on_open: |app| {
            app.clear_selection();
            app.tool = None;
            app.start_transform();
        },
        on_close: |app, _| {
            app.cancel_tool_drag();
            app.interactive = None;
        },
        ..T::new(
            TRANSFORM,
            "tool.transform.name",
            icons::TRANSFORM,
            Category::Arrange,
            10,
        )
    })?;
    shell.register_tool(T {
        // R: H toggles the selected objects' Solid/Hole role.
        shortcut: Some(egui::Key::R),
        help: "tool.repair.help",
        availability: needs_object,
        panel: Some(repair_panel),
        on_close: |app, _| app.tool = None,
        back: |app| {
            if app.selection.is_some() || app.inspected.is_some() {
                app.clear_selection();
                true
            } else if app.tool.is_some() {
                app.tool = None;
                true
            } else {
                false
            }
        },
        ..T::new(REPAIR, "tool.repair.name", icons::REPAIR, Category::Fix, 0)
    })?;
    shell.register_tool(T {
        shortcut: Some(egui::Key::I),
        help: "tool.inspect.help",
        availability: needs_object,
        panel: Some(inspect_panel),
        ..T::new(
            INSPECT,
            "tool.inspect.name",
            icons::INSPECT,
            Category::Fix,
            10,
        )
    })?;
    shell.register_tool(T {
        help: "tool.export.help",
        availability: needs_object,
        panel: Some(export_panel),
        ..T::new(
            EXPORT,
            "tool.export.name",
            icons::EXPORT,
            Category::Output,
            0,
        )
    })?;

    shell.register_inspector_section(InspectorSection {
        id: "summary",
        order: 0,
        ui: inspector_summary,
    })?;
    shell.register_inspector_section(InspectorSection {
        id: "printability",
        order: 10,
        ui: inspector_status,
    })?;

    for action in context_actions() {
        shell.register_context_action(action)?;
    }
    shell.register_settings_page(SettingsPage {
        id: "developer",
        title: "settings.dev",
        ui: |_, ui| {
            let mut on = crate::i18n::pseudo();
            if ui
                .checkbox(&mut on, t!("settings.dev.pseudo"))
                .on_hover_text(t!("settings.dev.pseudo.tip"))
                .changed()
            {
                crate::i18n::set_pseudo(on);
            }
        },
    })?;
    Ok(())
}

/// The defect a Browser row stands for, with its plain-language copy.
fn row_defect(app: &MeshFixerApp, item: &TreeItem) -> Option<(Defect, crate::defects::DefectCopy)> {
    let TreeItem::Defect(object, d) = item else {
        return None;
    };
    let units = app.session.document().units();
    let copy = match app.analysis_of(*object) {
        Some(a) => a.defects.copy(d, units),
        None => defect_copy(d, units),
    };
    Some((d.clone(), copy))
}

/// A defect row's name ("Hole 3") for its context actions' titles (they
/// apply to defect rows only).
fn row_name(app: &MeshFixerApp, item: &TreeItem) -> String {
    row_defect(app, item)
        .map(|(_, c)| c.name)
        .expect("defect actions apply to defect rows")
}

fn is_marked(app: &MeshFixerApp, item: &TreeItem) -> bool {
    let Some((_, id)) = (match item {
        TreeItem::Defect(_, d) => d.selection(),
        _ => None,
    }) else {
        return false;
    };
    app.active_openings().contains(&id)
}

/// The right-click actions on Browser rows (and later on the model).
/// The objects a role change from `item`'s menu applies to: the objects
/// selected together if the row's object is one of them, else just it.
fn role_targets(app: &MeshFixerApp, item: &TreeItem) -> Vec<mf_command::ObjectId> {
    let TreeItem::Object(id) = item else {
        return Vec::new();
    };
    let selected = app.selected_objects();
    if selected.contains(id) {
        selected
    } else {
        vec![*id]
    }
}

fn context_actions() -> Vec<ContextAction> {
    vec![
        // An object's Solid/Hole role (also H): on the objects selected
        // together if this row is one of them, otherwise on this object.
        ContextAction {
            id: "object.role",
            applies_to: |_, item| matches!(item, TreeItem::Object(_)),
            title: |app, item| {
                let ids = role_targets(app, item);
                let to_hole = !ids.iter().all(|&id| app.is_hole(id));
                match (to_hole, ids.len()) {
                    (true, 1) => t!("role.make_hole"),
                    (false, 1) => t!("role.make_solid"),
                    (true, n) => t!("role.make_holes", n = n),
                    (false, n) => t!("role.make_solids", n = n),
                }
            },
            run: |app, item| {
                let ids = role_targets(app, item);
                app.toggle_role(&ids);
            },
        },
        // Rename (also F2, or double-clicking the selection panel's name
        // field): starts the same inline edit as F2
        // (`MeshFixerApp::rename_object` commits it).
        ContextAction {
            id: "object.rename_menu",
            applies_to: |_, item| matches!(item, TreeItem::Object(_)),
            title: |_, _| t!("ctx.rename"),
            run: |app, item| {
                if let TreeItem::Object(id) = *item
                    && let Some(name) = app.session.document().object(id).map(|o| o.name.clone())
                {
                    app.renaming = Some((id, name));
                }
            },
        },
        ContextAction {
            id: "defect.fill",
            applies_to: |app, item| {
                matches!(item, TreeItem::Defect(o, d @ Defect::Hole { .. })
                    if !is_marked(app, item)
                        && app.analysis_of(*o).is_some_and(|a| a.defects.fillable(d)))
            },
            title: |app, item| {
                let name = row_name(app, item);
                t!("ctx.fill", name = name)
            },
            run: |app, item| {
                if let TreeItem::Defect(_, d) = item
                    && let Some((_, id)) = d.selection()
                {
                    app.fill_loops(vec![id]);
                }
            },
        },
        ContextAction {
            id: "defect.remove",
            applies_to: |_, item| matches!(item, TreeItem::Defect(_, Defect::LoosePiece { .. })),
            title: |app, item| {
                let name = row_name(app, item);
                t!("ctx.remove", name = name)
            },
            run: |app, item| {
                if let TreeItem::Defect(
                    _,
                    Defect::LoosePiece {
                        component,
                        triangles,
                        ..
                    },
                ) = item
                {
                    app.remove_piece(*component, *triangles);
                }
            },
        },
        ContextAction {
            id: "defect.mark",
            applies_to: |app, item| {
                matches!(
                    item,
                    TreeItem::Defect(
                        _,
                        Defect::Hole { .. } | Defect::OverCap { .. } | Defect::Crack { .. }
                    )
                ) && !is_marked(app, item)
            },
            title: |app, item| {
                let name = row_name(app, item);
                t!("ctx.mark", name = name)
            },
            run: |app, item| {
                if let TreeItem::Defect(_, d) = item
                    && let Some((_, id)) = d.selection()
                {
                    app.mark_openings(&[id], true);
                }
            },
        },
        ContextAction {
            id: "defect.unmark",
            applies_to: is_marked,
            title: |app, item| {
                let name = row_name(app, item);
                t!("ctx.unmark", name = name)
            },
            run: |app, item| {
                if let TreeItem::Defect(_, d) = item
                    && let Some((_, id)) = d.selection()
                {
                    app.mark_openings(&[id], false);
                }
            },
        },
        ContextAction {
            id: "defect.select_faces",
            applies_to: |_, item| matches!(item, TreeItem::Defect(_, Defect::Tangled { .. })),
            title: |app, item| {
                let name = row_name(app, item);
                t!("ctx.select_faces", name = name)
            },
            run: |app, item| {
                app.tree_select(item);
                app.select_tangled_faces();
            },
        },
        ContextAction {
            id: "item.frame",
            // A group has no single place to frame the camera on.
            applies_to: |_, item| !matches!(item, TreeItem::Group(_)),
            title: |app, item| {
                let name = match item {
                    TreeItem::Object(id) | TreeItem::Model(id) => app
                        .session
                        .document()
                        .object(*id)
                        .map(|o| o.name.clone())
                        .expect("a Browser row's object exists"),
                    TreeItem::Defect(..) => row_name(app, item),
                    TreeItem::Group(_) => String::new(),
                };
                t!("ctx.frame", name = name)
            },
            run: |app, item| app.tree_find(item),
        },
    ]
}

// ---------------------------------------------------------------------
// The rail.
// ---------------------------------------------------------------------

/// The rail: every registered tool, an icon and a short label, grouped by
/// category with thin rules between. The active tool has a filled tile
/// and an accent bar. On a compact window it shows icons only.
pub fn rail(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let compact = ui.ctx().content_rect().width() < COMPACT_WINDOW;
    let width = if compact {
        RAIL_WIDTH_COMPACT
    } else {
        RAIL_WIDTH
    };
    egui::Panel::left("rail")
        .resizable(false)
        .exact_size(width)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(theme::RAIL)
                .inner_margin(egui::Margin::symmetric(4, theme::SPACE_S as i8)),
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let tools: Vec<crate::shell::Tool> = app.shell.tools().to_vec();
            // Tiles shrink (to a floor) so every tool fits a short window.
            let rules = tools
                .windows(2)
                .filter(|w| w[0].category != w[1].category)
                .count() as f32;
            let full = if compact { 40.0 } else { 50.0 };
            let fit = (ui.available_height() - rules * 9.0) / tools.len().max(1) as f32 - 2.0;
            let tile = fit.clamp(full - 10.0, full);
            let mut last = None;
            for tool in tools {
                if last.is_some_and(|c| c != tool.category) {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 1.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().line_segment(
                        [
                            r.left_center() + egui::vec2(8.0, 0.0),
                            r.right_center() - egui::vec2(8.0, 0.0),
                        ],
                        egui::Stroke::new(1.0, theme::SEPARATOR),
                    );
                    ui.add_space(4.0);
                }
                last = Some(tool.category);
                rail_item(app, ui, &tool, compact, tile);
            }
        });
}

fn rail_item(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    tool: &crate::shell::Tool,
    compact: bool,
    tile: f32,
) {
    let active = app.active_tool == tool.id;
    let available = (tool.availability)(app);
    let enabled = available == Availability::Available;
    let name = crate::i18n::tr(tool.name, &[]);
    let size = egui::vec2(ui.available_width(), tile);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, r) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if active {
            p.rect_filled(rect, theme::RADIUS, theme::ACCENT_SOFT);
            // The accent bar, at the rail's inner edge.
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() - 4.0, rect.top() + 8.0),
                egui::pos2(rect.left() - 1.0, rect.bottom() - 8.0),
            );
            p.rect_filled(bar, 1.5, theme::ACCENT);
        } else if r.hovered() && enabled {
            p.rect_filled(
                rect,
                theme::RADIUS,
                theme::CONTROL_HOVER.gamma_multiply(0.7),
            );
        }
        let fg = if !enabled {
            theme::TEXT_FAINT
        } else if active {
            theme::TEXT_STRONG
        } else if r.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        };
        let icon_color = if active { theme::ACCENT_HOVER } else { fg };
        let icon_at = if compact {
            rect.center()
        } else {
            egui::pos2(rect.center().x, rect.top() + 18.0 - (50.0 - tile) * 0.5)
        };
        p.text(
            icon_at,
            egui::Align2::CENTER_CENTER,
            tool.icon,
            icons::font(theme::RAIL_ICON_SIZE),
            icon_color,
        );
        if !compact {
            let galley = p.layout_no_wrap(
                name.clone(),
                egui::FontId::proportional(theme::TYPE_CAPTION),
                fg,
            );
            // Truncated to the tile (the tooltip has the full name).
            let clip = rect.shrink2(egui::vec2(2.0, 0.0));
            let at = egui::pos2(
                (rect.center().x - galley.size().x / 2.0).max(clip.left()),
                rect.bottom() - 6.0 + (50.0 - tile) * 0.3 - galley.size().y,
            );
            p.with_clip_rect(clip).galley(at, galley, fg);
        }
    }
    let label = name.clone();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, active, &label));
    let shortcut = match tool.shortcut {
        Some(k) => format!(" ({})", k.name()),
        None => String::new(),
    };
    let help = crate::i18n::tr(tool.help, &[]);
    let tip = match &available {
        Availability::Available => format!("{name}{shortcut}\n{help}"),
        Availability::Unavailable(why) => format!("{name}{shortcut}\n{help}\n{why}"),
    };
    let r = r.on_hover_text(tip);
    if r.clicked() {
        if tool.id == TRANSFORM {
            app.open_transform();
        } else {
            app.open_rail_tool(tool.id);
        }
    }
}

// ---------------------------------------------------------------------
// The tool dock.
// ---------------------------------------------------------------------

/// True if the right dock (the Browser) is open now, given the window.
pub fn tool_panel_shown(app: &MeshFixerApp) -> bool {
    !app.side_panels_hidden && app.active_tool_def().panel.is_some()
}

/// The left tool dock: the open tool's panel only (header, sections, one
/// primary action). On a laptop-size window with the Browser open it
/// collapses to a strip with the tool's icon.
pub fn tool_dock(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if !tool_panel_shown(app) {
        return;
    }
    let tool = app.active_tool_def();
    let Some(panel) = tool.panel else { return };
    let width = ui.ctx().content_rect().width();
    let narrow = width < NARROW_WINDOW;
    let collapsed =
        narrow && crate::panels::browser_shown(app) && app.narrow_focus == NarrowFocus::Browser;
    if collapsed {
        egui::Panel::left("tool dock strip")
            .resizable(false)
            .exact_size(STRIP_WIDTH)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(4))
            .show(ui, |ui| {
                let name = crate::i18n::tr(tool.name, &[]);
                let label = t!("dock.collapsed_tool", tool = name);
                if widgets::icon_button(ui, tool.icon, &label, None, 28.0)
                    .on_hover_text(&label)
                    .clicked()
                {
                    app.narrow_focus = NarrowFocus::Tool;
                }
            });
        return;
    }
    egui::Panel::left("tool dock")
        .resizable(true)
        .default_size(if narrow {
            DOCK_WIDTH_NARROW
        } else {
            DOCK_WIDTH
        })
        .size_range(240.0..=420.0)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(theme::SPACE_M as i8)),
        )
        .show(ui, |ui| {
            dock_header(app, ui, &tool);
            egui::ScrollArea::vertical()
                .id_salt(("tool dock", tool.id))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    notice(app, ui);
                    panel(app, ui);
                });
        });
}

/// A panel's header: icon, name and one-line help, and a close button.
fn dock_header(app: &mut MeshFixerApp, ui: &mut egui::Ui, tool: &crate::shell::Tool) {
    let name = crate::i18n::tr(tool.name, &[]);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(tool.icon)
                .font(icons::font(20.0))
                .color(theme::ACCENT_HOVER),
        );
        let r = ui.add(
            egui::Label::new(
                RichText::new(&name)
                    .size(theme::TYPE_TITLE)
                    .color(theme::TEXT_STRONG),
            )
            .truncate(),
        );
        // Its accessible name says it's the panel, so it isn't taken for
        // the rail's button of the same name.
        let heading = format!("{name} panel");
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &heading));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = t!("panel.close", tool = name);
            if widgets::icon_button(ui, icons::CLOSE, &label, None, 24.0)
                .on_hover_text(&label)
                .clicked()
            {
                app.close_rail_tool();
            }
        });
    });
    widgets::caption(ui, crate::i18n::tr(tool.help, &[]));
    ui.add_space(theme::SPACE_S);
    ui.separator();
}

/// The last command's result when it carried a warning, in full (I1).
fn notice(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(text) = app.notice.clone() else {
        return;
    };
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::TEXT_MUTED),
            );
            ui.label(RichText::new("Last result").color(theme::TEXT_STRONG));
        });
        ui.add(egui::Label::new(&text).wrap());
        let r = widgets::button(ui, Kind::Tertiary, None, "Dismiss", 0.0);
        let r = labelled(r, "Dismiss note");
        if r.clicked() {
            app.notice = None;
        }
    });
    ui.add_space(theme::SPACE_S);
}

/// Give a widget an accessible name of its own (and the name tests find
/// it by), keeping its visible text short.
fn labelled(r: egui::Response, label: &str) -> egui::Response {
    let enabled = r.enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    r
}

/// A section label with the plan's section gap above it.
fn section(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_space(theme::SPACE_M);
    let r = ui.label(
        RichText::new(text)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    ui.add_space(2.0);
    r
}

/// A section heading with an icon in a meaning colour and a count badge.
fn counted_section(
    ui: &mut egui::Ui,
    icon: &str,
    color: egui::Color32,
    name: &str,
    n: usize,
) -> egui::Rect {
    ui.add_space(theme::SPACE_L);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
    let p = ui.painter();
    let mid = rect.center().y;
    p.text(
        egui::pos2(rect.left() + 8.0, mid),
        egui::Align2::CENTER_CENTER,
        icon,
        icons::font(theme::ICON_SIZE),
        if n > 0 { color } else { theme::TEXT_FAINT },
    );
    p.text(
        egui::pos2(rect.left() + 22.0, mid),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(theme::TYPE_SECTION),
        theme::TEXT_STRONG,
    );
    widgets::paint_badge(
        ui,
        egui::pos2(rect.right(), mid),
        &thousands(n as u64),
        theme::TEXT_MUTED,
    );
    ui.add_space(2.0);
    rect
}

// ---------------------------------------------------------------------
// Select: the inspector card (no tool panel).
// ---------------------------------------------------------------------

/// Select's summary while no tool panel is open: a small card at the
/// view's top-left corner (outside the protected centre), built from the
/// registered inspector sections, or the selection when there is one.
pub fn inspector_card(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if app.active_tool != SELECT || app.side_panels_hidden {
        return;
    }
    let Some(view) = app.viewport.last_rect else {
        return;
    };
    let width = 264.0f32.min(view.width() * 0.3).max(220.0);
    egui::Area::new(egui::Id::unique("inspector card"))
        .order(egui::Order::Middle)
        .fixed_pos(view.left_top() + egui::vec2(theme::SPACE_M, theme::SPACE_M))
        .constrain_to(view)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::CARD.gamma_multiply(0.96))
                .stroke(egui::Stroke::new(1.0, theme::SEPARATOR))
                .corner_radius(theme::RADIUS)
                .inner_margin(theme::SPACE_M)
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    notice(app, ui);
                    if app.active_mesh().is_none() {
                        empty_card(app, ui);
                        return;
                    }
                    if app.current_selection().is_some() || app.current_inspected().is_some() {
                        selection_card(app, ui, false);
                        return;
                    }
                    let sections: Vec<InspectorSection> = app.shell.inspector_sections().to_vec();
                    for s in sections {
                        (s.ui)(app, ui);
                    }
                });
        });
}

fn empty_card(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::title(ui, t!("empty.title"));
    widgets::caption(ui, crate::panels::empty_state());
    ui.add_space(theme::SPACE_S);
    let idle = app.busy.is_none();
    if ui
        .add_enabled_ui(idle, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::OPEN), &t!("empty.open"))
        })
        .inner
        .clicked()
    {
        app.open_dialog();
    }
}

/// The object's name, editable in place (the selection panel's name
/// field, Task "Project organisation" step 1): shows as a title until
/// clicked, then a text field that commits on Enter or losing focus
/// (`object.rename`) and cancels on Esc.
fn name_field(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: ObjectId, name: &str) {
    let editing = app.renaming.as_ref().filter(|(o, _)| *o == id).is_some();
    if !editing {
        let r = ui.add(
            egui::Label::new(
                RichText::new(name)
                    .size(theme::TYPE_TITLE)
                    .color(theme::TEXT_STRONG),
            )
            .truncate()
            .sense(egui::Sense::click()),
        );
        if r.clicked() {
            app.renaming = Some((id, name.to_owned()));
        }
        r.on_hover_text("Click to rename");
        return;
    }
    let mut text = app
        .renaming
        .as_ref()
        .map_or_else(|| name.to_owned(), |(_, t)| t.clone());
    let text_id = egui::Id::unique(("rename field", id));
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(text_id)
            .font(egui::FontId::proportional(theme::TYPE_TITLE))
            .desired_width(ui.available_width()),
    );
    if r.changed() {
        app.renaming = Some((id, text.clone()));
    }
    let committed = r.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape));
    let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if !r.has_focus() {
        ui.memory_mut(|m| m.request_focus(text_id));
    }
    if committed {
        app.renaming = None;
        app.rename_object(id, text);
    } else if cancelled {
        app.renaming = None;
    }
}

/// The object's name, size and triangle count.
fn inspector_summary(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(id) = app.active else { return };
    let Some(name) = app.session.document().object(id).map(|o| o.name.clone()) else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(icons::OBJECT)
                .font(icons::font(theme::ICON_SIZE))
                .color(theme::TEXT_MUTED),
        );
        name_field(app, ui, id, &name);
    });
    role_segmented(app, ui);
    let units = app.session.document().units();
    let Some(a) = app.current_analysis() else {
        widgets::caption(
            ui,
            if app.analysis_failed_for_active() {
                "Couldn't analyse this object"
            } else {
                "Analysing…"
            },
        );
        return;
    };
    if let Some(size) = a.defects.world_size {
        widgets::caption(
            ui,
            format!(
                "{} × {} × {} {}",
                measure(size[0]),
                measure(size[1]),
                measure(size[2]),
                unit_symbol(units)
            ),
        );
    }
    widgets::caption(ui, count(a.diagnostics.triangle_count, "triangle"));
}

/// The Solid/Hole segmented control for the objects selected together (H,
/// the Browser's menu, and this do the same thing, as one undo step): hard
/// to discover from the shortcut alone, so the summary offers it directly.
/// Its current segment is Hole only if every selected object is one (the
/// same rule [`MeshFixerApp::toggle_role`] uses to decide which way to
/// flip); clicking the other segment is what flips it.
fn role_segmented(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let ids = app.selected_objects();
    if ids.is_empty() {
        return;
    }
    let all_hole = ids.iter().all(|&id| app.is_hole(id));
    const SOLID: usize = 0;
    const HOLE: usize = 1;
    let options = [
        (icons::OBJECT, t!("combine.role.solid"), true),
        (icons::HOLE_ROLE, t!("combine.role.hole"), true),
    ];
    let labelled: Vec<(&str, &str, bool)> = options
        .iter()
        .map(|(icon, label, enabled)| (*icon, label.as_str(), *enabled))
        .collect();
    let current = if all_hole { HOLE } else { SOLID };
    ui.add_space(theme::SPACE_XS);
    ui.scope(|ui| {
        ui.set_max_width(160.0);
        let (clicked, responses) = widgets::segmented(ui, &labelled, current);
        for (r, (_, label, _)) in responses.into_iter().zip(&options) {
            // allow-swallow: the tooltip's response carries nothing more
            let _ = r.on_hover_text(format!("{label} (H)"));
        }
        if clicked.is_some_and(|i| i != current) {
            app.toggle_role(&ids);
        }
    });
}

/// The counts the summary and Repair show: (holes, cracks, marked, loose,
/// tangled), from the hole view when it's ready.
fn defect_counts(app: &MeshFixerApp) -> Option<(usize, usize, usize, usize, usize)> {
    let a = app.current_analysis()?;
    let (holes, cracks, marked) =
        app.hole_view()
            .map_or((a.defects.holes.len(), a.defects.cracks.len(), 0), |v| {
                (
                    v.listed.len(),
                    v.cracks.len(),
                    v.marked.len() + v.marked_cracks.len(),
                )
            });
    Some((
        holes,
        cracks,
        marked,
        a.defects.loose.len(),
        a.defects.tangled.len(),
    ))
}

/// Whether it will print, as a chip, and the single most useful next
/// action (the card's one primary button).
fn inspector_status(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(a) = app.current_analysis() else {
        return;
    };
    let Some((holes, cracks, marked, loose, tangled)) = defect_counts(app) else {
        return;
    };
    let d = a.diagnostics.clone();
    let dead = d.dead_slot_ratio;
    let verdict = print_verdict(&d, holes + cracks, marked, loose, tangled);
    let problems = holes + cracks + loose + tangled;
    ui.add_space(theme::SPACE_S);
    let (icon, color) = verdict_icon(holes + cracks, loose, tangled, &d);
    widgets::chip(ui, icon, color, &verdict);
    ui.add_space(theme::SPACE_S);
    if problems > 0 {
        let mut parts = Vec::new();
        if holes > 0 {
            parts.push(tp!("repair.holes.summary", holes));
        }
        if loose > 0 {
            parts.push(tp!("repair.loose.summary", loose));
        }
        if !parts.is_empty() {
            widgets::caption(ui, parts.join(" · "));
        }
        if widgets::wide_button(
            ui,
            Kind::Primary,
            Some(icons::REPAIR),
            &t!("inspector.next.repair"),
        )
        .on_hover_text("Fill holes, remove loose pieces (H)")
        .clicked()
        {
            app.open_rail_tool(REPAIR);
        }
    } else {
        widgets::caption(ui, t!("inspector.ready"));
        if widgets::wide_button(
            ui,
            Kind::Secondary,
            Some(icons::TRANSFORM),
            &t!("inspector.next.transform"),
        )
        .on_hover_text("Move, rotate or scale it (T)")
        .clicked()
        {
            app.open_transform();
        }
    }
    if dead > SUGGEST_COMPACT_ABOVE {
        widgets::caption(ui, COMPACT_SUGGESTION);
    }
}

/// The verdict chip's icon and its colour: a check in the text colour
/// when it prints, else the meaning colour of what's wrong.
fn verdict_icon(
    open: usize,
    loose: usize,
    tangled: usize,
    d: &mf_geometry::Diagnostics,
) -> (&'static str, egui::Color32) {
    if open > 0 || !d.is_closed {
        (icons::HOLE, theme::HOLE)
    } else if tangled > 0 || !d.is_manifold {
        (icons::TANGLED, theme::NON_MANIFOLD)
    } else if loose > 0 {
        (icons::LOOSE_PIECE, theme::LOOSE_PIECE)
    } else {
        (icons::OK, theme::TEXT)
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

/// Above this fraction of unused (tombstoned) slots, the summary and
/// Repair suggest `mesh.compact` (MF-ISSUE-020).
const SUGGEST_COMPACT_ABOVE: f64 = 0.5;
const COMPACT_SUGGESTION: &str =
    "This model has lots of unused space from earlier edits. Compact to reclaim it.";

// ---------------------------------------------------------------------
// The selection card (Select's inspector card and Repair).
// ---------------------------------------------------------------------

fn selection_title(sel: &Selection, n: usize) -> String {
    match sel.kind {
        SelectionKind::BoundaryLoop => format!("{} selected", count(n, "hole")),
        SelectionKind::Component => format!("{} selected", count(n, "piece")),
        SelectionKind::Faces => format!("{} selected", count(n, "triangle")),
        SelectionKind::Object => "Whole object".to_owned(),
    }
}

/// What the target is, in plain language, and (with `tools`, in Repair)
/// the operation for it with its settings and Apply, Mark intentional,
/// and Clear selection. In Select's card, a defect gets an "Open Repair"
/// button instead of its tools.
fn selection_card(app: &mut MeshFixerApp, ui: &mut egui::Ui, tools: bool) {
    let units = app.session.document().units();
    if let Some(inspected) = app.current_inspected() {
        let copy = defect_copy(&inspected.defect, units);
        widgets::title(ui, &copy.title);
        widgets::body(ui, &copy.detail);
        verdict(ui, &copy);
        ui.add_space(theme::SPACE_S);
        if widgets::wide_button(ui, Kind::Primary, None, &copy.next_action)
            .on_hover_text("Selects the triangles at this edge, so F frames them")
            .clicked()
        {
            app.select_tangled_faces();
        }
        clear_selection_link(app, ui);
        return;
    }
    let Some(sel) = app.current_selection().cloned() else {
        return;
    };
    let analysis = app.current_analysis();
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
            widgets::title(ui, &copy.title);
            widgets::body(ui, &copy.detail);
            verdict(ui, &copy);
        }
        (Some(defects), n) => {
            widgets::title(ui, selection_title(&sel, n));
            let blocked: Vec<_> = defects
                .iter()
                .filter(|d| {
                    !matches!(d, Defect::LoosePiece { .. })
                        && !analysis.is_some_and(|a| a.defects.fillable(d))
                })
                .collect();
            if let (Some(d), Some(a)) = (blocked.first(), analysis) {
                let copy = a.defects.copy(d, units);
                widgets::caption(
                    ui,
                    format!(
                        "Includes a {}: {}",
                        copy.title.to_lowercase(),
                        copy.next_action
                    ),
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
                widgets::title(ui, "Main body");
                if let Some(a) = analysis {
                    widgets::caption(ui, count(a.defects.main_body_triangles, "triangle"));
                }
            } else {
                widgets::title(ui, selection_title(&sel, n));
            }
            widgets::caption(
                ui,
                if tangled {
                    "These triangles meet at a tangled edge. Press F to frame them and look \
                     closer. MeshFixer can't untangle edges yet: repairing them is planned. \
                     The tools act on the whole object."
                } else {
                    "No operation works on this selection yet; the tools act on the whole \
                     object."
                },
            );
        }
    }
    let targeted: Vec<Tool> = app
        .tools()
        .into_iter()
        .filter(|t| t.selection.is_some())
        .collect();
    if tools {
        for tool in &targeted {
            section(ui, &tool.label);
            tool_body(app, ui, tool);
        }
    } else if let Some(tool) = targeted.first() {
        // In Select's card: the target's operation, as the one primary
        // button; it opens Repair with the target, where its settings and
        // Apply are.
        ui.add_space(theme::SPACE_S);
        if widgets::wide_button(ui, Kind::Primary, Some(icons::REPAIR), &tool.label)
            .on_hover_text("Opens Repair with this target: choose how, then Apply")
            .clicked()
        {
            app.open_rail_tool(REPAIR);
        }
    }
    let holes: Vec<u32> = match &found {
        Some(defects) if sel.kind == SelectionKind::BoundaryLoop => defects
            .iter()
            .filter_map(|d| d.selection().map(|(_, id)| id))
            .collect(),
        _ => Vec::new(),
    };
    if !holes.is_empty() {
        let marked = holes.iter().all(|id| app.active_openings().contains(id));
        ui.add_space(theme::SPACE_S);
        // Primary when nothing can fill the target (C1): it's the way on.
        let kind = if targeted.is_empty() && !marked {
            Kind::Primary
        } else {
            Kind::Secondary
        };
        let text = if marked {
            "Unmark".to_owned()
        } else if holes.len() == 1 {
            crate::defects::MARK_INTENTIONAL.to_owned()
        } else {
            format!("Mark {} as intentional openings", holes.len())
        };
        let r = ui
            .add_enabled_ui(app.busy.is_none(), |ui| {
                widgets::wide_button(ui, kind, None, &text)
            })
            .inner
            .on_hover_text(
                "It's meant to be open (a scan's base, a mouth, a socket): leave it out of \
                 defect counts and fills. Undo, or Unmark, to list it again.",
            );
        if r.clicked() {
            app.mark_openings(&holes, !marked);
        }
    }
    clear_selection_link(app, ui);
}

fn clear_selection_link(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if widgets::button(ui, Kind::Tertiary, None, "Clear selection", 0.0)
            .on_hover_text("Esc")
            .clicked()
        {
            app.clear_selection();
        }
        ui.label(
            RichText::new("Shift-click adds")
                .size(theme::TYPE_CAPTION)
                .color(theme::TEXT_FAINT),
        );
    });
}

/// What it means for printing, and why MeshFixer can't repair it yet (if
/// it can't), under a target's title.
fn verdict(ui: &mut egui::Ui, copy: &crate::defects::DefectCopy) {
    widgets::body(ui, &copy.verdict);
    if let Some(limit) = &copy.limit {
        widgets::caption(ui, limit);
    }
}

// ---------------------------------------------------------------------
// Transform.
// ---------------------------------------------------------------------

/// Transform's panel: a segmented mode control, Position / Rotation /
/// Scale fields with axis-coloured labels (Enter commits one move), the
/// options, Drop to plate and Reset (secondary), and Done (primary).
fn transform_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    use crate::transform_tool::{Field, MIRRORED_NOTE, SHEARED_NOTE};
    use mf_render::GizmoMode;
    let Some(p) = app.active_placement() else {
        widgets::caption(ui, "Measuring the object…");
        return;
    };
    let sheared = p.rotation_deg.is_none();
    let idle = app.busy.is_none();
    let mode = app.transform_tool().map_or(GizmoMode::Move, |t| t.mode);
    section(ui, &t!("transform.section.mode"));
    let modes = [
        (GizmoMode::Move, t!("transform.mode.move"), "W"),
        (GizmoMode::Rotate, t!("transform.mode.rotate"), "E"),
        (GizmoMode::Scale, t!("transform.mode.scale"), "R"),
    ];
    let options: Vec<(&str, &str, bool)> = modes
        .iter()
        .map(|(m, text, _)| {
            (
                text.as_str(),
                text.as_str(),
                *m == GizmoMode::Move || !sheared,
            )
        })
        .collect();
    let current = modes
        .iter()
        .position(|(m, ..)| *m == mode)
        .expect("every gizmo mode is listed");
    let (clicked, responses) = widgets::segmented(ui, &options, current);
    for (r, (_, text, key)) in responses.into_iter().zip(&modes) {
        // allow-swallow: the tooltips' responses carry nothing more
        let _ = r
            .on_hover_text(format!(
                "The gizmo {}s the object ({key})",
                text.to_lowercase()
            ))
            .on_disabled_hover_text(SHEARED_NOTE);
    }
    if let Some(i) = clicked {
        app.set_transform_mode(modes[i].0);
    }
    let units = unit_symbol(app.session.document().units());
    type Row = (String, Option<glam::DVec3>, fn(usize) -> Field, f64);
    let rows: [Row; 3] = [
        (
            t!("transform.section.position", unit = units),
            Some(p.position),
            Field::Position,
            1.0,
        ),
        (
            t!("transform.section.rotation"),
            p.rotation_deg,
            Field::Rotation,
            1.0,
        ),
        (t!("transform.section.scale"), p.scale, Field::Scale, 100.0),
    ];
    for (name, value, field, factor) in rows {
        section(ui, &name);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let gap = 6.0 * 2.0 + 3.0 * 14.0;
            let w = ((ui.available_width() - gap) / 3.0).max(36.0);
            for axis in 0..3 {
                let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][axis];
                ui.label(
                    RichText::new(crate::transform_tool::AXIS_NAMES[axis])
                        .size(theme::TYPE_CAPTION)
                        .color(color),
                );
                let f = field(axis);
                let v = value.map(|v| v[axis] * factor);
                transform_field(app, ui, f, v, idle && value.is_some(), w);
            }
        });
    }
    if sheared {
        widgets::caption(ui, SHEARED_NOTE).on_hover_text(
            "The file's placement slants the object, so it has no single rotation and scale: \
             those fields stay read-only. Move still works.",
        );
    }
    if p.mirrored {
        widgets::caption(ui, MIRRORED_NOTE)
            .on_hover_text("The placement flips the object, as in a mirror image");
    }
    section(ui, &t!("transform.section.options"));
    let pivot = Some(p.position);
    if let Some(t) = app.transform_tool_mut() {
        ui.add_enabled(
            !sheared,
            egui::Checkbox::new(&mut t.uniform, t!("transform.uniform")),
        )
        .on_hover_text(t!("transform.uniform.tip"));
    }
    crate::shape_tools::snap_checkbox(app, ui, pivot);
    ui.add_space(theme::SPACE_M);
    ui.horizontal(|ui| {
        let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        if ui
            .add_enabled_ui(idle, |ui| {
                widgets::button(
                    ui,
                    Kind::Secondary,
                    Some(icons::DROP_TO_PLATE),
                    &t!("transform.drop"),
                    half,
                )
            })
            .inner
            .on_hover_text(t!("transform.drop.tip"))
            .clicked()
        {
            app.drop_to_plate();
        }
        if ui
            .add_enabled_ui(idle && !sheared, |ui| {
                widgets::button(
                    ui,
                    Kind::Secondary,
                    Some(icons::RESET),
                    &t!("transform.reset"),
                    half,
                )
            })
            .inner
            .on_hover_text(t!("transform.reset.tip"))
            .on_disabled_hover_text(SHEARED_NOTE)
            .clicked()
        {
            app.reset_transform();
        }
    });
    crate::shape_panel::transform_section(app, ui);
    ui.add_space(theme::SPACE_L);
    if widgets::wide_button(ui, Kind::Primary, Some(icons::CHECK), &t!("transform.done"))
        .on_hover_text("Close the Transform tool (Esc)")
        .clicked()
    {
        app.close_rail_tool();
    }
}

/// One numeric field: shows `value` (`None`: read-only, blank), edits as
/// text, and commits with Enter (leaving it without Enter puts the value
/// back).
fn transform_field(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    field: crate::transform_tool::Field,
    value: Option<f64>,
    enabled: bool,
    width: f32,
) {
    let id = egui::Id::unique(("transform field", field));
    let editing = app
        .transform_tool()
        .and_then(|t| t.editing.clone())
        .filter(|(f, _)| *f == field)
        .map(|(_, text)| text);
    let mut text = editing
        .unwrap_or_else(|| value.map_or_else(|| "—".to_owned(), crate::transform_tool::field_text));
    let label = field.label();
    let r = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .desired_width(width)
            .margin(egui::vec2(6.0, 5.0)),
    );
    r.widget_info(|| {
        let mut info = egui::WidgetInfo::text_edit(enabled, "", &text, "");
        info.label = Some(label.clone());
        info
    });
    let r = r.on_disabled_hover_text(crate::transform_tool::SHEARED_NOTE);
    let entered = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let Some(tool) = app.transform_tool_mut() else {
        return;
    };
    if r.has_focus() {
        tool.editing = Some((field, text.clone()));
    } else if tool.editing.as_ref().is_some_and(|(f, _)| *f == field) {
        tool.editing = None;
    }
    if entered {
        let shown = value.map(crate::transform_tool::field_text);
        match text
            .trim()
            .trim_end_matches(['%', '°'])
            .trim()
            .parse::<f64>()
        {
            // Enter on the value as shown changes nothing (the field shows
            // 4 decimals: compare what it shows, not the bits behind it).
            Ok(v) if shown.as_deref() == Some(crate::transform_tool::field_text(v).as_str()) => {}
            Ok(v) if v.is_finite() => app.set_transform_field(field, v),
            _ => {
                app.status = Some(format!(
                    "{label}: \"{text}\" isn't a number; nothing changed"
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------
// Repair.
// ---------------------------------------------------------------------

/// Repair: one tool with sections (Holes, Narrow cracks, Loose pieces,
/// Tangled edges, Housekeeping), one level deep. Choosing an operation
/// (Fill all holes, Remove loose pieces, Compact) replaces the list with
/// its settings and Apply, and "All repairs" (or Esc) returns. A defect
/// selected in the Browser or the view shows the same way, with its
/// operation.
fn repair_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let defect_target = app.current_inspected().is_some()
        || app.current_selection().is_some_and(|s| {
            app.current_analysis()
                .is_some_and(|a| s.ids.iter().all(|&id| a.defects.find(s.kind, id).is_some()))
        });
    if defect_target {
        ui.horizontal(|ui| {
            back_link(app, ui);
            if app
                .current_selection()
                .is_some_and(|s| s.kind == SelectionKind::BoundaryLoop)
            {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    hole_steps_compact(app, ui);
                });
            }
        });
        selection_card(app, ui, true);
        return;
    }
    if let Some(op) = app.tool.clone() {
        back_link(app, ui);
        ui.add_space(theme::SPACE_XS);
        if let Some(tool) = app
            .tools()
            .into_iter()
            .find(|t| t.op_id == op && t.selection.is_none())
        {
            widgets::title(ui, &tool.label);
            tool_body(app, ui, &tool);
        }
        return;
    }
    if app.current_selection().is_some() {
        widgets::card(ui, |ui| selection_card(app, ui, true));
    }
    // Before the analysis is in, the sections still offer their
    // operations (a whole-object run fills what fill_holes finds).
    let analysing = if app.analysis_failed_for_active() {
        "Couldn't analyse this object"
    } else {
        "Analysing…"
    };
    let counts = defect_counts(app);
    if counts.is_none() {
        widgets::caption(ui, analysing);
    }
    let (holes, cracks, marked, loose, tangled) = counts.unwrap_or((0, 0, 0, 0, 0));
    // Where it stands, at the top.
    if let (Some(_), Some(a)) = (counts, app.current_analysis()) {
        let d = a.diagnostics.clone();
        let (icon, color) = verdict_icon(holes + cracks, loose, tangled, &d);
        widgets::chip(
            ui,
            icon,
            color,
            &print_verdict(&d, holes + cracks, marked, loose, tangled),
        );
    }
    let idle = app.busy.is_none();
    // The panel's one primary action: the first section with work to do.
    let primary_holes = holes > 0;
    let primary_loose = !primary_holes && loose > 0;

    // Each section: its header (icon, name, count; Holes adds Previous /
    // Next), one line saying what it is, its action, then its list.
    let header = counted_section(
        ui,
        icons::HOLE,
        theme::HOLE,
        &t!("repair.section.holes"),
        holes,
    );
    if holes > 0 {
        let steps = egui::Rect::from_min_max(
            egui::pos2(header.right() - 36.0 - 52.0, header.top()),
            egui::pos2(header.right() - 36.0, header.bottom()),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(steps)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                hole_steps_compact(app, ui);
            },
        );
    }
    if counts.is_some() {
        let mut line = if holes == 0 {
            t!("repair.holes.none")
        } else {
            tp!("repair.holes.summary", holes)
        };
        if marked > 0 {
            line = format!("{line} · {}", tp!("repair.openings.summary", marked));
        }
        widgets::caption(ui, line);
    }
    ui.add_space(theme::SPACE_XS);
    let kind = if primary_holes {
        Kind::Primary
    } else {
        Kind::Secondary
    };
    if ui
        .add_enabled_ui(idle && (holes > 0 || counts.is_none()), |ui| {
            widgets::wide_button(ui, kind, Some(icons::HOLE), &t!("cmd.fill_holes"))
        })
        .inner
        .on_hover_text("Choose how, then Apply: one step, one Undo")
        .clicked()
    {
        app.tool = Some("mesh.fill_holes".into());
    }
    if holes > 0 {
        hole_groups(app, ui);
    }

    counted_section(
        ui,
        icons::CRACK,
        theme::CRACK,
        &t!("repair.section.cracks"),
        cracks,
    );
    widgets::caption(
        ui,
        if cracks == 0 {
            t!("repair.cracks.none")
        } else {
            t!("repair.cracks.why")
        },
    );

    counted_section(
        ui,
        icons::LOOSE_PIECE,
        theme::LOOSE_PIECE,
        &t!("repair.section.loose"),
        loose,
    );
    if loose == 0 {
        widgets::caption(ui, t!("repair.loose.none"));
    } else {
        widgets::caption(ui, tp!("repair.loose.summary", loose));
    }
    ui.add_space(theme::SPACE_XS);
    let kind = if primary_loose {
        Kind::Primary
    } else {
        Kind::Secondary
    };
    if ui
        .add_enabled_ui(idle, |ui| {
            widgets::wide_button(ui, kind, Some(icons::REMOVE), &t!("cmd.remove_pieces"))
        })
        .inner
        .on_hover_text("Choose which, then Apply: one step, one Undo")
        .clicked()
    {
        app.tool = Some("mesh.remove_small_components".into());
    }

    counted_section(
        ui,
        icons::TANGLED,
        theme::NON_MANIFOLD,
        &t!("repair.section.tangled"),
        tangled,
    );
    widgets::caption(
        ui,
        if tangled == 0 {
            t!("repair.tangled.none")
        } else {
            t!("repair.tangled.why")
        },
    );

    section(ui, &t!("repair.section.housekeeping"));
    let dead = app
        .current_analysis()
        .is_some_and(|a| a.diagnostics.dead_slot_ratio > SUGGEST_COMPACT_ABOVE);
    if dead {
        widgets::caption(ui, COMPACT_SUGGESTION);
    } else {
        widgets::caption(ui, t!("repair.housekeeping.why"));
    }
    if ui
        .add_enabled_ui(idle, |ui| {
            widgets::button(
                ui,
                Kind::Tertiary,
                Some(icons::COMPACT),
                &t!("cmd.compact"),
                0.0,
            )
        })
        .inner
        .clicked()
    {
        app.tool = Some("mesh.compact".into());
    }
    ui.add_space(theme::SPACE_L);
    if widgets::wide_button(ui, Kind::Secondary, None, &t!("repair.done"))
        .on_hover_text("Close Repair (Esc)")
        .clicked()
    {
        app.close_rail_tool();
    }
}

/// Previous / Next hole (Shift+N / N) as two icon buttons, laid out right
/// to left: on the Holes header, and beside the back link while a hole is
/// shown.
fn hole_steps_compact(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let next = t!("repair.next_hole");
    if widgets::icon_button(ui, icons::CARET_RIGHT, &next, None, 24.0)
        .on_hover_text(t!("repair.next_hole.tip"))
        .clicked()
    {
        app.next_hole(true);
    }
    let previous = t!("repair.previous_hole");
    if widgets::icon_button(ui, icons::CARET_LEFT, &previous, None, 24.0)
        .on_hover_text(t!("repair.previous_hole.tip"))
        .clicked()
    {
        app.next_hole(false);
    }
}

/// "‹ All repairs": one level back (as Esc).
fn back_link(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if widgets::button(
        ui,
        Kind::Tertiary,
        Some(icons::CARET_LEFT),
        &t!("repair.back"),
        0.0,
    )
    .on_hover_text(t!("repair.back.tip"))
    .clicked()
    {
        let back = app.active_tool_def().back;
        back(app);
    }
}

/// Each size group of holes with its count and actions (largest first).
fn hole_groups(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(view) = app.hole_view().cloned() else {
        widgets::caption(ui, "Listing holes…");
        return;
    };
    let Some(defects) = app.current_analysis().map(|a| a.defects.clone()) else {
        return;
    };
    for group in HoleGroup::ALL {
        let range = view.group(group);
        if range.is_empty() {
            continue;
        }
        let n = range.len();
        let plural = if n == 1 { "hole" } else { "holes" };
        ui.add_space(theme::SPACE_XS);
        ui.add(
            egui::Label::new(RichText::new(format!("{} · {n}", group.title())).color(theme::TEXT))
                .truncate(),
        );
        ui.horizontal_wrapped(|ui| {
            if group == HoleGroup::TooLarge {
                // Over the fill method's limit (MF-ISSUE-043): say so, and
                // offer what works today.
                widgets::caption(
                    ui,
                    format!(
                        "Over this fill method's {}-edge limit. Often a scan's open base: \
                         mark it if it's meant to be open.",
                        thousands(crate::defects::FILL_CAP_EDGES as u64)
                    ),
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
                    .add_enabled_ui(app.busy.is_none() && k > 0, |ui| {
                        widgets::button(ui, Kind::Tertiary, None, &fill, 0.0)
                    })
                    .inner
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
            let r = ui
                .add_enabled_ui(app.busy.is_none(), |ui| {
                    widgets::button(ui, Kind::Tertiary, None, "Mark intentional", 0.0)
                })
                .inner
                .on_hover_text("Leave them open: they're left out of counts and fills");
            if labelled(r, &mark).clicked() {
                let ids: Vec<u32> = view.listed[range.clone()]
                    .iter()
                    .filter_map(|&i| defects.holes[i].selection().map(|(_, id)| id))
                    .collect();
                app.mark_openings(&ids, true);
            }
        });
    }
}

/// Why Refine is on and greyed out for a smooth or curvature fill.
const REFINE_FORCED: &str = "Smooth and Curvature always add points so the patch can curve";

/// A one-shot operation's settings: its plain summary, its parameters, a
/// preview line, and Apply (primary) with Defaults (tertiary).
fn tool_body(app: &mut MeshFixerApp, ui: &mut egui::Ui, tool: &Tool) {
    let Some(spec) = mf_spec::builtin::get(&tool.op_id) else {
        ui.label(format!("Unknown operation {}", tool.op_id));
        return;
    };
    if let Some(plain) = tool_summary(&tool.op_id) {
        widgets::body(ui, plain);
    }
    widgets::caption(ui, &spec.summary);
    let batch = tool.op_id == "mesh.fill_holes" && tool.selection.is_none();
    let values = app.params_for(&tool.op_id);
    // A fill that isn't flat always refines (MF-OP-002): it needs the
    // added points to curve.
    let refine_forced = tool.op_id == "mesh.fill_holes"
        && matches!(values.get("fill_style"), Some(ParamValue::Text(s)) if s != "flat");
    // The batch fill's size limit: a slider on its own line, as wide as
    // the panel allows.
    let limit = spec.parameters.iter().find(|p| p.name == "max_loop_edges");
    if batch && let (Some(p), Some(ParamValue::Int(v))) = (limit, values.get_mut("max_loop_edges"))
    {
        let (min, max) = match p.kind {
            ParamKind::Integer { min, max, .. } => (min.unwrap_or(3), max.unwrap_or(1000)),
            _ => (3, 1000),
        };
        section(ui, &param_label(&tool.op_id, &p.name)).on_hover_text(&p.description);
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
    let plain: Vec<&mf_spec::ParamSpec> = spec
        .parameters
        .iter()
        .filter(|p| !tool.fixed.contains_key(&p.name))
        .filter(|p| !(batch && p.name == "max_loop_edges"))
        .filter(|p| !matches!(p.kind, ParamKind::Choice { .. }))
        .collect();
    // One row per setting (no grid: a grid's rows settle a frame late,
    // which moved what's under the pointer).
    if !plain.is_empty() {
        ui.add_space(theme::SPACE_S);
    }
    for p in plain {
        let Some(value) = values.get_mut(&p.name) else {
            continue;
        };
        let name = param_label(&tool.op_id, &p.name);
        match (&p.kind, value) {
            (ParamKind::Integer { min, max, .. }, ParamValue::Int(v)) => {
                ui.horizontal(|ui| {
                    ui.label(&name).on_hover_text(&p.description);
                    let range = min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX);
                    ui.add(egui::DragValue::new(v).range(range))
                        .on_hover_text(&p.description);
                });
            }
            (ParamKind::Float { min, max, .. }, ParamValue::Float(v)) => {
                ui.horizontal(|ui| {
                    ui.label(&name).on_hover_text(&p.description);
                    let range = min.unwrap_or(f64::MIN)..=max.unwrap_or(f64::MAX);
                    let suffix = p
                        .unit
                        .symbol()
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
                });
            }
            (ParamKind::Bool { .. }, ParamValue::Bool(_))
                if refine_forced && p.name == "refine" =>
            {
                ui.add_enabled(false, egui::Checkbox::new(&mut true, &name))
                    .on_disabled_hover_text(&p.description);
            }
            (ParamKind::Bool { .. }, ParamValue::Bool(v)) => {
                ui.checkbox(v, &name).on_hover_text(&p.description);
            }
            _ => {
                ui.label(format!("{name}: type mismatch"));
            }
        }
    }
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
        section(ui, &param_label(&tool.op_id, &p.name)).on_hover_text(&p.description);
        for value in values {
            let (label, line) = choice_copy(&tool.op_id, &p.name, value)
                // allow-swallow: a choice without plain words shows its value
                .unwrap_or((value.as_str(), ""));
            ui.radio_value(v, value.clone(), label)
                .on_hover_text(&p.description);
            if !line.is_empty() {
                ui.indent(("choice", &p.name, value), |ui| {
                    widgets::caption(ui, line);
                });
            }
        }
    }
    if refine_forced {
        widgets::caption(ui, REFINE_FORCED);
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
    ui.add_space(theme::SPACE_M);
    let can_apply = app.busy.is_none() && app.active.is_some() && !nothing_to_fill;
    if ui
        .add_enabled_ui(can_apply, |ui| {
            widgets::wide_button(ui, Kind::Primary, None, "Apply")
        })
        .inner
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
    if editable
        && widgets::button(ui, Kind::Tertiary, None, "Defaults", 0.0)
            .on_hover_text("Put every setting back to its default")
            .clicked()
    {
        app.op_params.remove(&tool.op_id);
    }
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

// ---------------------------------------------------------------------
// Inspect.
// ---------------------------------------------------------------------

/// Inspect: whether it prints, then every number `inspect` reports, and
/// the import's warnings.
fn inspect_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(analysis) = app.current_analysis() else {
        widgets::caption(ui, "Analysing…");
        return;
    };
    let d = analysis.diagnostics.clone();
    if let Some((holes, cracks, marked, loose, tangled)) = defect_counts(app) {
        section(ui, &t!("inspect.section.print"));
        let (icon, color) = verdict_icon(holes + cracks, loose, tangled, &d);
        widgets::chip(
            ui,
            icon,
            color,
            &print_verdict(&d, holes + cracks, marked, loose, tangled),
        );
    }
    section(ui, &t!("inspect.section.counts"));
    let flag = |ok: bool| if ok { "yes" } else { "no" }.to_owned();
    let n = |v: usize| thousands(v as u64);
    // Short plain labels; the technical term is the tooltip.
    let rows: [(&str, &str, String); 18] = [
        ("Triangles", "Live triangles", n(d.triangle_count)),
        ("Vertices", "Live vertices", n(d.vertex_count)),
        ("Pieces", "Connected components", n(d.components)),
        ("Closed", "Watertight: no boundary edges", flag(d.is_closed)),
        (
            "Manifold",
            "Every edge has at most two faces",
            flag(d.is_manifold),
        ),
        ("Openings", "Boundary loops", n(d.boundary_loops.len())),
        ("Open edges", "Boundary edges", n(d.boundary_edges)),
        (
            "Untraced open edges",
            "Boundary edges at a pinched point, not in any loop",
            n(d.untraced_boundary_edges),
        ),
        (
            "Tangled edges",
            "Non-manifold edges: more than two faces share each",
            n(d.non_manifold_edges),
        ),
        (
            "Pinched points",
            "Non-manifold vertices",
            n(d.non_manifold_vertices),
        ),
        (
            "Flat triangles",
            "Degenerate (zero-area) triangles",
            n(d.degenerate_triangles),
        ),
        (
            "Duplicate points",
            "Exactly duplicate vertices",
            n(d.duplicate_vertices_exact),
        ),
        (
            "Near-duplicate points",
            "Vertices within tolerance of another",
            n(d.duplicate_vertices_tolerance),
        ),
        (
            "Duplicate faces",
            "Faces with the same three corners",
            n(d.duplicate_faces),
        ),
        (
            "Flipped edges",
            "Edges whose two faces disagree on orientation",
            n(d.inconsistent_orientation_edges),
        ),
        (
            "Surface area",
            "Total area of the faces",
            d.surface_area
                .map_or("too large".into(), |a| format!("{a:.4}")),
        ),
        (
            "Volume",
            "Signed volume (only for a closed mesh)",
            d.signed_volume
                .map_or("— (not closed)".into(), |v| format!("{v:.4}")),
        ),
        (
            "Unused space",
            "Unused (tombstoned) slots from earlier edits",
            format!("{:.0}% of slots", d.dead_slot_ratio * 100.0),
        ),
    ];
    for (label, term, value) in rows {
        stat_row(ui, label, term, &value);
    }
    inspect_units_section(app, ui);
    if !app.warnings.is_empty() {
        section(ui, &t!("inspect.section.warnings"));
        egui::CollapsingHeader::new(format!(
            "Import warnings ({})",
            thousands(app.warnings.len() as u64)
        ))
        .id_salt("import warnings")
        .show(ui, |ui| {
            for w in crate::panels::grouped_warnings(&app.warnings) {
                ui.add(egui::Label::new(RichText::new(w).small()).wrap());
            }
        });
    }
}

/// Inspect's Units control (import-units-brief.md, "Fix it afterwards"):
/// a unit picker and the two document-wide actions, each the recorded
/// command of the same name (consolidation plan C07) and so undoable
/// like any other command.
///
/// - **Reinterpret** (`document.reinterpret_units`): the numbers were
///   in the picked unit, not the document's own; only the document's
///   unit label changes, so the same coordinates now mean a different
///   physical size. Needs at least one object (any one does: the op
///   only reads the document's own unit).
/// - **Scale** (`document.scale_units`): the document keeps its own
///   unit, but every object's transform is scaled by the ratio between
///   the picked unit and the document's, so the physical size becomes
///   what it would have been had the numbers actually been in the
///   picked unit. Targets every object (`unit: document` reads this as
///   `many: true`).
fn inspect_units_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(first) = app.session.document().objects().first().map(|o| o.id) else {
        return;
    };
    let doc_unit = app.session.document().units();
    let picked = app.inspect_units.unwrap_or(doc_unit);
    section(ui, &t!("inspect.section.units"));
    widgets::caption(
        ui,
        t!(
            "inspect.units.help",
            unit = crate::defects::unit_symbol(doc_unit)
        ),
    );
    egui::ComboBox::from_id_salt("inspect units picker")
        .selected_text(crate::import_dialog::unit_name(picked))
        .show_ui(ui, |ui| {
            for unit in Unit::ALL {
                if ui
                    .selectable_label(picked == unit, crate::import_dialog::unit_name(unit))
                    .clicked()
                {
                    app.inspect_units = Some(unit);
                }
            }
        });
    let idle = app.busy.is_none();
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(idle, |ui| {
                widgets::button(
                    ui,
                    Kind::Secondary,
                    None,
                    &t!("inspect.units.reinterpret"),
                    0.0,
                )
            })
            .inner
            .on_hover_text(t!("inspect.units.reinterpret.help"))
            .clicked()
        {
            reinterpret_units(app, first, picked);
        }
        if ui
            .add_enabled_ui(idle, |ui| {
                widgets::button(ui, Kind::Secondary, None, &t!("inspect.units.scale"), 0.0)
            })
            .inner
            .on_hover_text(t!("inspect.units.scale.help"))
            .clicked()
        {
            scale_units(app, first, picked);
        }
    });
}

/// `document.reinterpret_units` and `document.scale_units`'s `unit`
/// param is a plain-ASCII choice (`um|mm|cm|m|in|ft`, matching
/// [`Unit::parse_symbol`]'s accepted set), not [`Unit::symbol`]'s
/// display form (`"µm"` for a micron, which the spec's `values:` list
/// doesn't contain and so would fail validation).
fn unit_choice_text(unit: Unit) -> &'static str {
    match unit {
        Unit::Micron => "um",
        other => other.symbol(),
    }
}

/// Runs `document.reinterpret_units` with `picked` as "the unit the
/// numbers were actually in".
fn reinterpret_units(app: &mut MeshFixerApp, target: ObjectId, picked: Unit) {
    let intent = crate::intent::Intent::op("document.reinterpret_units")
        .on(target)
        .param("unit", unit_choice_text(picked));
    app.run_intent(intent, t!("inspect.units.reinterpret"));
}

/// Runs `document.scale_units` with `picked`, targeting every object in
/// the document (the op's own `many: true` target).
fn scale_units(app: &mut MeshFixerApp, first: ObjectId, picked: Unit) {
    let rest: Vec<ObjectId> = app
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .filter(|&id| id != first)
        .collect();
    let mut intent = crate::intent::Intent::op("document.scale_units")
        .on(first)
        .param("unit", unit_choice_text(picked));
    for id in rest {
        intent = intent.operand(mf_command::TARGET_OPERAND, id, None);
    }
    app.run_intent(intent, t!("inspect.units.scale"));
}

/// One Inspect row: the label left (never wrapped), the value right
/// (tabular digits), the technical term as the label's tooltip.
fn stat_row(ui: &mut egui::Ui, label: &str, term: &str, value: &str) {
    let width = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    let p = ui.painter();
    let mid = rect.center().y;
    let value_galley = p.layout_no_wrap(
        value.to_owned(),
        egui::FontId::monospace(theme::TYPE_BODY - 1.0),
        theme::TEXT,
    );
    let room = (width - value_galley.size().x - theme::SPACE_M).max(40.0);
    let label_galley = egui::WidgetText::from(
        RichText::new(label)
            .size(theme::TYPE_BODY)
            .color(theme::TEXT_MUTED),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        room,
        egui::TextStyle::Body,
    );
    p.galley(
        egui::pos2(rect.left(), mid - label_galley.size().y / 2.0),
        label_galley,
        theme::TEXT_MUTED,
    );
    p.galley(
        egui::pos2(
            rect.right() - value_galley.size().x,
            mid - value_galley.size().y / 2.0,
        ),
        value_galley,
        theme::TEXT,
    );
    let text = format!("{label}: {value}");
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &text));
    // allow-swallow: the tooltip's response carries nothing more
    let _ = r.on_hover_text(term);
}

// ---------------------------------------------------------------------
// Export.
// ---------------------------------------------------------------------

/// Export: write the document to a file (primary), or save / replay the
/// command log (secondary).
fn export_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let idle = app.busy.is_none();
    section(ui, &t!("export.section.file"));
    widgets::caption(ui, t!("export.help"));
    ui.add_space(theme::SPACE_XS);
    if ui
        .add_enabled_ui(idle && app.active_mesh().is_some(), |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::EXPORT), &t!("cmd.export"))
        })
        .inner
        .clicked()
    {
        app.export_dialog();
    }
    section(ui, &t!("export.section.log"));
    widgets::caption(ui, t!("export.log_help"));
    ui.add_space(theme::SPACE_XS);
    if ui
        .add_enabled_ui(idle, |ui| {
            widgets::wide_button(ui, Kind::Secondary, None, &t!("cmd.save_log"))
        })
        .inner
        .clicked()
    {
        app.save_log_dialog();
    }
    if ui
        .add_enabled_ui(idle, |ui| {
            widgets::wide_button(ui, Kind::Secondary, None, &t!("cmd.replay"))
        })
        .inner
        .clicked()
    {
        app.replay_dialog();
    }
}

// ---------------------------------------------------------------------
// The first-use hint.
// ---------------------------------------------------------------------

/// "Pick a tool on the left…", at the view's top centre (outside the
/// protected centre), with a model open and no tool used yet. It goes
/// when a tool opens, or with its ✕.
pub fn first_use_hint(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if !app.first_use_hint
        || app.active_tool != SELECT
        || app.active_mesh().is_none()
        || app.side_panels_hidden
    {
        return;
    }
    let Some(view) = app.viewport.last_rect else {
        return;
    };
    egui::Area::new(egui::Id::unique("first use hint"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::CENTER_TOP)
        .fixed_pos(view.center_top() + egui::vec2(0.0, theme::SPACE_M))
        .constrain_to(view)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::CARD.gamma_multiply(0.95))
                .stroke(egui::Stroke::new(1.0, theme::SEPARATOR))
                .corner_radius(theme::RADIUS)
                .inner_margin(egui::Margin::symmetric(theme::SPACE_M as i8, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icons::HINT)
                                .font(icons::font(theme::ICON_SIZE))
                                .color(theme::ACCENT_HOVER),
                        );
                        ui.label(RichText::new(t!("hint.first_use")).color(theme::TEXT));
                        let label = t!("hint.first_use.dismiss");
                        if widgets::icon_button(ui, icons::CLOSE, &label, None, 22.0)
                            .on_hover_text(&label)
                            .clicked()
                        {
                            app.first_use_hint = false;
                        }
                    });
                });
        });
}
