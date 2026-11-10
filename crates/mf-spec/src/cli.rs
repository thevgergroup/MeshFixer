//! CLI arguments derived from operation specs (MF-SPEC-003).

use std::collections::BTreeMap;

use clap::{Arg, ArgMatches, Command};

use crate::model::{OperationSpec, ParamKind};

/// `mesh.remove_small_components` → `remove-small-components`.
pub fn subcommand_name(spec: &OperationSpec) -> String {
    let short = spec.id.split_once('.').map_or(spec.id.as_str(), |(_, s)| s);
    short.replace('_', "-")
}

/// One `--param-name <VALUE>` argument per spec parameter. Defaults are left
/// to spec validation so there's a single source of truth.
pub fn to_command(spec: &OperationSpec) -> Command {
    let mut cmd = Command::new(subcommand_name(spec)).about(spec.summary.clone());
    for p in &spec.parameters {
        let (value_name, default): (String, String) = match &p.kind {
            ParamKind::Integer { default, .. } => ("INT".into(), default.to_string()),
            ParamKind::Float { default, .. } => ("NUMBER".into(), default.to_string()),
            ParamKind::Bool { default } => ("true|false".into(), default.to_string()),
            ParamKind::Choice { default, values } => (values.join("|"), default.clone()),
            ParamKind::Vec3 { default } => ("X,Y,Z".into(), csv(default)),
            ParamKind::Affine { default } => ("M00,...,M32 (12 values)".into(), csv(default)),
            ParamKind::Text { default } => ("TEXT".into(), default.clone()),
            ParamKind::Samples => ("FACE,B0,B1,B2,PRESSURE;...".into(), String::new()),
            ParamKind::Faces => ("FACE,...".into(), String::new()),
        };
        let unit = p
            .unit
            .as_deref()
            .map(|u| format!(", unit: {u}"))
            // allow-swallow: no unit means no help suffix
            .unwrap_or_default();
        cmd = cmd.arg(
            Arg::new(p.name.clone())
                .long(p.name.replace('_', "-"))
                .value_name(value_name)
                .help(p.description.clone())
                .long_help(format!("{} [default: {default}{unit}]", p.description)),
        );
    }
    cmd
}

/// Comma-separated, as the CLI and command-log string form takes it.
fn csv(values: &[f64]) -> String {
    values
        .iter()
        .map(f64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Collect the string values the user actually supplied.
pub fn raw_params(spec: &OperationSpec, matches: &ArgMatches) -> BTreeMap<String, String> {
    spec.parameters
        .iter()
        .filter_map(|p| {
            matches
                .get_one::<String>(&p.name)
                .map(|v| (p.name.clone(), v.clone()))
        })
        .collect()
}
