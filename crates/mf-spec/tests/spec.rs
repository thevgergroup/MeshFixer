#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::collections::BTreeMap;

use mf_spec::{ParamValue, SpecError, builtin, cli};

fn raw(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn builtin_specs_validate_against_schema() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../specs/operation.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (name, text) in builtin::sources() {
        let value: serde_json::Value = serde_saphyr::from_str(text).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
    assert!(builtin::specs().len() >= 2);
}

#[test]
fn defaults_filled() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let p = spec.validate(&raw(&[])).unwrap();
    assert_eq!(p.get_int("max_triangles").unwrap(), 100);
    assert!(p.get_bool("keep_largest").unwrap());
}

#[test]
fn values_parsed() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let p = spec
        .validate(&raw(&[("max_triangles", "7"), ("keep_largest", "false")]))
        .unwrap();
    assert_eq!(p.0.get("max_triangles"), Some(&ParamValue::Int(7)));
    assert_eq!(p.0.get("keep_largest"), Some(&ParamValue::Bool(false)));
}

#[test]
fn out_of_range_rejected() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let err = spec.validate(&raw(&[("max_triangles", "0")])).unwrap_err();
    assert!(
        matches!(err, SpecError::OutOfRange { ref name, .. } if name == "max_triangles"),
        "{err:?}"
    );
}

#[test]
fn invalid_value_rejected() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let err = spec
        .validate(&raw(&[("max_triangles", "lots")]))
        .unwrap_err();
    assert!(matches!(err, SpecError::InvalidValue { .. }), "{err:?}");
}

#[test]
fn unknown_param_rejected() {
    let spec = builtin::get("mesh.inspect").unwrap();
    let err = spec.validate(&raw(&[("bogus", "1")])).unwrap_err();
    assert_eq!(err, SpecError::UnknownParameter("bogus".into()));
}

#[test]
fn params_serialize_as_json_map() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let p = spec.validate(&raw(&[])).unwrap();
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        serde_json::json!({"keep_largest": true, "max_triangles": 100})
    );
}

#[test]
fn cli_args_match_spec() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let cmd = cli::to_command(spec);
    assert_eq!(cmd.get_name(), "remove-small-components");
    let args: Vec<_> = cmd.get_arguments().filter_map(|a| a.get_long()).collect();
    assert!(
        args.contains(&"max-triangles") && args.contains(&"keep-largest"),
        "{args:?}"
    );
    for p in &spec.parameters {
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_id().as_str() == p.name)
            .unwrap();
        assert_eq!(arg.get_help().unwrap().to_string(), p.description);
    }
}

#[test]
fn cli_raw_params_roundtrip() {
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let m = cli::to_command(spec)
        .try_get_matches_from(["x", "--max-triangles", "12"])
        .unwrap();
    assert_eq!(cli::raw_params(spec, &m), raw(&[("max_triangles", "12")]));
}
