use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamValue::Bool(b) => write!(f, "{b}"),
            ParamValue::Int(i) => write!(f, "{i}"),
            ParamValue::Float(x) => write!(f, "{x}"),
        }
    }
}

/// Validated parameters: every spec parameter present, in range.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Params(pub BTreeMap<String, ParamValue>);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SpecError {
    #[error("unknown parameter '{0}'")]
    UnknownParameter(String),
    #[error("invalid value '{value}' for '{name}': {reason}")]
    InvalidValue {
        name: String,
        value: String,
        reason: String,
    },
    #[error("'{name}' = {value} is out of range (min {min:?}, max {max:?})")]
    OutOfRange {
        name: String,
        value: String,
        min: Option<String>,
        max: Option<String>,
    },
    #[error("unknown operation '{0}'")]
    UnknownOperation(String),
}

impl Params {
    fn get(&self, name: &str) -> Result<ParamValue, SpecError> {
        self.0
            .get(name)
            .copied()
            .ok_or_else(|| SpecError::UnknownParameter(name.to_owned()))
    }

    fn wrong(name: &str, v: ParamValue, want: &str) -> SpecError {
        SpecError::InvalidValue {
            name: name.to_owned(),
            value: v.to_string(),
            reason: format!("expected {want}"),
        }
    }

    pub fn get_int(&self, name: &str) -> Result<i64, SpecError> {
        match self.get(name)? {
            ParamValue::Int(i) => Ok(i),
            v => Err(Self::wrong(name, v, "an integer")),
        }
    }

    pub fn get_float(&self, name: &str) -> Result<f64, SpecError> {
        match self.get(name)? {
            ParamValue::Float(f) => Ok(f),
            ParamValue::Int(i) => Ok(i as f64),
            v => Err(Self::wrong(name, v, "a number")),
        }
    }

    pub fn get_bool(&self, name: &str) -> Result<bool, SpecError> {
        match self.get(name)? {
            ParamValue::Bool(b) => Ok(b),
            v => Err(Self::wrong(name, v, "a bool")),
        }
    }
}
