//! `meshfixer` command-line interface.
//!
//! Operation subcommands are generated from the built-in operation specs and
//! run through the same [`Session`] path as the desktop app (MF-SPEC-003,
//! MF-CMD-001). Exit codes: 0 success, 1 I/O or operation error, 2 usage or
//! parameter error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Arg, ArgMatches, Command};
use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, Session};
use mf_io::{Format, IoError};

enum Failure {
    Usage(String),
    Runtime(String),
}

impl From<IoError> for Failure {
    fn from(e: IoError) -> Self {
        Failure::Runtime(e.to_string())
    }
}

impl From<CommandError> for Failure {
    fn from(e: CommandError) -> Self {
        match e {
            CommandError::Spec(_) | CommandError::UnknownOperation(_) => {
                Failure::Usage(e.to_string())
            }
            _ => Failure::Runtime(e.to_string()),
        }
    }
}

fn input_arg() -> Arg {
    Arg::new("input")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Input mesh (.stl or .obj)")
}

fn output_arg() -> Arg {
    Arg::new("output")
        .short('o')
        .long("output")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Output mesh (.stl or .obj)")
}

fn cli() -> Command {
    let mut cmd = Command::new("meshfixer")
        .about("Inspect and repair meshes")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .subcommand(
            Command::new("inspect")
                .about("Report mesh diagnostics")
                .arg(input_arg())
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Print diagnostics as JSON"),
                ),
        )
        .subcommand(
            Command::new("render")
                .about("Render a mesh to a PNG image without opening a window")
                .arg(input_arg())
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .required(true)
                        .value_parser(clap::value_parser!(PathBuf))
                        .help("Output PNG"),
                )
                .arg(
                    Arg::new("width")
                        .long("width")
                        .default_value("1024")
                        .value_parser(clap::value_parser!(u32).range(1..=16384)),
                )
                .arg(
                    Arg::new("height")
                        .long("height")
                        .default_value("768")
                        .value_parser(clap::value_parser!(u32).range(1..=16384)),
                )
                .arg(
                    Arg::new("no-boundaries")
                        .long("no-boundaries")
                        .action(clap::ArgAction::SetTrue)
                        .help("Don't draw boundary-loop overlays"),
                ),
        );
    for spec in mf_spec::builtin::specs()
        .iter()
        .filter(|s| !s.is_read_only())
    {
        cmd = cmd.subcommand(
            mf_spec::cli::to_command(spec)
                .arg(input_arg())
                .arg(output_arg()),
        );
    }
    cmd
}

fn open(path: &Path) -> Result<(Session, ObjectId), Failure> {
    let imported = mf_io::read_path(path)?;
    for w in &imported.warnings {
        eprintln!("warning: {w}");
    }
    let mut session = Session::new();
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("object")
        .to_owned();
    let id = session.import(imported, name);
    Ok((session, id))
}

fn inspect(m: &ArgMatches) -> Result<(), Failure> {
    let path = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let (mut session, id) = open(path)?;
    let inv = Invocation {
        op_id: "mesh.inspect".into(),
        target: id,
        raw_params: Default::default(),
    };
    let CommitOutcome::Report(report) = session.execute(inv)? else {
        return Err(Failure::Runtime(
            "inspect unexpectedly changed the document".into(),
        ));
    };
    if m.get_flag("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else if let Some(obj) = report.as_object() {
        for (k, v) in obj {
            if k == "boundary_loops" {
                println!("{k:>32}: {}", v.as_array().map_or(0, |a| a.len()));
            } else {
                println!("{k:>32}: {v}");
            }
        }
    }
    Ok(())
}

fn render(m: &ArgMatches) -> Result<(), Failure> {
    let input = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let output = m
        .get_one::<PathBuf>("output")
        .ok_or_else(|| Failure::Usage("missing output".into()))?;
    let width = *m.get_one::<u32>("width").unwrap_or(&1024);
    let height = *m.get_one::<u32>("height").unwrap_or(&768);
    let imported = mf_io::read_path(input)?;
    let image = mf_render::offscreen::render_image(
        &imported.mesh,
        width,
        height,
        !m.get_flag("no-boundaries"),
    )
    .map_err(|e| Failure::Runtime(e.to_string()))?;
    image
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|e| Failure::Runtime(e.to_string()))?;
    Ok(())
}

fn run_operation(spec: &mf_spec::OperationSpec, m: &ArgMatches) -> Result<(), Failure> {
    let input = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let output = m
        .get_one::<PathBuf>("output")
        .ok_or_else(|| Failure::Usage("missing output".into()))?;
    let format = Format::from_path(output).ok_or_else(|| {
        Failure::Usage(format!("unsupported output format: {}", output.display()))
    })?;
    // Validate parameters before touching the filesystem.
    let raw_params = mf_spec::cli::raw_params(spec, m);
    spec.validate(&raw_params)
        .map_err(|e| Failure::Usage(e.to_string()))?;

    let (mut session, id) = open(input)?;
    let outcome = session.execute(Invocation {
        op_id: spec.id.clone(),
        target: id,
        raw_params,
    })?;
    if let CommitOutcome::NewRevision { report } = outcome {
        eprintln!("{}: {report}", spec.id);
    }
    let object = session
        .document()
        .object(id)
        .ok_or_else(|| Failure::Runtime("object disappeared".into()))?;
    mf_io::write_path(&object.mesh, output, format)?;
    Ok(())
}

fn main() -> ExitCode {
    let matches = cli().get_matches();
    let result = match matches.subcommand() {
        Some(("inspect", m)) => inspect(m),
        Some(("render", m)) => render(m),
        Some((name, m)) => match mf_spec::builtin::specs()
            .iter()
            .find(|s| mf_spec::cli::subcommand_name(s) == name)
        {
            Some(spec) => run_operation(spec, m),
            None => Err(Failure::Usage(format!("unknown command '{name}'"))),
        },
        None => Err(Failure::Usage("no command given".into())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
    }
}
