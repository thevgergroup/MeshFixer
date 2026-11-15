#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Parts tool on the rail (Phase 1 Task 23, MF-MODEL-004): the
//! library, placing a part with `object.add_part`, Join to scan
//! (Keep separate / Merge / Blend) and Save as part.
//!
//! With `MF_ARTIFACTS` set, frames go to
//! `<dir>/design/parts-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

const WINDOW: (f32, f32) = (1440.0, 900.0);

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "timed out waiting for: {what} (status {:?}, error {:?}, refusal {:?})",
            h.state().status,
            h.state().error,
            h.state().refusal,
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "never settled");
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

fn still(h: &mut Harness<'_, MeshFixerApp>, scene: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let size = h.ctx.content_rect().size();
    let name = format!("parts-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    opened_with(objects, Settings::default())
}

/// As [`opened`], with its own settings: tests that save a part set
/// `config_dir` to a temp directory, so the real user's settings folder
/// is never touched.
fn opened_with(
    objects: Vec<(&str, Mesh, [f64; 12])>,
    settings: Settings,
) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Some(Unit::Millimeter),
        source: SourceInfo {
            path: "parts.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some() && a.picking_ready() && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed 20 mm ball.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), 10.0)
}

fn objects(h: &Harness<'_, MeshFixerApp>) -> Vec<String> {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

/// Opening the Parts tool, clicking a library part, places it as a new
/// object and opens Transform on it (Task 23's "drop it, place it" step,
/// clicking rather than a cursor-tracked drag).
#[test]
fn clicking_a_library_part_adds_it_and_opens_transform() {
    require_gpu!("clicking_a_library_part_adds_it_and_opens_transform");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Parts").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "parts");
    still(&mut h, "library");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Sphere").click();
    wait_until(&mut h, "the part placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let names = objects(&h);
    assert!(names.contains(&"Sphere".to_owned()), "{names:?}");
    assert_eq!(
        h.state().active_tool,
        "transform",
        "Transform opens on the new part"
    );
    still(&mut h, "placed");
}

/// Merge (Join to scan) runs `mesh.boolean` union between the placed
/// part and the object it landed on, removing the part as its own
/// object (one more undoable command on top of the placement).
#[test]
fn merge_joins_the_part_into_the_scan_as_a_second_command() {
    require_gpu!("merge_joins_the_part_into_the_scan_as_a_second_command");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Parts").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Sphere").click();
    wait_until(&mut h, "the part placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let after_place = h.state().session.history_labels().len();
    // The part lands tangent to the scan's surface (drop_point's own
    // placement); sink it in by a few mm first, the same as a user
    // dragging it down in Transform before merging, so the union has a
    // real overlap to work with rather than two spheres meeting at one
    // point.
    let part = h.state().active.unwrap();
    let mut t = h.state().session.document().object(part).unwrap().transform;
    t[11] -= 4.0;
    h.state_mut().commit_transform(part, t, "test setup".into());
    wait_until(&mut h, "the sink-in", |a| {
        a.busy.is_none() && a.session.history_labels().len() > after_place
    });
    settle(&mut h);
    let after_sink = h.state().session.history_labels().len();
    // Back to Parts (Transform opened on the new part): the Join-to-
    // scan section is keyed off app.parts.placed, which survives the
    // tool switch.
    h.get_by_label("Parts").click();
    settle(&mut h);
    h.get_by_label("Combine with scan").click();
    settle(&mut h);
    h.get_by_label("Join").click();
    wait_until(&mut h, "the merge", |a| {
        a.busy.is_none() && a.session.history_labels().len() > after_sink
    });
    settle(&mut h);
    let names = objects(&h);
    assert_eq!(
        names,
        vec!["scan".to_owned()],
        "the part is merged away: {names:?}"
    );
    still(&mut h, "merged");
}

/// With no model open there's nothing to place a part on: Parts is
/// greyed out on the rail (`needs_object`, the same rule every whole-
/// object tool uses), not an error, so it adds nothing rather than
/// placing a part with no target.
#[test]
fn with_no_model_open_parts_is_unavailable() {
    require_gpu!("with_no_model_open_parts_is_unavailable");
    let h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap()
        });
    let tool = h.state().shell.tool("parts").unwrap();
    assert!(matches!(
        (tool.availability)(h.state()),
        mf_desktop::shell::Availability::Unavailable(_)
    ));
}

/// Save as part opens a dialog for the part's name, category and
/// description, then writes the active object to the user library and
/// says so by name: `Settings::config_dir` points it at a temp directory
/// (never the real user's settings folder), and the save shows up as a
/// file on disk. The saved part can then be renamed and deleted from the
/// library grid; a built-in has neither button.
#[test]
fn save_as_part_names_what_was_saved() {
    require_gpu!("save_as_part_names_what_was_saved");
    let dir = tempfile::tempdir().unwrap();
    let mut h = opened_with(
        vec![("widget", ball(), at([0.0, 0.0, 10.0]))],
        Settings {
            config_dir: Some(dir.path().to_owned()),
            ..Settings::default()
        },
    );
    h.get_by_label("Parts").click();
    settle(&mut h);
    // The library grid (fixed 96x116 pt tiles: fix round 2026-11-12) can
    // run well past the window's bottom with this many built-ins, so
    // "Save as part…" below it isn't necessarily on screen: click it by
    // accesskit action, as the rename/delete buttons below already do.
    h.get_by_label("Save as part…").click_accesskit();
    settle(&mut h);
    // The dialog: the name filled in from the object, a category and a
    // description typed.
    assert_eq!(
        h.state().parts.dialog.as_ref().map(|d| d.name.as_str()),
        Some("widget")
    );
    h.get_by_label("Category").click();
    h.get_by_label("Category").type_text("Gadgets");
    h.get_by_label("Description").click();
    h.get_by_label("Description").type_text("A round widget");
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    let status = h.state().status.clone().unwrap_or_default();
    assert!(status.contains("widget"), "{status:?}");
    assert!(
        dir.path()
            .join("parts")
            .read_dir()
            .unwrap()
            .next()
            .is_some(),
        "the part was written under the temp settings dir"
    );
    let ctx = mf_runtime::RunContext::none();
    let (lib, _) = mf_parts::Library::load(Some(&dir.path().join("parts")), &ctx);
    let saved = lib.parts.iter().find(|p| !p.is_builtin()).unwrap();
    assert_eq!(saved.manifest.category, "Gadgets");
    assert_eq!(saved.manifest.description, "A round widget");
    let id = saved.manifest.id.clone();
    still(&mut h, "saved");

    // Filed under its category, with rename and delete; built-ins have
    // neither.
    settle(&mut h);
    h.get_by_label("GADGETS");
    assert!(h.query_by_label("Rename Sphere").is_none());
    assert!(h.query_by_label("Delete Sphere").is_none());
    h.get_by_label("Rename widget").click_accesskit();
    settle(&mut h);
    h.get_by_label("Name").click();
    h.key_press(egui::Key::End);
    h.get_by_label("Name").type_text(" mk2");
    settle(&mut h);
    h.get_by_label("Save changes").click();
    settle(&mut h);
    let (lib, _) = mf_parts::Library::load(Some(&dir.path().join("parts")), &ctx);
    assert_eq!(lib.get(&id).unwrap().manifest.name, "widget mk2");

    h.get_by_label("Delete widget mk2").click_accesskit();
    settle(&mut h);
    h.get_by_label("Delete").click_accesskit();
    settle(&mut h);
    let (lib, _) = mf_parts::Library::load(Some(&dir.path().join("parts")), &ctx);
    assert!(lib.get(&id).is_none(), "deleted");
    assert!(!dir.path().join("parts").join(&id).exists());
}

/// Clicking a library shape places an editable shape: it keeps its
/// recipe (the `generator` property), and a hole-like one is placed as
/// a Hole with the chosen fit recorded.
#[test]
fn clicking_a_shape_places_an_editable_shape() {
    require_gpu!("clicking_a_shape_places_an_editable_shape");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Parts").click();
    settle(&mut h);
    for (label, hole) in [("Box", false), ("Magnet pocket", true)] {
        let history = h.state().session.history_labels().len();
        h.get_by_label("Parts").click();
        settle(&mut h);
        h.get_by_label(label).click_accesskit();
        wait_until(&mut h, "the shape placed", |a| {
            a.busy.is_none() && a.session.history_labels().len() > history
        });
        settle(&mut h);
        let id = h.state().active.unwrap();
        let o = h.state().session.document().object(id).unwrap();
        assert_eq!(o.name, label);
        assert!(o.is_parametric(), "{label} keeps its recipe");
        assert_eq!(o.is_hole(), hole, "{label}");
        let record =
            mf_parts::GeneratorRecord::from_json(&o.properties[mf_command::GENERATOR]).unwrap();
        if hole {
            assert_eq!(record.params.get_text("fit_class").unwrap(), "Snug");
        }
    }
    still(&mut h, "shapes");
}

struct Wedge;

fn wedge_spec() -> &'static mf_spec::OperationSpec {
    static S: std::sync::OnceLock<mf_spec::OperationSpec> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        mf_parts::shape::spec_of(
            "plugin.wedge",
            "A plugin's wedge.",
            vec![mf_parts::shape::P::Mm("size", 8.0, 0.1, None, "Size")],
        )
    })
}

impl mf_parts::Generator for Wedge {
    fn id(&self) -> &'static str {
        "plugin.wedge"
    }
    fn name(&self) -> &'static str {
        "Plugin wedge"
    }
    fn version(&self) -> u32 {
        1
    }
    fn spec(&self) -> &'static mf_spec::OperationSpec {
        wedge_spec()
    }
    fn is_shape(&self) -> bool {
        true
    }
    fn generate(
        &self,
        params: &mf_spec::Params,
        _: &mf_parts::GenContext,
    ) -> Result<Vec<mf_parts::ShapePart>, mf_parts::ConnectorError> {
        let s = params.get_float("size").unwrap();
        Ok(vec![mf_parts::ShapePart::new(
            "shape",
            mf_parts::ShapeRole::Solid,
            mf_parts::TargetSlot::Neither,
            Mesh::new(
                vec![[0.0, 0.0, 0.0], [s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]],
                vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
            )
            .unwrap(),
        )])
    }
}

/// A shape a plugin registers through the shell appears in the Parts
/// library and places like a built-in.
#[test]
fn a_plugin_registered_shape_appears_in_the_library() {
    require_gpu!("a_plugin_registered_shape_appears_in_the_library");
    static WEDGE: Wedge = Wedge;
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    h.state().shell.register_generator(&WEDGE).unwrap();
    h.get_by_label("Parts").click();
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    // A plugin's shape is appended last, so it's past the built-ins and
    // likely below the fold with the library grid's fixed, taller tiles
    // (fix round 2026-11-12): click it by accesskit action.
    h.get_by_label("Plugin wedge").click_accesskit();
    wait_until(&mut h, "the wedge placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let id = h.state().active.unwrap();
    let o = h.state().session.document().object(id).unwrap();
    let record =
        mf_parts::GeneratorRecord::from_json(&o.properties[mf_command::GENERATOR]).unwrap();
    assert_eq!(record.id, "plugin.wedge");
    assert!(
        h.state().shell.register_generator(&WEDGE).is_err(),
        "a shape id registers once"
    );
}

/// The library is a grid of thumbnails: each part rendered offscreen in
/// Clay at 128 px, cached on disk by its content hash (so a second
/// session shows them without rendering again), and hovering one shows
/// it larger with what it is.
#[test]
fn the_library_is_a_thumbnail_grid_cached_by_content_hash() {
    require_gpu!("the_library_is_a_thumbnail_grid_cached_by_content_hash");
    let dir = tempfile::tempdir().unwrap();
    let settings = || Settings {
        config_dir: Some(dir.path().to_owned()),
        ..Settings::default()
    };
    let mut h = opened_with(vec![("scan", ball(), at([0.0, 0.0, 10.0]))], settings());
    h.get_by_label("Parts").click();
    settle(&mut h);
    wait_until(&mut h, "the thumbnails", |a| {
        h_ready(a) > 10 && !a.parts.thumbs.busy()
    });
    let cached: Vec<_> = dir
        .path()
        .join("thumbnails")
        .read_dir()
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .collect();
    assert!(cached.len() > 10, "{} pictures cached", cached.len());
    let png = image::open(cached[0].path()).unwrap();
    assert_eq!((png.width(), png.height()), (128, 128));
    // Hovering a part shows it larger, with what it is.
    h.get_by_label("Box").hover();
    for _ in 0..120 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.query_by_label_contains("rounded corners").is_some());
    drop(h);

    // A second session: every picture comes from the disk cache, so none
    // is queued for rendering.
    let mut h = opened_with(vec![("scan", ball(), at([0.0, 0.0, 10.0]))], settings());
    h.get_by_label("Parts").click();
    h.run_steps(3);
    assert!(!h.state().parts.thumbs.busy(), "nothing rendered again");
    assert!(h.state().parts.thumbs.ready() > 10);
}

fn h_ready(a: &MeshFixerApp) -> usize {
    a.parts.thumbs.ready()
}

/// The tool rail's own width, left of the tool dock (`tool_panels::RAIL_WIDTH`,
/// not public outside the crate): the space the dock's resize splitter
/// measures its fraction against is the window's width minus this.
const RAIL_WIDTH: f32 = 64.0;

/// Resizes the tool dock (the Parts panel's own container) to `width`
/// points by driving its resize splitter's accesskit `SetValue`, the
/// same action assistive technologies use — no pointer-drag geometry to
/// get right. The splitter reports its position as a fraction of the
/// space available to the panel (the window's width less the rail's own,
/// fixed, to its left); egui clamps the result to the panel's own
/// `size_range` (240..=420), same as a pointer drag would.
fn resize_tool_dock(h: &mut Harness<'_, MeshFixerApp>, width: f32) {
    let splitter = h
        .get_all_by_label("Resize panel")
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .expect("the tool dock's resize splitter");
    let fraction = f64::from(width / (WINDOW.0 - RAIL_WIDTH));
    splitter.set_value_accesskit(fraction);
    h.run_steps(3);
    settle(h);
}

/// Fix round 2026-11-12 (owner: "the Parts library grid looks
/// terrible"): every tile is a fixed 96x116 pt (thumbnail on top, name
/// centred below on one line), the grid never overflows the panel at
/// any width that fits, and rows are uniform. Checked at two panel
/// widths (280, the tool dock's default; 360, part way to its max of
/// 420) so the column count differs between them.
#[test]
fn library_tiles_are_uniform_and_never_overflow_the_panel() {
    require_gpu!("library_tiles_are_uniform_and_never_overflow_the_panel");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Parts").click();
    settle(&mut h);
    wait_until(&mut h, "the built-ins listed", |a| h_ready(a) > 10);

    for width in [280.0_f32, 360.0] {
        resize_tool_dock(&mut h, width);
        let splitter = h
            .get_all_by_label("Resize panel")
            .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
            .expect("the tool dock's resize splitter");
        let outer_width = splitter.rect().center().x - RAIL_WIDTH;
        assert!(
            (outer_width - width).abs() < 2.0,
            "the dock didn't settle near {width} pt wide: splitter at {:?} ({outer_width} pt)",
            splitter.rect()
        );
        // The panel's own content rect: the search field spans it full
        // width, so its rect's x-range is the grid's too.
        let panel = h.get_by_role(egui::accesskit::Role::TextInput).rect();
        still(&mut h, &format!("library-{}", width as u32));

        // Every tile is its own `GenericContainer` node ("Part tile: …",
        // `cell_ui`'s), covering the picture, name and (reserved either
        // way) actions row as one footprint — distinct from the
        // click-target `Button` inside it, which only covers the
        // picture and name.
        let tiles: Vec<egui::Rect> = h
            .get_all_by_label_contains("Part tile: ")
            .map(|n| n.rect())
            .collect();
        assert!(
            tiles.len() > 10,
            "{} tiles found at {width} pt",
            tiles.len()
        );
        let first = tiles[0];
        for &t in &tiles {
            assert!(
                (t.width() - first.width()).abs() < 0.5
                    && (t.height() - first.height()).abs() < 0.5,
                "tiles aren't uniform at {width} pt: {first:?} vs {t:?}"
            );
            // The fixed tile size the fix specifies (about 96x116 pt):
            // not the old cramped "image left of text" button, and not
            // grown to fit a rename/delete row only some tiles have.
            assert!(
                (t.height() - 116.0).abs() < 1.0,
                "tile isn't the fixed 116 pt tall: {t:?}"
            );
            assert!(
                t.left() >= panel.left() - 0.5 && t.right() <= panel.right() + 0.5,
                "a tile overflows the panel at {width} pt: {t:?} outside {panel:?}"
            );
        }
        // Names are single-line: every cell's own click-target button
        // (picture + name, `THUMB + NAME_HEIGHT` tall; the reserved
        // actions row is outside it) is the same height whatever its
        // name's length — "Magnet pocket" doesn't wrap to a second line
        // and grow past "Box"'s.
        let name_heights: Vec<f32> = h
            .get_all_by_role(egui::accesskit::Role::Button)
            .map(|n| n.rect())
            .filter(|r| {
                r.top() >= panel.bottom() - 1.0
                    && r.left() >= panel.left() - 1.0
                    && r.right() <= panel.right() + 1.0
                    && r.width() > 50.0
                    && r.width() < 110.0
            })
            .map(|r| r.height())
            .collect();
        assert!(
            name_heights.len() > 10,
            "{} tile buttons found at {width} pt",
            name_heights.len()
        );
        for &nh in &name_heights {
            assert!(
                (nh - name_heights[0]).abs() < 0.5,
                "a tile's name wrapped to a second line at {width} pt: {name_heights:?}"
            );
        }

        // Every tile's name is centred on the tile, not on its left
        // edge (owner screenshot, "ox", "nder", "ere", "rus": a label
        // drawn centre-aligned at `name_rect.left()` puts the text's own
        // centre there instead of at the tile's middle, clipping its
        // left half off at the tile's own left edge — fix round
        // 2026-11-13, `name_galley`). Checked for every built-in's own
        // click-target button (picture + name; the same set
        // `name_heights` just measured by rect, re-found here by name so
        // each can be matched to its own cell).
        for name in ["Box", "Sphere", "Magnet pocket"] {
            let button = h.get_by_label(name).rect();
            let name_rect = mf_desktop::parts_tool::name_rect_in_tile_button(button);
            // The real function `cell_ui` paints with, not a
            // reimplementation: a regression in `cell_ui`'s own call
            // (or in `name_galley` itself) shows up here too.
            let painter = egui::Painter::new(h.ctx.clone(), egui::LayerId::background(), name_rect);
            let (galley, text_pos) = mf_desktop::parts_tool::name_galley(&painter, name, name_rect);
            let label_rect = egui::Rect::from_min_size(text_pos, galley.size());
            assert!(
                name_rect.left() - 0.5 <= label_rect.left()
                    && label_rect.right() <= name_rect.right() + 0.5,
                "{name:?}'s label ({label_rect:?}) at {width} pt doesn't lie inside its tile's \
                 name rect ({name_rect:?})"
            );
            let label_mid = (label_rect.left() + label_rect.right()) / 2.0;
            let tile_mid = (name_rect.left() + name_rect.right()) / 2.0;
            assert!(
                (label_mid - tile_mid).abs() <= 1.0,
                "{name:?}'s label isn't centred on its tile at {width} pt: label mid \
                 {label_mid}, tile mid {tile_mid}"
            );
            // None of these three names are long enough to need eliding
            // at either panel width this test checks (96 pt tiles, a
            // short label font): the galley shows every character, not
            // an ellipsis in its place.
            assert!(
                !galley.elided,
                "{name:?} was elided at {width} pt though it should fit: {label_rect:?} in \
                 {name_rect:?}"
            );
        }
    }
}
