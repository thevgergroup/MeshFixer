//! The Connectors tool on the rail (Phase 1 Task 29, MF-MODEL-005),
//! registered through `shell::Tool` like Parts (`parts_tool.rs`).
//!
//! - **The library.** Every connector the generator registry offers on
//!   a surface (`mf_parts::generators`), each a library entry (Task 28's
//!   `PartKind::Connector`, derived in `mf_parts::builtin`). A connector
//!   with params to pick before placing ("what's on the packet": a
//!   thread's size and length, `Generator::quick_params`) shows them as
//!   fields built from its own schema.
//! - **Fit class and Machine profile** pickers: the design-intent gap
//!   and the measured print-tolerance compensation (owner requirements
//!   2026-11-11); a profile the user hasn't calibrated says so.
//! - **Placing** adds the connector as a linked group of editable
//!   shapes (parametric-shapes brief): one object per part (a hinge's
//!   leaves, a clasp's catch and hook), each keeping its recipe and the
//!   group id, so selecting any of them edits the whole connector
//!   (`object.add_shape`, one undo step). Holes are placed as Holes.
//! - **Attach** joins each member onto the side it belongs to, solids
//!   joined and holes cut (`object.attach`, one undo step), checking the
//!   two sides stay `gap` apart.
//! - **The fit check**, in plain words with a one-click fix: the exact
//!   gap between the connector's own generated mating parts (its first
//!   `Side::A`/`Side::B` pair), measured against the chosen Fit class
//!   resolved through the chosen Machine profile
//!   ([`mf_parts::check_fit`]). Thread generates one part with no
//!   mating pair of its own (the fit there is against the hole it's cut
//!   into), so the check doesn't apply to it.

use std::collections::BTreeMap;

use eframe::egui;
use glam::DMat4;
use mf_command::ObjectId;
use mf_parts::{Compensation, FitCheck, FitClass, FitClasses, MachineProfile, MachineProfiles};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, RegisterError, Shell, Tool as T};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const CONNECTORS: &str = "connectors";

/// A connector just placed: its own object, the compensated parameters
/// it was generated with (so Merge regenerates the exact same parts),
/// and the id of the two objects it's meant to join, once picked.
#[derive(Debug, Clone)]
pub struct PlacedConnector {
    /// The group's first member (the object Transform opens on).
    pub object: ObjectId,
    /// The linked group's id, on every member.
    pub group: String,
    pub connector_id: String,
    pub connector_params_json: String,
    pub gap: f64,
    pub side_a: Option<ObjectId>,
    pub side_b: Option<ObjectId>,
}

/// The Connectors tool's state: the search filter, each connector's
/// quick params (picked before placing), the Fit class and Machine
/// profile choices, and the connector last placed.
#[derive(Debug, Clone)]
pub struct ConnectorsState {
    pub search: String,
    /// Per connector id: its quick params' values (its schema's defaults
    /// until picked).
    pub quick: BTreeMap<String, mf_spec::Params>,
    pub fit_class: String,
    pub machine_profile: String,
    pub placed: Option<PlacedConnector>,
}

impl Default for ConnectorsState {
    fn default() -> ConnectorsState {
        ConnectorsState {
            search: String::new(),
            quick: BTreeMap::new(),
            fit_class: mf_parts::FitClass::builtins()[1].name.clone(), // "Snug"
            machine_profile: mf_parts::UNCALIBRATED_NAME.into(),
            placed: None,
        }
    }
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
        help: "tool.connectors.help",
        availability: needs_object,
        panel: Some(panel),
        on_open: |app| app.refusal = None,
        ..T::new(
            CONNECTORS,
            "tool.connectors.name",
            icons::CONNECTORS,
            Category::Add,
            10,
        )
    })?;
    shell.register_settings_page(crate::shell::SettingsPage {
        id: "machine_profiles",
        title: "settings.machine_profiles",
        ui: machine_profiles_page,
    })?;
    shell.register_settings_page(crate::shell::SettingsPage {
        id: "fit_classes",
        title: "settings.fit_classes",
        ui: fit_classes_page,
    })
}

/// The user's Fit classes plus the built-ins, or the built-ins alone
/// with why if the file on disk couldn't be read (never silently
/// dropped: a malformed or unreadable file means the user's own classes
/// are missing, which the panel says so the file can be fixed, rather
/// than quietly falling back).
pub(crate) fn fit_classes(app: &MeshFixerApp) -> (FitClasses, Option<String>) {
    let Some(dir) = app.settings_dir() else {
        return (FitClasses::default(), None);
    };
    match FitClasses::load(&dir) {
        Ok(c) => (c, None),
        Err(e) => (FitClasses::default(), Some(e.to_string())),
    }
}

/// As [`fit_classes`], for Machine profiles.
pub(crate) fn machine_profiles(app: &MeshFixerApp) -> (MachineProfiles, Option<String>) {
    let Some(dir) = app.settings_dir() else {
        return (MachineProfiles::default(), None);
    };
    match MachineProfiles::load(&dir) {
        Ok(p) => (p, None),
        Err(e) => (MachineProfiles::default(), Some(e.to_string())),
    }
}

/// The Fit class and Machine profile chosen in this panel, resolved
/// (the built-in "Snug" and the uncalibrated profile if the chosen names
/// aren't there any more); an error naming the file if the user's own
/// settings couldn't be read (never silently replaced by the defaults).
pub(crate) fn chosen_fit(app: &MeshFixerApp) -> Result<(FitClass, MachineProfile), String> {
    let (classes, class_err) = fit_classes(app);
    let (profiles, profile_err) = machine_profiles(app);
    if let Some(e) = class_err.or(profile_err) {
        return Err(e);
    }
    let class = classes
        .get(&app.connectors.fit_class)
        .cloned()
        .unwrap_or_else(|| FitClass::builtins()[1].clone());
    let machine = profiles
        .get(&app.connectors.machine_profile)
        .cloned()
        .unwrap_or_else(MachineProfile::uncalibrated);
    Ok((class, machine))
}

fn connector_library(app: &MeshFixerApp) -> mf_parts::Library {
    let ctx = mf_runtime::RunContext::none();
    let dir = app.settings_dir().map(|d| d.join("parts"));
    mf_parts::Library::load(dir.as_deref(), &ctx).0
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

/// `id`'s connector parameters, fully resolved and validated against
/// its own spec (every parameter, not just the ones the Fit/Machine
/// pickers touch): every parameter whose spec declares a `fit_role`
/// (Thread's `fit_clearance`/`machine_tolerance`, the hinge's and the
/// dovetail's `gap`/`pin_clearance`/`fit_clearance`/`machine_tolerance`
/// and their `fit_class`/`machine_profile` names, ...) is filled
/// generically from the chosen Fit class and Machine profile
/// (`mf_parts::fill_fit_role_params`, parts-architecture-review.md: no
/// more per-connector `if id == ...` branch). Thread's `size`/`length`
/// are its own UI state, not a fit-role concern, so they're still set
/// from the card's quick params. The library part's own generated mesh
/// (`object.add_part`) is sized from the connector's spec defaults, not
/// this; what matters here is that `connector_params_json` (recorded on
/// placement so Merge can regenerate the exact same parts, D22) is a
/// complete, validated `mf_spec::Params`, the same shape
/// `connector.join` deserializes directly with no further validation —
/// never a sparse override map.
fn connector_params(app: &MeshFixerApp, id: &str) -> Result<(String, f64), String> {
    let (classes, class_err) = fit_classes(app);
    let (profiles, profile_err) = machine_profiles(app);
    if let Some(e) = class_err.or(profile_err) {
        return Err(e);
    }
    let class = classes
        .get(&app.connectors.fit_class)
        .cloned()
        .unwrap_or_else(|| FitClass::builtins()[1].clone());
    let machine = profiles
        .get(&app.connectors.machine_profile)
        .cloned()
        .unwrap_or_else(MachineProfile::uncalibrated);
    let gap = mf_parts::resolve(&class, &machine, Compensation::None).max(0.0);
    let spec = mf_parts::find_generator(id)
        .map(|c| c.spec())
        .ok_or_else(|| format!("'{id}' isn't a built-in connector"))?;
    // The quick params picked in the library card (a thread's size and
    // length), then the fit.
    let mut raw: BTreeMap<String, String> = app
        .connectors
        .quick
        .get(id)
        .map(|q| {
            q.0.iter()
                .map(|(k, v)| {
                    let text = match v {
                        mf_spec::ParamValue::Float(f) => format!("{f}"),
                        other => other.to_string(),
                    };
                    (k.clone(), text)
                })
                .collect()
        })
        .unwrap_or_default(); // allow-swallow: no quick params picked yet means the schema's defaults
    if spec.parameter("gap").is_some_and(|p| p.fit_role.is_none()) && !raw.contains_key("gap") {
        // A connector whose own `gap` has no `fit_role` of its own (the
        // Connect point-placement joint connectors, whose `gap` instead
        // comes from the cut frame, never this picker): the plain,
        // uncompensated clearance, same as every connector here used to
        // get by default before `fit_role` existed.
        raw.insert("gap".to_owned(), gap.to_string());
    }
    mf_parts::fill_fit_role_params(spec, &mut raw, &class, &machine);
    let params = spec
        .validate(&raw)
        .map_err(|e| format!("'{id}''s own parameters: {e}"))?;
    let json = serde_json::to_string(&params)
        .map_err(|e| format!("'{id}''s own parameters didn't serialize: {e}"))?;
    Ok((json, gap))
}

/// Places library connector `id` onto the active object, the same way
/// Parts does (`object.add_part`, its content hash recorded); remembers
/// the connector id and its resolved parameters (as JSON, matching what
/// `connector.join` records) so Merge can run without touching the
/// object's live state.
fn place_connector(app: &mut MeshFixerApp, id: &str) {
    let Some(scan) = app.active else { return };
    let Some(transform) = crate::parts_tool::drop_point(app, scan) else {
        return;
    };
    let lib = connector_library(app);
    let Some(part) = lib.get(id) else { return };
    let (connector_params_json, gap) = match connector_params(app, id) {
        Ok(p) => p,
        Err(e) => {
            app.refusal = Some(crate::shape_tools::Refusal {
                op_id: "object.add_shape".into(),
                code: "fit_settings_unreadable".into(),
                message: format!(
                    "Couldn't read your Fit classes or Machine profiles: {e}. Fix the file in \
                     Settings, or delete it to go back to the defaults."
                ),
            });
            return;
        }
    };
    let name = part.manifest.name.clone();
    let shapes = match group_members(id, &name, &connector_params_json, &transform, None) {
        Ok(s) => s,
        Err(message) => {
            app.refusal = Some(crate::shape_tools::Refusal {
                op_id: "object.add_shape".into(),
                code: "connector_failed".into(),
                message,
            });
            return;
        }
    };
    let group = new_group_id(app, id);
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
        ("group".to_owned(), group.clone()),
    ]);
    run_on(
        app,
        scan,
        "object.add_shape",
        t!("connectors.apply", name = name),
        params,
        Vec::new(),
    );
    if app.busy.is_some() {
        app.pending_open_transform = Some(crate::app::PendingPlacement::Connector {
            connector_id: id.to_owned(),
            connector_params_json,
            gap,
            group,
        });
    }
}

/// A fresh linked-group id for connector `id`: unique in the document
/// (the next number after every group already there).
pub(crate) fn new_group_id(app: &MeshFixerApp, id: &str) -> String {
    let n = app
        .session
        .document()
        .objects()
        .iter()
        .filter_map(|o| o.properties.get(mf_command::GROUP))
        .filter_map(|g| g.rsplit('-').next()?.parse::<u64>().ok())
        .max()
        .map_or(1, |n| n + 1);
    format!("{id}-{n}")
}

/// The objects connector `id` is placed as: one editable shape per part
/// it generates with `params_json` (named "<name> (<role>)"), holes as
/// Holes. `attach` names the object each side joins, when known (Connect
/// knows both pieces; the Connectors tool picks them later).
pub(crate) fn group_members(
    id: &str,
    name: &str,
    params_json: &str,
    transform: &[f64; 12],
    attach: Option<(ObjectId, ObjectId)>,
) -> Result<Vec<mf_operations::shapes::ShapeToAdd>, String> {
    let connector =
        mf_parts::find_generator(id).ok_or_else(|| format!("'{id}' isn't a built-in connector"))?;
    let raw: BTreeMap<String, mf_spec::ParamValue> =
        serde_json::from_str(params_json).map_err(|e| format!("its parameters: {e}"))?;
    let params = mf_spec::Params(raw);
    let ctx = mf_runtime::RunContext::none();
    let parts = connector
        .generate(&params, &mf_parts::GenContext::new(&ctx))
        .map_err(|e| e.to_string())?;
    let rotation = crate::shape_panel::rotation(transform);
    Ok(parts
        .iter()
        .map(|p| {
            let record = mf_parts::GeneratorRecord {
                id: id.to_owned(),
                version: connector.version(),
                params: params.clone(),
                part: Some(p.name.to_owned()),
            };
            mf_operations::shapes::ShapeToAdd {
                generator: mf_parts::shape::orient_hole(&record, rotation),
                name: format!("{name} ({})", p.name.replace('_', " ")),
                hole: p.role == mf_parts::ShapeRole::Hole,
                attach_to: attach.and_then(|(a, b)| match p.attach_to {
                    mf_parts::Side::A => Some(a.0),
                    mf_parts::Side::B => Some(b.0),
                    mf_parts::Side::Neither => None,
                }),
                transform: None,
                group: None,
            }
        })
        .collect())
}

/// Attaches `group`'s members to `a` (side A) and `b` (side B): each
/// member joins the side its part belongs to, solids joined and holes
/// cut (`object.attach`, one undo step). A loose part (a separate pin, a
/// dowel) stays its own object.
pub(crate) fn attach_group(
    app: &mut MeshFixerApp,
    group: &str,
    a: ObjectId,
    b: ObjectId,
    gap: f64,
    label: String,
) {
    let ctx = mf_runtime::RunContext::none();
    let doc = app.session.document();
    let mut members = Vec::new();
    let mut roles = Vec::new();
    for o in doc.objects() {
        if o.properties.get(mf_command::GROUP).map(String::as_str) != Some(group) {
            continue;
        }
        let Some(record) = crate::shape_panel::recipe(app, o.id) else {
            continue;
        };
        let side = match mf_parts::shape::part_of(&record, &ctx).map(|(s, _)| s) {
            Some(mf_parts::Side::A) => 'a',
            Some(mf_parts::Side::B) => 'b',
            _ => match o.properties.get(mf_operations::shapes::ATTACH_TO) {
                Some(to) if *to == a.0.to_string() => 'a',
                Some(to) if *to == b.0.to_string() => 'b',
                _ => continue,
            },
        };
        members.push(o.id);
        roles.push(mf_operations::shapes::AttachMember {
            side,
            hole: o.is_hole(),
        });
    }
    if members.is_empty() {
        return;
    }
    let mut operands = vec![("side_b".to_owned(), b, None)];
    operands.extend(members.iter().map(|&m| ("members".to_owned(), m, None)));
    run_on(
        app,
        a,
        "object.attach",
        label,
        BTreeMap::from([
            (
                "members".to_owned(),
                mf_operations::shapes::attach_param(&roles),
            ),
            ("gap".to_owned(), gap.max(0.0).to_string()),
        ]),
        operands,
    );
}

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    crate::shape_tools::refusal_card_for(app, ui, "object.add_part");
    crate::shape_tools::refusal_card_for(app, ui, "object.add_shape");
    crate::shape_tools::refusal_card_for(app, ui, "connector.join");
    crate::shape_tools::refusal_card_for(app, ui, "object.attach");
    widgets::section(ui, &t!("connectors.section.library"));
    ui.add(
        egui::TextEdit::singleline(&mut app.connectors.search)
            .hint_text(t!("connectors.search_hint"))
            .desired_width(ui.available_width()),
    );
    ui.add_space(theme::SPACE_XS);
    let lib = connector_library(app);
    let query = app.connectors.search.to_lowercase();
    let mut connectors: Vec<(String, String)> = lib
        .parts
        .iter()
        .filter(|p| matches!(p.manifest.kind, mf_parts::PartKind::Connector { .. }))
        .filter(|p| query.is_empty() || p.manifest.name.to_lowercase().contains(&query))
        .map(|p| (p.manifest.id.clone(), p.manifest.name.clone()))
        .collect();
    connectors.sort_by(|a, b| a.1.cmp(&b.1));
    for (id, name) in &connectors {
        library_entry(app, ui, id, name);
    }
    widgets::caption(ui, t!("connectors.click_help"));

    fit_pickers(app, ui);
    crate::hinge_tool::placement_section(app, ui);

    if let Some(placed) = app.connectors.placed.clone()
        && app.session.document().object(placed.object).is_some()
    {
        fit_check_section(app, ui, &placed);
        merge_section(app, ui, &placed);
    }
}

/// One library connector: a button that places it, or — for one with
/// quick params to pick first — a card with those fields (built from its
/// own schema) and an Add button.
fn library_entry(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: &str, name: &str) {
    let Some(g) = mf_parts::find_generator(id) else {
        return;
    };
    let quick = g.quick_params();
    if quick.is_empty() {
        if ui
            .add_enabled_ui(app.busy.is_none(), |ui| {
                widgets::wide_button(ui, Kind::Secondary, Some(icons::CONNECTORS), name)
            })
            .inner
            .clicked()
        {
            place_connector(app, id);
        }
        return;
    }
    let hidden: Vec<&str> = g
        .spec()
        .parameters
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| !quick.contains(n))
        .collect();
    let options = crate::shape_panel::text_options(app);
    let idle = app.busy.is_none();
    let entry = app
        .connectors
        .quick
        .entry(id.to_owned())
        .or_insert_with(|| {
            let defaults = g.spec().validate(&BTreeMap::new()).unwrap_or_default(); // allow-swallow: a spec always validates its own defaults
            mf_spec::Params(
                defaults
                    .0
                    .into_iter()
                    .filter(|(k, _)| quick.contains(&k.as_str()))
                    .collect(),
            )
        });
    let mut add = false;
    widgets::card(ui, |ui| {
        widgets::body(ui, name);
        if let Some((k, v)) = crate::shape_panel::param_grid(
            ui,
            ("connector quick params", id),
            g.spec(),
            entry,
            &crate::shape_panel::GridOptions {
                hidden: &hidden,
                live: true,
                idle,
                text_options: &options,
            },
        ) {
            entry.0.insert(k, v);
        }
        add = ui
            .add_enabled_ui(idle, |ui| {
                widgets::wide_button(
                    ui,
                    Kind::Secondary,
                    Some(icons::CONNECTORS),
                    &t!("connectors.add"),
                )
            })
            .inner
            .clicked();
    });
    if add {
        place_connector(app, id);
    }
}

fn fit_pickers(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::section(ui, &t!("connectors.section.fit"));
    let (classes, class_err) = fit_classes(app);
    ui.label(t!("connectors.fit_class"));
    for c in &classes.classes {
        let label = format!("{} ({:+.2} mm)", c.name, c.clearance);
        let on = app.connectors.fit_class == c.name;
        if widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            None,
            &label,
            ui.available_width(),
        )
        .clicked()
        {
            app.connectors.fit_class = c.name.clone();
        }
    }
    if let Some(e) = &class_err {
        widgets::caption(
            ui,
            format!("Your Fit classes file couldn't be read ({e}); showing the built-ins."),
        );
    }
    ui.add_space(theme::SPACE_XS);
    let (profiles, profile_err) = machine_profiles(app);
    ui.label(t!("connectors.machine_profile"));
    for p in &profiles.profiles {
        let label = if p.calibrated {
            p.name.clone()
        } else {
            t!("connectors.not_calibrated", name = p.name.clone())
        };
        let on = app.connectors.machine_profile == p.name;
        if widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            None,
            &label,
            ui.available_width(),
        )
        .clicked()
        {
            app.connectors.machine_profile = p.name.clone();
        }
    }
    if let Some(e) = &profile_err {
        widgets::caption(
            ui,
            format!("Your Machine profiles file couldn't be read ({e}); showing the built-in."),
        );
    }
    if !profiles
        .get(&app.connectors.machine_profile)
        .is_some_and(|p| p.calibrated)
    {
        widgets::caption(ui, t!("connectors.uncalibrated_help"));
    }
}

/// The first `Side::A`/`Side::B` part pair a connector's own generation
/// produces, for the exact fit check (never guessed from the
/// parameters: the same BVH-measured approach `mesh.split_plane`'s
/// alignment and `connector.join`'s post-check use).
enum MatingPair {
    /// Both meshes, in part space.
    Found(mf_geometry::Mesh, mf_geometry::Mesh),
    /// This connector has no mating pair of its own (Thread: its fit is
    /// against the hole it's cut into, not between two of its own
    /// generated parts) — expected, not an error.
    NotApplicable,
    /// The connector id isn't registered, the recorded parameters
    /// aren't valid JSON, or generating from them failed: a real
    /// problem, distinct from "not applicable", so the panel doesn't
    /// tell the user a broken check is just unavailable.
    Failed(String),
}

fn mating_pair(id: &str, params_json: &str, ctx: &mf_runtime::RunContext) -> MatingPair {
    let Some(connector) = mf_parts::find_generator(id) else {
        return MatingPair::Failed(format!("'{id}' isn't a built-in connector"));
    };
    let raw: BTreeMap<String, mf_spec::ParamValue> = match serde_json::from_str(params_json) {
        Ok(r) => r,
        Err(e) => return MatingPair::Failed(format!("its recorded parameters: {e}")),
    };
    let params = mf_spec::Params(raw);
    let generated = match connector.generate(&params, &mf_parts::GenContext::new(ctx)) {
        Ok(g) => g,
        Err(mf_parts::ConnectorError::Failed { message, .. }) => {
            return MatingPair::Failed(message);
        }
    };
    let a = generated.iter().find(|p| p.attach_to == mf_parts::Side::A);
    let b = generated.iter().find(|p| p.attach_to == mf_parts::Side::B);
    match (a, b) {
        (Some(a), Some(b)) => MatingPair::Found(a.mesh.clone(), b.mesh.clone()),
        _ => MatingPair::NotApplicable,
    }
}

fn fit_check_section(app: &mut MeshFixerApp, ui: &mut egui::Ui, placed: &PlacedConnector) {
    widgets::section(ui, &t!("connectors.section.fit_check"));
    let ctx = mf_runtime::RunContext::none();
    let (a, b) = match mating_pair(&placed.connector_id, &placed.connector_params_json, &ctx) {
        MatingPair::Found(a, b) => (a, b),
        MatingPair::NotApplicable => {
            widgets::caption(ui, t!("connectors.fit_check.not_applicable"));
            return;
        }
        MatingPair::Failed(reason) => {
            widgets::caption(ui, format!("Couldn't check the fit: {reason}."));
            return;
        }
    };
    let (classes, class_err) = fit_classes(app);
    let (profiles, profile_err) = machine_profiles(app);
    if let Some(e) = class_err.or(profile_err) {
        widgets::caption(
            ui,
            format!(
                "Your Fit/Machine settings couldn't be read ({e}); the check below uses the built-ins."
            ),
        );
    }
    let fit_class = classes
        .get(&app.connectors.fit_class)
        .cloned()
        .unwrap_or_else(|| FitClass::builtins()[1].clone());
    let machine = profiles
        .get(&app.connectors.machine_profile)
        .cloned()
        .unwrap_or_else(MachineProfile::uncalibrated);
    let check = (|| -> Option<FitCheck> {
        // allow-swallow: ctx is RunContext::none(), which never cancels, so Bvh::build's only error (Cancelled) can't occur here.
        let bvh = mf_geometry::Bvh::build(&a, &ctx).ok()?;
        mf_parts::check_fit(
            &t!("connectors.fit_check.pair"),
            mf_parts::MatingSurfaces {
                a: &a,
                a_bvh: &bvh,
                b: &b,
                b_to_a: &DMat4::IDENTITY,
            },
            mf_parts::FitExpectation {
                fit_class: &fit_class,
                machine: &machine,
                compensation: Compensation::None,
            },
            &ctx,
        )
        // allow-swallow: ctx is RunContext::none(), which never cancels, so check_fit's only error (Cancelled) can't occur here.
        .ok()?
    })();
    match check {
        Some(c) => {
            widgets::body(ui, c.message());
            if c.may_fuse {
                ui.add_space(theme::SPACE_XS);
                if widgets::wide_button(ui, Kind::Secondary, None, &t!("connectors.fit_check.fix"))
                    .clicked()
                {
                    let looser = classes
                        .classes
                        .iter()
                        .filter(|cl| cl.clearance > fit_class.clearance)
                        .min_by(|x, y| x.clearance.total_cmp(&y.clearance));
                    if let Some(looser) = looser {
                        app.connectors.fit_class = looser.name.clone();
                    }
                }
            }
        }
        None => {
            widgets::caption(ui, t!("connectors.fit_check.not_applicable"));
        }
    }
}

fn merge_section(app: &mut MeshFixerApp, ui: &mut egui::Ui, placed: &PlacedConnector) {
    widgets::section(ui, &t!("connectors.section.merge"));
    let doc_objects: Vec<(ObjectId, String)> = app
        .session
        .document()
        .objects()
        .iter()
        .filter(|o| o.properties.get(mf_command::GROUP) != Some(&placed.group))
        .map(|o| (o.id, o.name.clone()))
        .collect();
    let mut side_a = placed.side_a;
    let mut side_b = placed.side_b;
    ui.label(t!("connectors.side_a"));
    for (id, name) in &doc_objects {
        let on = side_a == Some(*id);
        let label = t!("connectors.side_a_option", name = name.clone());
        if widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            None,
            &label,
            ui.available_width(),
        )
        .clicked()
        {
            side_a = Some(*id);
        }
    }
    ui.add_space(theme::SPACE_XS);
    ui.label(t!("connectors.side_b"));
    for (id, name) in &doc_objects {
        let on = side_b == Some(*id);
        let label = t!("connectors.side_b_option", name = name.clone());
        if widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            None,
            &label,
            ui.available_width(),
        )
        .clicked()
        {
            side_b = Some(*id);
        }
    }
    if let Some(p) = &mut app.connectors.placed {
        p.side_a = side_a;
        p.side_b = side_b;
    }
    ui.add_space(theme::SPACE_S);
    let can = app.busy.is_none() && side_a.is_some() && side_b.is_some() && side_a != side_b;
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::CONNECTORS),
                &t!("connectors.merge"),
            )
        })
        .inner
        .clicked()
        && let (Some(a), Some(b)) = (side_a, side_b)
    {
        attach_group(app, &placed.group, a, b, placed.gap, t!("connectors.merge"));
        app.connectors.placed = None;
    }
    ui.add_space(theme::SPACE_L);
    widgets::caption(ui, t!("connectors.save_help"));
    if ui
        .add_enabled_ui(app.busy.is_none(), |ui| {
            widgets::wide_button(ui, Kind::Secondary, None, &t!("connectors.save_to_library"))
        })
        .inner
        .clicked()
    {
        save_connector_to_library(app, placed);
    }
}

fn save_connector_to_library(app: &mut MeshFixerApp, placed: &PlacedConnector) {
    let Ok(raw) = serde_json::from_str::<BTreeMap<String, mf_spec::ParamValue>>(
        &placed.connector_params_json,
    ) else {
        app.status = Some(t!("connectors.save_failed"));
        return;
    };
    let name = app
        .session
        .document()
        .object(placed.object)
        .map(|o| o.name.clone())
        .unwrap_or_else(|| placed.connector_id.clone());
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let manifest = mf_parts::PartManifest {
        id: format!("user-{slug}-{}", placed.object.0),
        name,
        version: 1,
        licence: "".into(),
        source: "Saved from the model".into(),
        kind: mf_parts::PartKind::Connector {
            connector: placed.connector_id.clone(),
            params: mf_spec::Params(raw),
        },
        attach: mf_parts::Attach {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        },
        default_scale: 1.0,
        mesh: String::new(),
        category: String::new(),
        description: String::new(),
    };
    let Some(dir) = app.settings_dir().map(|d| d.join("parts")) else {
        app.status = Some(t!("parts.save_no_settings_dir"));
        return;
    };
    match mf_parts::save_connector_as_part(&manifest, &dir) {
        Ok(_) => app.status = Some(t!("connectors.saved", name = manifest.name.clone())),
        Err(e) => app.status = Some(format!("{e}")),
    }
}

// ---------------------------------------------------------------------
// Settings: Machine profiles and Fit classes.
// ---------------------------------------------------------------------

/// "Custom" (typed W×D×H) in the print-volume preset picker, alongside
/// [`mf_parts::PRINT_VOLUME_PRESETS`]' named printers (Split's "Print
/// volume" cut mode, owner 2026-11-11): shown only as the picker's own
/// state when `volume` isn't one of the presets, never a selectable
/// button of its own (typing W×D×H below already gets there).
const CUSTOM_PRINT_VOLUME: &str = "Custom";

/// A profile's print-volume row: a preset picker (one toggle button per
/// printer, same house style as Split's plane-orientation presets in
/// `shape_tools.rs`) and the W×D×H fields themselves, always editable
/// (picking a preset is just a shortcut to typing the same numbers).
/// Returns true if `volume` changed.
fn print_volume_row(ui: &mut egui::Ui, volume: &mut [f64; 3]) -> bool {
    let mut changed = false;
    let current = mf_parts::PRINT_VOLUME_PRESETS
        .iter()
        .find(|(_, v)| *v == *volume)
        .map_or(CUSTOM_PRINT_VOLUME, |(name, _)| name);
    ui.label(format!("{}:", t!("settings.print_volume")));
    ui.horizontal_wrapped(|ui| {
        for &(name, preset) in mf_parts::PRINT_VOLUME_PRESETS {
            if ui.selectable_label(current == name, name).clicked() && *volume != preset {
                *volume = preset;
                changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        for (axis, v) in ["W", "D", "H"].into_iter().zip(volume.iter_mut()) {
            ui.label(axis);
            changed |= ui
                .add(
                    egui::DragValue::new(v)
                        .speed(1.0)
                        .range(1.0..=2000.0)
                        .suffix(" mm"),
                )
                .changed();
        }
    });
    changed
}

fn machine_profiles_page(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::body(ui, t!("settings.machine_profiles.help"));
    let Some(dir) = app.settings_dir() else {
        widgets::caption(ui, t!("parts.save_no_settings_dir"));
        return;
    };
    let (mut profiles, load_err) = machine_profiles(app);
    if let Some(e) = &load_err {
        widgets::caption(
            ui,
            format!(
                "Your saved machine profiles couldn't be read ({e}); showing the built-in only. \
                 Editing or adding one here will overwrite the file."
            ),
        );
    }
    for p in profiles.profiles.clone() {
        let editable = p.name != mf_parts::UNCALIBRATED_NAME;
        widgets::card(ui, |ui| {
            let label = if p.calibrated {
                p.name.clone()
            } else {
                t!("connectors.not_calibrated", name = p.name.clone())
            };
            widgets::section(ui, &label);
            if editable {
                let mut edited = p.clone();
                let mut changed = false;
                for (field, value, meaning) in p.fields() {
                    let mut v = value;
                    ui.horizontal(|ui| {
                        ui.label(format!("{meaning}:"));
                        changed |= ui
                            .add(egui::DragValue::new(&mut v).speed(0.01).suffix(" mm"))
                            .changed();
                    });
                    match field {
                        "outer_xy" => edited.outer_xy = v,
                        "hole_undersize" => edited.hole_undersize = v,
                        "z" => edited.z = v,
                        _ => {} // allow-swallow: fields() only ever names these three; an unknown name just has nothing to write back
                    }
                }
                if changed {
                    edited.calibrated = true;
                }
                if print_volume_row(ui, &mut edited.print_volume_mm) {
                    changed = true;
                }
                if changed && profiles.update(edited).is_ok() {
                    let _ = profiles.save(&dir); // allow-swallow: a failed save leaves the in-memory edit for this session; the next edit retries the write
                }
            } else {
                for (_, value, meaning) in p.fields() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{meaning}:"));
                        ui.label(format!("{value:+.3} mm"));
                    });
                }
                let [x, y, z] = p.print_volume_mm;
                ui.horizontal(|ui| {
                    ui.label(format!("{}:", t!("settings.print_volume")));
                    ui.label(format!("{x:.0} × {y:.0} × {z:.0} mm"));
                });
            }
        });
    }
    ui.add_space(theme::SPACE_S);
    if widgets::wide_button(
        ui,
        Kind::Secondary,
        None,
        &t!("settings.machine_profiles.new"),
    )
    .clicked()
    {
        let name = next_name(
            &profiles
                .profiles
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            "My printer",
        );
        if profiles
            .duplicate(mf_parts::UNCALIBRATED_NAME, &name)
            .is_ok()
        {
            let _ = profiles.save(&dir); // allow-swallow: a failed save leaves the new profile for this session; the next edit retries the write
            app.connectors.machine_profile = name;
        }
    }
}

fn fit_classes_page(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::body(ui, t!("settings.fit_classes.help"));
    let Some(dir) = app.settings_dir() else {
        widgets::caption(ui, t!("parts.save_no_settings_dir"));
        return;
    };
    let (mut classes, load_err) = fit_classes(app);
    if let Some(e) = &load_err {
        widgets::caption(
            ui,
            format!(
                "Your saved fit classes couldn't be read ({e}); showing the built-ins only. \
                 Editing or adding one here will overwrite the file."
            ),
        );
    }
    let builtin_names: Vec<String> = FitClass::builtins()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    for c in classes.classes.clone() {
        let editable = !builtin_names.contains(&c.name);
        widgets::card(ui, |ui| {
            widgets::section(ui, &c.name);
            if editable {
                let mut v = c.clearance;
                let changed = ui
                    .horizontal(|ui| {
                        ui.label(t!("settings.fit_classes.clearance"));
                        ui.add(egui::DragValue::new(&mut v).speed(0.01).suffix(" mm"))
                            .changed()
                    })
                    .inner;
                if changed {
                    let edited = FitClass {
                        name: c.name.clone(),
                        clearance: v,
                    };
                    if write_fit_class(&mut classes, edited).is_ok() {
                        let _ = classes.save(&dir); // allow-swallow: a failed save leaves the in-memory edit for this session; the next edit retries the write
                    }
                }
            } else {
                ui.label(format!("{:+.2} mm", c.clearance));
            }
        });
    }
    ui.add_space(theme::SPACE_S);
    if widgets::wide_button(ui, Kind::Secondary, None, &t!("settings.fit_classes.new")).clicked() {
        let name = next_name(
            &classes
                .classes
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
            "My fit",
        );
        classes.classes.push(FitClass {
            name: name.clone(),
            clearance: 0.0,
        });
        let _ = classes.save(&dir); // allow-swallow: a failed save leaves the new class for this session; the next edit retries the write
        app.connectors.fit_class = name;
    }
}

/// Replaces a user fit class's values by name (there's no `FitClasses`
/// update method, unlike `FitProfiles`/`MachineProfiles`, since fit
/// classes are new, Task 29's own addition).
fn write_fit_class(classes: &mut FitClasses, edited: FitClass) -> Result<(), ()> {
    match classes.classes.iter_mut().find(|c| c.name == edited.name) {
        Some(c) => {
            *c = edited;
            Ok(())
        }
        None => Err(()),
    }
}

/// `base`, or `base 2`, `base 3`, … until a name not already in `taken`.
fn next_name(taken: &[String], base: &str) -> String {
    if !taken.contains(&base.to_owned()) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !taken.contains(n))
        .unwrap_or_else(|| base.to_owned())
}
