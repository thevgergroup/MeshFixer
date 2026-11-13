//! The one generator trait (parts-architecture-review.md, "One
//! `Generator` trait"): a parametric shape and a multi-part connector are
//! the same thing — a param schema and a function from params to parts —
//! so every consumer (the parts library, Connect, `object.add_shape`,
//! `object.attach`, `split.place_connectors`, the checks, the previews
//! and the panels) asks one registry ([`generators`]) and never names a
//! particular id.
//!
//! - **[`Generator::generate`]** returns [`ShapePart`]s: each has a name
//!   (its role in a group, "peg", "leaf_a"), whether it's a solid or a
//!   hole ([`ShapeRole`]), which target it joins ([`TargetSlot`]: `A`,
//!   `B`, or `Neither` for a loose piece printed on its own) and its mesh.
//!   A plain shape is the one-part, `Neither` case.
//! - **The context** ([`GenContext`]) carries the run (cancellation) and,
//!   when the generator is placed against real objects, a [`Targets`]
//!   query so a generator can measure them (how far a cut face reaches
//!   along a direction) without knowing how they're stored.
//! - **Checks are data** ([`Generator::checks`]): each has an id, plain
//!   words and an optional one-click [`Fix`], which is a param patch.
//!   The generic checks every placement gets (on the cut face, edge
//!   distance, breakout, fit) live with the placement code; a generator
//!   supplies only what is its own, plus the facts those generic checks
//!   need ([`Generator::fit_probe`], [`Generator::anchor`],
//!   [`Generator::checks_breakout`]).
//! - **Placement** ([`Generator::placement_hint`]) says where the
//!   generator is offered (dropped on a surface from the library, a click
//!   or a drag on a cut face, or only by its own tool) and how it's
//!   placed, so input handling is data-driven.
//! - **Old logs** keep replaying through [`Generator::upgrade_params`],
//!   which rewrites a recorded param map from an older version of the
//!   schema before it's validated.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, ParamValue, Params};

pub use crate::connector::{ConnectorError, JoinKind, Side as TargetSlot};
pub use crate::shape::ShapeRole;

/// One generated piece.
#[derive(Debug, Clone)]
pub struct ShapePart {
    /// Its role in the group ("peg", "counterpart hole", "leaf_a"); a
    /// plain shape's one part is called "shape".
    pub name: &'static str,
    pub role: ShapeRole,
    /// The target it joins; `Neither` is a loose piece (printed on its
    /// own, never joined).
    pub attach_to: TargetSlot,
    pub mesh: Mesh,
}

impl ShapePart {
    pub fn new(name: &'static str, role: ShapeRole, attach_to: TargetSlot, mesh: Mesh) -> Self {
        ShapePart {
            name,
            role,
            attach_to,
            mesh,
        }
    }

    /// How it joins its target: a solid is unioned, a hole cut, a loose
    /// part becomes its own object.
    pub fn join(&self) -> JoinKind {
        match (self.attach_to, self.role) {
            (TargetSlot::Neither, _) => JoinKind::Separate,
            (_, ShapeRole::Solid) => JoinKind::Union,
            (_, ShapeRole::Hole) => JoinKind::Subtract,
        }
    }

    pub fn is_loose(&self) -> bool {
        self.attach_to == TargetSlot::Neither
    }
}

/// What a generator may ask about the objects its parts attach to, in its
/// own local frame and in millimetres.
pub trait Targets {
    /// How far from the generator's origin, along unit `dir` (in its own
    /// local x/y plane), the outline of `slot`'s target face is: `None`
    /// if it isn't known or the ray never meets it.
    fn boundary_along(&self, slot: TargetSlot, dir: [f64; 2]) -> Option<f64>;
}

/// What `generate` and `checks` run with.
#[derive(Clone, Copy)]
pub struct GenContext<'a> {
    pub run: &'a RunContext,
    pub targets: Option<&'a dyn Targets>,
}

impl<'a> GenContext<'a> {
    pub fn new(run: &'a RunContext) -> GenContext<'a> {
        GenContext { run, targets: None }
    }

    pub fn with_targets(run: &'a RunContext, targets: &'a dyn Targets) -> GenContext<'a> {
        GenContext {
            run,
            targets: Some(targets),
        }
    }

    /// See [`Targets::boundary_along`]; `None` without targets.
    pub fn boundary_along(&self, slot: TargetSlot, dir: [f64; 2]) -> Option<f64> {
        self.targets?.boundary_along(slot, dir)
    }
}

/// A set of param values to change: what every one-click fix is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamPatch {
    pub set: BTreeMap<String, ParamValue>,
}

impl ParamPatch {
    pub fn one(name: &str, value: ParamValue) -> ParamPatch {
        ParamPatch {
            set: BTreeMap::from([(name.to_owned(), value)]),
        }
    }

    pub fn float(name: &str, value: f64) -> ParamPatch {
        ParamPatch::one(name, ParamValue::Float(value))
    }

    /// `params` with the patch applied.
    pub fn apply(&self, params: &Params) -> Params {
        let mut out = params.clone();
        for (k, v) in &self.set {
            out.0.insert(k.clone(), v.clone());
        }
        out
    }
}

/// A one-click fix: its button's words and the params it changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub label: String,
    pub patch: ParamPatch,
}

/// One of a generator's own checks, in plain words.
#[derive(Debug, Clone, PartialEq)]
pub struct GenCheck {
    /// Stable, for tests and to tell checks apart ("dowel_length").
    pub id: String,
    pub message: String,
    pub fix: Option<Fix>,
}

/// Where a generator is offered, and how it's placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementHint {
    /// From the library, dropped on an object's surface.
    Surface,
    /// On a cut face, with a click.
    Point,
    /// On a cut face, by dragging a line across it (its direction and
    /// reach come from the drag).
    DragLine,
    /// Only by its own tool (a hinge between two picked edges).
    Manual,
}

impl PlacementHint {
    /// Placed on a cut face (Connect).
    pub fn on_cut(self) -> bool {
        matches!(self, PlacementHint::Point | PlacementHint::DragLine)
    }
}

/// How a placement holds a joint against turning (the joint-wide
/// "single round dowel" check).
#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    /// Doesn't matter for rotation (a magnet).
    None,
    /// A round pin: the joint can turn about it if it's the only one.
    /// `fix` makes this one resist turning on its own.
    RoundPin { fix: Fix },
    /// Resists turning by itself (a keyed pin, a dovetail, a screw).
    Resists,
}

/// The mating pair the exact fit check measures (`crate::check_fit`):
/// what goes in the hole, both in the generator's frame.
pub struct FitProbe {
    /// "Dowel to hole".
    pub label: String,
    pub hole: Mesh,
    pub pin: Mesh,
}

/// The drag handles Connect draws on a picked placement: which param the
/// rim handle sets (its value grows by the footprint's change in width
/// along the placement's direction) and which the depth handle sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Handles {
    pub size: Option<&'static str>,
    pub depth: Option<&'static str>,
}

/// A parametric generator: a shape, or a connector made of several parts.
pub trait Generator: Send + Sync {
    /// Stable id, namespaced (`"shape.box"`, `"joint.dowel"`).
    fn id(&self) -> &'static str;
    /// Its name in the library and the panels ("Box", "Dowel").
    fn name(&self) -> &'static str;
    /// Bumped when the generated geometry's meaning changes.
    fn version(&self) -> u32;
    /// The param schema (only `parameters:` is used).
    fn spec(&self) -> &'static OperationSpec;
    /// The parts, from validated `params`.
    fn generate(&self, params: &Params, ctx: &GenContext)
    -> Result<Vec<ShapePart>, ConnectorError>;

    /// A one-part shape's default role (for the library's icon; the
    /// person can flip it). A connector's parts carry their own.
    fn role(&self) -> ShapeRole {
        ShapeRole::Solid
    }
    /// True for a single shape (an object of its own, `part: None` in its
    /// record); false for a connector, whose parts are a linked group.
    fn is_shape(&self) -> bool {
        false
    }
    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }
    /// Kept only so old saves and logs keep working: never offered for a
    /// new placement.
    fn deprecated(&self) -> bool {
        false
    }
    /// Its own checks, beyond the generic ones every placement gets.
    fn checks(&self, _params: &Params, _ctx: &GenContext) -> Vec<GenCheck> {
        Vec::new()
    }
    /// The pair the exact fit check measures, if it has one of its own.
    fn fit_probe(&self, _params: &Params) -> Result<Option<FitProbe>, ConnectorError> {
        Ok(None)
    }
    fn anchor(&self, _params: &Params) -> Anchor {
        Anchor::None
    }
    /// Whether its holes are checked for breaking out of their piece (a
    /// groove that runs out of the piece on purpose isn't).
    fn checks_breakout(&self) -> bool {
        true
    }
    fn handles(&self) -> Handles {
        Handles::default()
    }
    /// The params shown before placing it from the library ("what's on
    /// the packet": a thread's size and length).
    fn quick_params(&self) -> &'static [&'static str] {
        &[]
    }
    /// The gap its two targets must keep where they meet once attached
    /// (a hinge's moving parts); 0 for a fixed joint.
    fn moving_gap(&self, _params: &Params) -> f64 {
        0.0
    }
    /// The params a placement dragged as a line gets (`DragLine`):
    /// `half` is half the dragged length in mm; `ctx`'s targets measure
    /// the cut face from the line's centre along its direction (local +x).
    fn line_params(&self, _half: f64, _ctx: &GenContext) -> ParamPatch {
        ParamPatch::default()
    }
    /// Rewrites `raw`, a param map recorded by an older version of this
    /// generator, into the current schema (before validation).
    fn upgrade_params(&self, _raw: &mut BTreeMap<String, String>) {}
}

// ---------------------------------------------------------------------
// The registry.
// ---------------------------------------------------------------------

fn builtin() -> &'static [&'static dyn Generator] {
    static ALL: OnceLock<Vec<&'static dyn Generator>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = crate::builtin_shapes();
        all.extend(crate::connectors::builtin_generators());
        all
    })
}

fn extra() -> &'static RwLock<Vec<&'static dyn Generator>> {
    static EXTRA: OnceLock<RwLock<Vec<&'static dyn Generator>>> = OnceLock::new();
    EXTRA.get_or_init(|| RwLock::new(Vec::new()))
}

/// Why a generator couldn't be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    #[error("a generator with the id '{0}' is already registered")]
    DuplicateId(String),
}

/// Adds a generator (a plugin's, MF-PLUG-001) after the built-ins.
/// Refuses an id already taken.
pub fn register_generator(g: &'static dyn Generator) -> Result<(), RegisterError> {
    if find_generator(g.id()).is_some() {
        return Err(RegisterError::DuplicateId(g.id().to_owned()));
    }
    extra().write().unwrap_or_else(|e| e.into_inner()).push(g);
    Ok(())
}

/// Every generator: the built-in shapes, the built-in connectors, then
/// the registered ones, in order.
pub fn generators() -> Vec<&'static dyn Generator> {
    let mut all = builtin().to_vec();
    all.extend(
        extra()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied(),
    );
    all
}

pub fn find_generator(id: &str) -> Option<&'static dyn Generator> {
    generators().into_iter().find(|g| g.id() == id)
}

/// The generators placed on a cut face (Connect), including deprecated
/// ones (old placements still show and replay).
pub fn cut_generators() -> Vec<&'static dyn Generator> {
    generators()
        .into_iter()
        .filter(|g| g.placement_hint().on_cut())
        .collect()
}

/// `params` validated against `g`'s schema after upgrading them from an
/// older version ([`Generator::upgrade_params`]).
pub fn validate_raw(
    g: &dyn Generator,
    mut raw: BTreeMap<String, String>,
) -> Result<Params, ConnectorError> {
    g.upgrade_params(&mut raw);
    g.spec().validate(&raw).map_err(|e| ConnectorError::Failed {
        code: "invalid_parameter",
        message: format!("{}'s parameters: {e}", g.name()),
    })
}

/// Recorded `params` (a saved shape's, a log's) brought up to `g`'s
/// current schema: [`Generator::upgrade_params`] applied, and any param
/// added since filled with its default. A record already current comes
/// back unchanged, so regenerating it stays Exact.
pub fn upgraded(g: &dyn Generator, params: &Params) -> Result<Params, ConnectorError> {
    let text = |v: &ParamValue| match v {
        ParamValue::Float(f) => format!("{f}"),
        other => other.to_string(),
    };
    let before: BTreeMap<String, String> =
        params.0.iter().map(|(k, v)| (k.clone(), text(v))).collect();
    let mut raw = before.clone();
    g.upgrade_params(&mut raw);
    if raw != before {
        raw.retain(|k, _| g.spec().parameter(k).is_some());
        return validate_raw(g, raw);
    }
    let missing = g
        .spec()
        .parameters
        .iter()
        .any(|p| !params.0.contains_key(&p.name));
    if !missing {
        return Ok(params.clone());
    }
    let defaults = validate_raw(g, BTreeMap::new())?;
    let mut out = params.clone();
    for (k, v) in defaults.0 {
        out.0.entry(k).or_insert(v);
    }
    Ok(out)
}

/// The outline a generator's parts make on a plane, from the parts
/// themselves (parts-architecture-review.md, "Generic preview"): the
/// convex hull, in x/y, of every part attached to `slot` cut by the plane
/// z = `z`, or of their whole x/y projection if none crosses it. What
/// Connect draws as a placement's footprint and measures its edge
/// distance with: one source of truth, so a new generator never needs a
/// hand-drawn outline.
pub fn section_outline(parts: &[ShapePart], slot: TargetSlot, z: f64) -> Vec<[f64; 2]> {
    let mine: Vec<&ShapePart> = parts.iter().filter(|p| p.attach_to == slot).collect();
    let mut points = Vec::new();
    for part in &mine {
        for (_, c) in part.mesh.faces() {
            let q = c.map(|v| part.mesh.corner_position(v));
            for i in 0..3 {
                let (a, b) = (q[i], q[(i + 1) % 3]);
                let (da, db) = (a[2] - z, b[2] - z);
                if da == 0.0 {
                    points.push([a[0], a[1]]);
                } else if da * db < 0.0 {
                    let t = da / (da - db);
                    points.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
                }
            }
        }
    }
    if points.is_empty() {
        for part in &mine {
            points.extend(part.mesh.vertices().map(|(_, p)| [p[0], p[1]]));
        }
    }
    convex_hull(points)
}

/// The convex hull of `points`, counter-clockwise, collinear points
/// dropped (Andrew's monotone chain).
pub fn convex_hull(mut points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
    if points.len() < 3 {
        return points;
    }
    let span = points
        .iter()
        .fold(0.0_f64, |m, p| m.max(p[0].abs()).max(p[1].abs()))
        .max(1.0);
    let eps = 1e-12 * span * span;
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &[f64; 2]>> = if pass == 0 {
            Box::new(points.iter())
        } else {
            Box::new(points.iter().rev())
        };
        for &p in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= eps
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}
