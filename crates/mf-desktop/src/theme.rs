//! Colour tokens (docs/design/visual-language.md). Every colour the app
//! shows comes from here, panels and 3D view alike, and each token has one
//! meaning: a colour never means two things.

use eframe::egui::{self, Color32};

/// The current target: selected loops, pieces and faces, the selected tree
/// row, the current history step.
pub const SELECTED: Color32 = Color32::from_rgb(40, 210, 255);
/// What the pointer is over (a light outline).
pub const HOVER: Color32 = Color32::from_rgb(232, 238, 245);
/// Holes: every open boundary's outline.
pub const HOLE: Color32 = Color32::from_rgb(255, 128, 32);
/// Narrow cracks: boundaries whose edges almost touch.
pub const CRACK: Color32 = Color32::from_rgb(230, 70, 210);
/// Loose pieces: parts not connected to the model.
pub const LOOSE_PIECE: Color32 = Color32::from_rgb(255, 200, 40);
/// Non-manifold ("tangled") edges.
pub const NON_MANIFOLD: Color32 = Color32::from_rgb(235, 52, 52);
/// Faces created by the last few commands (toggle: View ▸ Highlight recent repairs).
pub const RECENT_REPAIR: Color32 = Color32::from_rgb(80, 205, 120);

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
/// Secondary text: details, hints, steps you can redo.
pub const TEXT_MUTED: Color32 = Color32::from_gray(150);
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

/// Panels use the same tokens: selection is cyan, hover a light outline.
pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals());
    ctx.set_theme(egui::Theme::Dark);
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.selection.bg_fill = SELECTED.gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, SELECTED);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, HOVER);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, HOVER);
    v.extreme_bg_color = VIEWPORT_BACKGROUND;
    v
}
