use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::params::{ParamValue, Params, SpecError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationSpec {
    pub id: String,
    pub version: u32,
    pub summary: String,
    pub geometry_kinds: Vec<String>,
    pub target: TargetSpec,
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
}

impl ParamKind {
    pub fn default_value(&self) -> ParamValue {
        match self {
            ParamKind::Integer { default, .. } => ParamValue::Int(*default),
            ParamKind::Float { default, .. } => ParamValue::Float(*default),
            ParamKind::Bool { default } => ParamValue::Bool(*default),
            ParamKind::Choice { default, .. } => ParamValue::Text(default.clone()),
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
            Scalar::Bool(_) | Scalar::Text(_) => Err(format!("{field} must be a number")),
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
