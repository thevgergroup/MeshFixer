//! The Align tool (Phase 1 modelling tools brief, Tinkercad-style
//! Align), in the Arrange group, shortcut L: dot handles on the
//! selection's (or the anchor's) world bounding box, one per axis at
//! min/centre/max; hovering one previews every target's new position,
//! clicking it applies `object.align`. Clicking a selected object in the
//! view makes it the anchor (highlighted; the dots move to its box; it
//! never moves itself). The panel's Distribute section runs Equal gaps
//! or Equal centres per axis.
//!
//! Registered through `shell::Tool` exactly like the Shape tools
//! (`shape_tools.rs`): an icon, a panel, and a share of the viewport's
//! input (`claims`/`viewport_input`) for the dots and the anchor pick.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::{Invocation, ObjectId};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, Claims, Input, RegisterError, Shell, Tool as T};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const ID: &str = "align";

/// One dot handle: which axis, where on it (min/centre/max), and its
/// world position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    pub axis: usize,
    pub mode: Mode,
    pub world: DVec3,
}

/// What a dot, or the panel's one-shot buttons, asks `object.align` to
/// do (the spec's `mode` values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Min,
    Centre,
    Max,
    DistributeGaps,
    DistributeCentres,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Min => "min",
            Mode::Centre => "centre",
            Mode::Max => "max",
            Mode::DistributeGaps => "distribute_gaps",
            Mode::DistributeCentres => "distribute_centres",
        }
    }
}

/// The Align tool's state.
#[derive(Debug, Clone, Default)]
pub struct AlignState {
    /// Clicked in the view while Align is open: one of the selected
    /// objects, whose box the dots sit on and which never moves. Cleared
    /// when the selection changes or the tool closes.
    pub anchor: Option<ObjectId>,
    /// The dot under the pointer this frame.
    pub hover: Option<Dot>,
    /// Every target's id and the translation-only transform it would end
    /// up at if the hovered dot (or a just-clicked one, until the command
    /// commits) were applied: the ghost preview, drawn through
    /// `MeshFixerApp::shown_transform` exactly like a live drag.
    pub preview: Vec<(ObjectId, [f64; 12])>,
}

fn available(app: &MeshFixerApp) -> Availability {
    if app.selected_objects().len() >= 2 {
        Availability::Available
    } else {
        Availability::Unavailable(t!("align.select_more"))
    }
}

pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_tool(T {
        shortcut: Some(egui::Key::L),
        help: "tool.align.help",
        availability: available,
        panel: Some(panel),
        claims: |app| {
            let dots = dots(app);
            if dots.is_empty() {
                return None;
            }
            let (cam, rect) = (app.camera?, app.viewport.last_rect?);
            let view = (rect.width() as f64, rect.height() as f64);
            Some(Box::new(move |px: (f64, f64)| {
                hit_dot(&dots, &cam, view, px).is_some()
            }) as Claims)
        },
        viewport_input: input,
        on_open: |app| {
            app.align = AlignState::default();
            app.refusal = None;
        },
        on_close: |app, _| {
            app.align.hover = None;
            app.align.preview.clear();
        },
        ..T::new(ID, "tool.align.name", icons::ALIGN, Category::Arrange, 20)
    })?;
    Ok(())
}

// ---------------------------------------------------------------------
// Geometry: the box the dots sit on, and the dots themselves.
// ---------------------------------------------------------------------

/// The world bounding box the dots sit on: the anchor's, if one is set
/// and still selected; otherwise the overall bounds of every selected
/// object.
fn dot_box(app: &MeshFixerApp) -> Option<(DVec3, DVec3)> {
    let selected = app.selected_objects();
    if selected.len() < 2 {
        return None;
    }
    if let Some(anchor) = app.align.anchor
        && selected.contains(&anchor)
    {
        return crate::shape_tools::world_bounds(app, anchor);
    }
    let mut bounds: Option<(DVec3, DVec3)> = None;
    for &id in &selected {
        let Some((lo, hi)) = crate::shape_tools::world_bounds(app, id) else {
            continue;
        };
        bounds = Some(match bounds {
            Some((blo, bhi)) => (blo.min(lo), bhi.max(hi)),
            None => (lo, hi),
        });
    }
    bounds
}

/// One axis's rail: the box edge its 3 dots sit on, as a world-space
/// line segment from its low end to its high end (`world` field order
/// matches `Mode::Min`/`Mode::Max`: `ends.0` is always the low end,
/// `ends.1` the high end, whichever corner of the box that turned out to
/// be — see [`rails`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rail {
    pub axis: usize,
    pub ends: (DVec3, DVec3),
}

/// Whether the box's `lo`-side face on `axis` (outward normal pointing
/// in `axis`'s negative direction) is back-facing from `eye` — i.e.
/// hidden behind the box from the current view: `dot(-axis, eye - mid)
/// <= 0`, the standard convex-hull back-face test, done per axis
/// without needing a normal vector (just which side of `mid[axis]` the
/// eye sits on).
fn back_facing_lo(axis: usize, mid: DVec3, eye: DVec3) -> bool {
    eye[axis] >= mid[axis]
}

/// The box corner (as `lo`/`hi` picks on the two axes named by `other`,
/// a rail's own axis's "other two") its rail runs along: the brief's
/// own fixed corner (`lo`/`lo`) unless the camera genuinely can't see
/// that edge — both its adjacent faces back-facing at once, the only
/// case a box edge is actually hidden rather than merely not the
/// nearest — in which case it flips to the diagonally opposite corner on
/// those two axes (`hi`/`hi`), which is then guaranteed front-facing on
/// both (a box's two "visible extreme" corners, along any one axis, are
/// always this pair).
fn rail_corner(other: (usize, usize), lo: DVec3, hi: DVec3, eye: DVec3) -> (f64, f64) {
    let mid = (lo + hi) * 0.5;
    let (oa, ob) = other;
    let hidden = back_facing_lo(oa, mid, eye) && back_facing_lo(ob, mid, eye);
    if hidden {
        (hi[oa], hi[ob])
    } else {
        (lo[oa], lo[ob])
    }
}

/// How far apart [`rails`] nudges the three rails' shared corner, as a
/// fraction of the box's own diagonal (kept visually proportionate to
/// the model, not a fixed world size): small enough that the three
/// rails still read as meeting at one corner, but never so small that
/// float error could still land two dots on the same point.
const RAIL_CORNER_SPREAD_FRAC: f64 = 0.04;
/// The floor under that nudge in world units, for a box so small (or so
/// flat on two axes — exactly `dot_box`'s own common case: objects
/// spread out on only one axis, so the box is a sliver on the other two)
/// that a fraction of its diagonal would otherwise round away to nothing.
const RAIL_CORNER_SPREAD_MIN: f64 = 1e-3;

/// The 3 axis rails the dots sit on (see [`dots`]): a different box edge
/// per axis, meeting by default at the box's own min corner (owner
/// brief: X along the bottom-front edge, Y along the bottom-left edge, Z
/// up the front-left vertical edge — this app's own front=-Y/bottom=-Z/
/// left=-X convention, `edge_word`'s), each flipped to the opposite
/// corner on its own two cross-axes if the camera genuinely can't see
/// that edge (`rail_corner`), so the rails (and the dots on them) are
/// never stranded on the far side of the mesh.
///
/// Each rail's own cross-position is nudged a little further from the
/// shared corner along exactly one of its two cross-axes (X's own rail
/// nudges on Z; Y's on X; Z's on Y — a stable, distinct choice per axis,
/// `nudge_axis` below), so the three rails meet *near* one corner
/// without any two ever sharing an exact point. Without this, three
/// objects spread out on only one axis (`dot_box`'s box is then a sliver
/// on the other two, e.g. three same-radius balls placed apart on X
/// alone) put every rail's own min corner at the identical world point:
/// `hit_dot` would then pick whichever axis's dot happened to iterate
/// first for a click plainly meant for a different axis — the dot the
/// owner brief's tests, and a real user, click by its drawn position,
/// not by winning a tie-break (fix round 2026-11-13, found by
/// `align_min_centre_max_on_each_axis` timing out on "nothing to
/// change" for the wrong axis). `None` with fewer than 2 objects
/// selected, same as [`dots`].
pub fn rails(app: &MeshFixerApp) -> Option<[Rail; 3]> {
    let (lo, hi) = dot_box(app)?;
    let eye = app.camera.map(|c| c.eye()).unwrap_or(hi + DVec3::ONE);
    let diagonal = (hi - lo).length();
    let spread = (diagonal * RAIL_CORNER_SPREAD_FRAC).max(RAIL_CORNER_SPREAD_MIN);
    // (this axis's rail, the other two axes, in a stable order so every
    // rail's cross-position is picked deterministically) and which of
    // those two axes this rail nudges outward.
    let others = [(1usize, 2usize), (0usize, 2usize), (0usize, 1usize)];
    let nudge_axis = [2usize, 0usize, 1usize];
    let mut out = [Rail {
        axis: 0,
        ends: (DVec3::ZERO, DVec3::ZERO),
    }; 3];
    for axis in 0..3 {
        let (oa, ob) = others[axis];
        let (ca, cb) = rail_corner((oa, ob), lo, hi, eye);
        let mut low_end = lo;
        low_end[oa] = ca;
        low_end[ob] = cb;
        low_end[axis] = lo[axis];
        // Nudge towards the box's own centre on `nudge_axis`, whichever
        // corner `rail_corner` picked for it: moving towards `mid` can
        // never cross past it (clamped below), so this always lands
        // strictly between the picked corner and the centre, inside the
        // box, whether that corner was `lo` or (flipped) `hi`.
        let n = nudge_axis[axis];
        let mid_n = (lo[n] + hi[n]) * 0.5;
        let towards_mid = (mid_n - low_end[n]).clamp(-spread, spread);
        low_end[n] += towards_mid;
        let mut high_end = low_end;
        high_end[axis] = hi[axis];
        out[axis] = Rail {
            axis,
            ends: (low_end, high_end),
        };
    }
    Some(out)
}

/// The 9 dot handles (3 axes x min/centre/max), each exactly on its
/// axis's rail (see [`rails`]) at that rail's low end, midpoint, or high
/// end — no two dots share a point, since `rails` nudges each rail's own
/// cross-position a little apart from the others near their shared
/// corner (`RAIL_CORNER_SPREAD_FRAC`).
pub fn dots(app: &MeshFixerApp) -> Vec<Dot> {
    let Some(the_rails) = rails(app) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(9);
    for rail in the_rails {
        let (low, high) = rail.ends;
        let mid = (low + high) * 0.5;
        for (mode, world) in [(Mode::Min, low), (Mode::Centre, mid), (Mode::Max, high)] {
            out.push(Dot {
                axis: rail.axis,
                mode,
                world,
            });
        }
    }
    out
}

/// The dot (if any) within a few screen points of `px`.
fn hit_dot(
    dots: &[Dot],
    cam: &mf_render::OrbitCamera,
    view: (f64, f64),
    px: (f64, f64),
) -> Option<Dot> {
    const TOLERANCE: f64 = 10.0;
    dots.iter()
        .copied()
        .filter_map(|d| {
            let p = mf_render::screen_position(cam, cam.target, view, d.world.to_array())?;
            let dist = (p.0 - px.0).hypot(p.1 - px.1);
            (dist <= TOLERANCE).then_some((dist, d))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, d)| d)
}

// ---------------------------------------------------------------------
// Alignment maths, mirrored from `mf_operations::Align` (client-side,
// for the live hover preview before Apply; the operation is what
// actually commits — this never writes the document).
// ---------------------------------------------------------------------

/// Every selected object's world bounding box, in `selected_objects`'s
/// order, measured from the *document's own* transform (never a live
/// drag or hover preview — see [`crate::shape_tools::world_bounds_committed`]):
/// `None` for an object with no geometry yet.
fn target_boxes(app: &MeshFixerApp) -> Vec<(ObjectId, Option<(DVec3, DVec3)>)> {
    app.selected_objects()
        .into_iter()
        .map(|id| (id, crate::shape_tools::world_bounds_committed(app, id)))
        .collect()
}

/// The translation-only transform each selected object would end up at
/// for `axis`/`mode`, anchored as [`dot_box`] decides (the same rule the
/// `object.align` operation uses): `(object id, new transform)`, one per
/// object with geometry.
///
/// Measures every box from the document's own committed transform, not
/// [`crate::MeshFixerApp::shown_transform`] (which would prefer this very
/// preview's own last answer once Apply has committed but the pointer
/// never left the dot — a feedback loop that never converges, since
/// each frame's preview would be computed on top of the previous
/// frame's preview instead of the real document, D-ALIGN-2 fix round
/// 2026-11-13).
pub fn preview_for(app: &MeshFixerApp, axis: usize, mode: Mode) -> Vec<(ObjectId, [f64; 12])> {
    let targets = target_boxes(app);
    let anchor_box = app
        .align
        .anchor
        .filter(|&a| app.selected_objects().contains(&a))
        .and_then(|a| crate::shape_tools::world_bounds_committed(app, a));
    match mode {
        Mode::Min | Mode::Centre | Mode::Max => {
            let value_of = |lo: f64, hi: f64| match mode {
                Mode::Min => lo,
                Mode::Max => hi,
                _ => (lo + hi) * 0.5,
            };
            let target_value = match anchor_box {
                Some((lo, hi)) => value_of(lo[axis], hi[axis]),
                None => {
                    let mut lo = f64::INFINITY;
                    let mut hi = f64::NEG_INFINITY;
                    for (_, b) in &targets {
                        if let Some((blo, bhi)) = b {
                            lo = lo.min(blo[axis]);
                            hi = hi.max(bhi[axis]);
                        }
                    }
                    value_of(lo, hi)
                }
            };
            targets
                .into_iter()
                .filter_map(|(id, b)| {
                    let (lo, hi) = b?;
                    let own = value_of(lo[axis], hi[axis]);
                    let delta = target_value - own;
                    let t = app.session.document().object(id)?.transform;
                    Some((id, translated(t, axis, delta)))
                })
                .collect()
        }
        Mode::DistributeGaps | Mode::DistributeCentres => {
            let mut order: Vec<(ObjectId, DVec3, DVec3)> = targets
                .into_iter()
                .filter_map(|(id, b)| b.map(|(lo, hi)| (id, lo, hi)))
                .collect();
            order.sort_by(|a, b| {
                let ca = (a.1[axis] + a.2[axis]) * 0.5;
                let cb = (b.1[axis] + b.2[axis]) * 0.5;
                ca.total_cmp(&cb)
            });
            if order.len() < 2 {
                return Vec::new();
            }
            let mut out = Vec::new();
            let (first_id, first_lo, first_hi) = order[0];
            let (last_id, last_lo, last_hi) = *order.last().expect("at least two");
            let between = &order[1..order.len() - 1];
            if mode == Mode::DistributeGaps {
                let span_lo = first_hi[axis];
                let span_hi = last_lo[axis];
                let widths: f64 = between.iter().map(|(_, lo, hi)| hi[axis] - lo[axis]).sum();
                let gap = if between.is_empty() {
                    0.0
                } else {
                    ((span_hi - span_lo - widths) / (between.len() + 1) as f64).max(0.0)
                };
                let mut cursor = span_lo + gap;
                for &(id, lo, hi) in between {
                    let width = hi[axis] - lo[axis];
                    let delta = cursor - lo[axis];
                    if let Some(t) = app.session.document().object(id).map(|o| o.transform) {
                        out.push((id, translated(t, axis, delta)));
                    }
                    cursor += width + gap;
                }
            } else {
                let c_lo = (first_lo[axis] + first_hi[axis]) * 0.5;
                let c_hi = (last_lo[axis] + last_hi[axis]) * 0.5;
                let n = between.len() + 1;
                for (i, &(id, lo, hi)) in between.iter().enumerate() {
                    let centre = (lo[axis] + hi[axis]) * 0.5;
                    let want = c_lo + (c_hi - c_lo) * (i + 1) as f64 / n as f64;
                    let delta = want - centre;
                    if let Some(t) = app.session.document().object(id).map(|o| o.transform) {
                        out.push((id, translated(t, axis, delta)));
                    }
                }
            }
            let _ = (first_id, last_id);
            out
        }
    }
}

fn translated(mut t: [f64; 12], axis: usize, delta: f64) -> [f64; 12] {
    t[9 + axis] += delta;
    t
}

// ---------------------------------------------------------------------
// Running the command.
// ---------------------------------------------------------------------

/// The axis's name ("X"/"Y"/"Z") for the History label and the panel.
fn axis_name(axis: usize) -> &'static str {
    crate::transform_tool::AXIS_NAMES[axis]
}

/// The edge/centre word for the History label ("left", "right", "centre",
/// "bottom", "top", "front", "back"), from the axis and mode: X follows
/// Transform's own left=-X/right=+X convention; Y and Z use
/// front/back and bottom/top the same way the rest of the app orients
/// them (`theme::AXIS_*`'s order, Z up).
fn edge_word(axis: usize, mode: Mode) -> &'static str {
    match (axis, mode) {
        (_, Mode::Centre) => "centre",
        (0, Mode::Min) => "left",
        (0, Mode::Max) => "right",
        (1, Mode::Min) => "front",
        (1, Mode::Max) => "back",
        (2, Mode::Min) => "bottom",
        (2, Mode::Max) => "top",
        _ => "centre",
    }
}

/// The History label for aligning `n` objects (an anchor named `anchor`,
/// or `None` for the selection's own bounds): "Aligned 3 objects to the
/// left edge of base", "Aligned 2 objects to the centre on Y",
/// "Distributed 4 objects with equal gaps on X".
fn history_label(n: usize, axis: usize, mode: Mode, anchor: Option<&str>) -> String {
    let objects = if n == 1 { "object" } else { "objects" };
    match mode {
        Mode::DistributeGaps => format!("Distributed {n} {objects} with equal gaps on {}", axis_name(axis)),
        Mode::DistributeCentres => {
            format!("Distributed {n} {objects} with equal centres on {}", axis_name(axis))
        }
        Mode::Centre => match anchor {
            Some(name) => format!("Aligned {n} {objects} to the centre of {name}"),
            None => format!("Aligned {n} {objects} to the centre on {}", axis_name(axis)),
        },
        Mode::Min | Mode::Max => {
            let edge = edge_word(axis, mode);
            match anchor {
                Some(name) => format!("Aligned {n} {objects} to the {edge} edge of {name}"),
                None => format!("Aligned {n} {objects} to the {edge} edge on {}", axis_name(axis)),
            }
        }
    }
}

/// Every selected object, expanded with any linked-group members not
/// already selected (`object.move_group`'s own job, reused here so a
/// connector's peg and hole keep their relative placement when the group
/// is aligned): `(targets in order, leader index per target or `None`)`.
fn targets_with_groups(app: &MeshFixerApp) -> (Vec<ObjectId>, Vec<Option<usize>>) {
    let doc = app.session.document();
    let selected = app.selected_objects();
    let mut targets = selected.clone();
    let mut leaders: Vec<Option<usize>> = vec![None; selected.len()];
    for (k, &id) in selected.iter().enumerate() {
        let Some(group) = doc
            .object(id)
            .and_then(|o| o.properties.get(mf_command::GROUP))
            .cloned()
        else {
            continue;
        };
        for o in doc.objects() {
            if o.properties.get(mf_command::GROUP) == Some(&group)
                && !targets.contains(&o.id)
            {
                targets.push(o.id);
                leaders.push(Some(k));
            }
        }
    }
    (targets, leaders)
}

/// Applies `axis`/`mode` now: builds and runs `object.align` on every
/// selected object (plus linked-group members), with the current anchor.
pub fn apply(app: &mut MeshFixerApp, axis: usize, mode: Mode) {
    let selected = app.selected_objects();
    if selected.len() < 2 || app.busy.is_some() {
        return;
    }
    let anchor = app.align.anchor.filter(|&a| selected.contains(&a));
    let anchor_name = anchor
        .and_then(|a| app.session.document().object(a))
        .map(|o| o.name.clone());
    // The anchor is its own operand, never one of the (`many`) targets:
    // drop it, and its leader entry, from the target list the operation
    // moves. Dropping an entry shifts every later index, so leader
    // indices (which point into this same list) are remapped, not just
    // copied across.
    let (all_targets, all_leaders) = targets_with_groups(app);
    let mut old_to_new: Vec<Option<usize>> = Vec::with_capacity(all_targets.len());
    let mut targets = Vec::with_capacity(all_targets.len());
    for &id in &all_targets {
        if Some(id) == anchor {
            old_to_new.push(None);
        } else {
            old_to_new.push(Some(targets.len()));
            targets.push(id);
        }
    }
    let leaders: Vec<Option<usize>> = all_targets
        .iter()
        .zip(&all_leaders)
        .filter(|&(&id, _)| Some(id) != anchor)
        .map(|(_, leader)| leader.and_then(|old| old_to_new[old]))
        .collect();
    if targets.len() < 2 {
        return;
    }
    let Some((&first, rest)) = targets.split_first() else {
        return;
    };
    let mut operands: Vec<(String, ObjectId, Option<mf_selection::Selection>)> = rest
        .iter()
        .map(|&id| ("target".to_owned(), id, None))
        .collect();
    if let Some(a) = anchor {
        operands.push(("anchor".to_owned(), a, None));
    }
    let leaders_json = serde_json::to_string(&leaders).expect("leaders always serialize");
    let params = BTreeMap::from([
        ("axis".to_owned(), ["x", "y", "z"][axis].to_owned()),
        ("mode".to_owned(), mode.as_str().to_owned()),
        ("leaders".to_owned(), leaders_json),
    ]);
    let label = history_label(selected.len(), axis, mode, anchor_name.as_deref());
    app.align.preview.clear();
    app.run_invocation(
        Invocation {
            op_id: "object.align".into(),
            target: first,
            selection: None,
            operands,
            raw_params: params,
        },
        label,
    );
}

// ---------------------------------------------------------------------
// Viewport input: hovering and clicking a dot; clicking a selected
// object sets the anchor.
// ---------------------------------------------------------------------

fn input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    use crate::viewport::ToolPointer;
    let the_dots = dots(app);
    let (Some(cam), Some(rect)) = (app.camera, app.viewport.last_rect) else {
        return Input::Pass;
    };
    let view = (rect.width() as f64, rect.height() as f64);
    match input.pointer {
        Some(ToolPointer::Click(px)) => {
            if let Some(dot) = hit_dot(&the_dots, &cam, view, px) {
                apply(app, dot.axis, dot.mode);
            }
        }
        None => {}
        _ => {}
    }
    app.align.hover = input.hover.and_then(|px| hit_dot(&the_dots, &cam, view, px));
    app.align.preview = match app.align.hover {
        Some(d) => preview_for(app, d.axis, d.mode),
        None => Vec::new(),
    };
    Input::Consumed
}

/// The world-space point where a screen click lands on the nearest
/// selected object's own surface, if any — used only to tell *which*
/// selected object a click (that missed every dot) landed on, so it can
/// become the anchor (the same synchronous ray-pick
/// `shape_tools::surface_click_point` uses for Split's "click a side").
fn pick_selected_object(
    app: &MeshFixerApp,
    cam: &mf_render::OrbitCamera,
    view: (f64, f64),
    px: (f64, f64),
) -> Option<ObjectId> {
    let (origin, dir) = cam.ray(cam.target, view, px);
    let (origin, dir) = (DVec3::from(origin), DVec3::from(dir));
    app.selected_objects()
        .into_iter()
        .filter_map(|id| {
            let bvh = app.picking_bvh(id)?;
            let mesh = app.session.document().object(id)?.mesh()?;
            let transform = app.shown_transform(id).unwrap_or_else(|| {
                app.session
                    .document()
                    .object(id)
                    .map(|o| o.transform)
                    // allow-swallow: id just came from selected_objects, which only names live objects; a vanished one falls back to identity rather than panicking this frame.
                    .unwrap_or_default()
            });
            let model = mf_render::model_matrix(&transform);
            let (_, t) = bvh.ray_pick(mesh, origin.to_array(), dir.to_array(), &model)?;
            Some((id, t))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// A click in the view that neither a dot nor the ordinary pick claimed:
/// if it lands on one of the selected objects' own surface, that object
/// becomes the anchor (owner brief: "Clicking one of the selected
/// objects in the view while Align is open makes it the anchor"). Called
/// from the app's own click handling (`MeshFixerApp::click`), ahead of
/// the asynchronous pick-and-select path, so Align's anchor pick never
/// races a background worker or changes the selection.
pub fn anchor_click(app: &mut MeshFixerApp, px: (f64, f64)) -> bool {
    if app.active_tool != ID {
        return false;
    }
    let (Some(cam), Some(rect)) = (app.camera, app.viewport.last_rect) else {
        return false;
    };
    let view = (rect.width() as f64, rect.height() as f64);
    let Some(id) = pick_selected_object(app, &cam, view, px) else {
        return false;
    };
    // Clicking the anchor again clears it (back to the selection's own
    // overall bounds).
    app.align.anchor = (app.align.anchor != Some(id)).then_some(id);
    true
}

// ---------------------------------------------------------------------
// The panel.
// ---------------------------------------------------------------------

fn section(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_space(theme::SPACE_M);
    let r = ui.label(
        RichText::new(text)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    ui.add_space(2.0);
    r
}

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    crate::shape_tools::refusal_card_for(app, ui, "object.align");
    let selected = app.selected_objects();
    if selected.len() < 2 {
        widgets::body(ui, t!("align.select_more"));
        return;
    }
    widgets::body(ui, t!("align.what"));
    let anchor_name = app
        .align
        .anchor
        .filter(|&a| selected.contains(&a))
        .and_then(|a| app.session.document().object(a))
        .map(|o| o.name.clone());
    match &anchor_name {
        Some(name) => {
            widgets::caption(ui, t!("align.anchor_set", name = name.clone()));
        }
        None => {
            widgets::caption(ui, t!("align.anchor_hint"));
        }
    }
    section(ui, &t!("align.section.align"));
    widgets::caption(ui, t!("align.dots_hint"));
    for axis in 0..3 {
        ui.horizontal(|ui| {
            let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][axis];
            ui.label(RichText::new(axis_name(axis)).color(color));
            let w = (ui.available_width() - 3.0 * ui.spacing().item_spacing.x) / 3.0;
            for (mode, label) in [
                (Mode::Min, t!("align.min")),
                (Mode::Centre, t!("align.centre")),
                (Mode::Max, t!("align.max")),
            ] {
                let r = widgets::button(ui, Kind::Secondary, None, &label, w);
                // Each axis's own accessible name ("Align X: Centre"):
                // the visible text repeats per axis row, but every
                // button still needs a name that's unique in the panel.
                let full = format!("Align {}: {label}", axis_name(axis));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &full));
                if r.clicked() {
                    apply(app, axis, mode);
                }
            }
        });
    }
    section(ui, &t!("align.section.distribute"));
    widgets::caption(ui, t!("align.distribute_hint"));
    for axis in 0..3 {
        ui.horizontal(|ui| {
            let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][axis];
            ui.label(RichText::new(axis_name(axis)).color(color));
            let can = selected.len() >= 3;
            ui.add_enabled_ui(can, |ui| {
                let w = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 2.0;
                let gaps = widgets::button(ui, Kind::Secondary, None, &t!("align.equal_gaps"), w);
                let gaps_name = format!("Distribute {}: {}", axis_name(axis), t!("align.equal_gaps"));
                gaps.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, can, &gaps_name));
                if gaps.clicked() {
                    apply(app, axis, Mode::DistributeGaps);
                }
                let centres =
                    widgets::button(ui, Kind::Secondary, None, &t!("align.equal_centres"), w);
                let centres_name =
                    format!("Distribute {}: {}", axis_name(axis), t!("align.equal_centres"));
                centres
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, can, &centres_name));
                if centres.clicked() {
                    apply(app, axis, Mode::DistributeCentres);
                }
            });
        });
    }
    if selected.len() < 3 {
        widgets::caption(ui, t!("align.distribute_needs_three"));
    }
}

// ---------------------------------------------------------------------
// The view overlay (Tinkercad-style, fix round 2026-11-13: the dots used
// to float free with nothing tying them to an axis): the dots' box as a
// faint wireframe, one coloured axis rail per axis along a box edge (see
// `rails`), the 9 dots on their rails, the hovered one larger with a
// label, and (still) the anchor's own box outlined more strongly.
// ---------------------------------------------------------------------

pub fn view_overlay(app: &MeshFixerApp, ctx: &egui::Context) {
    if app.active_tool != ID {
        return;
    }
    let (Some(rect), Some(cam)) = (app.viewport.last_rect, app.camera) else {
        return;
    };
    let view = (rect.width() as f64, rect.height() as f64);
    let Some((lo, hi)) = dot_box(app) else {
        return;
    };
    let Some(the_rails) = rails(app) else {
        return;
    };
    let the_dots = dots(app);
    if the_dots.is_empty() {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::unique("align overlay"),
    ));
    let painter = painter.with_clip_rect(rect);
    let at = |p: DVec3| {
        mf_render::screen_position(&cam, cam.target, view, p.to_array())
            .map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    // The box itself, faint: it's what ties every rail and dot to an
    // axis, so it's always drawn, not just when there's an anchor.
    draw_box(&painter, &cam, view, rect, lo, hi, theme::TEXT_MUTED.gamma_multiply(0.35));
    // One coloured rail per axis, along the edge `rails` picked.
    for rail in &the_rails {
        let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][rail.axis];
        if let (Some(a), Some(b)) = (at(rail.ends.0), at(rail.ends.1)) {
            painter.line_segment([a, b], egui::Stroke::new(2.5, color.gamma_multiply(0.9)));
        }
    }
    for dot in &the_dots {
        let Some(p) = at(dot.world) else { continue };
        let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][dot.axis];
        let hovered = app.align.hover == Some(*dot);
        let radius = if hovered { 7.0 } else { 5.0 };
        painter.circle_filled(p, radius, color);
        painter.circle_stroke(p, radius, egui::Stroke::new(1.5, theme::TEXT_STRONG));
        if hovered {
            hover_label(&painter, p, dot.axis, dot.mode);
        }
    }
    if let Some(id) = app.align.anchor
        && app.selected_objects().contains(&id)
        && let Some((alo, ahi)) = crate::shape_tools::world_bounds(app, id)
    {
        draw_box(&painter, &cam, view, rect, alo, ahi, theme::SELECTED);
    }
}

/// The hovered dot's label, just above it: "X min: align left edges", "X
/// centre", "X max: align right edges" (owner brief's own examples) —
/// built from the same `axis_name`/`edge_word` the History label and the
/// panel already use, through the `align.hover.*` catalogue entries, so
/// it's one of the few things in the overlay that reads through `t!`
/// rather than being painted from a raw literal.
fn hover_label(painter: &egui::Painter, dot_pos: egui::Pos2, axis: usize, mode: Mode) {
    let text = match mode {
        Mode::Centre => t!("align.hover.centre", axis = axis_name(axis)),
        Mode::Min => t!(
            "align.hover.min",
            axis = axis_name(axis),
            edge = edge_word(axis, mode)
        ),
        Mode::Max => t!(
            "align.hover.max",
            axis = axis_name(axis),
            edge = edge_word(axis, mode)
        ),
        Mode::DistributeGaps | Mode::DistributeCentres => return,
    };
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(theme::TYPE_CAPTION),
        theme::TEXT_STRONG,
    );
    let pad = egui::vec2(6.0, 3.0);
    let size = galley.size() + 2.0 * pad;
    let rect = egui::Rect::from_min_size(
        dot_pos - egui::vec2(size.x / 2.0, size.y + 12.0),
        size,
    );
    painter.rect_filled(rect, theme::RADIUS, theme::PANEL.gamma_multiply(0.95));
    painter.galley(rect.min + pad, galley, theme::TEXT_STRONG);
}

/// A world box's 12 edges as screen lines, for the anchor's highlight.
fn draw_box(
    painter: &egui::Painter,
    cam: &mf_render::OrbitCamera,
    view: (f64, f64),
    rect: egui::Rect,
    lo: DVec3,
    hi: DVec3,
    color: egui::Color32,
) {
    let corner = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    let at = |p: DVec3| {
        mf_render::screen_position(cam, cam.target, view, p.to_array())
            .map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    const EDGES: [(usize, usize); 12] = [
        (0, 1), (0, 2), (0, 4), (1, 3), (1, 5), (2, 3),
        (2, 6), (3, 7), (4, 5), (4, 6), (5, 7), (6, 7),
    ];
    for (a, b) in EDGES {
        if let (Some(pa), Some(pb)) = (at(corner(a)), at(corner(b))) {
            painter.line_segment([pa, pb], egui::Stroke::new(1.5, color.gamma_multiply(0.8)));
        }
    }
}
