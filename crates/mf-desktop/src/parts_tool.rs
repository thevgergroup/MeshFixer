//! The Parts tool on the rail (Phase 1 Task 23, MF-MODEL-004), registered
//! through `shell::Tool` like Split, Combine and Make solid
//! (`shape_tools.rs`).
//!
//! - **The library.** Built-in parts (`mf_parts::Library`, D16) listed
//!   with a search box.
//! - **Placing.** Clicking a part adds it to the model on the active
//!   object's own highest point, its attach normal pointing straight up
//!   (world +Z): `object.add_part`, one undo step. The new part becomes
//!   the active object so Transform (T) is one key away.
//! - **Join to scan**, on a placed part: Keep separate (the default,
//!   nothing further to do), Merge (`mesh.boolean` union into the object
//!   it was placed on) or Blend (the same union with a blend width).
//! - **Save as part** on any object writes it to the user library
//!   (`mf_parts::save_object_as_part`).

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
    Merge,
    Blend,
}

/// The Parts tool's state: the search filter, which part is highlighted
/// in the library, and the join settings for the part last placed.
#[derive(Debug, Clone)]
pub struct PartsState {
    pub search: String,
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
            placed: None,
            join: JoinMode::KeepSeparate,
            blend_width: 2.0,
        }
    }
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

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    crate::shape_tools::refusal_card_for(app, ui, "object.add_part");
    crate::shape_tools::refusal_card_for(app, ui, "mesh.boolean");
    widgets::section(ui, &t!("parts.section.library"));
    ui.add(
        egui::TextEdit::singleline(&mut app.parts.search)
            .hint_text(t!("parts.search_hint"))
            .desired_width(ui.available_width()),
    );
    ui.add_space(theme::SPACE_XS);
    let lib = library(app);
    let query = app.parts.search.to_lowercase();
    let mut parts: Vec<(String, String, bool)> = lib
        .parts
        .iter()
        .filter(|p| query.is_empty() || p.manifest.name.to_lowercase().contains(&query))
        .map(|p| {
            (
                p.manifest.id.clone(),
                p.manifest.name.clone(),
                matches!(p.manifest.kind, PartKind::Connector { .. }),
            )
        })
        .filter(|(_, _, is_connector)| !is_connector)
        .collect();
    parts.sort_by(|a, b| a.1.cmp(&b.1));
    if parts.is_empty() {
        widgets::caption(ui, t!("parts.none_found"));
    }
    for (id, name, _) in &parts {
        if ui
            .add_enabled_ui(app.busy.is_none(), |ui| {
                widgets::wide_button(ui, Kind::Secondary, Some(icons::PARTS), name)
            })
            .inner
            .clicked()
        {
            place_part(app, id);
        }
    }
    widgets::caption(ui, t!("parts.click_help"));

    if let Some(placed) = app.parts.placed
        && app.session.document().object(placed.part).is_some()
        && app.session.document().object(placed.scan).is_some()
    {
        join_section(app, ui, placed);
    }

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
    {
        save_active_as_part(app);
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
    let modes = [
        (JoinMode::KeepSeparate, t!("parts.join.keep_separate")),
        (JoinMode::Merge, t!("parts.join.merge")),
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
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::COMBINE),
                &t!("parts.apply_join"),
            )
        })
        .inner
        .clicked()
    {
        let blend_width = if app.parts.join == JoinMode::Blend {
            app.parts.blend_width
        } else {
            0.0
        };
        let params = BTreeMap::from([
            ("operation".to_owned(), "union".to_owned()),
            ("blend_width".to_owned(), blend_width.to_string()),
        ]);
        run_on(
            app,
            placed.scan,
            "mesh.boolean",
            t!("parts.apply_join"),
            params,
            vec![("tool".to_owned(), placed.part, None)],
        );
        app.parts.placed = None;
    }
}

fn save_active_as_part(app: &mut MeshFixerApp) {
    let Some(id) = app.active else { return };
    let Some(o) = app.session.document().object(id) else {
        return;
    };
    let Some(mesh) = o.mesh().cloned() else {
        return;
    };
    let name = o.name.clone();
    let open = !mesh.topology().boundary_loops.is_empty();
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let manifest = mf_parts::PartManifest {
        id: format!("user-{slug}-{}", id.0),
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
    };
    let Some(dir) = app.settings_dir().map(|d| d.join("parts")) else {
        app.status = Some(t!("parts.save_no_settings_dir"));
        return;
    };
    match mf_parts::save_object_as_part(&mesh, &manifest, &dir) {
        Ok(_) => app.status = Some(t!("parts.saved", name = manifest.name.clone())),
        Err(e) => app.status = Some(format!("{e}")),
    }
}
