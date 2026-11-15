//! [`Intent`]: the one typed builder every desktop tool uses to ask for an
//! operation (consolidation plan C13, ADR-0006 rule 2).
//!
//! A tool says what it means: `Intent::new(spec).on(id).set("wall",
//! Length::new(2.0, Unit::Millimeter))`. Every setter is checked against
//! the param's [`ParamUnit`] in the spec, so a length can only be set as a
//! [`Length`] (never a bare number someone already converted), a
//! document-unit point only through [`Intent::set_doc`], and a count or a
//! choice only through [`Intent::param`]. A length serialises as `"<value>
//! <symbol>"` ("10 in"); `Session::prepare` parses it in the param's spec
//! unit and converts to document units, once. Nothing here (or in any
//! panel) converts a length to document units: that boundary is
//! `prepare`'s alone.
//!
//! The one conversion this builder does do is for `vec3` lengths: the
//! spec's text form for a vec3 is a bare `x,y,z`, so a point given in a
//! [`Unit`] is written in the param's spec unit (ADR-0006 rule 2: "the
//! shell converts display units to the param's spec unit"). `prepare`
//! still does the document-unit conversion.
//!
//! A wrong unit kind, an unknown param, a non-finite number or a missing
//! target is an [`IntentError`] from [`Intent::build`], never a silently
//! dropped or zeroed value.

use std::collections::BTreeMap;
use std::fmt;

use mf_command::{Invocation, ObjectId};
use mf_selection::Selection;
use mf_spec::{OperationSpec, ParamKind, ParamSpec, ParamUnit, ParamValue};
use mf_units::{Length, Unit};

/// Why an [`Intent`] could not become an [`Invocation`]. Each one is a
/// programming error in the tool that built it (the spec says what kind
/// each param is), reported rather than papered over.
#[derive(Debug, Clone, PartialEq)]
pub enum IntentError {
    /// No built-in spec has this id.
    UnknownOperation(String),
    /// The spec has no param of this name.
    UnknownParameter { op: String, name: String },
    /// The setter used doesn't match the param's declared unit: a
    /// [`Length`] for a param that isn't a length, a bare value for one
    /// that is, or a document-unit value for a param in a spec unit.
    WrongUnitKind {
        op: String,
        name: String,
        declared: ParamUnit,
        setter: &'static str,
    },
    /// The setter used doesn't match the param's type (a float setter
    /// on a vec3, say).
    WrongKind {
        op: String,
        name: String,
        setter: &'static str,
    },
    /// A length or point component was NaN or infinite.
    NotFinite { op: String, name: String },
    /// [`Intent::on`] was never called.
    MissingTarget { op: String },
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::UnknownOperation(op) => write!(f, "unknown operation '{op}'"),
            IntentError::UnknownParameter { op, name } => {
                write!(f, "{op} has no parameter '{name}'")
            }
            IntentError::WrongUnitKind {
                op,
                name,
                declared,
                setter,
            } => write!(
                f,
                "{op}'s '{name}' is declared {declared:?}; it can't be set with {setter}"
            ),
            IntentError::WrongKind { op, name, setter } => {
                write!(f, "{op}'s '{name}' is not the type {setter} sets")
            }
            IntentError::NotFinite { op, name } => {
                write!(f, "{op}'s '{name}' was given a value that isn't finite")
            }
            IntentError::MissingTarget { op } => write!(f, "{op} was built with no target"),
        }
    }
}

impl std::error::Error for IntentError {}

/// A typed request to run one operation; see the module docs.
#[derive(Debug, Clone)]
pub struct Intent<'s> {
    spec: Option<&'s OperationSpec>,
    op_id: String,
    target: Option<ObjectId>,
    selection: Option<Selection>,
    operands: Vec<(String, ObjectId, Option<Selection>)>,
    raw: BTreeMap<String, String>,
    error: Option<IntentError>,
}

impl<'s> Intent<'s> {
    /// An intent to run `spec`, with no params set yet (each unset param
    /// takes its spec default in `prepare`).
    pub fn new(spec: &'s OperationSpec) -> Intent<'s> {
        Intent {
            spec: Some(spec),
            op_id: spec.id.clone(),
            target: None,
            selection: None,
            operands: Vec::new(),
            raw: BTreeMap::new(),
            error: None,
        }
    }

    /// The object it acts on (required).
    pub fn on(mut self, target: ObjectId) -> Self {
        self.target = Some(target);
        self
    }

    /// The part of the target to act on; `None` (the default) is the
    /// whole object.
    pub fn selection(mut self, selection: Option<Selection>) -> Self {
        self.selection = selection;
        self
    }

    /// One more operand (D6), named as in the spec's `operands:`.
    pub fn operand(mut self, name: &str, object: ObjectId, selection: Option<Selection>) -> Self {
        self.operands.push((name.to_owned(), object, selection));
        self
    }

    /// Several operands, in order.
    pub fn operands(
        mut self,
        operands: impl IntoIterator<Item = (String, ObjectId, Option<Selection>)>,
    ) -> Self {
        self.operands.extend(operands);
        self
    }

    /// Sets a length param (`type: float`, `unit: <length unit>`) to
    /// `value`, in whatever unit it carries; serialised `"<v> <sym>"`.
    pub fn set(mut self, name: &str, value: Length) -> Self {
        let Some(p) = self.param_spec(name, "set(Length)") else {
            return self;
        };
        if !matches!(p.unit, ParamUnit::Length(_)) {
            return self.wrong_unit(name, p.unit, "set(Length)");
        }
        if !matches!(p.kind, ParamKind::Float { .. }) {
            return self.wrong_kind(name, "set(Length)");
        }
        if !value.value.is_finite() {
            return self.not_finite(name);
        }
        self.raw.insert(
            name.to_owned(),
            format!("{} {}", value.value, value.unit.symbol()),
        );
        self
    }

    /// Sets a length point param (`type: vec3`, `unit: <length unit>`) to
    /// `point`, whose components are in `unit`. Written in the param's
    /// spec unit, since a vec3's text form has no unit suffix.
    pub fn set_point(mut self, name: &str, point: [f64; 3], unit: Unit) -> Self {
        let Some(p) = self.param_spec(name, "set_point") else {
            return self;
        };
        let ParamUnit::Length(spec_unit) = p.unit else {
            return self.wrong_unit(name, p.unit, "set_point");
        };
        if !matches!(p.kind, ParamKind::Vec3 { .. }) {
            return self.wrong_kind(name, "set_point");
        }
        if point.iter().any(|c| !c.is_finite()) {
            return self.not_finite(name);
        }
        let c = point.map(|v| Length::new(v, unit).to(spec_unit).value);
        self.raw
            .insert(name.to_owned(), format!("{},{},{}", c[0], c[1], c[2]));
        self
    }

    /// Sets a param declared `unit: document` (a point, box corner,
    /// transform or gap already in document units, which `prepare` never
    /// converts) from its text form.
    pub fn set_doc(mut self, name: &str, value: impl fmt::Display) -> Self {
        let Some(p) = self.param_spec(name, "set_doc") else {
            return self;
        };
        if p.unit != ParamUnit::Document {
            return self.wrong_unit(name, p.unit, "set_doc");
        }
        self.raw.insert(name.to_owned(), value.to_string());
        self
    }

    /// Sets a param that isn't a length (a count, a choice, a flag, an
    /// angle, a percentage, a direction, text) from its text form.
    pub fn param(mut self, name: &str, value: impl fmt::Display) -> Self {
        let Some(p) = self.param_spec(name, "param") else {
            return self;
        };
        if matches!(p.unit, ParamUnit::Length(_) | ParamUnit::Document) {
            return self.wrong_unit(name, p.unit, "param");
        }
        self.raw.insert(name.to_owned(), value.to_string());
        self
    }

    /// Sets every param in `values` (a spec-driven panel's own typed
    /// values, each in its param's spec unit, as `params_for` holds
    /// them), routing each through the setter its unit calls for.
    pub fn values(mut self, values: &BTreeMap<String, ParamValue>) -> Self {
        for (name, value) in values {
            let Some(p) = self.param_spec(name, "values") else {
                return self;
            };
            let unit = p.unit;
            self = match (unit, value) {
                (ParamUnit::Length(u), ParamValue::Float(v)) => self.set(name, Length::new(*v, u)),
                (ParamUnit::Length(u), ParamValue::Vec3(v)) => self.set_point(name, *v, u),
                (ParamUnit::Length(_), _) => self.wrong_kind(name, "values"),
                (ParamUnit::Document, v) => self.set_doc(name, v),
                (_, v) => self.param(name, v),
            };
        }
        self
    }

    /// The op this intent is for.
    pub fn op_id(&self) -> &str {
        &self.op_id
    }

    /// The `Invocation` this intent describes, or the first error any
    /// setter recorded.
    pub fn build(self) -> Result<Invocation, IntentError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let Some(target) = self.target else {
            return Err(IntentError::MissingTarget { op: self.op_id });
        };
        Ok(Invocation {
            op_id: self.op_id,
            target,
            selection: self.selection,
            operands: self.operands,
            raw_params: self.raw,
        })
    }

    fn param_spec(&mut self, name: &str, _setter: &'static str) -> Option<&'s ParamSpec> {
        if self.error.is_some() {
            return None;
        }
        let spec = self.spec?;
        match spec.parameter(name) {
            Some(p) => Some(p),
            None => {
                self.error = Some(IntentError::UnknownParameter {
                    op: self.op_id.clone(),
                    name: name.to_owned(),
                });
                None
            }
        }
    }

    fn fail(mut self, e: IntentError) -> Self {
        if self.error.is_none() {
            self.error = Some(e);
        }
        self
    }

    fn wrong_unit(self, name: &str, declared: ParamUnit, setter: &'static str) -> Self {
        let op = self.op_id.clone();
        self.fail(IntentError::WrongUnitKind {
            op,
            name: name.to_owned(),
            declared,
            setter,
        })
    }

    fn wrong_kind(self, name: &str, setter: &'static str) -> Self {
        let op = self.op_id.clone();
        self.fail(IntentError::WrongKind {
            op,
            name: name.to_owned(),
            setter,
        })
    }

    fn not_finite(self, name: &str) -> Self {
        let op = self.op_id.clone();
        self.fail(IntentError::NotFinite {
            op,
            name: name.to_owned(),
        })
    }
}

impl Intent<'static> {
    /// An intent for the built-in op `op_id`. An id with no built-in spec
    /// is an [`IntentError::UnknownOperation`] from [`Intent::build`].
    pub fn op(op_id: &str) -> Intent<'static> {
        match mf_spec::builtin::get(op_id) {
            Some(spec) => Intent::new(spec),
            None => Intent {
                spec: None,
                op_id: op_id.to_owned(),
                target: None,
                selection: None,
                operands: Vec::new(),
                raw: BTreeMap::new(),
                error: Some(IntentError::UnknownOperation(op_id.to_owned())),
            },
        }
    }
}

/// `v` as a spec `vec3`'s text form, `x,y,z`.
pub fn vec3_text(v: [f64; 3]) -> String {
    format!("{},{},{}", v[0], v[1], v[2])
}

/// `m` as a spec `affine`'s text form: 12 comma-separated numbers.
pub fn affine_text(m: &[f64; 12]) -> String {
    m.iter().map(f64::to_string).collect::<Vec<_>>().join(",")
}

impl crate::MeshFixerApp {
    /// Builds `intent` and runs it ([`Self::run_invocation`]). An intent
    /// that doesn't match its spec is shown in the error bar, as a
    /// `prepare` failure would be: never dropped silently.
    pub fn run_intent(&mut self, intent: Intent<'_>, label: String) {
        match intent.build() {
            Ok(inv) => self.run_invocation(inv, label),
            Err(e) => self.error = Some(format!("{label} can't run: {e}.")),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    fn id() -> ObjectId {
        ObjectId(1)
    }

    #[test]
    fn length_serialises_with_its_own_unit() {
        let inv = Intent::op("mesh.hollow")
            .on(id())
            .set("wall_thickness", Length::new(0.25, Unit::Inch))
            .build()
            .unwrap();
        assert_eq!(inv.raw_params["wall_thickness"], "0.25 in");
        // ...and the spec reads it back in its own unit (mm), exactly
        // as `Session::prepare` will.
        let spec = mf_spec::builtin::get("mesh.hollow").unwrap();
        let params = spec.validate(&inv.raw_params).unwrap();
        let ParamValue::Float(mm) = params.0["wall_thickness"] else {
            panic!("float")
        };
        assert!((mm - 6.35).abs() < 1e-9, "{mm}");
    }

    #[test]
    fn intent_rejects_wrong_unit_kind() {
        // A length setter on a param that isn't a length.
        let e = Intent::op("mesh.hollow")
            .on(id())
            .set("hole_count", Length::new(3.0, Unit::Millimeter))
            .build()
            .unwrap_err();
        assert!(
            matches!(e, IntentError::WrongUnitKind { ref name, .. } if name == "hole_count"),
            "{e}"
        );
        // A bare number for a length (the double-conversion bug's shape).
        let e = Intent::op("mesh.hollow")
            .on(id())
            .param("wall_thickness", 2.0)
            .build()
            .unwrap_err();
        assert!(
            matches!(e, IntentError::WrongUnitKind { ref name, .. } if name == "wall_thickness"),
            "{e}"
        );
        // A spec-unit length on a document-unit param.
        let e = Intent::op("mesh.split_box")
            .on(id())
            .set_point("box_min", [0.0; 3], Unit::Millimeter)
            .build()
            .unwrap_err();
        assert!(matches!(e, IntentError::WrongUnitKind { .. }), "{e}");
        // A document-unit value on a spec-unit length.
        let e = Intent::op("mesh.split_box")
            .on(id())
            .set_doc("cut_gap", 1.0)
            .build()
            .unwrap_err();
        assert!(matches!(e, IntentError::WrongUnitKind { .. }), "{e}");
    }

    #[test]
    fn unknown_param_op_and_missing_target_are_errors() {
        let e = Intent::op("mesh.hollow")
            .on(id())
            .param("nope", 1)
            .build()
            .unwrap_err();
        assert!(matches!(e, IntentError::UnknownParameter { .. }), "{e}");
        let e = Intent::op("mesh.nope").on(id()).build().unwrap_err();
        assert!(matches!(e, IntentError::UnknownOperation(_)), "{e}");
        let e = Intent::op("mesh.hollow").build().unwrap_err();
        assert!(matches!(e, IntentError::MissingTarget { .. }), "{e}");
        let e = Intent::op("mesh.hollow")
            .on(id())
            .set("wall_thickness", Length::new(f64::NAN, Unit::Millimeter))
            .build()
            .unwrap_err();
        assert!(matches!(e, IntentError::NotFinite { .. }), "{e}");
    }

    #[test]
    fn point_is_written_in_the_spec_unit() {
        let inv = Intent::op("mesh.sculpt_stroke")
            .on(id())
            .set_point("pivot", [1.0, 0.0, -0.5], Unit::Meter)
            .build()
            .unwrap();
        assert_eq!(inv.raw_params["pivot"], "1000,0,-500");
    }

    #[test]
    fn values_route_by_unit() {
        let values = BTreeMap::from([
            ("wall_thickness".to_owned(), ParamValue::Float(2.0)),
            ("hole_count".to_owned(), ParamValue::Int(3)),
        ]);
        let inv = Intent::op("mesh.hollow")
            .on(id())
            .values(&values)
            .build()
            .unwrap();
        assert_eq!(inv.raw_params["wall_thickness"], "2 mm");
        assert_eq!(inv.raw_params["hole_count"], "3");
    }
}
