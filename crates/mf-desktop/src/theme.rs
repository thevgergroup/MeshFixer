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
/// Sculpt ▸ "Show sculpt changes": a chartreuse heatmap, apart from every
/// other defect/overlay hue (`HOLE`'s orange, `LOOSE_PIECE`'s amber,
/// `RECENT_REPAIR`'s green, `CRACK`'s magenta…), strongest where a vertex
/// has moved most since the tool opened.
pub const SCULPT_CHANGED: Color32 = Color32::from_rgb(180, 244, 52);

/// Where two objects cross (`mesh.intersections`), drawn while Combine is
/// open: a violet, apart from the accent's indigo and `CRACK`'s magenta.
pub const INTERSECTION: Color32 = Color32::from_rgb(0xB3, 0x6B, 0xFF);

/// Split's preview and Keep buttons (owner 2026-11-12): the side the cut
/// plane's normal points to (`keep: above`'s side; "Top"/"Right"/"Back"/
/// "Side A" depending on orientation) — a warm amber, apart from every
/// defect/overlay hue above and from `SPLIT_SIDE_B`.
pub const SPLIT_SIDE_A: Color32 = Color32::from_rgb(255, 176, 59);
/// Split's other side (`keep: below`'s side; "Bottom"/"Left"/"Front"/
/// "Side B") — a cool teal, apart from `SPLIT_SIDE_A` and the accent's
/// indigo.
pub const SPLIT_SIDE_B: Color32 = Color32::from_rgb(45, 197, 188);

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
pub const PLATE: Color32 = Color32::from_gray(66);
/// The plate's major lines (every 50 mm): the same neutral, a step
/// brighter, so the scale reads at any zoom.
pub const PLATE_MAJOR: Color32 = Color32::from_gray(78);

/// An inactive object keeps this much of its saturation…
pub const INACTIVE_SATURATION: f32 = 0.0;
/// …and this much of its brightness (desaturated, so the active one stands out).
pub const INACTIVE_BRIGHTNESS: f32 = 0.55;

/// How strongly a face layer replaces the surface colour (0–1).
pub const SELECTED_FACE_STRENGTH: f32 = 0.18;
pub const HOVER_FACE_STRENGTH: f32 = 0.2;
pub const LOOSE_PIECE_STRENGTH: f32 = 0.6;
pub const RECENT_REPAIR_STRENGTH: f32 = 0.25;
/// Faces around a hole (View ▸ Show hole overview): the hole colour, low
/// alpha, so a model with hundreds of holes shows where they are.
pub const HOLE_NEIGHBOURHOOD_STRENGTH: f32 = 0.3;
/// Faces next to a tangled edge: a light red, so the red edge line stays
/// the strongest mark.
pub const NON_MANIFOLD_FACE_STRENGTH: f32 = 0.25;
/// Sculpt ▸ "Show sculpt changes": the top tier's strength (the shader
/// scales it down for the lower two, `renderer.rs` `base_color`).
pub const SCULPT_CHANGED_STRENGTH: f32 = 0.7;

/// The selection where nearer surfaces hide it (the x-ray pass) is drawn
/// in `SELECTED` at this opacity: the same colour, so the same meaning.
pub const XRAY_OPACITY: f32 = 0.55;
/// A Hole's hidden part — buried inside or behind a solid — drawn as a
/// ghost (View ▸ Overlays ▸ "Show holes through solids"): well under the
/// visible part's ~35% opacity, so it never competes with real geometry.
pub const HOLE_XRAY_OPACITY: f32 = 0.2;

/// Outline widths in the 3D view, in points: defect outlines, and the
/// selection / hover / crack / tangled-edge overlays drawn over them.
pub const BOUNDARY_WIDTH: f32 = 2.5;
pub const OVERLAY_WIDTH: f32 = 3.5;

// Neutrals: no meaning beyond "the model" and "the room it sits in".
/// Front faces of the model.
pub const SURFACE: Color32 = Color32::from_rgb(0xC9, 0xCC, 0xD1);
/// Back faces, seen through holes or flipped (a dark slate, not a defect colour).
pub const BACK_FACE: Color32 = Color32::from_rgb(78, 84, 100);
pub const VIEWPORT_BACKGROUND: Color32 = Color32::from_rgb(0x2B, 0x2F, 0x35);
/// The top of the stage's soft vertical gradient (UI plan §4); the bottom
/// is `VIEWPORT_BACKGROUND`.
pub const VIEWPORT_BACKGROUND_TOP: Color32 = Color32::from_rgb(0x3A, 0x3F, 0x46);
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

/// Object colours (UI plan §11, View ▸ Colour objects): a muted palette,
/// moderate saturation (~38%) so the chrome's defect and selection
/// colours (`HOLE`'s orange, `CRACK`'s magenta, `SELECTED`'s cyan…) stay
/// readable over it and the model still reads apart from the dark chrome.
/// Nine hues spread evenly, at the same lightness so no object reads as
/// "more important" just from its slot. Kept light enough (~70%
/// lightness) that even the darkest hue's (violet's) desaturated,
/// `INACTIVE_BRIGHTNESS`-dimmed grey still clears the stage background
/// by a comfortable margin — an inactive object must never blend into
/// the chrome (MF-ISSUE: a too-dark hue nearly vanished there).
pub const OBJECT_PALETTE: [Color32; 9] = [
    Color32::from_rgb(156, 184, 211), // blue
    Color32::from_rgb(211, 179, 156), // clay orange
    Color32::from_rgb(156, 211, 184), // green
    Color32::from_rgb(193, 156, 211), // violet
    Color32::from_rgb(211, 202, 156), // ochre
    Color32::from_rgb(211, 156, 175), // rose
    Color32::from_rgb(156, 202, 211), // teal
    Color32::from_rgb(179, 211, 156), // moss
    Color32::from_rgb(211, 170, 156), // terracotta
];

/// A shade of `OBJECT_PALETTE[index]`, lightened or darkened by `variant`
/// (siblings in a linked group, 0 the base shade, alternating out from
/// there): a related colour, not a jarring, unrelated one.
fn object_shade(index: usize, variant: u32) -> Color32 {
    let base = OBJECT_PALETTE[index % OBJECT_PALETTE.len()];
    if variant == 0 {
        return base;
    }
    // ±1 step 12%, ±2 step 24%, alternating lighter/darker: a family of
    // shades that stays inside a sane range (never near-black or
    // near-white) for up to a handful of siblings.
    let step = variant.div_ceil(2) as f32 * 0.12;
    let lighten = variant % 2 == 1;
    let mix = |c: u8| -> u8 {
        let c = f32::from(c);
        let target = if lighten { 255.0 } else { 0.0 };
        (c + (target - c) * step).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(mix(base.r()), mix(base.g()), mix(base.b()))
}

/// The object colour for an object created in `creation_order` (an
/// `ObjectId`'s own value: monotonic, never reused, so stable for the
/// session), optionally sharing `group` with other objects (a connector's
/// linked parts): the group's members share a hue and vary only in shade,
/// keyed on `creation_order` so each member of the group gets a distinct,
/// stable variant.
pub fn object_color(creation_order: u64, group: Option<&str>) -> Color32 {
    match group {
        Some(g) => {
            let index = (hash_str(g) % OBJECT_PALETTE.len() as u64) as usize;
            object_shade(index, (creation_order % 5) as u32)
        }
        None => object_shade((creation_order % OBJECT_PALETTE.len() as u64) as usize, 0),
    }
}

/// A small stable string hash (FNV-1a): just needs to spread group names
/// across the palette, not resist attack.
fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The 3D view's palette, from these tokens.
pub fn palette() -> mf_render::Palette {
    mf_render::Palette {
        background: rgba(VIEWPORT_BACKGROUND),
        background_top: rgba(VIEWPORT_BACKGROUND_TOP),
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
            sculpt_changed: tint(SCULPT_CHANGED, SCULPT_CHANGED_STRENGTH),
        },
        xray_opacity: XRAY_OPACITY,
        hole_xray_opacity: HOLE_XRAY_OPACITY,
        split_tint_a: rgba(SPLIT_SIDE_A),
        split_tint_b: rgba(SPLIT_SIDE_B),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The first several creation-order slots get visibly distinct
    /// colours (the owner's complaint: a multi-object model's parts all
    /// read the same).
    #[test]
    fn the_first_few_objects_get_distinct_colours() {
        let colors: Vec<Color32> = (0..OBJECT_PALETTE.len() as u64)
            .map(|id| object_color(id, None))
            .collect();
        for (i, a) in colors.iter().enumerate() {
            for (j, b) in colors.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "slot {i} and {j} got the same colour");
                }
            }
        }
    }

    /// The same object always gets the same colour (stable across frames
    /// and across a session: it's keyed on the id alone).
    #[test]
    fn the_same_id_always_gets_the_same_colour() {
        for id in [0, 1, 7, 42, 1_000_000] {
            assert_eq!(object_color(id, None), object_color(id, None));
        }
    }

    /// Past the palette's length, colours repeat rather than panic (a
    /// document with more objects than palette slots).
    #[test]
    fn colours_wrap_past_the_palette_length() {
        let n = OBJECT_PALETTE.len() as u64;
        assert_eq!(object_color(0, None), object_color(n, None));
    }

    /// Linked parts (a connector's peg and its hole) share a hue — the
    /// group's base palette slot — and differ only in shade, so they read
    /// as a related family, not two unrelated objects.
    #[test]
    fn group_siblings_share_a_hue_but_vary_in_shade() {
        let a = object_color(10, Some("joint-1"));
        let b = object_color(11, Some("joint-1"));
        assert_ne!(a, b, "siblings should get distinct shades");
        // Same hue family: at least two of the three channels keep their
        // relative order (a tinted/shaded version of the same base,
        // rather than an unrelated hue).
        let base_index = OBJECT_PALETTE.iter().position(|&c| {
            let variants: Vec<Color32> = (0..5).map(|v| object_shade_for_test(c, v)).collect();
            variants.contains(&a) && variants.contains(&b)
        });
        assert!(
            base_index.is_some(),
            "both siblings should come from the same base shade family"
        );
    }

    /// A different group gets a different (stable) base hue.
    #[test]
    fn different_groups_can_get_different_hues() {
        let hues: std::collections::HashSet<Color32> = ["alpha", "bravo", "charlie", "delta"]
            .iter()
            .map(|g| object_color(0, Some(g)))
            .collect();
        assert!(
            hues.len() > 1,
            "distinct group names should not all collide on one hue"
        );
    }

    fn object_shade_for_test(base: Color32, variant: u32) -> Color32 {
        let index = OBJECT_PALETTE
            .iter()
            .position(|&c| c == base)
            .expect("test base is in the palette");
        object_shade(index, variant)
    }

    /// An inactive object is desaturated to its own luminance, then dimmed
    /// by `INACTIVE_BRIGHTNESS`: every palette colour's result must still
    /// clear the stage background by a real margin, or an object with
    /// that hue nearly vanishes against the chrome the moment it isn't
    /// active (a real bug this regression test catches: an earlier,
    /// darker palette let a violet-hued object's inactive grey land
    /// right at the background's own brightness).
    #[test]
    fn every_palette_colour_stays_visible_when_inactive_and_dimmed() {
        let luma = |c: Color32| {
            0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b())
        };
        let bg_luma = luma(VIEWPORT_BACKGROUND).max(luma(VIEWPORT_BACKGROUND_TOP));
        for (i, &c) in OBJECT_PALETTE.iter().enumerate() {
            let inactive_luma = luma(c) * INACTIVE_BRIGHTNESS;
            assert!(
                inactive_luma > bg_luma + 20.0,
                "palette colour {i} ({c:?}) only reaches {inactive_luma} when inactive, \
                 too close to the background's {bg_luma}"
            );
        }
    }
}
