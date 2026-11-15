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

/// Every registered operation spec declares a History panel label (owner
/// 2026-11-13: the History panel showed `mesh.hollow (drain_holes=none,
/// ..., open_through_faces=37048, ...)` — a raw parameter dump — for the
/// third time; this is the structural fix, not a one-off patch). The
/// schema already requires `history_label` with `minLength: 1`
/// (`builtin_specs_validate_against_schema`); this iterates the parsed
/// specs directly so a future field that bypasses the schema (a
/// Rust-constructed spec, `mf_parts::shape::spec_of`) is still caught.
/// Whether the id actually resolves in mf-desktop's i18n catalogue is
/// checked there (`mf-spec` has no i18n dependency):
/// `crates/mf-desktop/src/app.rs`'s `every_built_in_spec_has_a_catalogued_history_label`.
#[test]
fn every_built_in_spec_has_a_history_label() {
    for spec in builtin::specs() {
        assert!(
            !spec.history_label.trim().is_empty(),
            "{} has no history_label",
            spec.id
        );
    }
}

/// Every registered operation spec declares its coalescing (ADR-0009):
/// `Coalesce::Never` by default (so a delta command — a sculpt stroke,
/// a fill — queued behind a running one keeps its own undo step rather
/// than being merged away), `Coalesce::Absolute` only for a command
/// whose params are the whole resulting state (`object.set_transform`'s
/// placement, `object.set_shape_params`'s recipe), where a newly queued
/// invocation fully supersedes the one already waiting. This is a
/// closed, named list rather than "anything not Never": a future spec
/// that wants to coalesce must be added here deliberately, so the
/// contract stays reviewed rather than silently widening.
#[test]
fn every_spec_declares_coalescing() {
    use mf_spec::Coalesce;
    const ABSOLUTE: &[&str] = &["object.set_transform", "object.set_shape_params"];
    for spec in builtin::specs() {
        let expected = if ABSOLUTE.contains(&spec.id.as_str()) {
            Coalesce::Absolute
        } else {
            Coalesce::Never
        };
        assert_eq!(
            spec.coalesce, expected,
            "{} declares {:?}, expected {:?}",
            spec.id, spec.coalesce, expected
        );
    }
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
        serde_json::json!({"keep_largest": true, "max_relative_size": 1.0, "max_triangles": 100})
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

#[test]
fn float_params_roundtrip_exactly_through_json() {
    // Command logs must reproduce parameters bit-for-bit (MF-CMD-004).
    // A value seen to drift (0.171417 → 0.17141699999999999) with
    // serde_json's default parser, then a broad sweep.
    for v in [0.171417_f64, 0.9595, 1e-300, 5e-324, f64::MAX] {
        let back: ParamValue =
            serde_json::from_str(&serde_json::to_string(&ParamValue::Float(v)).unwrap()).unwrap();
        assert_eq!(
            back,
            ParamValue::Float(v),
            "value {v:e} changed in a JSON round trip"
        );
    }
    let mut x = 0.123_456_789_f64;
    for i in 0..20_000 {
        x = (x * 1.75 + i as f64 * 1e-7).fract() * 10f64.powi((i % 12) - 6);
        let p = ParamValue::Float(x);
        let back: ParamValue = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p, "value {x:e} changed in a JSON round trip");
    }
}

/// A `choice` parameter (MF-OP-002 fill styles): one of a listed set of
/// words, its default filled in, anything else rejected by name.
#[test]
fn choice_params_parse_and_reject_unknown_values() {
    let spec = builtin::get("mesh.fill_holes").unwrap();
    let p = spec.validate(&raw(&[])).unwrap();
    assert_eq!(p.get_choice("fill_style").unwrap(), "smooth");
    let p = spec.validate(&raw(&[("fill_style", "flat")])).unwrap();
    assert_eq!(
        p.0.get("fill_style"),
        Some(&ParamValue::Text("flat".into()))
    );
    let err = spec.validate(&raw(&[("fill_style", "bumpy")])).unwrap_err();
    assert!(
        matches!(err, SpecError::InvalidValue { ref name, ref reason, .. }
            if name == "fill_style" && reason.contains("flat, smooth or curvature")),
        "{err:?}"
    );
    // Text round-trips through a command log as a JSON string.
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"fill_style\":\"flat\""), "{json}");
    let back: mf_spec::Params = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    // The CLI shows the choices.
    let cmd = cli::to_command(spec);
    let arg = cmd
        .get_arguments()
        .find(|a| a.get_id().as_str() == "fill_style")
        .unwrap();
    assert_eq!(
        arg.get_value_names().unwrap()[0].as_str(),
        "flat|smooth|curvature"
    );
}
