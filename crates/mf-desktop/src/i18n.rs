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

/// True if `id` is in the catalogue either as a plain message or as a
/// plural pair (`<id>.one` present; [`tr_plural`] picks `.one` or
/// `.other` from a count). A spec's `history_label` is one or the other,
/// depending on whether its history row needs a count.
pub fn has_message_or_plural(id: &str) -> bool {
    has(id) || has(&format!("{id}.one"))
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
        "align.anchor_hint",
        "Click one of the selected objects to anchor it: it won't move, the others align to it",
    ),
    ("align.anchor_set", "Anchored to {name}: it won't move"),
    ("align.centre", "Centre"),
    (
        "align.distribute_hint",
        "Spaces the objects between the two outermost ones evenly along the axis",
    ),
    (
        "align.distribute_needs_three",
        "Select 3 or more objects to distribute",
    ),
    (
        "align.dots_hint",
        "Click a dot to align every selected object to it",
    ),
    ("align.equal_centres", "Equal centres"),
    ("align.equal_gaps", "Equal gaps"),
    ("align.hover.centre", "{axis} centre"),
    ("align.hover.max", "{axis} max: align {edge} edges"),
    ("align.hover.min", "{axis} min: align {edge} edges"),
    ("align.max", "Max"),
    ("align.min", "Min"),
    ("align.section.align", "Align"),
    ("align.section.distribute", "Distribute"),
    ("align.select_more", "Select 2 or more objects to align"),
    (
        "align.what",
        "Dots mark each axis's min, centre and max; click one to align every selected object to it",
    ),
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
    ("connect.dovetail_shortcut", "Dovetail"),
    (
        "connect.dovetail_shortcut.tip",
        "Drop a linked dovetail pair (a tail and its groove) onto this joint, aligned to the cap plane and pointing to the nearest edge. Move, rotate and resize it like any other shape, then Attach.",
    ),
    ("connect.fit.class", "Fit: {name}"),
    ("connect.fit.machine", "Printer: {name}"),
    (
        "connect.fit.unreadable",
        "Your Fit classes or Machine profiles couldn't be read ({reason}); using the built-ins.",
    ),
    ("connect.fix.failed", "Couldn't fix it: {reason}"),
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
    ("connect.joints.open_split", "Open Split"),
    (
        "connect.joints.pick",
        "Pick a joint to add connectors to it.",
    ),
    ("connect.min_wall", "Thinnest wall"),
    ("connect.param.angle", "Direction"),
    (
        "connect.place_help",
        "Click on the cut face of {name} to place a connector; drag one to move it, its rim to resize it and the square to change its depth. Delete removes the picked one.",
    ),
    ("connect.placed.none", "None yet: click on the cut face."),
    ("connect.placed.row_ok", "{n}. {kind}"),
    ("connect.placed.row_remove", "Remove connector {n}"),
    (
        "connect.placed.row_warn.one",
        "{index}. {kind}: {n} warning",
    ),
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
    ("connect.shapes_label", "Add connectors as editable shapes"),
    (
        "connect.sideways",
        "Hole {n} prints sideways and will sag at the top; switch to Teardrop",
    ),
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
    (
        "connectors.uncalibrated_help",
        "This machine profile has never been calibrated, so no tolerance is compensated; the Machine profiles settings page offers a calibration print.",
    ),
    ("ctx.fill", "Fill {name}"),
    ("ctx.frame", "Frame {name}"),
    ("ctx.mark", "Mark {name} as intentional"),
    ("ctx.remove", "Remove {name}"),
    ("ctx.rename", "Rename…"),
    ("ctx.select_faces", "Select the faces at {name}"),
    ("ctx.unmark", "Unmark {name}"),
    (
        "dock.collapsed_tool",
        "Show the {tool} panel (hides the Browser on a small window)",
    ),
    (
        "dovetail.ends_inside",
        "Ends inside the piece; it can't slide in",
    ),
    ("dovetail.ends_inside.fix", "Extend through the edge"),
    ("dovetail.name", "Dovetail"),
    ("dovetail.place", "Place dovetail"),
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
    ("gizmo.snap", "Snap: {step}"),
    (
        "gizmo.snap.tip",
        "To a step sized to stay visible at this zoom (15° steps and 10% scale steps too, in Transform). Hold Cmd/Ctrl while dragging to invert.",
    ),
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
    ("hinge.position", "Position along the edge"),
    (
        "hinge.position.help",
        "Centred on the edge's own middle by default; move it to place a second hinge clear of the first.",
    ),
    ("hinge.same_object", "Pick two different objects."),
    ("hinge.section", "Hinge between two objects"),
    ("hinge.shared_edge", "Shared edge ({edge})"),
    ("hinge.swing", "Swing preview"),
    ("hinge.swing.checking", "Checking the swing…"),
    (
        "hinge.swing.failed",
        "The swing check stopped unexpectedly ({why}). Save the command log and report it.",
    ),
    (
        "hinge.swing.clear",
        "Opens the whole {limit}° without touching anything",
    ),
    (
        "hinge.swing.colliding",
        "Colliding here in {n} place(s), highlighted in the view",
    ),
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
    ("history.label.connector.join", "Joined connector to {name}"),
    (
        "history.label.document.reinterpret_units",
        "Reinterpreted {name}'s units",
    ),
    (
        "history.label.document.scale_units",
        "Scaled {name} to new units",
    ),
    // A generator's own spec (a shape or connector's parameter schema,
    // crate mf-parts): never shown by this id. The command recorded is
    // always object.add_shape, object.set_shape_params or
    // connector.join, whose own history_label names the row; this
    // placeholder only satisfies the catalogue-coverage test.
    ("history.label.generator", "Updated {name}"),
    ("history.label.mesh.boolean", "{op} on {name}"),
    ("history.label.mesh.combine", "Combined objects into {name}"),
    ("history.label.mesh.compact", "Compacted {name}"),
    (
        "history.label.mesh.delete_faces.one",
        "Deleted {n} face of {name}",
    ),
    (
        "history.label.mesh.delete_faces.other",
        "Deleted {n} faces of {name}",
    ),
    (
        "history.label.mesh.fill_holes.one",
        "Filled {n} hole in {name}",
    ),
    (
        "history.label.mesh.fill_holes.other",
        "Filled {n} holes in {name}",
    ),
    ("history.label.mesh.hollow", "Hollowed {name}: {wall} walls"),
    (
        "history.label.mesh.hollow.open",
        "Hollowed {name}: {wall} walls, open at the cut face",
    ),
    ("history.label.mesh.inspect", "Inspected {name}"),
    (
        "history.label.mesh.intersections",
        "Checked {name} for intersections",
    ),
    ("history.label.mesh.make_solid", "Made {name} solid"),
    (
        "history.label.mesh.remove_small_components.one",
        "Removed {n} loose piece from {name}",
    ),
    (
        "history.label.mesh.remove_small_components.other",
        "Removed {n} loose pieces from {name}",
    ),
    (
        "history.label.mesh.sculpt_stroke",
        "Sculpted {name} ({brush})",
    ),
    (
        "history.label.mesh.split_box",
        "Split {name} along the build volume",
    ),
    (
        "history.label.mesh.split_plane",
        "Split {name} along a plane",
    ),
    (
        "history.label.mesh.split_region",
        "Split the selected faces off {name}",
    ),
    ("history.label.object.add_part", "Added a part to {name}"),
    (
        "history.label.object.add_shape.one",
        "Added a shape to {name}",
    ),
    (
        "history.label.object.add_shape.other",
        "Added {n} shapes to {name}",
    ),
    ("history.label.object.align.one", "Aligned {n} object"),
    ("history.label.object.align.other", "Aligned {n} objects"),
    (
        "history.label.object.arrange_on_plate",
        "Arranged {name} on the plate",
    ),
    (
        "history.label.object.attach.one",
        "Attached {n} part to {name}",
    ),
    (
        "history.label.object.attach.other",
        "Attached {n} parts to {name}",
    ),
    ("history.label.object.delete", "Deleted {name}"),
    (
        "history.label.object.drop_to_plate",
        "Dropped {name} to the plate",
    ),
    ("history.label.object.move_group", "Moved {name}'s group"),
    ("history.label.object.rename", "Renamed {name}"),
    ("history.label.object.set_shape_params", "Rebuilt {name}"),
    ("history.label.object.set_transform", "Transformed {name}"),
    (
        "history.label.split.place_connectors",
        "Added connectors to {name}'s joint",
    ),
    (
        "history.label.test.consume_tool",
        "Test: consumed a tool into {name}",
    ),
    (
        "history.label.test.unit_conversion",
        "Test: unit conversion on {name}",
    ),
    (
        "history.label.test_fixture",
        "Placeholder: never shown (a unit test's inline spec fixture)",
    ),
    ("history.tab", "History"),
    ("history.tip", "Click a step to go back to it"),
    ("hole.auto", "Auto"),
    ("hole.flat_top", "Flat-top"),
    ("hole.round", "Round"),
    ("hole.teardrop", "Teardrop"),
    ("hollow.apply", "Hollow"),
    ("hollow.detail", "Cell"),
    ("hollow.detail_auto", "Automatic"),
    (
        "hollow.detail_help",
        "The size of the grid the inside is built on: smaller follows the wall more closely but takes longer. It can be at most half the wall.",
    ),
    ("hollow.detail_is", "Built on a {cell} grid"),
    (
        "hollow.done",
        "Hollowed. Every part was thick enough to hollow.",
    ),
    ("hollow.holes.auto", "Auto"),
    (
        "hollow.holes.auto_help",
        "At the lowest points of the inside, as the object sits on the plate.",
    ),
    ("hollow.holes.clear", "Clear holes"),
    ("hollow.holes.count", "How many"),
    ("hollow.holes.diameter", "Diameter"),
    ("hollow.holes.none", "None"),
    ("hollow.holes.placed", "Placed"),
    ("hollow.holes.placed_count", "Holes placed: {n}"),
    (
        "hollow.holes.placed_help",
        "Click the surface where each hole goes.",
    ),
    ("hollow.holes.profile", "Shape"),
    (
        "hollow.holes_help",
        "Holes from the hollow inside to the outside, so resin or powder can drain out.",
    ),
    (
        "hollow.holes_hint",
        "Or place a cylinder from Parts, mark it as a Hole, and Combine for a custom opening.",
    ),
    ("hollow.measuring", "Measuring the object…"),
    ("hollow.open_through", "Open through this surface"),
    (
        "hollow.open_through_help",
        "Opens the cavity out through the picked surface, with a clean rim wall, instead of a closed shell there.",
    ),
    (
        "hollow.open_through_needs_pick",
        "Click a surface on the object to pick where it opens.",
    ),
    ("hollow.open_through_picked", "Opens through: {name}"),
    (
        "hollow.preview_ready",
        "Inner wall: {faces} faces, on a {cell} grid",
    ),
    ("hollow.preview_refused", "This can't be hollowed yet"),
    ("hollow.previewing", "Working out the walls…"),
    ("hollow.section.detail", "Detail"),
    ("hollow.section.holes", "Drain holes"),
    ("hollow.section.open_through", "Open through"),
    ("hollow.section.preview", "Preview"),
    ("hollow.section.wall", "Wall thickness"),
    ("hollow.section_view", "Section view"),
    (
        "hollow.section_view.tip",
        "Cut the preview open so you can see the walls",
    ),
    ("hollow.status", "Hollowed {name}: {wall} {unit} walls"),
    (
        "hollow.status_open",
        "Hollowed {name}: {wall} {unit} walls, open at the cut face",
    ),
    ("hollow.thin", "Parts thinner than {size} were left solid"),
    ("hollow.thin_share", "{percent}% of the surface (outlined)"),
    (
        "hollow.tradeoff",
        "Thinner walls save more material but are weaker.",
    ),
    ("hollow.wall", "Wall"),
    (
        "hollow.wall_help",
        "How thick the walls are; parts thinner than twice this stay solid",
    ),
    (
        "hollow.what",
        "Makes the object a shell: the outside stays exactly as it is, and the inside is hollowed out to walls of the thickness you choose.",
    ),
    (
        "inspect.help",
        "Everything the analysis measured, in plain numbers",
    ),
    ("inspect.section.counts", "Mesh"),
    ("inspect.section.print", "Printability"),
    ("inspect.section.units", "Units"),
    ("inspect.section.warnings", "Import warnings"),
    (
        "inspect.units.help",
        "The file's numbers were read as {unit}. If that's wrong, say what they were actually in:",
    ),
    ("inspect.units.reinterpret", "Reinterpret"),
    (
        "inspect.units.reinterpret.help",
        "Same numbers, new unit: the model's physical size changes, nothing else does.",
    ),
    ("inspect.units.scale", "Scale"),
    (
        "inspect.units.scale.help",
        "Same unit, new physical size: every object is scaled by the ratio between the two units.",
    ),
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
    ("parts.blend_width_help", "Blend width"),
    ("parts.cancel", "Cancel"),
    ("parts.click_help", "Click a part to add it to the model"),
    ("parts.delete", "Delete"),
    (
        "parts.delete_confirm",
        "Delete \"{name}\" from your library? This can't be undone.",
    ),
    ("parts.delete_named", "Delete {name}"),
    ("parts.deleted", "\"{name}\" deleted from your library"),
    ("parts.dialog.apply", "Save changes"),
    ("parts.dialog.category", "Category"),
    ("parts.dialog.description", "Description"),
    ("parts.dialog.edit_title", "Edit part"),
    ("parts.dialog.name", "Name"),
    ("parts.dialog.save", "Save"),
    ("parts.dialog.save_title", "Save as part"),
    (
        "parts.hole_tip",
        "Placed as a hole: it cuts whatever it's combined with",
    ),
    ("parts.join.apply", "Join"),
    ("parts.join.blend", "Blend"),
    ("parts.join.blend_into", "Blend into {target}"),
    ("parts.join.combine", "Combine with {target}"),
    ("parts.join.keep_separate", "Keep separate"),
    ("parts.none_found", "No parts match your search"),
    (
        "parts.open_stamp",
        "Open part: it will be closed and sunk into the surface when merged.",
    ),
    ("parts.rename_named", "Rename {name}"),
    ("parts.renamed", "\"{name}\" updated"),
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
    ("parts.section.yours", "Your parts"),
    (
        "parts.shape_tip",
        "Adds an editable shape: select it to change its size and settings",
    ),
    (
        "refusal.blend_did_not_converge",
        "The blend couldn't be smoothed ({detail})",
    ),
    (
        "refusal.blend_needs_union",
        "Only a union can be blended ({detail})",
    ),
    (
        "refusal.blend_self_intersects",
        "The blend folds over itself ({detail})",
    ),
    (
        "refusal.blend_too_narrow",
        "The blend is narrower than the faces at the seam ({detail})",
    ),
    (
        "refusal.blend_too_wide",
        "The blend is wider than the model ({detail})",
    ),
    ("refusal.cell_too_coarse", "{detail}"),
    ("refusal.dowel_too_short", "{detail}"),
    ("refusal.dowel_too_thin", "{detail}"),
    ("refusal.empty", "{detail}"),
    ("refusal.empty_object", "'{name}' has no geometry to align"),
    ("refusal.generator_version", "{detail}"),
    ("refusal.grid_too_large", "{detail}"),
    ("refusal.hinge_too_short", "{detail}"),
    ("refusal.hole_misses_cavity", "{detail}"),
    ("refusal.hole_misses_outside", "{detail}"),
    ("refusal.hole_point_invalid", "{detail}"),
    ("refusal.hollow_invalid_parameter", "{detail}"),
    ("refusal.hollow_singular_transform", "{detail}"),
    ("refusal.inconsistent_input", "{detail}"),
    ("refusal.inside_out_input", "{detail}"),
    ("refusal.invalid_plane", "{detail}"),
    (
        "refusal.kernel_internal",
        "The boolean engine failed on this input ({detail})",
    ),
    ("refusal.leaf_failed", "{detail}"),
    ("refusal.leaf_too_thick", "{detail}"),
    ("refusal.make_solid", "Run Make solid first"),
    ("refusal.make_solid_invalid_parameter", "{detail}"),
    ("refusal.missing_generator", "{detail}"),
    ("refusal.move_plane", "Move the plane"),
    (
        "refusal.moving_parts_fused",
        "'{a}' and '{b}' would print fused ({measured} apart where they meet, at least \
         {expected} expected); move the parts apart or pick a looser Fit class",
    ),
    ("refusal.no_hole_points", "{detail}"),
    ("refusal.no_overlap", "These objects don't touch ({detail})"),
    ("refusal.non_manifold_input", "{detail}"),
    (
        "refusal.nothing_to_combine",
        "'{object}' is the only object: there is nothing to join or cut. Select more objects.",
    ),
    ("refusal.open_input", "{detail}"),
    ("refusal.open_through_cuts_around", "{detail}"),
    ("refusal.open_through_needs_faces", "{detail}"),
    ("refusal.open_through_nothing_under", "{detail}"),
    ("refusal.open_through_region_degenerate", "{detail}"),
    ("refusal.open_through_region_invalid", "{detail}"),
    ("refusal.open_through_region_too_curved", "{detail}"),
    ("refusal.open_through_region_too_small", "{detail}"),
    (
        "refusal.opening_not_covered",
        "The part doesn't cover the opening it was joined over ({detail})",
    ),
    (
        "refusal.opening_too_large_for_join",
        "An opening near the part is too large to close for the join ({detail})",
    ),
    (
        "refusal.operand_not_closed",
        "The model has holes; cutting needs a closed model ({detail})",
    ),
    (
        "refusal.operand_overlaps_itself",
        "This part of the model overlaps itself near the tool ({detail})",
    ),
    ("refusal.pin_too_wide", "{detail}"),
    ("refusal.plane_misses_object", "{detail}"),
    ("refusal.region_not_supported", "{detail}"),
    ("refusal.region_not_supported_make_solid", "{detail}"),
    ("refusal.repair", "Repair holes first"),
    ("refusal.result_empty", "Nothing would be left ({detail})"),
    ("refusal.result_empty_make_solid", "{detail}"),
    (
        "refusal.result_not_manifold",
        "The objects only touch along an edge or at a point, so the result wouldn't be a clean \
         solid ({detail})",
    ),
    ("refusal.result_not_valid", "{detail}"),
    (
        "refusal.result_self_intersects",
        "The result would intersect itself ({detail})",
    ),
    (
        "refusal.section_self_intersects",
        "The model folds onto itself where the tool's working area cuts it ({detail})",
    ),
    ("refusal.section_too_thin", "{detail}"),
    ("refusal.split_invalid_mesh", "{detail}"),
    ("refusal.split_operand_not_closed", "{detail}"),
    ("refusal.split_region_crosses_bound", "{detail}"),
    ("refusal.split_section_degenerate", "{detail}"),
    ("refusal.split_section_self_intersects", "{detail}"),
    (
        "refusal.stamp_not_embedded",
        "The part's open side isn't inside the model ({detail})",
    ),
    ("refusal.title", "This can't be done yet"),
    (
        "refusal.too_few_knuckles",
        "A hinge needs at least 2 knuckles, one for each leaf.",
    ),
    (
        "refusal.too_few_objects",
        "Align needs at least two objects",
    ),
    ("refusal.too_thin", "{detail}"),
    (
        "refusal.tool_not_closed",
        "The tool isn't a closed solid ({detail})",
    ),
    ("refusal.undo", "Undo the last step"),
    ("refusal.unknown_generator", "{detail}"),
    ("refusal.unknown_part", "{detail}"),
    ("refusal.use_larger_cell", "Use a larger cell size"),
    (
        "refusal.window_degenerate",
        "The model's surface near the tool is too thin to cut cleanly ({detail})",
    ),
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
    ("sculpt.radius_size", "Brush size"),
    ("sculpt.section.brush", "Brush"),
    ("sculpt.section.detail", "Detail"),
    ("sculpt.section.falloff", "Falloff"),
    ("sculpt.section.overlay", "Overlay"),
    ("sculpt.section.radius", "Radius"),
    ("sculpt.section.radius.approx", "≈ {size} at this zoom"),
    ("sculpt.section.strength", "Strength"),
    ("sculpt.section.symmetry", "Symmetry"),
    ("sculpt.show_changes", "Show sculpt changes"),
    (
        "sculpt.show_changes.tip",
        "Tint vertices by how far they've moved since this tool opened",
    ),
    ("sculpt.stay_on_surface", "Stay on this surface"),
    (
        "sculpt.stay_on_surface.tip",
        "Affect only faces connected to the one under the cursor, without crossing a sharp \
         crease or spilling onto another part (like a split cap's flat face)",
    ),
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
    (
        "shape.group",
        "Linked group of {n}: {names}. A change here rebuilds all of them.",
    ),
    ("shape.mesh", "Mesh"),
    (
        "shape.mesh.tip",
        "A plain mesh: sculpt, cut, combine or repair it. Its size can't be changed by parameters (a mesh operation baked it, or it was imported).",
    ),
    (
        "shape.reorient",
        "Re-aim hole profiles for the new placement",
    ),
    ("shape.section", "Shape"),
    (
        "shape.sideways",
        "Hole {n} prints sideways and will sag at the top; switch to Teardrop",
    ),
    ("shape.sideways.fix", "Switch to Teardrop"),
    (
        "shape.unknown",
        "This shape's generator ({id}) isn't available, so it can't be edited.",
    ),
    ("solid.apply", "Make it solid"),
    ("solid.cell", "Cell"),
    ("solid.offset", "Grow or shrink by"),
    (
        "solid.offset_help",
        "Grow (positive) or shrink (negative) the surface",
    ),
    ("solid.section.cell", "Cell size"),
    ("solid.section.offset", "Offset"),
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
        "Detail smaller than {cell} is lost; smaller cells keep more but take longer and make more triangles.",
    ),
    (
        "solid.what",
        "Rebuilds the surface as one closed solid, closing holes and removing overlaps.",
    ),
    ("split.apply", "Cut"),
    ("split.bound", "Limit to area"),
    ("split.bound.diameter", "Diameter"),
    ("split.bound.disc", "Disc"),
    ("split.bound.height", "Height"),
    ("split.bound.rect", "Rectangle"),
    ("split.bound.width", "Width"),
    (
        "split.bound_help",
        "Only material inside the shown area is cut. A region crossing its edge is refused.",
    ),
    ("split.box.centre", "Centre on selection"),
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
    ("split.cap", "Cap the cut"),
    (
        "split.cap_help",
        "Close each kept part with a flat cap where it was cut. Turn off to leave it open, for a vase or bowl",
    ),
    ("split.connect_link", "Add connectors to these joints ->"),
    ("split.cut.apply_volume", "Cut"),
    (
        "split.cut.joints_help",
        "Every cut face is capped flat, in one undo step. Each pair of cut faces becomes a joint the Connect tool can add connectors to.",
    ),
    ("split.cut.names", "Cuts: {names}"),
    (
        "split.cut.volume_summary.one",
        "Cuts the model into {n} piece along the box's faces",
    ),
    (
        "split.cut.volume_summary.other",
        "Cuts the model into {n} pieces along the box's faces",
    ),
    ("split.gap", "Gap"),
    (
        "split.gap_help",
        "A slab this thick is removed at the cut, so the parts don't touch",
    ),
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
    ("split.not_cut.names", "Not cut (not selected): {names}"),
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
    // `{area}` already carries its own unit and `²` (`mf_units::format_area`,
    // consolidation plan C11: this used to be a bare `{:.0}` of a
    // document-units² value next to the unit's *symbol*, so a small
    // region on a metres document rounded to "0 m²").
    ("split.region.area", "{area}"),
    ("split.regions", "Cross-section regions"),
    (
        "split.regions_help",
        "Each separate piece the plane would cut. Untick one to leave it whole.",
    ),
    (
        "split.regions_multi_object",
        "Picking regions needs a single selected object.",
    ),
    (
        "split.result",
        "Both parts are closed with a flat cap: each selected object the plane crosses becomes \"(top)\" and a new \"(bottom)\" appears beside it.",
    ),
    ("split.section.cut", "What this cuts"),
    ("split.section.gap", "Gap at the cut"),
    ("split.section.keep", "Keep"),
    ("split.section.mode", "Cut by"),
    ("split.section.pieces", "Pieces"),
    ("split.section.plane", "Cut plane"),
    ("split.section.point", "Point"),
    ("split.section.position", "Box centre"),
    ("split.section.printer", "Printer"),
    ("split.section.size", "Box size W × D × H"),
    ("split.section.tilt", "Tilt"),
    ("split.section_view", "Section view"),
    (
        "split.section_view.tip",
        "See through the part above the plane and into the cut",
    ),
    ("split.side.a", "Side A"),
    ("split.side.b", "Side B"),
    ("split.side.back", "Back"),
    ("split.side.bottom", "Bottom"),
    ("split.side.front", "Front"),
    ("split.side.left", "Left"),
    ("split.side.right", "Right"),
    ("split.side.top", "Top"),
    ("split.status.cut.one", "Cut {name} into {n} piece"),
    ("split.status.cut.other", "Cut {name} into {n} pieces"),
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
    (
        "status.placed_beside_plate",
        "The plate is full; placed beside it",
    ),
    ("status.placed_on_plate", "Arranged on the plate"),
    ("status.ready", "Ready"),
    ("status.sculpt.one", "{brush} ({name}): {n} point moved"),
    ("status.sculpt.other", "{brush} ({name}): {n} points moved"),
    (
        "status.sculpt.remeshed.one",
        "{brush} ({name}): {n} point moved, {splits} edge split",
    ),
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
        "tool.align.help",
        "Line up or evenly space the selected objects along an axis",
    ),
    ("tool.align.name", "Align"),
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
    (
        "tool.hollow.help",
        "Make a solid a shell with walls of a chosen thickness",
    ),
    ("tool.hollow.name", "Hollow"),
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
