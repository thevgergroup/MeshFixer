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
//! - **The registry** is the built-ins ([`builtin_shapes`]) followed by
//!   whatever a plugin registered ([`register_shape`]), looked up by id
//!   ([`find_shape`]); the desktop exposes it through its shell.

use std::sync::{OnceLock, RwLock};

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

/// A parametric shape generator. Built-ins are registered in
/// [`builtin_shapes`]; a plugin adds its own with [`register_shape`].
pub trait Shape: Send + Sync {
    /// Stable id, namespaced (`"shape.box"`).
    fn id(&self) -> &'static str;
    /// Its name in the library ("Box").
    fn name(&self) -> &'static str;
    /// Bumped when the generated geometry's meaning changes.
    fn version(&self) -> u32;
    /// Its parameters (only `parameters:` is used).
    fn spec(&self) -> &'static OperationSpec;
    /// How it's placed by default.
    fn role(&self) -> ShapeRole {
        ShapeRole::Solid
    }
    /// One closed mesh from validated `params`.
    fn generate(&self, params: &Params, ctx: &RunContext) -> Result<Mesh, ConnectorError>;
}

/// Why a shape couldn't be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShapeRegisterError {
    #[error("a shape with the id '{0}' is already registered")]
    DuplicateId(String),
}

fn extra() -> &'static RwLock<Vec<&'static dyn Shape>> {
    static EXTRA: OnceLock<RwLock<Vec<&'static dyn Shape>>> = OnceLock::new();
    EXTRA.get_or_init(|| RwLock::new(Vec::new()))
}

/// Adds a shape to the registry (a plugin's, MF-PLUG-001): the library
/// lists it after the built-ins and `object.set_shape_params` finds it
/// by id. Refuses an id already taken.
pub fn register_shape(shape: &'static dyn Shape) -> Result<(), ShapeRegisterError> {
    if find_shape(shape.id()).is_some() {
        return Err(ShapeRegisterError::DuplicateId(shape.id().to_owned()));
    }
    extra()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push(shape);
    Ok(())
}

/// Every shape: the built-ins, then the registered ones in order.
pub fn shapes() -> Vec<&'static dyn Shape> {
    let mut all = builtin_shapes();
    all.extend(
        extra()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied(),
    );
    all
}

pub fn find_shape(id: &str) -> Option<&'static dyn Shape> {
    shapes().into_iter().find(|s| s.id() == id)
}

/// The record a new shape gets: `shape`'s own version and its spec's
/// defaults, overridden by `raw` (validated, so an unknown name or an
/// out-of-range value is refused by name).
pub fn new_record(
    shape: &dyn Shape,
    raw: &std::collections::BTreeMap<String, String>,
) -> Result<GeneratorRecord, ConnectorError> {
    let params = shape
        .spec()
        .validate(raw)
        .map_err(|e| ConnectorError::Failed {
            code: "invalid_parameter",
            message: e.to_string(),
        })?;
    Ok(GeneratorRecord {
        id: shape.id().to_owned(),
        version: shape.version(),
        params,
        part: None,
    })
}

/// The spec a record's params are checked against (a shape's, or the
/// connector's for a group member).
pub fn record_spec(record: &GeneratorRecord) -> Result<&'static OperationSpec, ConnectorError> {
    if record.part.is_some() {
        let c = crate::connectors()
            .into_iter()
            .find(|c| c.id() == record.id)
            .ok_or_else(|| unknown(&record.id))?;
        Ok(c.spec())
    } else {
        Ok(find_shape(&record.id)
            .ok_or_else(|| unknown(&record.id))?
            .spec())
    }
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
/// record always gives the same mesh, so replay is Exact).
pub fn regenerate(record: &GeneratorRecord, ctx: &RunContext) -> Result<Mesh, ConnectorError> {
    match &record.part {
        None => {
            let shape = find_shape(&record.id).ok_or_else(|| unknown(&record.id))?;
            if shape.version() != record.version {
                return Err(version_mismatch(
                    &record.id,
                    record.version,
                    shape.version(),
                ));
            }
            shape.generate(&record.params, ctx)
        }
        Some(role) => {
            let c = crate::connectors()
                .into_iter()
                .find(|c| c.id() == record.id)
                .ok_or_else(|| unknown(&record.id))?;
            if c.version() != record.version {
                return Err(version_mismatch(&record.id, record.version, c.version()));
            }
            let parts = c.generate(&record.params, ctx)?;
            parts
                .parts
                .into_iter()
                .find(|p| p.role == role)
                .map(|p| p.mesh)
                .ok_or_else(|| ConnectorError::Failed {
                    code: "unknown_part",
                    message: format!("'{}' has no part named '{role}'", record.id),
                })
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
                description: description.into(),
            },
            P::Num(name, default, min, max, unit, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Float { default, min, max },
                unit: unit.map(Into::into),
                default_from: None,
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
                description: description.into(),
            },
            P::Text(name, default, description) => ParamSpec {
                name: name.into(),
                kind: ParamKind::Text {
                    default: default.into(),
                },
                unit: None,
                default_from: None,
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

/// The parameter `name` of `params` as plain text (for showing it).
pub fn param_text(params: &Params, name: &str) -> Option<String> {
    params.0.get(name).map(|v| match v {
        ParamValue::Float(f) => format!("{f}"),
        other => other.to_string(),
    })
}

pub use crate::builtin_shapes::builtin_shapes;
