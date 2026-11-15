use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use mf_io::threemf::Unit;

use crate::params::{ParamValue, Params, SpecError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationSpec {
    pub id: String,
    pub version: u32,
    pub summary: String,
    /// A message id in mf-desktop's i18n catalogue (under `history.label.`)
    /// for this operation's History panel row, such as
    /// `"history.label.mesh.hollow"`. Its catalogue text has `{object}`
    /// and other placeholders the frontend fills from the command's own
    /// words (the object's name, a count) — never a raw parameter value
    /// or an id list (the owner's recurring bug: a history row printing
    /// `mesh.hollow (drain_holes=none, ..., open_through_faces=37048,
    /// ...)`). Required on every spec, enforced by a test that iterates
    /// every built-in spec and fails if the id isn't in the catalogue.
    pub history_label: String,
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
    /// Whether the command queue may merge a newly queued invocation of
    /// this op into the one already waiting behind the running command
    /// (ADR-0009): `Never` (the default) keeps every queued invocation
    /// as its own undo step — right for a delta command (a sculpt
    /// stroke, a fill), where coalescing silently drops work the owner
    /// did. `Absolute` merges two queued invocations with the same key
    /// (same op, same target) into one, latest params winning — right
    /// only for a command whose params are the whole resulting state
    /// (`object.set_transform`'s placement, `object.set_shape_params`'s
    /// recipe), where the queued one fully supersedes the one before it.
    #[serde(default)]
    pub coalesce: Coalesce,
}

/// See [`OperationSpec::coalesce`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coalesce {
    #[default]
    Never,
    Absolute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetSpec {
    pub whole_object: bool,
    pub kinds: Vec<String>,
    /// The command may act on several targets at once (a split of every
    /// selected object): the first is the invocation's `target`, the rest
    /// are operands named `target`, in order. One command, one undo step.
    #[serde(default)]
    pub many: bool,
}

/// An operand other than the command's target (D6, MF-P1-SEL-001): names
/// the operand, what happens to it, and what kinds of thing it can be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperandSpec {
    pub name: String,
    pub role: OperandRole,
    pub kinds: Vec<String>,
    /// For a single operand: it must be given. For a `many` operand: at
    /// least one object must be given (otherwise any number, even none).
    pub required: bool,
    /// The operand is a list: an invocation may name it any number of
    /// times, and each object is its own operand (its own provenance index,
    /// recorded input and output), in the order given.
    #[serde(default)]
    pub many: bool,
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
    /// Changed in place (its geometry or transform), never removed
    /// (for example `connector.join`'s side_a/side_b, each folded
    /// into by the connector's own parts but never consumed).
    Changed,
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
    /// Where a frontend takes this parameter's default from instead of
    /// `default`, when the caller didn't set it (D20): `fit.<field>` of
    /// the active print fit profile. See [`OperationSpec::fill_defaults`].
    pub default_from: Option<String>,
    /// How a connector/shape generator's Fit class and Machine profile
    /// pickers (the desktop's newer dual-pane fit system, distinct from
    /// `default_from`'s older single `FitProfile`) fill this parameter,
    /// so a generic filler can replace a per-connector `if id == ...`
    /// match (parts-architecture-review.md's `fit_role`): one of
    /// `"clearance"` (the class's own `clearance`, no machine
    /// compensation), `"hole"`/`"outer_xy"`/`"z"` (the clearance
    /// compensated by the machine profile's matching field,
    /// `mf_parts::Compensation`), or `"fit_class_name"`/
    /// `"machine_profile_name"` (the chosen class's or profile's own
    /// name, recorded so a saved placement shows which pair it came
    /// from). `mf-spec` only carries the string — it has no dependency
    /// on `mf-parts`, which interprets it (`mf-desktop`'s
    /// `connector_tool::fill_fit_role_params`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit_role: Option<String>,
    pub description: String,
}

/// Every `fit_role` a spec parameter may declare (kept as plain text in
/// the spec itself, since `mf-spec` doesn't depend on `mf-parts`'s
/// `Compensation`; validated against this list at parse time so a typo
/// is refused by name rather than silently never filled).
///
/// `"clearance"`/`"hole"`/`"outer_xy"`/`"z"` are the Fit class's own
/// `clearance`, resolved through the Machine profile's matching
/// compensation (`mf_parts::Compensation::{None,Hole,OuterXy,Z}`) — the
/// combined value every connector's own generated gap/clearance param
/// already wants. `"machine_hole_undersize"` is the Machine profile's
/// raw `hole_undersize` field on its own, uncompensated (Thread's and
/// the dovetail's own `machine_tolerance`, which is *added to* an
/// already-resolved `fit_clearance` rather than folded into one
/// combined value). `"fit_class_name"`/`"machine_profile_name"` are the
/// chosen pair's own names, recorded so a saved placement shows which
/// pair it came from.
pub const FIT_ROLES: &[&str] = &[
    "clearance",
    "hole",
    "outer_xy",
    "z",
    "machine_hole_undersize",
    "fit_class_name",
    "machine_profile_name",
];

/// A source of print-fit values for [`OperationSpec::fill_defaults`]
/// (D20), implemented by `mf_parts::FitProfile`. `field` is the part
/// after `fit.` in a parameter's `default_from`, in mm.
pub trait FitDefaults {
    fn fit_value(&self, field: &str) -> Option<f64>;
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
    /// Free text (`type: text`): a name or an id, such as a library part's.
    Text {
        default: String,
    },
    /// A sculpt stroke's dabs (`type: samples`, default `[]`); see
    /// [`ParamValue::Samples`].
    Samples,
    /// A list of face ids (`type: faces`, default `[]`); see
    /// [`ParamValue::Faces`].
    Faces,
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
            ParamKind::Text { default } => ParamValue::Text(default.clone()),
            ParamKind::Samples => ParamValue::Samples(Vec::new()),
            ParamKind::Faces => ParamValue::Faces(Default::default()),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fit_role: Option<String>,
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
            "text" => match r.default {
                Scalar::Text(default) => ParamKind::Text { default },
                _ => return Err("a text default must be a string".into()),
            },
            "samples" | "faces" => {
                if !matches!(&r.default, Scalar::Array(a) if a.is_empty()) {
                    return Err(format!("a {} parameter's default must be []", r.ty));
                }
                if r.ty == "samples" {
                    ParamKind::Samples
                } else {
                    ParamKind::Faces
                }
            }
            other => return Err(format!("unknown parameter type '{other}'")),
        };
        if let Some(from) = &r.default_from
            && !(from.starts_with("fit.") && from.len() > 4)
        {
            return Err(format!(
                "default_from '{from}' must name a print fit field (fit.<field>)"
            ));
        }
        if let Some(role) = &r.fit_role
            && !FIT_ROLES.contains(&role.as_str())
        {
            return Err(format!("fit_role '{role}' isn't one of {FIT_ROLES:?}"));
        }
        Ok(ParamSpec {
            name: r.name,
            kind,
            unit: r.unit,
            default_from: r.default_from,
            fit_role: r.fit_role,
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
            ParamKind::Text { default } => ("text", Scalar::Text(default), None, None),
            ParamKind::Samples => ("samples", Scalar::Array(Vec::new()), None, None),
            ParamKind::Faces => ("faces", Scalar::Array(Vec::new()), None, None),
        };
        RawParam {
            name: p.name,
            ty: ty.into(),
            default,
            min,
            max,
            values,
            unit: p.unit,
            default_from: p.default_from,
            fit_role: p.fit_role,
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
                    "transform_changes" | "removes_objects" | "creates_objects" | "renames_objects"
                )
            })
    }

    pub fn parameter(&self, name: &str) -> Option<&ParamSpec> {
        self.parameters.iter().find(|p| p.name == name)
    }

    /// Fill every parameter the caller didn't set whose spec says
    /// `default_from: fit.<field>` from `fit` (D20), as text in mm, before
    /// [`Self::validate`]. A frontend calls this at prepare time with the
    /// active print fit profile, so the recorded parameters hold the values
    /// used and replay never reads a profile. A parameter the caller set
    /// is left alone. A field `fit` doesn't have is an error by name.
    pub fn fill_defaults(
        &self,
        raw: &mut BTreeMap<String, String>,
        fit: &dyn FitDefaults,
    ) -> Result<(), SpecError> {
        for p in &self.parameters {
            let Some(from) = &p.default_from else {
                continue;
            };
            if raw.contains_key(&p.name) {
                continue;
            }
            let field = from.strip_prefix("fit.").unwrap_or(from);
            let value = fit
                .fit_value(field)
                .ok_or_else(|| SpecError::InvalidValue {
                    name: p.name.clone(),
                    value: from.clone(),
                    reason: "the print fit profile has no such field".into(),
                })?;
            raw.insert(p.name.clone(), value.to_string());
        }
        Ok(())
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

/// A length in document units, to three significant figures: never
/// "0.000" the way a plain `{:.3}` would print a sub-millimetre value
/// on a metre document (the owner's bug — Make solid's refusal saying
/// "at least 0.000" for a genuinely positive cell size). Whole numbers
/// from 100 up drop the decimals; smaller ones get enough to keep three
/// significant figures, trimming any insignificant trailing zeros. No
/// unit suffix: operations never see which unit the document is in
/// (D20 keeps them working purely in document units), so a message
/// built from this reads "at least 0.00025" — the frontend's own words
/// around it name the unit, as every other length in its panels does.
///
/// A thin delegation to `mf_units::significant_figures` (consolidation
/// plan C03, ADR-0006): the same unitless, 3-significant-figure core
/// every length/area/volume formatter in the app now shares, called
/// with no `Unit` because this function's own contract (D20) is that it
/// never has one.
pub fn format_length(v: f64) -> String {
    mf_units::significant_figures(v, 3)
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
            ParamKind::Text { .. } => Ok(ParamValue::Text(text.to_owned())),
            ParamKind::Samples => parse_samples(text).map(ParamValue::Samples).ok_or_else(|| {
                invalid("expected face,b0,b1,b2,pressure per dab, dabs separated by ;")
            }),
            ParamKind::Faces => parse_faces(text)
                .map(|faces| ParamValue::Faces(crate::params::FaceList { faces }))
                .ok_or_else(|| invalid("expected comma-separated face ids")),
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

/// Sculpt samples: `face,b0,b1,b2,pressure` per dab, dabs separated by
/// `;` (empty text: no dabs). The face must be a whole number.
fn parse_samples(text: &str) -> Option<Vec<[f64; 5]>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    text.split(';')
        .map(|d| {
            let s = parse_f64_csv::<5>(d)?;
            (s[0] >= 0.0 && s[0] <= u32::MAX as f64 && s[0].fract() == 0.0).then_some(s)
        })
        .collect()
}

/// Comma-separated face ids (empty text: none).
fn parse_faces(text: &str) -> Option<Vec<u32>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    // allow-swallow: any unparsable id makes the whole value invalid; the caller reports it by name
    text.split(',').map(|f| f.trim().parse().ok()).collect()
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
