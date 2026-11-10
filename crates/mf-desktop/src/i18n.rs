//! Internationalisation basis (docs/design/ui-plan.md §10): every new or
//! moved user-facing string is a message id looked up through [`t!`], in a
//! static English catalogue. v1 ships English only; Fluent (`fluent-rs`)
//! replaces the table when the first translation lands, behind the same
//! macro. Existing literals are converted over time (docs/issues.md).
//!
//! Rules the catalogue follows:
//! - Whole sentences with `{name}` placeholders, never fragments joined in
//!   code (word order differs between languages).
//! - Plurals are two entries, `<id>.one` and `<id>.other`, picked by
//!   [`tp!`] from a count (Fluent's plural rules replace this).
//! - A missing id is a bug: looking it up panics with the id, so any test
//!   that draws the string finds it.
//!
//! The **pseudo-locale** (View ▸ Pseudo-locale, or `MF_PSEUDO_LOCALE=1`)
//! accents every looked-up string, lengthens it by about 40% and brackets
//! it in [[…]] (the plan's ⟦…⟧ isn't in the bundled fonts), so a literal that bypasses the catalogue, or a layout that
//! breaks with longer text, shows in the frames.

use std::sync::atomic::{AtomicBool, Ordering};

static PSEUDO: AtomicBool = AtomicBool::new(false);

/// Turn the pseudo-locale on or off (debug).
pub fn set_pseudo(on: bool) {
    PSEUDO.store(on, Ordering::Relaxed);
}

/// True while the pseudo-locale is on.
pub fn pseudo() -> bool {
    PSEUDO.load(Ordering::Relaxed)
}

/// Read `MF_PSEUDO_LOCALE` (`1` turns the pseudo-locale on) at start-up.
pub fn init_from_env() {
    if std::env::var("MF_PSEUDO_LOCALE").is_ok_and(|v| v == "1") {
        set_pseudo(true);
    }
}

/// True if message `id` is in the catalogue.
pub fn has(id: &str) -> bool {
    CATALOGUE.binary_search_by(|(k, _)| k.cmp(&id)).is_ok()
}

/// The English text of message `id`. Panics if `id` isn't in the
/// catalogue: a missing message is a bug, never shown as a blank.
pub fn english(id: &str) -> &'static str {
    match CATALOGUE.binary_search_by(|(k, _)| k.cmp(&id)) {
        Ok(i) => CATALOGUE[i].1,
        Err(_) => panic!("i18n: no message {id:?} in the English catalogue"),
    }
}

/// Message `id` with its `{name}` placeholders filled from `args`, in the
/// current locale.
pub fn tr(id: &str, args: &[(&str, String)]) -> String {
    let mut text = english(id).to_owned();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    if pseudo() { pseudo_text(&text) } else { text }
}

/// The plural form of message `id` for count `n` (English rules: `.one`
/// for 1, `.other` otherwise), with `{n}` and `args` filled.
pub fn tr_plural(id: &str, n: u64, args: &[(&str, String)]) -> String {
    let form = if n == 1 { "one" } else { "other" };
    let mut all = vec![("n", crate::defects::thousands(n))];
    all.extend(args.iter().cloned());
    tr(&format!("{id}.{form}"), &all)
}

/// The pseudo-locale's version of `text`: vowels accented, about 40%
/// longer, in [[…]]. Placeholders are already filled.
fn pseudo_text(text: &str) -> String {
    let accented: String = text
        .chars()
        .map(|c| match c {
            'a' => 'á',
            'e' => 'é',
            'i' => 'í',
            'o' => 'ó',
            'u' => 'ú',
            'A' => 'Á',
            'E' => 'É',
            'I' => 'Í',
            'O' => 'Ó',
            'U' => 'Ú',
            other => other,
        })
        .collect();
    let extra = (text.chars().count() * 2).div_ceil(5);
    let pad: String = "·~".chars().cycle().take(extra).collect();
    format!("[[{accented}{pad}]]")
}

/// A message by id: `t!("tool.transform.name")`, or with placeholders,
/// `t!("status.moved", object = name, by = delta)`.
#[macro_export]
macro_rules! t {
    ($id:literal) => {
        $crate::i18n::tr($id, &[])
    };
    ($id:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::tr($id, &[$((stringify!($name), ($value).to_string())),+])
    };
}

/// A plural message by id and count: `tp!("repair.holes", n)` picks
/// `repair.holes.one` or `repair.holes.other` and fills `{n}`.
#[macro_export]
macro_rules! tp {
    ($id:literal, $n:expr) => {
        $crate::i18n::tr_plural($id, ($n) as u64, &[])
    };
    ($id:literal, $n:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::tr_plural($id, ($n) as u64, &[$((stringify!($name), ($value).to_string())),+])
    };
}

/// The English catalogue, sorted by id (a unit test checks the order, so
/// lookups can binary-search).
static CATALOGUE: &[(&str, &str)] = &[
    (
        "browser.empty",
        "Open a mesh to see its objects and defects here",
    ),
    ("browser.hide", "Hide Browser"),
    ("browser.show", "Show Browser"),
    ("browser.tab", "Browser"),
    (
        "browser.tip",
        "Objects and what the analysis found in each (Cmd/Ctrl+\\ shows or hides the side panels)",
    ),
    ("cat.add", "Add"),
    ("cat.arrange", "Arrange"),
    ("cat.fix", "Fix"),
    ("cat.output", "Output"),
    ("cat.shape", "Shape"),
    ("cmd.compact", "Compact"),
    ("cmd.export", "Export…"),
    ("cmd.fill_holes", "Fill all holes"),
    ("cmd.frame", "Frame selection"),
    ("cmd.open", "Open…"),
    ("cmd.redo", "Redo"),
    ("cmd.remove_pieces", "Remove loose pieces"),
    ("cmd.replay", "Replay Command Log…"),
    ("cmd.reset_view", "Reset view"),
    ("cmd.save_log", "Save Command Log…"),
    ("cmd.undo", "Undo"),
    ("combine.apply", "Combine"),
    ("combine.intersect", "Intersect"),
    (
        "combine.intersect_help",
        "Keep only where the two selected objects overlap",
    ),
    (
        "combine.intersect_needs_two",
        "Intersect works on exactly two selected objects",
    ),
    ("combine.keep_holes", "Keep the hole objects"),
    ("combine.more", "More"),
    (
        "combine.needs_solid",
        "Every selected object is a hole: make one a solid (H)",
    ),
    (
        "combine.needs_two",
        "Combine needs two objects: open or import another",
    ),
    ("combine.role.hole", "Hole"),
    ("combine.role.solid", "Solid"),
    (
        "combine.role_help",
        "Press H, or right-click a Browser row, to switch an object between Solid and Hole",
    ),
    ("combine.section.objects", "Objects"),
    (
        "combine.select_more",
        "Shift-click objects in the view or the Browser to combine them",
    ),
    (
        "combine.summary.one_one",
        "Joins {s} solid and cuts {h} hole",
    ),
    (
        "combine.summary.one_other",
        "Joins {s} solid and cuts {h} holes",
    ),
    (
        "combine.summary.other_one",
        "Joins {s} solids and cuts {h} hole",
    ),
    (
        "combine.summary.other_other",
        "Joins {s} solids and cuts {h} holes",
    ),
    ("ctx.fill", "Fill {name}"),
    ("ctx.frame", "Frame {name}"),
    ("ctx.mark", "Mark {name} as intentional"),
    ("ctx.remove", "Remove {name}"),
    ("ctx.select_faces", "Select the faces at {name}"),
    ("ctx.unmark", "Unmark {name}"),
    (
        "dock.collapsed_tool",
        "Show the {tool} panel (hides the Browser on a small window)",
    ),
    ("empty.open", "Open a mesh…"),
    ("empty.title", "Nothing open yet"),
    (
        "export.help",
        "Write the document to a file for printing or another app",
    ),
    (
        "export.log_help",
        "A command log replays this session's steps on the original file.",
    ),
    ("export.section.file", "File"),
    ("export.section.log", "Command log"),
    (
        "hint.first_use",
        "Pick a tool on the left: Transform (T) to move, rotate or scale",
    ),
    ("hint.first_use.dismiss", "Dismiss hint"),
    ("history.empty", "No history yet"),
    ("history.tab", "History"),
    ("history.tip", "Click a step to go back to it"),
    (
        "inspect.help",
        "Everything the analysis measured, in plain numbers",
    ),
    ("inspect.section.counts", "Mesh"),
    ("inspect.section.print", "Printability"),
    ("inspect.section.warnings", "Import warnings"),
    ("inspector.next.repair", "Open Repair"),
    ("inspector.next.transform", "Open Transform"),
    (
        "inspector.ready",
        "Ready to print. Next: place it with Transform.",
    ),
    ("menu.edit", "Edit"),
    ("menu.file", "File"),
    ("menu.help", "Help"),
    ("menu.settings", "Settings"),
    ("menu.tools", "Tools"),
    ("menu.view", "View"),
    ("nav.box", "Box select"),
    (
        "nav.box.tip",
        "Drag a box: selects the visible holes in it (B for one box; Shift adds)",
    ),
    ("nav.frame.tip", "Frame selection (F)"),
    ("nav.orbit", "Orbit"),
    ("nav.orbit.tip", "Drag orbits"),
    ("nav.overlays", "Overlays"),
    (
        "nav.overlays.tip",
        "What each colour in the view means, and which show (View > Overlays)",
    ),
    ("nav.pan", "Pan"),
    ("nav.pan.tip", "Drag pans (or Shift + drag)"),
    ("nav.reset.tip", "Reset view (Home, or Cmd/Ctrl+0)"),
    ("nav.wireframe", "Wireframe"),
    (
        "nav.wireframe.tip",
        "Show every triangle's edges (View > Wireframe)",
    ),
    ("nav.zoom", "Zoom"),
    ("nav.zoom.tip", "Drag up/down zooms (or Option/Alt + drag)"),
    ("nav.zoom_in", "Zoom in"),
    ("nav.zoom_in.tip", "Zoom in (+ or =); hold to repeat"),
    ("nav.zoom_out", "Zoom out"),
    ("nav.zoom_out.tip", "Zoom out (−); hold to repeat"),
    ("overlays.model", "Model"),
    ("overlays.model.back", "Back faces"),
    (
        "overlays.model.back.tip",
        "Inside surfaces, seen through holes or flipped",
    ),
    ("overlays.model.surface", "Surface"),
    (
        "overlays.model.surface.tip",
        "The model's outside, in clay (it carries no colours of its own)",
    ),
    ("overlays.overlays", "Overlays"),
    ("overlays.title", "Overlays"),
    ("panel.back", "Back"),
    ("panel.close", "Close {tool} (Esc)"),
    ("refusal.make_solid", "Run Make solid first"),
    ("refusal.repair", "Repair holes first"),
    ("refusal.title", "This can't be done yet"),
    ("repair.back", "All repairs"),
    ("repair.back.tip", "Back to every repair (Esc)"),
    ("repair.cracks.none", "No narrow cracks"),
    (
        "repair.cracks.why",
        "Edges that almost touch. Welding them shut is planned; until then, Fill all holes leaves them open.",
    ),
    ("repair.done", "Done"),
    ("repair.fill_settings", "Fill all holes…"),
    (
        "repair.help",
        "Fix what the analysis found: holes, cracks and loose pieces",
    ),
    ("repair.holes.none", "No holes to fill"),
    ("repair.holes.summary.one", "{n} hole to fill"),
    ("repair.holes.summary.other", "{n} holes to fill"),
    (
        "repair.housekeeping.why",
        "Reclaims the space earlier edits left unused. Changes nothing you can see.",
    ),
    ("repair.loose.none", "No loose pieces"),
    (
        "repair.loose.summary.one",
        "{n} piece not connected to the model",
    ),
    (
        "repair.loose.summary.other",
        "{n} pieces not connected to the model",
    ),
    ("repair.next_hole", "Next hole"),
    ("repair.next_hole.tip", "Select and frame the next hole (N)"),
    (
        "repair.openings.summary.one",
        "{n} intentional opening, left open",
    ),
    (
        "repair.openings.summary.other",
        "{n} intentional openings, left open",
    ),
    ("repair.previous_hole", "Previous hole"),
    (
        "repair.previous_hole.tip",
        "Select and frame the previous hole (Shift+N)",
    ),
    ("repair.remove_settings", "Remove loose pieces…"),
    ("repair.section.cracks", "Narrow cracks"),
    ("repair.section.holes", "Holes"),
    ("repair.section.housekeeping", "Housekeeping"),
    ("repair.section.loose", "Loose pieces"),
    ("repair.section.tangled", "Tangled edges"),
    ("repair.tangled.none", "No tangled edges"),
    (
        "repair.tangled.why",
        "More than two faces share these edges, so slicers may print them wrongly. Untangling is planned; select one in the Browser to see its faces.",
    ),
    ("role.make_hole", "Make it a hole"),
    ("role.make_holes", "Make {n} objects holes"),
    ("role.make_solid", "Make it a solid"),
    ("role.make_solids", "Make {n} objects solids"),
    ("role.row_hole", "{name} (hole)"),
    ("settings.dev", "Developer"),
    ("settings.dev.pseudo", "Pseudo-locale"),
    (
        "settings.dev.pseudo.tip",
        "Shows every catalogued string accented, 40% longer and in [[…]], so untranslated text and tight layouts stand out",
    ),
    ("settings.title", "Settings"),
    ("solid.apply", "Make it solid"),
    (
        "solid.offset_help",
        "Grow (positive) or shrink (negative) the surface",
    ),
    ("solid.section.cell", "Cell size ({unit})"),
    ("solid.section.offset", "Offset ({unit})"),
    ("solid.smooth", "Smooth the surface once"),
    (
        "solid.smooth_help",
        "Off: keep the surface where the grid puts it",
    ),
    (
        "solid.too_fine",
        "This grid is too fine for one go: the operation will refuse it and say which size fits.",
    ),
    (
        "solid.tradeoff",
        "Detail smaller than {cell} {unit} is lost; smaller cells keep more but take longer and make more triangles.",
    ),
    (
        "solid.what",
        "Rebuilds the surface as one closed solid, closing holes and removing overlaps.",
    ),
    ("split.apply", "Cut in two"),
    (
        "split.gap_help",
        "A slab this thick is removed at the cut, so the parts don't touch",
    ),
    ("split.horizontal", "Horizontal"),
    ("split.keep.above", "Above"),
    ("split.keep.below", "Below"),
    ("split.keep.both", "Both"),
    ("split.move", "Move"),
    (
        "split.plane_help",
        "Drag the gizmo in the view to move or tilt the plane, or pick a direction",
    ),
    (
        "split.refuse_open",
        "The model has holes; cutting needs a closed model. Fill the holes in Repair, then split.",
    ),
    (
        "split.result",
        "Both parts are closed with a flat cap: each selected object the plane crosses becomes \"(top)\" and a new \"(bottom)\" appears beside it.",
    ),
    ("split.section.gap", "Gap at the cut ({unit})"),
    ("split.section.keep", "Keep"),
    ("split.section.plane", "Cut plane"),
    ("split.section.point", "Point ({unit})"),
    ("split.section.tilt", "Tilt"),
    ("split.section_view", "Section view"),
    (
        "split.section_view.tip",
        "See through the part above the plane and into the cut",
    ),
    ("split.targets.one", "Cuts {n} selected object"),
    ("split.targets.other", "Cuts {n} selected objects"),
    ("split.tilt", "Tilt"),
    ("split.vertical_x", "Vertical X"),
    ("split.vertical_y", "Vertical Y"),
    ("status.ready", "Ready"),
    ("status.stats", "{tris} · {size}"),
    (
        "status.stats.tip",
        "The active object: triangles, and size along X, Y and Z",
    ),
    (
        "tool.combine.help",
        "Join the selected solids and cut the holes",
    ),
    ("tool.combine.name", "Combine"),
    ("tool.export.help", "Write the model to 3MF, STL or OBJ"),
    ("tool.export.name", "Export"),
    ("tool.inspect.help", "Every number the analysis reports"),
    ("tool.inspect.name", "Inspect"),
    (
        "tool.make_solid.help",
        "Rebuild the surface as one closed solid",
    ),
    ("tool.make_solid.name", "Make solid"),
    (
        "tool.repair.help",
        "Fill holes, remove loose pieces, mark openings",
    ),
    ("tool.repair.name", "Repair"),
    (
        "tool.select.help",
        "Click holes, pieces and objects to work on them",
    ),
    ("tool.select.name", "Select"),
    ("tool.split.help", "Cut the object in two along a plane"),
    ("tool.split.name", "Split"),
    ("tool.transform.help", "Move, rotate and scale the object"),
    ("tool.transform.name", "Transform"),
    ("transform.done", "Done"),
    ("transform.drop", "Drop to plate"),
    (
        "transform.drop.tip",
        "Move it straight down (or up) until its lowest point rests on the plate",
    ),
    (
        "transform.help",
        "Drag the handles in the view, or type exact values (Enter applies them)",
    ),
    ("transform.mode.move", "Move"),
    ("transform.mode.rotate", "Rotate"),
    ("transform.mode.scale", "Scale"),
    ("transform.reset", "Reset"),
    (
        "transform.reset.tip",
        "No rotation, 100% scale; it stays where it is",
    ),
    ("transform.section.mode", "Mode"),
    ("transform.section.options", "Options"),
    ("transform.section.position", "Position ({unit})"),
    ("transform.section.rotation", "Rotation (°)"),
    ("transform.section.scale", "Scale (%)"),
    ("transform.snap", "Snap"),
    (
        "transform.snap.tip",
        "To the 1 mm grid, 15° steps, 10% steps and the plate. Hold Cmd/Ctrl while dragging to invert.",
    ),
    ("transform.uniform", "Uniform scale"),
    ("transform.uniform.tip", "Scale fields change together"),
    ("view.clay", "Clay"),
    ("view.overlays", "Overlays…"),
    ("view.pseudo", "Pseudo-locale (debug)"),
    ("view.studio", "Studio"),
    ("view.wireframe", "Wireframe"),
    ("view.xray", "X-ray"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Lookups binary-search the catalogue: it must stay sorted, with no
    /// duplicate ids.
    #[test]
    fn the_catalogue_is_sorted_and_unique() {
        for w in CATALOGUE.windows(2) {
            assert!(w[0].0 < w[1].0, "{:?} before {:?}", w[0].0, w[1].0);
        }
    }

    /// Placeholders fill by name; plurals pick `.one` / `.other`.
    #[test]
    fn placeholders_and_plurals() {
        assert_eq!(tr("ctx.fill", &[("name", "Hole 3".into())]), "Fill Hole 3");
        assert_eq!(tr_plural("repair.holes.summary", 1, &[]), "1 hole to fill");
        assert_eq!(
            tr_plural("repair.holes.summary", 1200, &[]),
            "1,200 holes to fill"
        );
    }

    /// The pseudo-locale brackets, accents and lengthens.
    #[test]
    fn pseudo_text_is_bracketed_accented_and_longer() {
        let p = pseudo_text("Fill all holes");
        assert!(p.starts_with("[[") && p.ends_with("]]"), "{p}");
        assert!(p.contains("Fíll áll hólés"), "{p}");
        assert!(p.chars().count() >= "Fill all holes".len() * 14 / 10, "{p}");
    }

    #[test]
    #[should_panic(expected = "no message")]
    fn a_missing_message_is_a_bug() {
        english("no.such.message");
    }
}
