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
    ("alignment.depth.above", "Above"),
    ("alignment.depth.below", "Below"),
    ("alignment.dowel_size", "Dowel {diameter} × {length} {unit}"),
    (
        "alignment.dowel_too_short",
        "These depths and allowance leave no dowel length; deepen the holes or shrink the allowance",
    ),
    ("alignment.holes_placed.one", "{n} hole placed"),
    ("alignment.holes_placed.other", "{n} holes placed"),
    ("alignment.pegs.attached", "Pegs"),
    ("alignment.pegs.dovetail", "Dovetail"),
    ("alignment.pegs.dowels", "Dowels"),
    ("alignment.pegs.none", "None"),
    (
        "alignment.regions_skipped.one",
        "{n} sliver region left without a hole",
    ),
    (
        "alignment.regions_skipped.other",
        "{n} sliver regions left without a hole",
    ),
    ("alignment.section.depth", "Hole depth ({unit})"),
    ("alignment.section.diameter", "Hole diameter ({unit})"),
    ("alignment.section.pegs", "Alignment"),
    ("alignment.section.shape", "Shape"),
    ("alignment.shape.keyed", "Keyed"),
    ("alignment.shape.round", "Round"),
    ("alignment.shape.square", "Square"),
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
    ("connect.apply", "Add connectors"),
    ("connect.apply.again", "Update connectors"),
    (
        "connect.apply.later.one",
        "Updating goes back to before these connectors; the {n} step made after them stays in History on its own branch.",
    ),
    (
        "connect.apply.later.other",
        "Updating goes back to before these connectors; the {n} steps made after them stay in History on their own branch.",
    ),
    (
        "connect.apply.replaces",
        "Updating replaces the connectors added before.",
    ),
    ("connect.as_shapes", "Add as editable shapes"),
    (
        "connect.as_shapes.tip",
        "Place each connector as a linked group of editable shapes (the part on one piece, its hole on the other) instead of cutting them in now. Edit them, then Attach.",
    ),
    ("connect.check", "Connector {n}: {message}"),
    ("connect.fit.class", "Fit: {name}"),
    ("connect.fit.machine", "Printer: {name}"),
    (
        "connect.fit.unreadable",
        "Your Fit classes or Machine profiles couldn't be read ({reason}); using the built-ins.",
    ),
    ("connect.fix.add_second_dowel", "Add second dowel"),
    ("connect.fix.breakout", "Make it shallower"),
    ("connect.fix.dovetail_edge", "Extend to the edge"),
    ("connect.fix.dowel", "Shorten the dowel"),
    ("connect.fix.failed", "Couldn't fix it: {reason}"),
    ("connect.fix.fit", "Use the fit's clearance"),
    ("connect.fix.inwards", "Move it inwards"),
    ("connect.fix.make_square", "Make square"),
    (
        "connect.fix.none",
        "No change fixes this here; move the connector or make it smaller.",
    ),
    (
        "connect.geometry_failed",
        "Couldn't read this joint's cut faces: {reason}.",
    ),
    ("connect.handle.depth", "Depth"),
    ("connect.joint.applied.one", "{n} connector added"),
    ("connect.joint.applied.other", "{n} connectors added"),
    ("connect.joint.cut", "cut"),
    ("connect.joint.draft.one", "{n} connector, not added yet"),
    ("connect.joint.draft.other", "{n} connectors, not added yet"),
    ("connect.joint.empty", "No connectors yet"),
    ("connect.joint.face", "{face} face"),
    ("connect.joint.row", "{a} / {b} ({label})"),
    (
        "connect.joints.none",
        "Split a model first to create joints",
    ),
    (
        "connect.joints.open_split",
        "Open Split",
    ),
    (
        "connect.joints.pick",
        "Pick a joint to add connectors to it.",
    ),
    ("connect.kind.dovetail", "Dovetail"),
    ("connect.kind.dovetail.plural", "dovetails"),
    ("connect.kind.dowel", "Dowel"),
    ("connect.kind.dowel.plural", "dowels"),
    ("connect.kind.insert", "Insert"),
    ("connect.kind.insert.plural", "inserts"),
    ("connect.kind.magnet", "Magnet"),
    ("connect.kind.magnet.plural", "magnets"),
    ("connect.kind.peg", "Peg"),
    ("connect.kind.peg.plural", "pegs"),
    ("connect.min_wall", "Thinnest wall"),
    ("connect.param.angle", "Direction"),
    ("connect.param.clearance", "Clearance"),
    ("connect.param.depth_on", "Depth, this side"),
    ("connect.param.depth_other", "Depth, other side"),
    ("connect.param.diameter", "Diameter"),
    ("connect.param.dowel_length", "Dowel length"),
    ("connect.param.groove_a", "Reach, start end"),
    ("connect.param.groove_b", "Reach, finish end"),
    ("connect.param.hole_profile", "Hole shape"),
    ("connect.param.insert", "Insert diameter"),
    ("connect.param.length", "Tail length"),
    ("connect.param.magnet", "Magnet diameter"),
    ("connect.param.peg_root", "Sunk into this side"),
    ("connect.param.root", "Root, this side"),
    ("connect.param.screw", "Screw diameter"),
    ("connect.param.shape", "Cross-section"),
    ("connect.param.sides", "Sides"),
    ("connect.param.through_a", "Through (start end)"),
    ("connect.param.through_b", "Through (finish end)"),
    ("connect.param.width", "Width"),
    (
        "connect.place_help",
        "Click on the cut face of {name} to place a connector; drag one to move it, its rim to resize it and the square to change its depth. Delete removes the picked one.",
    ),
    ("connect.placed.none", "None yet: click on the cut face."),
    ("connect.placed.row_ok", "{n}. {kind}"),
    ("connect.placed.row_remove", "Remove connector {n}"),
    ("connect.placed.row_warn.one", "{index}. {kind}: {n} warning"),
    (
        "connect.placed.row_warn.other",
        "{index}. {kind}: {n} warnings",
    ),
    ("connect.reading", "Reading the cut faces…"),
    ("connect.remove", "Remove connector"),
    ("connect.section.fit", "Fit"),
    ("connect.section.joints", "Joints"),
    ("connect.section.kind", "Connector"),
    ("connect.section.placed", "Connectors on this joint"),
    ("connect.section.transparent", "See-through piece"),
    ("connect.selected", "Connector {n}: {kind}"),
    ("connect.shape.keyed", "Keyed"),
    ("connect.shape.nsided", "N-sided"),
    ("connect.shape.round", "Round"),
    ("connect.shape.square", "Square"),
    ("connect.shapes_label", "Add connectors as editable shapes"),
    ("connect.sideways", "Hole {n} prints sideways and will sag at the top; switch to Teardrop"),
    ("connectors.add", "Add"),
    ("connectors.apply", "Add \"{name}\""),
    (
        "connectors.click_help",
        "Click a connector to add it to the model as editable shapes, one per part",
    ),
    ("connectors.fit_check.fix", "Use a looser fit"),
    (
        "connectors.fit_check.not_applicable",
        "No fit check for this connector",
    ),
    ("connectors.fit_check.pair", "Connector mating parts"),
    ("connectors.fit_class", "Fit class"),
    ("connectors.machine_profile", "Machine profile"),
    ("connectors.merge", "Attach"),
    ("connectors.not_calibrated", "{name} (not calibrated)"),
    (
        "connectors.save_failed",
        "Couldn't read the connector's saved parameters",
    ),
    (
        "connectors.save_help",
        "Save the placed connector to your library, recipe and all",
    ),
    ("connectors.save_to_library", "Save to library…"),
    ("connectors.saved", "\"{name}\" saved to your library"),
    ("connectors.search_hint", "Search connectors"),
    ("connectors.section.fit", "Fit"),
    ("connectors.section.fit_check", "Fit check"),
    ("connectors.section.library", "Library"),
    ("connectors.section.merge", "Attach to the model"),
    ("connectors.side_a", "Side A"),
    ("connectors.side_a_option", "Side A: {name}"),
    ("connectors.side_b", "Side B"),
    ("connectors.side_b_option", "Side B: {name}"),
    ("connectors.thread_preset", "M{size} × {length} mm"),
    (
        "connectors.uncalibrated_help",
        "This machine profile has never been calibrated, so no tolerance is compensated; the Machine profiles settings page offers a calibration print.",
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
    ("hinge.axis", "Hinge axis"),
    ("hinge.edge", "{face} edge, along {along}"),
    (
        "hinge.fit",
        "Gaps: {gap} mm between the knuckles, {clearance} mm around the pin (from the Fit class and Machine profile)",
    ),
    (
        "hinge.help",
        "Pick the two objects the hinge joins. Its axis snaps to the edge they share; leaf A joins A, leaf B joins B, and the pin is its own piece.",
    ),
    ("hinge.name", "Pin hinge"),
    (
        "hinge.no_edge",
        "These two objects don't share a face, so there's no edge to put a hinge on. Move them next to each other first.",
    ),
    ("hinge.object_a", "Object A (stays put)"),
    ("hinge.object_b", "Object B (swings open)"),
    ("hinge.place", "Place hinge"),
    ("hinge.same_object", "Pick two different objects."),
    ("hinge.section", "Hinge between two objects"),
    ("hinge.shared_edge", "Shared edge ({edge})"),
    ("hinge.swing", "Swing preview"),
    ("hinge.swing.checking", "Checking the swing…"),
    ("hinge.swing.clear", "Opens the whole {limit}° without touching anything"),
    ("hinge.swing.colliding", "Colliding here in {n} place(s), highlighted in the view"),
    ("hinge.swing.first_contact", "First touches at {angle}°"),
    ("hinge.swing.open", "Open"),
    (
        "hinge.too_tight",
        "{class} leaves {gap} mm between the moving parts, so the hinge would print fused. Pick a looser Fit class.",
    ),
    ("hinge.use_free_running", "Use Free running"),
    (
        "hint.first_use",
        "Pick a tool on the left: Transform (T) to move, rotate or scale",
    ),
    ("hint.first_use.dismiss", "Dismiss hint"),
    ("history.empty", "No history yet"),
    ("history.tab", "History"),
    ("history.tip", "Click a step to go back to it"),
    ("hole.auto", "Auto"),
    ("hole.flat_top", "Flat-top"),
    ("hole.round", "Round"),
    ("hole.teardrop", "Teardrop"),
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
    ("parts.apply", "Add \"{name}\""),
    ("parts.apply_join", "Join to scan"),
    ("parts.blend_width_help", "Blend width {unit}"),
    ("parts.click_help", "Click a part to add it to the model"),
    ("parts.join.blend", "Blend"),
    ("parts.join.keep_separate", "Keep separate"),
    ("parts.join.merge", "Merge"),
    ("parts.none_found", "No parts match your search"),
    (
        "parts.open_stamp",
        "Open part: it will be closed and sunk into the surface when merged.",
    ),
    ("parts.save_as_part", "Save as part…"),
    (
        "parts.save_help",
        "Write the active object to your parts library",
    ),
    (
        "parts.save_no_settings_dir",
        "No settings folder is available, so nothing was saved",
    ),
    ("parts.saved", "\"{name}\" saved to your library"),
    ("parts.search_hint", "Search parts"),
    ("parts.section.join", "Join to scan"),
    ("parts.section.library", "Library"),
    ("parts.section.save", "Save"),
    ("parts.shape_tip", "Adds an editable shape: select it to change its size and settings"),
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
    ("sculpt.apply", "Sculpt"),
    ("sculpt.brush.carve", "Carve"),
    ("sculpt.brush.crease", "Crease"),
    ("sculpt.brush.deflate", "Deflate"),
    ("sculpt.brush.drag", "Drag"),
    ("sculpt.brush.flatten", "Flatten"),
    ("sculpt.brush.inflate", "Inflate"),
    ("sculpt.brush.pinch", "Pinch"),
    ("sculpt.brush.smooth", "Smooth"),
    ("sculpt.detail", "Detail"),
    (
        "sculpt.detail.tip",
        "Split, collapse and flip triangles under the brush toward this edge length",
    ),
    ("sculpt.falloff.flat", "Flat"),
    ("sculpt.falloff.sharp", "Sharp"),
    ("sculpt.falloff.smooth", "Smooth"),
    ("sculpt.falloff.sphere", "Sphere"),
    (
        "sculpt.help",
        "Click and drag on the surface; release to end the stroke. Esc drops it.",
    ),
    ("sculpt.section.brush", "Brush"),
    ("sculpt.section.detail", "Detail"),
    ("sculpt.section.falloff", "Falloff"),
    ("sculpt.section.radius", "Radius ({unit})"),
    ("sculpt.section.strength", "Strength"),
    ("sculpt.section.symmetry", "Symmetry"),
    ("sculpt.symmetry.x", "X"),
    ("sculpt.symmetry.y", "Y"),
    ("sculpt.symmetry.z", "Z"),
    ("settings.dev", "Developer"),
    ("settings.dev.pseudo", "Pseudo-locale"),
    (
        "settings.dev.pseudo.tip",
        "Shows every catalogued string accented, 40% longer and in [[…]], so untranslated text and tight layouts stand out",
    ),
    ("settings.fit_classes", "Fit classes"),
    ("settings.fit_classes.clearance", "Clearance"),
    (
        "settings.fit_classes.help",
        "A fit class is the gap between mating parts your design wants (design intent), independent of any one printer's tolerance. Negative is interference (a press fit).",
    ),
    ("settings.fit_classes.new", "New fit class…"),
    ("settings.machine_profiles", "Machine profiles"),
    (
        "settings.machine_profiles.help",
        "A machine profile is how far your printer's own output deviates from the design (measured tolerance), not what gap your design wants. Defaults are \"not calibrated\" until you measure a calibration print.",
    ),
    ("settings.machine_profiles.new", "New machine profile…"),
    ("settings.print_volume", "Print volume"),
    ("settings.title", "Settings"),
    ("shape.attach", "Attach to {a} and {b}"),
    (
        "shape.attach.tip",
        "Join each part onto the piece it belongs to: solids joined, holes cut, as one undo step",
    ),
    ("shape.change", "Change \"{name}\""),
    ("shape.editable", "Editable shape · {name}"),
    (
        "shape.editable.tip",
        "Change its settings below and it's rebuilt, as one undo step. Combine, Sculpt, Split or Make solid bake it into a plain mesh.",
    ),
    (
        "shape.fit",
        "Holes are {total} mm wider than their nominal size (Fit class plus Machine profile)",
    ),
    ("shape.group", "Linked group of {n}: {names}. A change here rebuilds all of them."),
    ("shape.mesh", "Mesh"),
    (
        "shape.mesh.tip",
        "A plain mesh: sculpt, cut, combine or repair it. Its size can't be changed by parameters (a mesh operation baked it, or it was imported).",
    ),
    ("shape.reorient", "Re-aim hole profiles for the new placement"),
    ("shape.section", "Shape"),
    ("shape.sideways", "Hole {n} prints sideways and will sag at the top; switch to Teardrop"),
    ("shape.sideways.fix", "Switch to Teardrop"),
    ("shape.unknown", "This shape's generator ({id}) isn't available, so it can't be edited."),
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
    ("split.apply", "Cut"),
    (
        "split.box.centre",
        "Centre on selection",
    ),
    (
        "split.box.fits",
        "{name} already fits in this box, so there's nothing to cut. Make the box smaller or move it.",
    ),
    (
        "split.box.help",
        "Move the box with the arrows, or drag a face's dot to resize it. Its faces are where the model is cut.",
    ),
    (
        "split.box.misses",
        "The box doesn't reach {name}; move it onto the model.",
    ),
    ("split.box.plate", "Rest on the plate"),
    ("split.connect_link", "Add connectors to these joints ->"),
    ("split.cut.apply_volume", "Cut"),
    (
        "split.cut.joints_help",
        "Every cut face is capped flat, in one undo step. Each pair of cut faces becomes a joint the Connect tool can add connectors to.",
    ),
    (
        "split.cut.volume_summary.one",
        "Cuts the model into {n} piece along the box's faces",
    ),
    (
        "split.cut.volume_summary.other",
        "Cuts the model into {n} pieces along the box's faces",
    ),
    ("split.gap_help", "A slab this thick is removed at the cut, so the parts don't touch"),
    ("split.horizontal", "Horizontal"),
    ("split.keep.above", "Above"),
    ("split.keep.below", "Below"),
    ("split.keep.both", "Both"),
    (
        "split.misses_warning",
        "The plane doesn't cross {name}; drag it into the model",
    ),
    ("split.mode.slice", "Slice"),
    ("split.mode.volume", "Print volume"),
    ("split.move", "Move"),
    ("split.piece.inside", "In the box"),
    ("split.piece.outside", "Outside {face}"),
    ("split.piece.size", "{piece}: about {w} × {d} × {h}"),
    (
        "split.pieces.none",
        "No pieces: the box doesn't cut the model as placed.",
    ),
    (
        "split.plane_help",
        "Drag the gizmo in the view to move or tilt the plane, or pick a direction",
    ),
    ("split.preset.custom", "Custom"),
    (
        "split.refuse_open",
        "The model has holes; cutting needs a closed model. Fill the holes in Repair, then split.",
    ),
    (
        "split.result",
        "Both parts are closed with a flat cap: each selected object the plane crosses becomes \"(top)\" and a new \"(bottom)\" appears beside it.",
    ),
    ("split.section.cut", "What this cuts"),
    ("split.section.gap", "Gap at the cut ({unit})"),
    ("split.section.keep", "Keep"),
    ("split.section.mode", "Cut by"),
    ("split.section.pieces", "Pieces"),
    ("split.section.plane", "Cut plane"),
    ("split.section.point", "Point ({unit})"),
    ("split.section.position", "Box centre ({unit})"),
    ("split.section.printer", "Printer"),
    ("split.section.size", "Box size W × D × H ({unit})"),
    ("split.section.tilt", "Tilt"),
    ("split.section_view", "Section view"),
    (
        "split.section_view.tip",
        "See through the part above the plane and into the cut",
    ),
    ("split.status.cut.one", "Cut {name} into {n} piece"),
    ("split.status.cut.other", "Cut {name} into {n} pieces"),
    ("split.targets.one", "Cuts {n} selected object"),
    ("split.targets.other", "Cuts {n} selected objects"),
    ("split.the_model", "the model"),
    ("split.tilt", "Tilt"),
    ("split.vertical_x", "Vertical X"),
    ("split.vertical_y", "Vertical Y"),
    ("status.boolean.one", "{op} added {n} triangle"),
    ("status.boolean.other", "{op} added {n} triangles"),
    ("status.connector_join", "Joined with a {gap} {unit} gap"),
    ("status.delete_faces.one", "Deleted {n} triangle"),
    ("status.delete_faces.other", "Deleted {n} triangles"),
    ("status.part_added.one", "Added the part: {n} triangle"),
    ("status.part_added.other", "Added the part: {n} triangles"),
    ("status.placed_beside_plate", "The plate is full; placed beside it"),
    ("status.placed_on_plate", "Arranged on the plate"),
    ("status.ready", "Ready"),
    ("status.sculpt.one", "{brush} ({name}): {n} point moved"),
    ("status.sculpt.other", "{brush} ({name}): {n} points moved"),
    ("status.sculpt.remeshed.one", "{brush} ({name}): {n} point moved, {splits} edge split"),
    (
        "status.sculpt.remeshed.other",
        "{brush} ({name}): {n} points moved, {splits} edges split",
    ),
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
    (
        "tool.connect.help",
        "Add connectors (dowels, pegs, dovetails, magnets or inserts) to a cut joint",
    ),
    ("tool.connect.name", "Connect"),
    (
        "tool.connectors.help",
        "Add a connector (a hinge, clasp or thread) to a model",
    ),
    ("tool.connectors.name", "Connectors"),
    ("tool.export.help", "Write the model to 3MF, STL or OBJ"),
    ("tool.export.name", "Export"),
    ("tool.inspect.help", "Every number the analysis reports"),
    ("tool.inspect.name", "Inspect"),
    (
        "tool.make_solid.help",
        "Rebuild the surface as one closed solid",
    ),
    ("tool.make_solid.name", "Make solid"),
    ("tool.parts.help", "Add a library part to a model"),
    ("tool.parts.name", "Parts"),
    (
        "tool.repair.help",
        "Fill holes, remove loose pieces, mark openings",
    ),
    ("tool.repair.name", "Repair"),
    (
        "tool.sculpt.help",
        "Push, pull and smooth the surface with a brush",
    ),
    ("tool.sculpt.name", "Sculpt"),
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
    ("view.clay_objects", "Clay"),
    ("view.object_colors", "Colour objects"),
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
