#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Phase 1 Task 2 (MF-P1-CMD-001, MF-P1-SEL-001): the operand contract and
//! the vec3/affine parameter types in operation specs. `operands:` stays
//! optional, so every spec YAML that existed before Phase 1 still
//! validates unchanged.

use mf_io::threemf::Unit;
use mf_spec::{OperandRole, ParamValue, Params, builtin};

/// `ParamKind::Vec3` and `ParamKind::Affine` parse from their CLI/JSON
/// forms and round-trip, including through a command log (JSON).
#[test]
fn vec3_and_affine_params_validate_and_round_trip() {
    let yaml = r#"
id: mesh.test_vec3_affine
version: 1
summary: Test fixture for vec3 and affine parameters.
geometry_kinds: [mesh]
target:
  whole_object: true
  kinds: []
parameters:
  - name: plane_point
    type: vec3
    default: [0.0, 0.0, 0.0]
    unit: mm
    description: A point on the plane
  - name: plane_normal
    type: vec3
    default: [0.0, 0.0, 1.0]
    description: The plane's normal (no unit - a direction)
  - name: transform
    type: affine
    default: [1,0,0, 0,1,0, 0,0,1, 0,0,0]
    description: The absolute resulting transform
preconditions: []
postconditions: []
effects: [geometry_changes]
runtime:
  cancellable: true
  progress: true
"#;
    let spec: mf_spec::OperationSpec = serde_saphyr::from_str(yaml).unwrap();

    // Defaults fill in as Vec3/Affine values.
    let p = spec.validate(&Default::default()).unwrap();
    assert_eq!(
        p.0.get("plane_point"),
        Some(&ParamValue::Vec3([0.0, 0.0, 0.0]))
    );
    assert_eq!(
        p.0.get("plane_normal"),
        Some(&ParamValue::Vec3([0.0, 0.0, 1.0]))
    );
    assert_eq!(
        p.0.get("transform"),
        Some(&ParamValue::Affine([
            1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0
        ]))
    );

    // CLI form: comma-separated values.
    let mut raw = std::collections::BTreeMap::new();
    raw.insert("plane_point".to_string(), "1.5,2.5,-3.0".to_string());
    raw.insert(
        "transform".to_string(),
        "1,0,0,0,1,0,0,0,1,10,20,30".to_string(),
    );
    let p = spec.validate(&raw).unwrap();
    assert_eq!(
        p.0.get("plane_point"),
        Some(&ParamValue::Vec3([1.5, 2.5, -3.0]))
    );
    assert_eq!(
        p.0.get("transform"),
        Some(&ParamValue::Affine([
            1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 10.0, 20.0, 30.0
        ]))
    );

    // A wrong element count is a named error, not a panic or a silent
    // truncation.
    let mut bad = std::collections::BTreeMap::new();
    bad.insert("plane_point".to_string(), "1,2".to_string());
    assert!(spec.validate(&bad).is_err());

    // Vec3/Affine round-trip through JSON (as a command log would store
    // them) bit for bit.
    let json = serde_json::to_value(&p).unwrap();
    assert_eq!(json["plane_point"], serde_json::json!([1.5, 2.5, -3.0]));
    let back: Params = serde_json::from_value(json).unwrap();
    assert_eq!(back, p);
}

/// Specs gain an optional `operands:` list, validated by the schema, and
/// older spec YAML (with no `operands:` key at all) still validates.
#[test]
fn operands_section_validates_against_schema() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../specs/operation.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let with_operands = r#"
id: mesh.test_with_operands
version: 1
summary: Test fixture with an operands section.
geometry_kinds: [mesh]
target:
  whole_object: true
  kinds: []
operands:
  - name: tool
    role: consumed
    kinds: [object, component]
    required: true
parameters: []
effects: [geometry_changes]
runtime:
  cancellable: true
  progress: true
"#;
    let value: serde_json::Value = serde_saphyr::from_str(with_operands).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    // The operand model parses with its role and kinds.
    let spec: mf_spec::OperationSpec = serde_saphyr::from_str(with_operands).unwrap();
    assert_eq!(spec.operands.len(), 1);
    assert_eq!(spec.operands[0].name, "tool");
    assert_eq!(spec.operands[0].role, OperandRole::Consumed);
    assert_eq!(spec.operands[0].kinds, vec!["object", "component"]);
    assert!(spec.operands[0].required);

    // Every built-in spec validates. Those written before Phase 1 (none of
    // which declare `operands:`) parse unchanged with an empty operands
    // list; later specs (the test-only `test.consume_tool`, Phase 1 Task 3;
    // `mesh.boolean`, Task 8) may declare operands.
    for (name, text) in builtin::sources() {
        let value: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let spec: mf_spec::OperationSpec = serde_saphyr::from_str(text).unwrap();
        const PHASE_0: [&str; 4] = [
            "mesh.inspect",
            "mesh.fill_holes",
            "mesh.remove_small_components",
            "mesh.compact",
        ];
        if PHASE_0.contains(&spec.id.as_str()) {
            assert!(spec.operands.is_empty(), "{name} should have no operands");
        }
    }
}

/// `ParamSpec.unit: Some("mm")` marks a length; `to_document_units`
/// converts every such parameter's value and runs in `prepare` (D20's
/// "everything is converted to document units when the command is
/// prepared").
#[test]
fn lengths_convert_to_document_units() {
    let yaml = r#"
id: mesh.test_lengths
version: 1
summary: Test fixture with length parameters.
geometry_kinds: [mesh]
target:
  whole_object: true
  kinds: []
parameters:
  - name: hole_diameter
    type: float
    default: 5.0
    unit: mm
    description: Hole diameter
  - name: offset
    type: vec3
    default: [1.0, 2.0, 3.0]
    unit: mm
    description: An offset point
  - name: peg_count
    type: integer
    default: 2
    description: Not a length
  - name: plane_normal
    type: vec3
    default: [0.0, 0.0, 1.0]
    description: A direction, not a length (no unit)
preconditions: []
postconditions: []
effects: [geometry_changes]
runtime:
  cancellable: true
  progress: true
"#;
    let spec: mf_spec::OperationSpec = serde_saphyr::from_str(yaml).unwrap();
    let mut params = spec.validate(&Default::default()).unwrap();

    // A metre-scale document: 1 document unit = 1000 mm, so 5 mm becomes
    // 0.005 document units (Review Focus 4).
    spec.to_document_units(&mut params, Unit::Meter);
    assert_eq!(params.get_float("hole_diameter").unwrap(), 0.005);
    match params.0.get("offset") {
        Some(ParamValue::Vec3(v)) => {
            assert_eq!(*v, [0.001, 0.002, 0.003]);
        }
        other => panic!("expected Vec3, got {other:?}"),
    }
    // Non-length parameters are untouched.
    assert_eq!(params.get_int("peg_count").unwrap(), 2);
    match params.0.get("plane_normal") {
        Some(ParamValue::Vec3(v)) => assert_eq!(*v, [0.0, 0.0, 1.0]),
        other => panic!("expected Vec3, got {other:?}"),
    }

    // Millimetre documents (the common case) convert by exactly 1.0.
    let mut params_mm = spec.validate(&Default::default()).unwrap();
    spec.to_document_units(&mut params_mm, Unit::Millimeter);
    assert_eq!(params_mm.get_float("hole_diameter").unwrap(), 5.0);
}
