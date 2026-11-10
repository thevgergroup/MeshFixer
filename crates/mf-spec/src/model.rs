use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use mf_io::threemf::Unit;

use crate::params::{ParamValue, Params, SpecError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationSpec {
    pub id: String,
    pub version: u32,
    pub summary: String,
    pub geometry_kinds: Vec<String>,
    pub target: TargetSpec,
    /// Other objects or selections the operation reads, consumes or keeps
    /// (D6, MF-P1-CMD-001). Optional, so every spec YAML written before
    /// Phase 1 still validates unchanged.
    #[serde(default)]
    pub operands: Vec<OperandSpec>,
    pub parameters: Vec<ParamSpec>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub postconditions: Vec<String>,
    pub effects: Vec<String>,
    pub runtime: RuntimeSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetSpec {
    pub whole_object: bool,
    pub kinds: Vec<String>,
}

/// An operand other than the command's target (D6, MF-P1-SEL-001): names
/// the operand, what happens to it, and what kinds of thing it can be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperandSpec {
    pub name: String,
    pub role: OperandRole,
    pub kinds: Vec<String>,
    pub required: bool,
}

/// What an operation does to an operand's object once the command
/// commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperandRole {
    /// Read for provenance or geometry, but not changed or removed.
    Read,
    /// Removed once the command commits (for example a boolean's tool,
    /// unless `keep_tool` is set).
    Consumed,
    /// Read, and explicitly kept (for example `keep_tool: true`).
    Kept,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub cancellable: bool,
    pub progress: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawParam", into = "RawParam")]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    pub unit: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Integer {
        default: i64,
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        default: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
    Bool {
        default: bool,
    },
    /// One of a fixed list of words (`type: choice`, listed in `values`).
    Choice {
        default: String,
        values: Vec<String>,
    },
    /// A point or direction (D7). A `unit` of `mm` marks it as a length,
    /// converted by [`OperationSpec::to_document_units`].
    Vec3 {
        default: [f64; 3],
    },
    /// The absolute resulting transform (D7): row-major 3x4 (the 3x3
    /// linear part, then translation), 12 values.
    Affine {
        default: [f64; 12],
    },
}

impl ParamKind {
    pub fn default_value(&self) -> ParamValue {
        match self {
            ParamKind::Integer { default, .. } => ParamValue::Int(*default),
            ParamKind::Float { default, .. } => ParamValue::Float(*default),
            ParamKind::Bool { default } => ParamValue::Bool(*default),
            ParamKind::Choice { default, .. } => ParamValue::Text(default.clone()),
            ParamKind::Vec3 { default } => ParamValue::Vec3(*default),
            ParamKind::Affine { default } => ParamValue::Affine(*default),
        }
    }
}

/// On-disk form of a parameter (flat YAML keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawParam {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    default: Scalar,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit: Option<String>,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    /// `default: [x, y, z]` (vec3) or the 12 values of an affine
    /// transform. Elements may be written as integers in YAML (`[1, 0,
    /// 0, ...]`), so this accepts either and widens to `f64`.
    Array(Vec<Scalar>),
}

impl Scalar {
    fn as_i64(&self, field: &str) -> Result<i64, String> {
        match *self {
            Scalar::Int(i) => Ok(i),
            _ => Err(format!("{field} must be an integer")),
        }
    }
    fn as_f64(&self, field: &str) -> Result<f64, String> {
        match *self {
            Scalar::Int(i) => Ok(i as f64),
            Scalar::Float(f) => Ok(f),
            Scalar::Bool(_) | Scalar::Text(_) | Scalar::Array(_) => {
                Err(format!("{field} must be a number"))
            }
        }
    }

    fn as_f64_array<const N: usize>(&self, field: &str) -> Result<[f64; N], String> {
        match self {
            Scalar::Array(items) if items.len() == N => {
                let mut out = [0.0_f64; N];
                for (slot, item) in out.iter_mut().zip(items) {
                    *slot = item.as_f64(field)?;
                }
                Ok(out)
            }
            Scalar::Array(items) => Err(format!(
                "{field} must have {N} numbers, found {}",
                items.len()
            )),
            _ => Err(format!("{field} must be a list of {N} numbers")),
        }
    }
}

impl TryFrom<RawParam> for ParamSpec {
    type Error = String;

    fn try_from(r: RawParam) -> Result<Self, String> {
        let kind = match r.ty.as_str() {
            "integer" => ParamKind::Integer {
                default: r.default.as_i64("default")?,
                min: r.min.map(|m| m.as_i64("min")).transpose()?,
                max: r.max.map(|m| m.as_i64("max")).transpose()?,
            },
            "float" => ParamKind::Float {
                default: r.default.as_f64("default")?,
                min: r.min.map(|m| m.as_f64("min")).transpose()?,
                max: r.max.map(|m| m.as_f64("max")).transpose()?,
            },
            "bool" => match r.default {
                Scalar::Bool(default) => ParamKind::Bool { default },
                _ => return Err("bool default must be true or false".into()),
            },
            "choice" => {
                let values = r.values.ok_or("a choice lists its values")?;
                match r.default {
                    Scalar::Text(default) if values.contains(&default) => {
                        ParamKind::Choice { default, values }
                    }
                    _ => return Err("a choice's default must be one of its values".into()),
                }
            }
            "vec3" => ParamKind::Vec3 {
                default: r.default.as_f64_array::<3>("default")?,
            },
            "affine" => ParamKind::Affine {
                default: r.default.as_f64_array::<12>("default")?,
            },
            other => return Err(format!("unknown parameter type '{other}'")),
        };
        Ok(ParamSpec {
            name: r.name,
            kind,
            unit: r.unit,
            description: r.description,
        })
    }
}

impl From<ParamSpec> for RawParam {
    fn from(p: ParamSpec) -> RawParam {
        let mut values = None;
        let (ty, default, min, max) = match p.kind {
            ParamKind::Integer { default, min, max } => (
                "integer",
                Scalar::Int(default),
                min.map(Scalar::Int),
                max.map(Scalar::Int),
            ),
            ParamKind::Float { default, min, max } => (
                "float",
                Scalar::Float(default),
                min.map(Scalar::Float),
                max.map(Scalar::Float),
            ),
            ParamKind::Bool { default } => ("bool", Scalar::Bool(default), None, None),
            ParamKind::Choice { default, values: v } => {
                values = Some(v);
                ("choice", Scalar::Text(default), None, None)
            }
            ParamKind::Vec3 { default } => (
                "vec3",
                Scalar::Array(default.into_iter().map(Scalar::Float).collect()),
                None,
                None,
            ),
            ParamKind::Affine { default } => (
                "affine",
                Scalar::Array(default.into_iter().map(Scalar::Float).collect()),
                None,
                None,
            ),
        };
        RawParam {
            name: p.name,
            ty: ty.into(),
            default,
            min,
            max,
            values,
            unit: p.unit,
            description: p.description,
        }
    }
}

impl OperationSpec {
    pub fn is_read_only(&self) -> bool {
        self.effects.iter().any(|e| e == "read_only")
    }

    /// True for an operation on objects rather than one mesh (D6): it
    /// places, removes or creates objects, or takes other operands. Such
    /// an operation needs its own tool (the gizmo, Delete, a boolean's
    /// tool picker), so frontends don't list it among a mesh's repairs.
    pub fn acts_on_objects(&self) -> bool {
        !self.operands.is_empty()
            || self.effects.iter().any(|e| {
                matches!(
                    e.as_str(),
                    "transform_changes" | "removes_objects" | "creates_objects"
                )
            })
    }

    pub fn parameter(&self, name: &str) -> Option<&ParamSpec> {
        self.parameters.iter().find(|p| p.name == name)
    }

    /// Parse and validate string parameters, filling defaults.
    /// Unknown names and out-of-range values are errors, never ignored.
    pub fn validate(&self, raw: &BTreeMap<String, String>) -> Result<Params, SpecError> {
        if let Some(unknown) = raw.keys().find(|k| self.parameter(k).is_none()) {
            return Err(SpecError::UnknownParameter(unknown.clone()));
        }
        let mut out = BTreeMap::new();
        for p in &self.parameters {
            let value = match raw.get(&p.name) {
                None => p.kind.default_value(),
                Some(text) => p.parse(text)?,
            };
            p.check_range(&value)?;
            out.insert(p.name.clone(), value);
        }
        Ok(Params(out))
    }

    /// Validate already-typed parameters (GUI path), filling defaults.
    pub fn validate_typed(
        &self,
        values: &BTreeMap<String, ParamValue>,
    ) -> Result<Params, SpecError> {
        let raw = values
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        self.validate(&raw)
    }

    /// Convert every parameter marked `unit: mm` (a length, D20) from
    /// millimetres to the document's units, in place. Runs once, in
    /// `prepare` (Global Constraints: "Printing parameters are in
    /// document units... converted at prepare time. The converted value
    /// is what's recorded"), so replay never re-reads a unit or a
    /// profile.
    ///
    /// `float` parameters convert their number; `vec3` parameters
    /// convert all three components (a length vec3, such as a point;
    /// a direction vec3 has no `unit` and is left alone). Other
    /// parameter kinds are never marked `unit: mm` and are untouched.
    pub fn to_document_units(&self, params: &mut Params, unit: Unit) {
        let mm_per_unit = millimetres_per(unit);
        for p in &self.parameters {
            if p.unit.as_deref() != Some("mm") {
                continue;
            }
            match params.0.get_mut(&p.name) {
                Some(ParamValue::Float(v)) => *v *= mm_per_unit,
                Some(ParamValue::Vec3(v)) => {
                    for c in v.iter_mut() {
                        *c *= mm_per_unit;
                    }
                }
                _ => {}
            }
        }
    }
}

/// How many document units make up one millimetre. The document's own
/// numbers are in `unit`, so a length given in mm is multiplied by this
/// to land in document units (a metre document: 1 mm = 0.001 document
/// units).
fn millimetres_per(unit: Unit) -> f64 {
    match unit {
        Unit::Micron => 1000.0,
        Unit::Millimeter => 1.0,
        Unit::Centimeter => 0.1,
        Unit::Meter => 0.001,
        Unit::Inch => 1.0 / 25.4,
        Unit::Foot => 1.0 / 304.8,
    }
}

impl ParamSpec {
    fn parse(&self, text: &str) -> Result<ParamValue, SpecError> {
        let invalid = |reason: &str| SpecError::InvalidValue {
            name: self.name.clone(),
            value: text.to_owned(),
            reason: reason.to_owned(),
        };
        match &self.kind {
            ParamKind::Integer { .. } => text
                .trim()
                .parse()
                .map(ParamValue::Int)
                .map_err(|_| invalid("expected an integer")),
            ParamKind::Float { .. } => match text.trim().parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(ParamValue::Float(f)),
                _ => Err(invalid("expected a finite number")),
            },
            ParamKind::Bool { .. } => match text.trim() {
                "true" => Ok(ParamValue::Bool(true)),
                "false" => Ok(ParamValue::Bool(false)),
                _ => Err(invalid("expected true or false")),
            },
            ParamKind::Choice { values, .. } => {
                let word = text.trim();
                if values.iter().any(|v| v == word) {
                    Ok(ParamValue::Text(word.to_owned()))
                } else {
                    Err(invalid(&format!("expected {}", one_of(values))))
                }
            }
            ParamKind::Vec3 { .. } => parse_f64_csv::<3>(text)
                .map(ParamValue::Vec3)
                .ok_or_else(|| invalid("expected x,y,z")),
            ParamKind::Affine { .. } => parse_f64_csv::<12>(text)
                .map(ParamValue::Affine)
                .ok_or_else(|| invalid("expected 12 comma-separated numbers")),
        }
    }

    fn check_range(&self, value: &ParamValue) -> Result<(), SpecError> {
        let (v, min, max) = match (&self.kind, value) {
            (ParamKind::Integer { min, max, .. }, ParamValue::Int(i)) => {
                (*i as f64, min.map(|m| m as f64), max.map(|m| m as f64))
            }
            (ParamKind::Float { min, max, .. }, ParamValue::Float(f)) => (*f, *min, *max),
            _ => return Ok(()),
        };
        if min.is_some_and(|m| v < m) || max.is_some_and(|m| v > m) {
            return Err(SpecError::OutOfRange {
                name: self.name.clone(),
                value: value.to_string(),
                min: min.map(|m| m.to_string()),
                max: max.map(|m| m.to_string()),
            });
        }
        Ok(())
    }
}

/// "a, b or c".
pub(crate) fn one_of(values: &[String]) -> String {
    match values.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// Parse exactly `N` comma-separated finite `f64`s (the CLI and
/// command-log string form of `vec3` and `affine` parameters, D7).
/// `None` on a wrong count, a non-finite value, or anything that doesn't
/// parse, so the caller reports it by name rather than truncating.
fn parse_f64_csv<const N: usize>(text: &str) -> Option<[f64; N]> {
    let parts: Vec<&str> = text.trim().split(',').map(str::trim).collect();
    if parts.len() != N {
        return None;
    }
    let mut out = [0.0_f64; N];
    for (slot, part) in out.iter_mut().zip(parts) {
        // allow-swallow: any unparsable part makes the whole value invalid; the caller reports it by name (SpecError::InvalidValue) with the expected form
        let v: f64 = part.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        *slot = v;
    }
    Some(out)
}
