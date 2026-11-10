//! Colour tokens (docs/design/visual-language.md). Every colour the app
//! shows comes from here, panels and 3D view alike, and each token has one
//! meaning: a colour never means two things.

use eframe::egui::{self, Color32};

/// The current target: selected loops, pieces and faces, the selected tree
/// row, the current history step.
pub const SELECTED: Color32 = Color32::from_rgb(40, 210, 255);
/// What the pointer is over (a light outline).
pub const HOVER: Color32 = Color32::from_rgb(232, 238, 245);
/// Holes: every open boundary's outline (except intentional openings).
pub const HOLE: Color32 = Color32::from_rgb(255, 128, 32);
/// Intentional openings: a boundary the user said is meant to be open (a
/// scan's base, a socket). A neutral slate, drawn over the hole outline so
/// a marked opening never reads as a hole to fix.
pub const OPENING: Color32 = Color32::from_rgb(132, 146, 160);
/// Narrow cracks: boundaries whose edges almost touch.
pub const CRACK: Color32 = Color32::from_rgb(230, 70, 210);
/// Loose pieces: parts not connected to the model.
pub const LOOSE_PIECE: Color32 = Color32::from_rgb(255, 200, 40);
/// Non-manifold ("tangled") edges.
pub const NON_MANIFOLD: Color32 = Color32::from_rgb(235, 52, 52);
/// Faces created by the last few commands (toggle: View ▸ Highlight recent repairs).
pub const RECENT_REPAIR: Color32 = Color32::from_rgb(80, 205, 120);

/// The gizmo's X handle only (D8): the conventional axis colour, familiar
/// from Meshmixer, Fusion and Blender. Never used for anything else, so a
/// colour still has one meaning while the Transform tool is open.
pub const AXIS_X: Color32 = Color32::from_rgb(229, 72, 77);
/// The gizmo's Y handle only (D8).
pub const AXIS_Y: Color32 = Color32::from_rgb(70, 167, 88);
/// The gizmo's Z handle only (D8).
pub const AXIS_Z: Color32 = Color32::from_rgb(62, 139, 240);
/// The gizmo's handles with no single axis (the centre, the view ring,
/// uniform scale) only (D9): a light neutral grey, distinct from `HOVER`.
pub const GIZMO_FREE: Color32 = Color32::from_gray(180);
/// The build-plate grid: a neutral, so it never competes with a defect or
/// selection colour. Light enough to read on `VIEWPORT_BACKGROUND`.
pub const PLATE: Color32 = Color32::from_gray(100);
/// The plate's major lines (every 50 mm): the same neutral, a step
/// brighter, so the scale reads at any zoom.
pub const PLATE_MAJOR: Color32 = Color32::from_gray(140);

/// An inactive object keeps this much of its saturation…
pub const INACTIVE_SATURATION: f32 = 0.0;
/// …and this much of its brightness (desaturated, so the active one stands out).
pub const INACTIVE_BRIGHTNESS: f32 = 0.55;

/// How strongly a face layer replaces the surface colour (0–1).
pub const SELECTED_FACE_STRENGTH: f32 = 0.6;
pub const HOVER_FACE_STRENGTH: f32 = 0.85;
pub const LOOSE_PIECE_STRENGTH: f32 = 0.6;
pub const RECENT_REPAIR_STRENGTH: f32 = 0.45;
/// Faces around a hole (View ▸ Show hole overview): the hole colour, low
/// alpha, so a model with hundreds of holes shows where they are.
pub const HOLE_NEIGHBOURHOOD_STRENGTH: f32 = 0.3;
/// Faces next to a tangled edge: a light red, so the red edge line stays
/// the strongest mark.
pub const NON_MANIFOLD_FACE_STRENGTH: f32 = 0.35;

/// The selection where nearer surfaces hide it (the x-ray pass) is drawn
/// in `SELECTED` at this opacity: the same colour, so the same meaning.
pub const XRAY_OPACITY: f32 = 0.55;

/// Outline widths in the 3D view, in points: defect outlines, and the
/// selection / hover / crack / tangled-edge overlays drawn over them.
pub const BOUNDARY_WIDTH: f32 = 2.0;
pub const OVERLAY_WIDTH: f32 = 3.0;

// Neutrals: no meaning beyond "the model" and "the room it sits in".
/// Front faces of the model.
pub const SURFACE: Color32 = Color32::from_rgb(200, 194, 184);
/// Back faces, seen through holes or flipped (a dark slate, not a defect colour).
pub const BACK_FACE: Color32 = Color32::from_rgb(78, 84, 100);
pub const VIEWPORT_BACKGROUND: Color32 = Color32::from_rgb(38, 41, 46);
/// Primary text drawn over the 3D view (the open tool's line): a neutral,
/// never a geometry colour.
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xEB);
/// Secondary text: details, hints, steps you can redo.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9A, 0xA0, 0xA8);
/// The running job's progress bar (a neutral: progress is not a defect,
/// nor the selection).
pub const PROGRESS: Color32 = Color32::from_gray(120);
/// The error banner's background (a dark rose: failures, never geometry).
pub const ERROR_FILL: Color32 = Color32::from_rgb(110, 32, 58);

/// A token as linear RGBA for the renderer (its target isn't sRGB, so the
/// bytes are written as given).
pub fn rgba(c: Color32) -> [f32; 4] {
    [c.r(), c.g(), c.b(), c.a()].map(|v| f32::from(v) / 255.0)
}

/// The 3D view's palette, from these tokens.
pub fn palette() -> mf_render::Palette {
    mf_render::Palette {
        background: rgba(VIEWPORT_BACKGROUND),
        surface: rgba(SURFACE),
        back_face: rgba(BACK_FACE),
        boundary: rgba(HOLE),
        plate: rgba(PLATE),
        plate_major: rgba(PLATE_MAJOR),
        inactive_saturation: INACTIVE_SATURATION,
        inactive_brightness: INACTIVE_BRIGHTNESS,
        boundary_width: BOUNDARY_WIDTH,
        overlay_width: OVERLAY_WIDTH,
        tints: mf_render::FaceTints {
            loose_piece: tint(LOOSE_PIECE, LOOSE_PIECE_STRENGTH),
            hole_neighbourhood: tint(HOLE, HOLE_NEIGHBOURHOOD_STRENGTH),
            non_manifold_adj: tint(NON_MANIFOLD, NON_MANIFOLD_FACE_STRENGTH),
            recent_repair: tint(RECENT_REPAIR, RECENT_REPAIR_STRENGTH),
            selected: tint(SELECTED, SELECTED_FACE_STRENGTH),
            hover: tint(HOVER, HOVER_FACE_STRENGTH),
        },
        xray_opacity: XRAY_OPACITY,
    }
}

/// A face tint: the token's colour, `strength` in alpha.
fn tint(c: Color32, strength: f32) -> [f32; 4] {
    let [r, g, b, _] = rgba(c);
    [r, g, b, strength]
}

/// The gizmo's own colours, from these tokens (D8/D9). The build plate's
/// colour is part of `palette()` above (`mf_render::Palette::plate`): the
/// renderer has no colours of its own for either.
pub fn gizmo_palette() -> mf_render::GizmoPalette {
    mf_render::GizmoPalette {
        axis_x: rgba(AXIS_X),
        axis_y: rgba(AXIS_Y),
        axis_z: rgba(AXIS_Z),
        neutral: rgba(GIZMO_FREE),
    }
}

// The chrome (ui-plan.md §4): neutral charcoal, deliberately dimmer than
// the model, stepped by elevation. None of these means anything beyond
// "a surface".
/// The rail, the menu bar and the status bar: the frame around everything.
pub const RAIL: Color32 = Color32::from_rgb(0x17, 0x19, 0x1C);
/// The docked panels (the tool panel, the Browser).
pub const PANEL: Color32 = Color32::from_rgb(0x1E, 0x20, 0x24);
/// Separators, and a secondary button's fill.
pub const SEPARATOR: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x32);
/// A raised card: the inspector card, the Overlays popover, the nav bar.
pub const CARD: Color32 = Color32::from_rgb(0x25, 0x28, 0x2D);
/// A secondary button's fill (the plan's subtle fill).
pub const CONTROL: Color32 = SEPARATOR;
/// The same, under the pointer.
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x35, 0x39, 0x40);
/// The same, pressed.
pub const CONTROL_ACTIVE: Color32 = Color32::from_rgb(0x40, 0x45, 0x4D);
/// Text fields and a segmented control's track: inset, darker than the panel.
pub const FIELD: Color32 = Color32::from_rgb(0x14, 0x16, 0x19);
/// Titles: the brightest text.
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xF4, 0xF5, 0xF7);
/// Disabled text.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5C, 0x61, 0x68);

/// The accent: "active and primary" in the chrome and nothing else (the
/// active tool on the rail, the current segment of a segmented control,
/// the current history step, a focused field, the one primary button of
/// a panel). An indigo blue: the plan's #3D8BFD is within a few degrees
/// of `AXIS_Z` (#3E8BF0), and principle 7 (axis colours are never reused
/// for chrome) wins, so the accent moves towards indigo, still a calm
/// blue, clearly apart from the Z axis and from `CRACK`'s magenta. White
/// text on it reads at 4.9:1.
pub const ACCENT: Color32 = Color32::from_rgb(0x5B, 0x5F, 0xEF);
/// The accent under the pointer.
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x6D, 0x71, 0xF5);
/// The accent as a wash behind an active item (the rail's active tool).
pub const ACCENT_SOFT: Color32 = Color32::from_rgba_premultiplied(0x1F, 0x20, 0x51, 0x55);
/// Text and icons on an accent fill.
pub const ON_ACCENT: Color32 = Color32::WHITE;

// Spacing (ui-plan.md §4): a 4 pt grid.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_S: f32 = 8.0;
/// Panel padding.
pub const SPACE_M: f32 = 12.0;
/// The gap between a panel's sections.
pub const SPACE_L: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;
/// The one corner radius: buttons, fields, cards, chips, rail tiles.
pub const RADIUS: f32 = 6.0;
/// A control's height (buttons, fields, segmented controls) and a
/// Browser row's height.
pub const ROW_HEIGHT: f32 = 28.0;
pub const CONTROL_HEIGHT: f32 = ROW_HEIGHT;

// The type scale (ui-plan.md §4), in points. The bundled font has one
// weight, so a section label's "semibold" is the brightest text colour.
/// A panel's title (the tool's name, the object's name).
pub const TYPE_TITLE: f32 = 16.0;
/// Section labels ("Position", "Holes").
pub const TYPE_SECTION: f32 = 13.0;
/// Body text, buttons, fields, rows.
pub const TYPE_BODY: f32 = 13.0;
/// Captions: one-line help, sizes in rows, hints, the rail's labels.
pub const TYPE_CAPTION: f32 = 11.0;
/// An icon beside text.
pub const ICON_SIZE: f32 = 16.0;
/// A rail tool's icon.
pub const RAIL_ICON_SIZE: f32 = 22.0;

/// Panels use the same tokens: selection is cyan, hover a light outline.
pub fn apply(ctx: &egui::Context) {
    crate::icons::install(ctx);
    crate::i18n::init_from_env();
    ctx.set_visuals_of(egui::Theme::Dark, visuals());
    ctx.style_mut_of(egui::Theme::Dark, style);
    ctx.set_theme(egui::Theme::Dark);
}

/// The type scale and the 4 pt spacing grid, as egui's style.
fn style(s: &mut egui::Style) {
    use egui::{FontFamily::Proportional, FontId, TextStyle};
    s.text_styles = [
        (TextStyle::Heading, FontId::new(TYPE_TITLE, Proportional)),
        (TextStyle::Body, FontId::new(TYPE_BODY, Proportional)),
        (TextStyle::Button, FontId::new(TYPE_BODY, Proportional)),
        (TextStyle::Small, FontId::new(TYPE_CAPTION, Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(12.0, egui::FontFamily::Monospace),
        ),
    ]
    .into();
    let sp = &mut s.spacing;
    sp.item_spacing = egui::vec2(SPACE_S, SPACE_XS);
    sp.button_padding = egui::vec2(SPACE_M, SPACE_XS);
    sp.interact_size = egui::vec2(40.0, 24.0);
    sp.indent = SPACE_L;
    sp.icon_spacing = 6.0;
    sp.menu_margin = egui::Margin::same(6);
    sp.window_margin = egui::Margin::same(SPACE_M as i8);
    // Solid scroll bars: a floating bar that widens under the pointer
    // re-wraps the panel's text, so what's under the pointer moves.
    sp.scroll.floating = false;
    sp.scroll.bar_width = 6.0;
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let radius = egui::CornerRadius::same(RADIUS as u8);
    // A selected row is the current target: the selection's cyan, as a
    // wash with cyan text (never the accent, which means "active tool").
    v.selection.bg_fill = SELECTED.gamma_multiply(0.16);
    v.selection.stroke = egui::Stroke::new(1.0, SELECTED);
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.window_stroke = egui::Stroke::new(1.0, SEPARATOR);
    v.window_corner_radius = egui::CornerRadius::same(8);
    v.menu_corner_radius = radius;
    v.faint_bg_color = CARD;
    v.extreme_bg_color = FIELD;
    v.text_edit_bg_color = Some(FIELD);
    v.override_text_color = None;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    let w = &mut v.widgets;
    // Labels and frames.
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, SEPARATOR);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;
    // Secondary buttons, checkboxes, fields at rest.
    w.inactive.bg_fill = CONTROL;
    w.inactive.weak_bg_fill = CONTROL;
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = CONTROL_HOVER;
    w.hovered.weak_bg_fill = CONTROL_HOVER;
    w.hovered.bg_stroke = egui::Stroke::new(1.0, HOVER.gamma_multiply(0.35));
    w.hovered.fg_stroke = egui::Stroke::new(1.5, HOVER);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = CONTROL_ACTIVE;
    w.active.weak_bg_fill = CONTROL_ACTIVE;
    w.active.bg_stroke = egui::Stroke::new(1.0, HOVER.gamma_multiply(0.5));
    w.active.fg_stroke = egui::Stroke::new(1.5, TEXT_STRONG);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;
    w.open.bg_fill = CONTROL_ACTIVE;
    w.open.weak_bg_fill = CONTROL;
    w.open.corner_radius = radius;
    v
}
