//! Parametric shapes as first-class objects (owner-approved 2026-11-11,
//! `parametric-shapes-brief.md`): a placed shape keeps its recipe, the
//! generator id plus its params ([`GeneratorRecord`], saved as the
//! object's `generator` property), so selecting it shows those params
//! and editing one regenerates the mesh (`object.set_shape_params`).
//!
//! - **Shapes** ([`Shape`]) generate one closed mesh each, in the same
//!   frame as the parts library's built-ins: sitting on z = 0, centred on
//!   the z axis, growing along +z. A hole-like shape (a pocket, a
//!   threaded hole) instead reaches *down* from z = 0 into whatever it's
//!   placed on, its axis the local z, so the teardrop profile
//!   ([`crate::connectors::hole_profile`]) can read its tilt.
//! - **Connector parts** are generators too: a record with `part` set
//!   names a connector ([`crate::Connector`]) and one of its parts by
//!   role, so a connector placed as a linked group of objects (a peg on
//!   side A, its hole on side B; a hinge's two leaves and its pin) keeps
//!   each member parametric, all regenerated together from one edit.
//! - **The registry** is the one generator registry
//!   ([`crate::generators`], [`crate::find_generator`]): shapes and
//!   connectors alike, the desktop exposing it through its shell.

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, ParamKind, ParamSpec, ParamValue, Params, RuntimeSpec, TargetSpec};
use serde::{Deserialize, Serialize};

use crate::connector::ConnectorError;

/// A placed shape's recipe: what made its mesh and with what (the
/// object's `generator` property, as JSON, D16). `version` is the
/// generator's own version when the record was made: a shape (or
/// connector) whose meaning changed refuses an old record by name
/// rather than silently building something else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratorRecord {
    pub id: String,
    pub version: u32,
    pub params: Params,
    /// For a member of a connector group: which of the connector's
    /// parts (its role, "peg", "leaf_a", ...) this object is. `None`
    /// for a plain library shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

impl GeneratorRecord {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a generator record always serializes")
    }

    pub fn from_json(text: &str) -> Result<GeneratorRecord, ConnectorError> {
        serde_json::from_str(text).map_err(|e| ConnectorError::Failed {
            code: "invalid_generator",
            message: format!("the shape's recorded recipe isn't valid: {e}"),
        })
    }
}

/// Whether a shape is placed as a solid (joined on Combine) or a hole
/// (cut on Combine). Its default only: the person can flip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeRole {
    Solid,
    Hole,
}

/// The record a new shape or connector gets: `g`'s own version and its
/// spec's defaults, overridden by `raw` (upgraded from an older schema,
/// then validated, so an unknown name or an out-of-range value is
/// refused by name).
pub fn new_record(
    g: &dyn crate::Generator,
    raw: &std::collections::BTreeMap<String, String>,
) -> Result<GeneratorRecord, ConnectorError> {
    let params = crate::generator::validate_raw(g, raw.clone())?;
    Ok(GeneratorRecord {
        id: g.id().to_owned(),
        version: g.version(),
        params,
        part: None,
    })
}

/// The spec a record's params are checked against.
pub fn record_spec(record: &GeneratorRecord) -> Result<&'static OperationSpec, ConnectorError> {
    Ok(crate::find_generator(&record.id)
        .ok_or_else(|| unknown(&record.id))?
        .spec())
}

fn unknown(id: &str) -> ConnectorError {
    ConnectorError::Failed {
        code: "unknown_generator",
        message: format!(
            "'{id}' isn't a shape or connector this build has (was it added by a plugin that \
             isn't installed?)"
        ),
    }
}

fn version_mismatch(id: &str, recorded: u32, current: u32) -> ConnectorError {
    ConnectorError::Failed {
        code: "generator_version",
        message: format!(
            "'{id}' was made with version {recorded}, but this build has version {current}; \
             it can't be regenerated exactly"
        ),
    }
}

/// The mesh `record` describes, built fresh (deterministic: the same
/// record always gives the same mesh, so replay is Exact): its named part
/// for a group member, a shape's one part otherwise.
pub fn regenerate(record: &GeneratorRecord, ctx: &RunContext) -> Result<Mesh, ConnectorError> {
    let g = crate::find_generator(&record.id).ok_or_else(|| unknown(&record.id))?;
    if g.version() != record.version {
        return Err(version_mismatch(&record.id, record.version, g.version()));
    }
    let params = crate::generator::upgraded(g, &record.params)?;
    let parts = g.generate(&params, &crate::GenContext::new(ctx))?;
    match &record.part {
        Some(role) => parts
            .into_iter()
            .find(|p| p.name == role)
            .map(|p| p.mesh)
            .ok_or_else(|| ConnectorError::Failed {
                code: "unknown_part",
                message: format!("'{}' has no part named '{role}'", record.id),
            }),
        None => {
            let n = parts.len();
            let mut parts = parts.into_iter();
            match (parts.next(), n) {
                (Some(p), 1) => Ok(p.mesh),
                _ => Err(ConnectorError::Failed {
                    code: "unknown_part",
                    message: format!(
                        "'{}' makes {n} parts; the recipe must name which one this is",
                        record.id
                    ),
                }),
            }
        }
    }
}

// ---------------------------------------------------------------------
// Spec building, so a shape's parameters are declared in one place.
// ---------------------------------------------------------------------

/// One parameter of a shape's spec.
pub enum P {
    /// A length in mm: name, default, min, max, description.
    Mm(&'static str, f64, f64, Option<f64>, &'static str),
    /// A plain number (an angle, a code): name, default, min, max, unit,
    /// description.
    Num(
        &'static str,
        f64,
        Option<f64>,
        Option<f64>,
        Option<&'static str>,
        &'static str,
    ),
    /// A whole number: name, default, min, max, description.
    Int(&'static str, i64, i64, i64, &'static str),
    /// One of a list: name, default, values, description.
    Choice(
        &'static str,
        &'static str,
        &'static [&'static str],
        &'static str,
    ),
    /// Free text: name, default, description.
    Text(&'static str, &'static str, &'static str),
}

/// A shape's spec from its parameters.
pub fn spec_of(id: &str, summary: &str, params: Vec<P>) -> OperationSpec {
    let parameters = params
        .into_iter()
        .map(|p| match p {
            P::Mm(name, default, min, max, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Float {
                    default,
                    min: Some(min),
                    max,
                },
                unit: Some("mm".into()),
                default_from: None,
                fit_role: None,
                description: description.into(),
            },
            P::Num(name, default, min, max, unit, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Float { default, min, max },
                unit: unit.map(Into::into),
                default_from: None,
                fit_role: None,
                description: description.into(),
            },
            P::Int(name, default, min, max, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Integer {
                    default,
                    min: Some(min),
                    max: Some(max),
                },
                unit: None,
                default_from: None,
                fit_role: None,
                description: description.into(),
            },
            P::Choice(name, default, values, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Choice {
                    default: default.into(),
                    values: values.iter().map(|v| (*v).to_owned()).collect(),
                },
                unit: None,
                default_from: None,
                fit_role: None,
                description: description.into(),
            },
            P::Text(name, default, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Text {
                    default: default.into(),
                },
                unit: None,
                default_from: None,
                fit_role: None,
                description: description.into(),
            },
        })
        .collect();
    OperationSpec {
        id: id.into(),
        version: 1,
        summary: summary.into(),
        geometry_kinds: vec!["mesh".into()],
        target: TargetSpec {
            whole_object: true,
            kinds: Vec::new(),
            many: false,
        },
        operands: Vec::new(),
        parameters,
        preconditions: Vec::new(),
        postconditions: Vec::new(),
        effects: vec!["creates_objects".into()],
        runtime: RuntimeSpec {
            cancellable: false,
            progress: false,
        },
    }
}

pub(crate) fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

pub(crate) fn float(params: &Params, name: &str) -> Result<f64, ConnectorError> {
    params
        .get_float(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

pub(crate) fn int(params: &Params, name: &str) -> Result<i64, ConnectorError> {
    params
        .get_int(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

pub(crate) fn choice<'a>(params: &'a Params, name: &str) -> Result<&'a str, ConnectorError> {
    params
        .get_choice(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

/// The parameter `name` of `params` as plain text (for showing it).
pub fn param_text(params: &Params, name: &str) -> Option<String> {
    params.0.get(name).map(|v| match v {
        ParamValue::Float(f) => format!("{f}"),
        other => other.to_string(),
    })
}

pub use crate::builtin_shapes::{HOLE_OVERSHOOT, builtin_shapes, chosen_profile};

/// For a connector group member: which side of the pair its part
/// belongs to and how it joins it (`None` for a plain shape, or a part
/// the connector doesn't make with these params).
pub fn part_of(
    record: &GeneratorRecord,
    ctx: &RunContext,
) -> Option<(crate::Side, crate::JoinKind)> {
    let role = record.part.as_deref()?;
    let g = crate::find_generator(&record.id)?;
    // allow-swallow: a part that no longer generates simply has no side; regenerating it reports why
    let params = crate::generator::upgraded(g, &record.params).ok()?;
    // allow-swallow: as above
    let parts = g.generate(&params, &crate::GenContext::new(ctx)).ok()?;
    parts
        .iter()
        .find(|p| p.name == role)
        .map(|p| (p.attach_to, p.join()))
}

/// True if `record`'s params carry a hole profile (`hole_profile`,
/// `profile`, `up_x`, `up_y`): a hole-like shape, or a connector part
/// whose holes take one.
pub fn has_hole_profile(record: &GeneratorRecord) -> bool {
    record.params.0.contains_key("hole_profile") && record.params.0.contains_key("up_x")
}

/// `record` with its hole profile resolved for a placement whose linear
/// part is `local_to_world` (fix item 9, D24): the teardrop's point turned
/// to face world up as seen from the hole (`up_x`, `up_y`), and, when the
/// choice is `auto`, `profile` set to Teardrop if the hole's axis (its
/// local z) is within 45 degrees of horizontal, Round otherwise. A record
/// without a hole profile comes back unchanged. Recorded in the params,
/// so replay builds the same profile.
pub fn orient_hole(record: &GeneratorRecord, local_to_world: glam::DMat3) -> GeneratorRecord {
    if !has_hole_profile(record) {
        return record.clone();
    }
    GeneratorRecord {
        params: orient_params(&record.params, local_to_world),
        ..record.clone()
    }
}

/// [`orient_hole`] on bare params (a joint connector's placement, which
/// `split.place_connectors` resolves for where it sits on the joint).
/// Params without a hole profile come back unchanged.
pub fn orient_params(params: &Params, local_to_world: glam::DMat3) -> Params {
    use crate::connectors::hole_profile::{HoleProfile, axis_tilt, local_up};
    use glam::DVec3;
    if !(params.0.contains_key("hole_profile") && params.0.contains_key("up_x")) {
        return params.clone();
    }
    let mut out = params.clone();
    // Only the directions matter: a uniform scale (document units) or a
    // mirror can't change which way is up.
    let rot = glam::DMat3::from_cols(
        local_to_world.x_axis.normalize_or(DVec3::X),
        local_to_world.y_axis.normalize_or(DVec3::Y),
        local_to_world.z_axis.normalize_or(DVec3::Z),
    );
    if let Some([x, y]) = local_up(rot, DVec3::X, DVec3::Y, DVec3::Z) {
        // Rounded so a transform's last-bit noise doesn't make a new
        // recipe (and a new history step) for the same direction.
        let r = |v: f64| (v * 1e9).round() / 1e9;
        out.0.insert("up_x".into(), ParamValue::Float(r(x)));
        out.0.insert("up_y".into(), ParamValue::Float(r(y)));
    }
    if params.get_choice("hole_profile").ok() == Some("auto") {
        let tilt = axis_tilt(rot, DVec3::Z);
        let code = HoleProfile::auto(tilt).code();
        out.0.insert("profile".into(), ParamValue::Float(code));
    }
    out
}

/// True if `record` describes a round hole that, placed with
/// `local_to_world`, prints sideways (its axis within 45 degrees of
/// horizontal) and so would sag at its unsupported top: the live
/// "switch to Teardrop" warning (fix item 9).
pub fn sags_sideways(record: &GeneratorRecord, local_to_world: glam::DMat3) -> bool {
    has_hole_profile(record) && params_sag_sideways(&record.params, local_to_world)
}

/// [`sags_sideways`] on bare params (a joint connector's placement). A
/// dowel or peg that isn't round has no round hole to sag.
pub fn params_sag_sideways(params: &Params, local_to_world: glam::DMat3) -> bool {
    use crate::connectors::hole_profile::{HoleProfile, axis_tilt};
    let round_shape = params.get_float("shape").map_or(true, |s| s.round() == 0.0);
    params.0.contains_key("hole_profile")
        && round_shape
        && chosen_profile(params) == HoleProfile::Round
        && HoleProfile::auto(axis_tilt(local_to_world, glam::DVec3::Z)) == HoleProfile::Teardrop
}
