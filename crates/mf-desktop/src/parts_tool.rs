//! The Parts tool on the rail (Phase 1 Task 23, MF-MODEL-004), registered
//! through `shell::Tool` like Split, Combine and Make solid
//! (`shape_tools.rs`).
//!
//! - **The library.** The parametric shapes (`mf_parts::shapes`: the
//!   built-ins and any a plugin registered through the shell), then the
//!   user's own parts and the open stamps (`mf_parts::Library`, D16),
//!   listed with a search box.
//! - **Placing a shape** adds it as an editable shape (`object.add_shape`,
//!   one undo step): it keeps its recipe, so selecting it shows its
//!   params in the panel. A hole-like shape is placed as a Hole, with the
//!   Fit class and Machine profile chosen in Connectors.
//! - **Placing.** Clicking a part adds it to the model on the active
//!   object's own highest point, its attach normal pointing straight up
//!   (world +Z): `object.add_part`, one undo step. The new part becomes
//!   the active object so Transform (T) is one key away.
//! - **Join to scan**, on a placed part: Keep separate (the default,
//!   nothing further to do), Combine with <the object it was placed on>
//!   (`mesh.combine`: a solid part unioned into it, a hole part cut from
//!   it, the part's own object removed — one object left, one undo step)
//!   or Blend (`mesh.boolean`'s union with a blend width).
//! - **The grid.** Every part is a thumbnail (`crate::part_thumbs`: an
//!   offscreen 128 px Clay render, cached by its content hash) with its
//!   name; hovering one shows it larger with its category and
//!   description. The user's own parts are filed under their categories.
//! - **Save as part…** on any object opens a dialog for its name,
//!   category and description, then writes it to the user library
//!   (`mf_parts::save_object_as_part`). A user part can be renamed (the
//!   same dialog) or deleted; a built-in can't.

use std::collections::BTreeMap;

use eframe::egui;
use mf_command::ObjectId;
use mf_parts::{Attach, Library, PartKind, SurfaceFrame};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, RegisterError, Shell, Tool as T};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const PARTS: &str = "parts";

/// How a placed part joins the object it sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    KeepSeparate,
    /// `mesh.combine` into the object it was placed on.
    Combine,
    Blend,
}

/// The Parts tool's state: the search filter, which part is highlighted
/// in the library, and the join settings for the part last placed.
#[derive(Debug, Clone)]
pub struct PartsState {
    pub search: String,
    /// The save / edit dialog, while open.
    pub dialog: Option<PartDialog>,
    /// A user part waiting for its delete to be confirmed.
    pub confirm_delete: Option<String>,
    pub thumbs: crate::part_thumbs::Thumbs,
    /// The part most recently placed (so its Join-to-scan section can
    /// offer to act between it and the object it landed on).
    pub placed: Option<PlacedPart>,
    pub join: JoinMode,
    pub blend_width: f64,
}

impl Default for PartsState {
    fn default() -> PartsState {
        PartsState {
            search: String::new(),
            dialog: None,
            confirm_delete: None,
            thumbs: Default::default(),
            placed: None,
            join: JoinMode::KeepSeparate,
            blend_width: 2.0,
        }
    }
}

/// The save-part dialog (saving an object) or the edit dialog (renaming
/// a user part): the fields as typed.
#[derive(Debug, Clone, PartialEq)]
pub struct PartDialog {
    pub target: DialogTarget,
    pub name: String,
    pub category: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DialogTarget {
    /// Save this object as a new user part.
    Save(ObjectId),
    /// Rename the user part with this id.
    Edit(String),
}

/// A part just placed: its own object and the object it was placed on
/// (Join to scan acts between the two).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedPart {
    pub part: ObjectId,
    pub scan: ObjectId,
}

fn needs_object(app: &MeshFixerApp) -> Availability {
    if app.active_mesh().is_some() {
        Availability::Available
    } else {
        Availability::Unavailable(crate::panels::empty_state())
    }
}

pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_tool(T {
        help: "tool.parts.help",
        availability: needs_object,
        panel: Some(panel),
        on_open: |app| app.refusal = None,
        ..T::new(PARTS, "tool.parts.name", icons::PARTS, Category::Add, 0)
    })
}

/// The library, built-ins and the user's own parts, loaded fresh each
/// time the panel draws (cheap: in-code generators and small files, no
/// worker needed, same as Properties reading the session each frame).
fn library(app: &MeshFixerApp) -> Library {
    let ctx = mf_runtime::RunContext::none();
    // allow-swallow: without a settings folder there are no user parts; built-ins still load
    let dir = app.settings_dir().map(|d| d.join("parts"));
    Library::load(dir.as_deref(), &ctx).0
}

/// The world placement for a part dropped on `id`: its highest point,
/// attach normal straight up (world +Z), tangent world +Y projected onto
/// that plane (or +X if +Y is parallel to the normal).
pub(crate) fn drop_point(app: &MeshFixerApp, id: ObjectId) -> Option<[f64; 12]> {
    let o = app.session.document().object(id)?;
    let mesh = o.mesh()?;
    let bb = mesh.bounding_box()?;
    let t = app.shown_transform(id).unwrap_or(o.transform);
    let top_object = [
        (bb.min[0] + bb.max[0]) / 2.0,
        (bb.min[1] + bb.max[1]) / 2.0,
        bb.max[2],
    ];
    let world = crate::transform_tool::world_point(&t, glam::DVec3::from(top_object));
    Some((world, [0.0, 0.0, 1.0])).map(|(p, n)| {
        let frame = SurfaceFrame {
            point: p.to_array(),
            normal: n,
            tangent: [0.0, 1.0, 0.0],
        };
        let attach = Attach {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        };
        mf_parts::place(&attach, &frame, 1.0, 0.0)
    })
}

fn run_on(
    app: &mut MeshFixerApp,
    target: ObjectId,
    op_id: &str,
    label: String,
    params: BTreeMap<String, String>,
    operands: Vec<(String, ObjectId, Option<mf_selection::Selection>)>,
) {
    app.refusal = None;
    app.run_invocation(
        mf_command::Invocation {
            op_id: op_id.into(),
            target,
            selection: None,
            operands,
            raw_params: params,
        },
        label,
    );
}

/// Places library part `id` onto the active object, with its content
/// hash recorded (so replay refuses if the library part's content ever
/// changes): `object.add_part`, one undo step. `app.parts.placed`
/// (naming the new part and the scan it landed on) and the Transform
/// tool both follow once the command actually commits
/// ([`MeshFixerApp::pending_open_transform`]: placing runs on the job
/// pool, so neither can happen synchronously here).
fn place_part(app: &mut MeshFixerApp, id: &str) {
    let Some(scan) = app.active else { return };
    let Some(transform) = drop_point(app, scan) else {
        return;
    };
    let lib = library(app);
    let Some(part) = lib.get(id) else { return };
    // allow-swallow: a bad hash just skips the replay check; add_part reports the real reason
    let hash = part.content_hash().unwrap_or_default();
    let params = BTreeMap::from([
        ("part_id".to_owned(), id.to_owned()),
        ("part_hash".to_owned(), hash),
        (
            "transform".to_owned(),
            transform
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
    ]);
    run_on(
        app,
        scan,
        "object.add_part",
        t!("parts.apply", name = part.manifest.name.clone()),
        params,
        Vec::new(),
    );
    if app.busy.is_some() {
        app.pending_open_transform = Some(crate::app::PendingPlacement::Part { scan });
    }
}

/// The built-in library parts the parametric shapes replace (a sphere,
/// a box, ...): placing one of those is placing its shape, which stays
/// editable. Kept in the library itself so old logs that placed them
/// with `object.add_part` still replay.
const SUPERSEDED: &[&str] = &[
    "sphere",
    "cylinder",
    "cone",
    "box",
    "rounded-box",
    "torus",
    "capsule",
];

/// A shape's starting recipe: its spec's defaults, with a hole-like
/// shape's fit inputs from the Fit class and Machine profile chosen in
/// Connectors (the resolved values recorded, D20), and its hole profile
/// resolved for `transform`.
pub(crate) fn shape_record(
    app: &MeshFixerApp,
    shape: &dyn mf_parts::Generator,
    transform: &[f64; 12],
) -> Result<mf_parts::GeneratorRecord, String> {
    let mut raw = BTreeMap::new();
    if shape.spec().parameter("fit_class").is_some() {
        let (class, machine) = crate::connector_tool::chosen_fit(app)?;
        raw.insert("fit_class".to_owned(), class.name.clone());
        raw.insert("machine_profile".to_owned(), machine.name.clone());
        raw.insert("fit_clearance".to_owned(), class.clearance.to_string());
        raw.insert(
            "machine_tolerance".to_owned(),
            machine.hole_undersize.to_string(),
        );
    }
    let record = mf_parts::new_record(shape, &raw).map_err(|e| e.to_string())?;
    Ok(mf_parts::shape::orient_hole(
        &record,
        crate::shape_panel::rotation(transform),
    ))
}

/// Places shape `id` on the active object's highest point as an editable
/// shape (`object.add_shape`, one undo step); Transform opens on it once
/// it commits, as for a part.
pub(crate) fn place_shape(app: &mut MeshFixerApp, id: &str) {
    let Some(scan) = app.active else { return };
    let Some(transform) = drop_point(app, scan) else {
        return;
    };
    let Some(shape) = mf_parts::find_generator(id).filter(|g| g.is_shape()) else {
        return;
    };
    let record = match shape_record(app, shape, &transform) {
        Ok(r) => r,
        Err(e) => {
            app.refusal = Some(crate::shape_tools::Refusal {
                op_id: "object.add_shape".into(),
                code: "fit_settings_unreadable".into(),
                message: format!(
                    "Couldn't set up the shape's fit: {e}. Fix your Fit classes or Machine \
                     profiles in Settings, or delete the file to go back to the defaults."
                ),
            });
            return;
        }
    };
    let shapes = [mf_operations::shapes::ShapeToAdd {
        generator: record,
        name: shape.name().to_owned(),
        hole: shape.role() == mf_parts::ShapeRole::Hole,
        attach_to: None,
        transform: None,
        group: None,
    }];
    // The shape's own mesh is generated in mm (D16/D20): scale the
    // placement transform's linear part by document-units-per-mm, or
    // it lands at its literal mm size in document units.
    let mm = 1.0 / crate::transform_tool::mm_per(app.session.document().units());
    let transform = crate::transform_tool::scale_linear(&transform, mm);
    let params = BTreeMap::from([
        (
            "shapes".to_owned(),
            mf_operations::shapes::shapes_param(&shapes),
        ),
        (
            "transform".to_owned(),
            transform
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        ("group".to_owned(), String::new()),
    ]);
    run_on(
        app,
        scan,
        "object.add_shape",
        t!("parts.apply", name = shape.name()),
        params,
        Vec::new(),
    );
    if app.busy.is_some() {
        app.pending_open_transform = Some(crate::app::PendingPlacement::Part { scan });
    }
}

/// One cell of the library grid.
struct Cell {
    /// The library id (a part) or generator id (a shape).
    id: String,
    name: String,
    hole: bool,
    shape: bool,
    /// A user part (renamed and deleted from here; built-ins can't be).
    user: bool,
    category: String,
    description: String,
    /// The thumbnail's cache key (from the content hash).
    key: String,
}

/// The library as grid cells: the shapes, then the built-in parts the
/// shapes don't supersede, then the user's parts by category; filtered
/// by `query`.
fn cells(app: &MeshFixerApp, query: &str) -> (Vec<Cell>, Vec<Cell>) {
    let shown = |name: &str| query.is_empty() || name.to_lowercase().contains(query);
    let mut builtin: Vec<Cell> = mf_parts::generators()
        .into_iter()
        .filter(|g| g.is_shape() && shown(g.name()))
        .map(|g| {
            let defaults = g.spec().validate(&BTreeMap::new()).unwrap_or_default(); // allow-swallow: a spec always validates its own defaults
            let recipe = serde_json::to_string(&defaults).unwrap_or_default(); // allow-swallow: Params is plain data; serializing it can't fail
            Cell {
                id: g.id().to_owned(),
                name: g.name().to_owned(),
                hole: g.role() == mf_parts::ShapeRole::Hole,
                shape: true,
                user: false,
                category: String::new(),
                description: g.spec().summary.clone(),
                key: crate::part_thumbs::key(&format!("shape {} {} {recipe}", g.id(), g.version())),
            }
        })
        .collect();
    let lib = library(app);
    let mut user = Vec::new();
    for p in &lib.parts {
        if matches!(p.manifest.kind, PartKind::Connector { .. })
            || (p.is_builtin() && SUPERSEDED.contains(&p.manifest.id.as_str()))
            || !(shown(&p.manifest.name) || shown(&p.manifest.category))
        {
            continue;
        }
        let cell = Cell {
            id: p.manifest.id.clone(),
            name: p.manifest.name.clone(),
            hole: false,
            shape: false,
            user: !p.is_builtin(),
            category: p.manifest.category.clone(),
            description: p.manifest.description.clone(),
            key: crate::part_thumbs::key(
                &p.content_hash().unwrap_or_else(|_| p.manifest.id.clone()),
            ), // allow-swallow: an unreadable part's picture is keyed by its id; placing it reports why
        };
        if cell.user {
            user.push(cell);
        } else {
            builtin.push(cell);
        }
    }
    user.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    (builtin, user)
}

/// A cell's mesh, for its thumbnail.
fn cell_mesh(app: &MeshFixerApp, cell: &Cell) -> Option<mf_geometry::Mesh> {
    let ctx = mf_runtime::RunContext::none();
    if cell.shape {
        let g = mf_parts::find_generator(&cell.id)?;
        let record = mf_parts::new_record(g, &BTreeMap::new()).ok()?; // allow-swallow: a shape with no default recipe has no picture; its name still shows
        mf_parts::regenerate(&record, &ctx).ok() // allow-swallow: as above
    } else {
        library(app).get(&cell.id)?.mesh(&ctx).ok() // allow-swallow: a part that can't be read has no picture; placing it says why
    }
}

/// Grid tiles' size, in points (owner 2026-11-12: a fixed tile so every
/// one is the same size whether or not it's a user part with rename and
/// delete icons under its name).
const TILE_WIDTH: f32 = 96.0;
const TILE_HEIGHT: f32 = 116.0;
/// The thumbnail's own size, square, centred at the tile's top.
const THUMB: f32 = 80.0;
/// The name's own row, under the thumbnail: one line, centred.
const NAME_HEIGHT: f32 = 18.0;
/// The rename/delete icons' row, under the name: always reserved (even
/// for a built-in, which has neither), so every tile is `TILE_HEIGHT`
/// regardless.
const ACTIONS_HEIGHT: f32 = TILE_HEIGHT - THUMB - NAME_HEIGHT;

/// The name's own sub-rect within a tile's clickable button rect (the
/// picture-plus-name footprint the `Role::Button` node covers, `THUMB +
/// NAME_HEIGHT` tall — the parts-grid layout test already measures it by
/// accesskit): the bottom `NAME_HEIGHT` of it, full width, exactly as
/// `cell_ui` computes `name_rect` from `tile_rect`.
pub fn name_rect_in_tile_button(button_rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(button_rect.left(), button_rect.bottom() - NAME_HEIGHT),
        egui::vec2(button_rect.width(), NAME_HEIGHT),
    )
}

/// A tile's name, laid out as one line (ellipsised, never wrapped to a
/// second line, if it doesn't fit `name_rect`'s width — the full name is
/// still in the tooltip and the hover preview) and centred horizontally
/// in `name_rect`: `(galley, the point to paint it at)`.
///
/// `LayoutJob::halign` centres a row on its own *natural* width (egui's
/// `halign_and_justify_row`, since this job never justifies), not on
/// `wrap.max_width`, so the galley's own origin (`x = 0`) already sits at
/// the text's centre; painting it at `name_rect.left()` put that centre
/// at the tile's *left* edge instead of its middle, clipping off the
/// left half of anything but the shortest names (owner screenshot,
/// "ox", "nder", "ere", "rus": fix round 2026-11-13). Painting at
/// `name_rect.center().x` instead lines the text's own centre up with
/// the tile's.
pub fn name_galley(
    painter: &egui::Painter,
    name: &str,
    name_rect: egui::Rect,
) -> (std::sync::Arc<egui::Galley>, egui::Pos2) {
    let mut job = egui::text::LayoutJob::simple_singleline(
        name.to_owned(),
        egui::FontId::proportional(theme::TYPE_CAPTION),
        theme::TEXT,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(name_rect.width());
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    let text_pos = egui::pos2(
        name_rect.center().x - galley.size().x / 2.0,
        name_rect.center().y - galley.size().y / 2.0,
    );
    (galley, text_pos)
}

/// One grid tile: its picture (once rendered, or just its place before
/// then) on top, its name centred below on one line (ellipsised if it
/// doesn't fit; the tooltip and hover preview have it whole); a click
/// places it. A user part gets rename and delete icons under its name,
/// in the same reserved row every tile has (empty for a built-in), so
/// every tile is the same size.
fn cell_ui(app: &mut MeshFixerApp, ui: &mut egui::Ui, cell: &Cell) {
    let dir = app.settings_dir().map(|d| d.join("thumbnails"));
    let thumb = {
        let ctx = ui.ctx().clone();
        let mut thumbs = std::mem::take(&mut app.parts.thumbs);
        let t = thumbs.get(&ctx, dir.as_deref(), &cell.key, || cell_mesh(app, cell));
        app.parts.thumbs = thumbs;
        t
    };
    let idle = app.busy.is_none();
    let tile = ui.allocate_ui_with_layout(
        egui::vec2(TILE_WIDTH, TILE_HEIGHT),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.set_width(TILE_WIDTH);
            ui.spacing_mut().item_spacing.y = 0.0;
            // The whole tile (picture, name and all) is one click target,
            // its own rect reserved up front so the name row below can't
            // shift it (the rename/delete row is separate and still
            // clickable on its own).
            let (tile_rect, mut response) = ui.allocate_exact_size(
                egui::vec2(TILE_WIDTH, THUMB + NAME_HEIGHT),
                if idle {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                },
            );
            if ui.is_rect_visible(tile_rect) {
                let thumb_rect = egui::Rect::from_center_size(
                    egui::pos2(tile_rect.center().x, tile_rect.top() + THUMB / 2.0),
                    egui::vec2(THUMB, THUMB),
                );
                if response.hovered() {
                    ui.painter().rect_filled(
                        thumb_rect.expand(2.0),
                        theme::RADIUS,
                        theme::CONTROL_HOVER.gamma_multiply(0.5),
                    );
                }
                if let Some(t) = &thumb {
                    egui::Image::new((t.id(), thumb_rect.size())).paint_at(ui, thumb_rect);
                } else {
                    ui.painter().rect_filled(
                        thumb_rect,
                        theme::RADIUS,
                        theme::CONTROL.gamma_multiply(0.5),
                    );
                }
                let name_rect = egui::Rect::from_min_size(
                    egui::pos2(tile_rect.left(), thumb_rect.bottom()),
                    egui::vec2(TILE_WIDTH, NAME_HEIGHT),
                );
                let (galley, text_pos) = name_galley(ui.painter(), &cell.name, name_rect);
                ui.painter()
                    .with_clip_rect(name_rect)
                    .galley(text_pos, galley, theme::TEXT);
            }
            let label = cell.name.clone();
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, idle, &label));
            if response.hovered() && idle {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            response = response.on_hover_ui(|ui| {
                if let Some(t) = &thumb {
                    let side = crate::part_thumbs::SIZE as f32;
                    ui.image((t.id(), egui::vec2(side, side)));
                }
                ui.label(egui::RichText::new(&cell.name).color(theme::TEXT_STRONG));
                if !cell.category.is_empty() {
                    widgets::caption(ui, &cell.category);
                }
                if !cell.description.is_empty() {
                    widgets::caption(ui, &cell.description);
                }
                if cell.shape {
                    widgets::caption(ui, t!("parts.shape_tip"));
                }
                if cell.hole {
                    widgets::caption(ui, t!("parts.hole_tip"));
                }
            });
            if response.clicked() {
                if cell.shape {
                    place_shape(app, &cell.id);
                } else {
                    place_part(app, &cell.id);
                }
            }
            // Always this row's height, user part or not — reserved by
            // hand (`allocate_ui_with_layout` would instead shrink an
            // empty row, a built-in's, down to nothing) so every tile in
            // the grid is the same size (fix round 2026-11-12: "Tiles
            // have uneven heights").
            let (actions_rect, _) = ui
                .allocate_exact_size(egui::vec2(TILE_WIDTH, ACTIONS_HEIGHT), egui::Sense::hover());
            // No `is_rect_visible` gate here: `icon_button` already
            // skips its own painting when off-screen but still reports
            // its response and accessible label either way, so Rename
            // and Delete stay reachable (keyboard, screen reader, or a
            // test's `click_accesskit`) even scrolled out of view.
            if cell.user {
                let mut row = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(actions_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if widgets::icon_button(
                    &mut row,
                    icons::EDIT,
                    &t!("parts.rename_named", name = cell.name.clone()),
                    None,
                    ACTIONS_HEIGHT,
                )
                .clicked()
                {
                    app.parts.dialog = Some(PartDialog {
                        target: DialogTarget::Edit(cell.id.clone()),
                        name: cell.name.clone(),
                        category: cell.category.clone(),
                        description: cell.description.clone(),
                    });
                }
                if widgets::icon_button(
                    &mut row,
                    icons::TRASH,
                    &t!("parts.delete_named", name = cell.name.clone()),
                    None,
                    ACTIONS_HEIGHT,
                )
                .clicked()
                {
                    app.parts.confirm_delete = Some(cell.id.clone());
                }
            }
        },
    );
    // The whole tile's own footprint (image, name and the actions row,
    // whether or not this cell has one): a `GenericContainer`, not a
    // `Button` (that's the click target above, which doesn't cover the
    // actions row), so a layout test can find "every tile" without
    // picking up the rename/delete buttons, or anything else, by role.
    let label = format!("Part tile: {}", cell.name);
    tile.response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::GenericContainer, true, &label));
}

/// `cells` as a grid of fixed-size tiles, as many columns as fit the
/// available width with a consistent gap between them (and the same gap
/// left over at the right, never a partial column overflowing the
/// panel): owner 2026-11-12, "the 4th column overflows and is clipped
/// at the panel edge."
fn grid(app: &mut MeshFixerApp, ui: &mut egui::Ui, salt: &str, cells: &[Cell]) {
    let gap = theme::SPACE_S;
    let columns = (((ui.available_width() + gap) / (TILE_WIDTH + gap)).floor() as usize).max(1);
    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.push_id(("parts grid", salt), |ui| {
        for row in cells.chunks(columns) {
            ui.horizontal(|ui| {
                for cell in row {
                    cell_ui(app, ui, cell);
                }
            });
        }
    });
}

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    crate::shape_tools::refusal_card_for(app, ui, "object.add_part");
    crate::shape_tools::refusal_card_for(app, ui, "object.add_shape");
    crate::shape_tools::refusal_card_for(app, ui, "mesh.boolean");
    crate::shape_tools::refusal_card_for(app, ui, "mesh.combine");
    // The part just placed comes first: joining it is the next step.
    if let Some(placed) = app.parts.placed
        && app.session.document().object(placed.part).is_some()
        && app.session.document().object(placed.scan).is_some()
    {
        join_section(app, ui, placed);
    }

    widgets::section(ui, &t!("parts.section.library"));
    ui.add(
        egui::TextEdit::singleline(&mut app.parts.search)
            .hint_text(t!("parts.search_hint"))
            .desired_width(ui.available_width()),
    );
    ui.add_space(theme::SPACE_XS);
    let query = app.parts.search.to_lowercase();
    let (builtin, user) = cells(app, &query);
    if builtin.is_empty() && user.is_empty() {
        widgets::caption(ui, t!("parts.none_found"));
    }
    grid(app, ui, "built-in", &builtin);
    let mut start = 0;
    while start < user.len() {
        let category = user[start].category.clone();
        let end = user[start..]
            .iter()
            .position(|c| c.category != category)
            .map_or(user.len(), |n| start + n);
        widgets::section(
            ui,
            &if category.is_empty() {
                t!("parts.section.yours")
            } else {
                category.clone()
            },
        );
        grid(app, ui, &format!("user {category}"), &user[start..end]);
        start = end;
    }
    delete_confirmation(app, ui);
    widgets::caption(ui, t!("parts.click_help"));

    ui.add_space(theme::SPACE_L);
    widgets::section(ui, &t!("parts.section.save"));
    widgets::caption(ui, t!("parts.save_help"));
    let can_save = app.busy.is_none() && app.active_mesh().is_some();
    if ui
        .add_enabled_ui(can_save, |ui| {
            widgets::wide_button(ui, Kind::Secondary, None, &t!("parts.save_as_part"))
        })
        .inner
        .clicked()
        && let Some(id) = app.active
    {
        open_save_dialog(app, id);
    }
    dialog(app, ui.ctx());
}

/// Opens the save-part dialog for object `id`, its name filled in.
pub fn open_save_dialog(app: &mut MeshFixerApp, id: ObjectId) {
    let Some(o) = app.session.document().object(id) else {
        return;
    };
    app.parts.dialog = Some(PartDialog {
        target: DialogTarget::Save(id),
        name: o.name.clone(),
        category: String::new(),
        description: String::new(),
    });
}

/// "Delete <name>?" with Delete and Cancel, for the user part waiting on it.
fn delete_confirmation(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(id) = app.parts.confirm_delete.clone() else {
        return;
    };
    let name = library(app)
        .get(&id)
        .map_or_else(|| id.clone(), |p| p.manifest.name.clone());
    let mut close = false;
    widgets::card(ui, |ui| {
        widgets::body(ui, t!("parts.delete_confirm", name = name.clone()));
        ui.horizontal(|ui| {
            if widgets::button(
                ui,
                Kind::Primary,
                Some(icons::TRASH),
                &t!("parts.delete"),
                0.0,
            )
            .clicked()
            {
                delete_part(app, &id);
                close = true;
            }
            if widgets::button(ui, Kind::Secondary, None, &t!("parts.cancel"), 0.0).clicked() {
                close = true;
            }
        });
    });
    if close {
        app.parts.confirm_delete = None;
    }
}

/// Deletes user part `id` (never a built-in: the library refuses).
pub fn delete_part(app: &mut MeshFixerApp, id: &str) {
    let Some(dir) = app.settings_dir().map(|d| d.join("parts")) else {
        app.status = Some(t!("parts.save_no_settings_dir"));
        return;
    };
    let name = library(app)
        .get(id)
        .map_or_else(|| id.to_owned(), |p| p.manifest.name.clone());
    app.status = Some(match mf_parts::delete_user_part(&dir, id) {
        Ok(()) => t!("parts.deleted", name = name),
        Err(e) => e.to_string(),
    });
}

/// The save / edit dialog, while open: name, category and description.
fn dialog(app: &mut MeshFixerApp, ctx: &egui::Context) {
    let Some(mut d) = app.parts.dialog.clone() else {
        return;
    };
    let title = match d.target {
        DialogTarget::Save(_) => t!("parts.dialog.save_title"),
        DialogTarget::Edit(_) => t!("parts.dialog.edit_title"),
    };
    let (mut done, mut cancel) = (false, false);
    // Modal, like the app's other dialogs: nothing behind it answers
    // until it's saved or cancelled.
    egui::Modal::new(egui::Id::unique("part dialog")).show(ctx, |ui| {
        ui.heading(title);
        for (label, value, multiline) in [
            (t!("parts.dialog.name"), &mut d.name, false),
            (t!("parts.dialog.category"), &mut d.category, false),
            (t!("parts.dialog.description"), &mut d.description, true),
        ] {
            let l = ui.label(label);
            let edit = if multiline {
                egui::TextEdit::multiline(value).desired_rows(3)
            } else {
                egui::TextEdit::singleline(value)
            };
            ui.add(edit.desired_width(280.0)).labelled_by(l.id);
        }
        ui.add_space(theme::SPACE_S);
        ui.horizontal(|ui| {
            let ok = !d.name.trim().is_empty();
            let label = match d.target {
                DialogTarget::Save(_) => t!("parts.dialog.save"),
                DialogTarget::Edit(_) => t!("parts.dialog.apply"),
            };
            done = ui
                .add_enabled_ui(ok, |ui| {
                    widgets::button(ui, Kind::Primary, None, &label, 0.0)
                })
                .inner
                .clicked();
            cancel = widgets::button(ui, Kind::Secondary, None, &t!("parts.cancel"), 0.0)
                .on_hover_text("Esc")
                .clicked();
        });
    });
    cancel |= ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if cancel {
        app.parts.dialog = None;
        return;
    }
    if !done {
        app.parts.dialog = Some(d);
        return;
    }
    app.parts.dialog = None;
    match &d.target {
        DialogTarget::Save(id) => save_as_part(app, *id, &d.name, &d.category, &d.description),
        DialogTarget::Edit(id) => {
            let Some(dir) = app.settings_dir().map(|p| p.join("parts")) else {
                app.status = Some(t!("parts.save_no_settings_dir"));
                return;
            };
            app.status = Some(
                match mf_parts::update_user_part(&dir, id, &d.name, &d.category, &d.description) {
                    Ok(m) => t!("parts.renamed", name = m.name),
                    Err(e) => e.to_string(),
                },
            );
        }
    }
}

fn join_section(app: &mut MeshFixerApp, ui: &mut egui::Ui, placed: PlacedPart) {
    let units = crate::defects::unit_symbol(app.session.document().units());
    widgets::section(ui, &t!("parts.section.join"));
    let open_stamp = app
        .session
        .document()
        .object(placed.part)
        .and_then(|o| o.mesh())
        .map(|m| !m.topology().boundary_loops.is_empty())
        .unwrap_or(false);
    if open_stamp {
        widgets::caption(ui, t!("parts.open_stamp"));
    }
    let target = app
        .session
        .document()
        .object(placed.scan)
        .map(|o| o.name.clone())
        .unwrap_or_default(); // allow-swallow: the caller checked the scan still exists
    let combine_label = t!("parts.join.combine", target = target.clone());
    let modes = [
        (JoinMode::KeepSeparate, t!("parts.join.keep_separate")),
        (JoinMode::Combine, combine_label.clone()),
        (JoinMode::Blend, t!("parts.join.blend")),
    ];
    let options: Vec<(&str, &str, bool)> = modes
        .iter()
        .map(|(_, l)| (l.as_str(), l.as_str(), true))
        .collect();
    let current = modes
        .iter()
        .position(|(m, _)| *m == app.parts.join)
        // allow-swallow: `modes` is the exhaustive, fixed list of JoinMode values, so app.parts.join is always found; the default is unreachable.
        .unwrap_or(0);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.parts.join = modes[i].0;
    }
    if app.parts.join == JoinMode::Blend {
        ui.add(
            egui::Slider::new(&mut app.parts.blend_width, 0.0..=20.0)
                .suffix(format!(" {units}"))
                .max_decimals(2),
        );
        widgets::caption(ui, t!("parts.blend_width_help", unit = units.to_owned()));
    }
    ui.add_space(theme::SPACE_S);
    let can = app.busy.is_none() && app.parts.join != JoinMode::KeepSeparate;
    let label = match app.parts.join {
        JoinMode::Blend => t!("parts.join.blend_into", target = target),
        _ => combine_label,
    };
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::COMBINE),
                &t!("parts.join.apply"),
            )
        })
        .inner
        .on_hover_text(&label)
        .clicked()
    {
        join_part(app, placed, app.parts.join, label);
    }
}

/// Joins a placed part into the object it was placed on, as one undo
/// step: Combine is `mesh.combine` (the part unioned in as a solid, or
/// cut as a hole by its role; the part's own object removed, so one
/// object is left); Blend is `mesh.boolean`'s union with the blend width.
pub fn join_part(app: &mut MeshFixerApp, placed: PlacedPart, mode: JoinMode, label: String) {
    match mode {
        JoinMode::KeepSeparate => return,
        JoinMode::Combine => {
            let hole = app
                .session
                .document()
                .object(placed.part)
                .is_some_and(|o| o.is_hole());
            let slot = if hole { "holes" } else { "solids" };
            run_on(
                app,
                placed.scan,
                "mesh.combine",
                label,
                BTreeMap::from([("keep_holes".to_owned(), "false".to_owned())]),
                vec![(slot.to_owned(), placed.part, None)],
            );
        }
        JoinMode::Blend => {
            // `blend_width` is a `unit: mm` param (D20); `app.parts.blend_width`
            // is document units (the panel's own slider), so cross to mm
            // here, once, or `Session::prepare`'s single mm-to-document
            // conversion runs on an already-document-units number.
            let mm = crate::transform_tool::mm_per(app.session.document().units());
            let params = BTreeMap::from([
                ("operation".to_owned(), "union".to_owned()),
                (
                    "blend_width".to_owned(),
                    (app.parts.blend_width * mm).to_string(),
                ),
            ]);
            run_on(
                app,
                placed.scan,
                "mesh.boolean",
                label,
                params,
                vec![("tool".to_owned(), placed.part, None)],
            );
        }
    }
    app.parts.placed = None;
}

/// Saves object `id` as a user part named `name`, filed under
/// `category`, with `description` (`mf_parts::save_object_as_part`): its
/// id is the name as a slug, numbered if that's taken.
pub fn save_as_part(
    app: &mut MeshFixerApp,
    id: ObjectId,
    name: &str,
    category: &str,
    description: &str,
) {
    let Some(o) = app.session.document().object(id) else {
        return;
    };
    let Some(mesh) = o.mesh().cloned() else {
        return;
    };
    let name = name.trim().to_owned();
    let open = !mesh.topology().boundary_loops.is_empty();
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let Some(dir) = app.settings_dir().map(|d| d.join("parts")) else {
        app.status = Some(t!("parts.save_no_settings_dir"));
        return;
    };
    let lib = library(app);
    let base = format!("user-{slug}");
    let part_id = (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|c| lib.get(c).is_none() && !dir.join(c).exists())
        .unwrap_or(base); // allow-swallow: the range is endless; `find` always returns
    let manifest = mf_parts::PartManifest {
        id: part_id,
        name,
        version: 1,
        licence: "".into(),
        source: "Saved from the model".into(),
        kind: if open {
            PartKind::Open
        } else {
            PartKind::Solid
        },
        attach: Attach {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        },
        default_scale: 1.0,
        mesh: String::new(),
        category: category.trim().to_owned(),
        description: description.trim().to_owned(),
    };
    match mf_parts::save_object_as_part(&mesh, &manifest, &dir) {
        Ok(_) => app.status = Some(t!("parts.saved", name = manifest.name.clone())),
        Err(e) => app.status = Some(format!("{e}")),
    }
}
