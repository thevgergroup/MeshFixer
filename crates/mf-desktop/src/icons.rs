//! The app's icons: the Phosphor icon font (regular weight, MIT; see
//! THIRD_PARTY_NOTICES.md), embedded and registered as a fallback after
//! the text font, so an icon's codepoint can sit in any label. One
//! constant per icon the app uses, named for what it shows, so a glyph
//! means one thing everywhere (docs/design/visual-language.md, "Icons").
//!
//! The codepoints are Phosphor 2.x's (the same table `egui-phosphor` 0.14
//! ships; that crate itself targets egui 0.36, so the font is vendored
//! instead of depending on it).

use eframe::egui;

/// The font file (Phosphor 2.x, regular weight).
const FONT: &[u8] = include_bytes!("../assets/fonts/Phosphor.ttf");
/// Its name in egui's font definitions.
const FONT_NAME: &str = "phosphor";

/// Register the icon font: after the text font in the proportional family
/// (so text keeps its own glyphs and an icon codepoint falls through to
/// Phosphor), and as its own family, [`family`].
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_NAME.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(FONT)),
    );
    if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let at = keys.len().min(1);
        keys.insert(at, FONT_NAME.to_owned());
    }
    fonts.families.insert(family(), vec![FONT_NAME.to_owned()]);
    ctx.set_fonts(fonts);
}

/// The icon font on its own (for an icon drawn with the painter).
pub fn family() -> egui::FontFamily {
    egui::FontFamily::Name(FONT_NAME.into())
}

/// An icon's font at `size` points.
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::new(size, family())
}

// Tools (the rail). Tools still being built have their glyph reserved here
// so the rail reads as one family when they arrive.
pub const SELECT: &str = "\u{E1DC}"; // cursor
pub const TRANSFORM: &str = "\u{E0A4}"; // arrows-out-cardinal
pub const REPAIR: &str = "\u{E0B2}"; // bandaids
pub const INSPECT: &str = "\u{EBE0}"; // list-magnifying-glass
pub const SPLIT: &str = "\u{EAE0}"; // scissors
pub const COMBINE: &str = "\u{E87E}"; // unite
pub const MAKE_SOLID: &str = "\u{E1DA}"; // cube
pub const SCULPT: &str = "\u{E6F0}"; // paint-brush
pub const PARTS: &str = "\u{E596}"; // puzzle-piece
pub const EXPORT: &str = "\u{EAF0}"; // export

// The tree.
pub const OBJECT: &str = "\u{E1DA}"; // cube
pub const MODEL: &str = "\u{ED0A}"; // cube-focus
pub const HOLE: &str = "\u{E602}"; // circle-dashed
pub const CRACK: &str = "\u{E2DE}"; // lightning
pub const LOOSE_PIECE: &str = "\u{EC5E}"; // shapes
pub const TANGLED: &str = "\u{E280}"; // git-merge
pub const OPENING: &str = "\u{E18A}"; // circle
pub const CARET_RIGHT: &str = "\u{E13A}";
pub const CARET_DOWN: &str = "\u{E136}";
pub const CARET_LEFT: &str = "\u{E138}";

// Actions.
pub const UNDO: &str = "\u{E08A}"; // arrow-u-up-left
pub const REDO: &str = "\u{E08C}"; // arrow-u-up-right
pub const HISTORY: &str = "\u{E1A0}"; // clock-counter-clockwise
pub const CLOSE: &str = "\u{E4F6}"; // x
pub const PANEL: &str = "\u{EC24}"; // sidebar-simple
pub const BROWSER: &str = "\u{EE48}"; // tree-view
pub const SETTINGS: &str = "\u{E270}"; // gear
pub const OPEN: &str = "\u{E256}"; // folder-open
pub const DROP_TO_PLATE: &str = "\u{E05C}"; // arrow-line-down
pub const RESET: &str = "\u{E038}"; // arrow-counter-clockwise
pub const SNAP: &str = "\u{E680}"; // magnet
pub const REMOVE: &str = "\u{EC54}"; // broom
pub const COMPACT: &str = "\u{E09E}"; // arrows-in-simple
pub const CHECK: &str = "\u{E182}";
pub const OK: &str = "\u{E184}"; // check-circle
pub const WARNING: &str = "\u{E4E2}"; // warning-circle
pub const INFO: &str = "\u{E2CE}";
pub const HINT: &str = "\u{E2DC}"; // lightbulb

// The navigation toolbar.
pub const ORBIT: &str = "\u{E094}"; // arrows-clockwise
pub const PAN: &str = "\u{E298}"; // hand
pub const ZOOM: &str = "\u{E30C}"; // magnifying-glass
pub const BOX_SELECT: &str = "\u{E69A}"; // selection
pub const ZOOM_OUT: &str = "\u{E32A}"; // minus
pub const ZOOM_IN: &str = "\u{E3D4}"; // plus
pub const FRAME: &str = "\u{E626}"; // frame-corners
pub const HOME: &str = "\u{E2C2}"; // house
pub const OVERLAYS: &str = "\u{E468}"; // stack-simple
