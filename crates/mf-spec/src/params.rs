use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A `choice` parameter's word.
    Text(String),
    /// A point or direction (D7): `[x, y, z]`, a JSON array in a command
    /// log, comma-separated on the CLI.
    Vec3([f64; 3]),
    /// The absolute resulting transform (D7), row-major 3x4 (12 values:
    /// the 3x3 linear part then the translation), recorded whole so
    /// replay runs no trigonometry.
    Affine([f64; 12]),
    /// A sculpt stroke's sampled path (MF-SCULPT-003, MF-HIST-002): one
    /// `[face, b0, b1, b2, pressure]` per dab, the face an input-revision
    /// face id and `b` barycentric weights on it. A JSON array of arrays;
    /// on the CLI, dabs separated by `;` and numbers by `,`.
    Samples(Vec<[f64; 5]>),
    /// A list of face ids (a sculpt stroke's frozen faces). Serialized as
    /// `{"faces": [...]}` so a 3-face list can't be read as a `vec3`; on
    /// the CLI, comma-separated.
    Faces(FaceList),
}

/// See [`ParamValue::Faces`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FaceList {
    pub faces: Vec<u32>,
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamValue::Bool(b) => write!(f, "{b}"),
            ParamValue::Int(i) => write!(f, "{i}"),
            ParamValue::Float(x) => write!(f, "{x}"),
            ParamValue::Text(t) => write!(f, "{t}"),
            ParamValue::Vec3(v) => write!(f, "{},{},{}", v[0], v[1], v[2]),
            ParamValue::Affine(m) => {
                let parts: Vec<String> = m.iter().map(f64::to_string).collect();
                write!(f, "{}", parts.join(","))
            }
            ParamValue::Samples(s) => {
                let parts: Vec<String> = s
                    .iter()
                    .map(|d| d.iter().map(f64::to_string).collect::<Vec<_>>().join(","))
                    .collect();
                write!(f, "{}", parts.join(";"))
            }
            ParamValue::Faces(l) => {
                let parts: Vec<String> = l.faces.iter().map(u32::to_string).collect();
                write!(f, "{}", parts.join(","))
            }
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
            .cloned()
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

    /// A `choice` parameter's word.
    pub fn get_choice(&self, name: &str) -> Result<&str, SpecError> {
        match self.0.get(name) {
            Some(ParamValue::Text(t)) => Ok(t),
            Some(v) => Err(Self::wrong(name, v.clone(), "a word")),
            None => Err(SpecError::UnknownParameter(name.to_owned())),
        }
    }

    pub fn get_bool(&self, name: &str) -> Result<bool, SpecError> {
        match self.get(name)? {
            ParamValue::Bool(b) => Ok(b),
            v => Err(Self::wrong(name, v, "a bool")),
        }
    }

    pub fn get_vec3(&self, name: &str) -> Result<[f64; 3], SpecError> {
        match self.get(name)? {
            ParamValue::Vec3(v) => Ok(v),
            v => Err(Self::wrong(name, v, "a vec3")),
        }
    }

    pub fn get_samples(&self, name: &str) -> Result<Vec<[f64; 5]>, SpecError> {
        match self.get(name)? {
            ParamValue::Samples(s) => Ok(s),
            v => Err(Self::wrong(name, v, "a list of samples")),
        }
    }

    pub fn get_faces(&self, name: &str) -> Result<Vec<u32>, SpecError> {
        match self.get(name)? {
            ParamValue::Faces(l) => Ok(l.faces),
            v => Err(Self::wrong(name, v, "a list of face ids")),
        }
    }

    pub fn get_affine(&self, name: &str) -> Result<[f64; 12], SpecError> {
        match self.get(name)? {
            ParamValue::Affine(m) => Ok(m),
            v => Err(Self::wrong(name, v, "an affine transform")),
        }
    }
}
