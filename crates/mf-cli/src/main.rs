//! `meshfixer` command-line interface.
//!
//! Operation subcommands are generated from the built-in operation specs and
//! run through the same [`Session`] path as the desktop app (MF-SPEC-003,
//! MF-CMD-001). Exit codes: 0 success, 1 I/O or operation error, 2 usage or
//! parameter error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Arg, ArgMatches, Command};
use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, Session};
use mf_io::{Format, IoError};
use mf_metrics::{CountingAlloc, Phase, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Per-run measurements written by `--metrics` (MF-RT-005, MF-ISSUE-013).
#[derive(Default)]
struct Metrics {
    phases: Vec<Phase>,
    mesh_bytes: u64,
    topology_bytes: u64,
}

impl Metrics {
    fn record<R>(&mut self, name: &str, f: impl FnOnce() -> R) -> R {
        let (r, phase) = measure(name, f);
        self.phases.push(phase);
        r
    }

    fn structures(&mut self, mesh: &mf_geometry::Mesh) {
        self.mesh_bytes = mesh.heap_bytes();
        self.topology_bytes = mesh.topology().heap_bytes();
    }

    fn write(&self, path: &Path) -> Result<(), Failure> {
        let json = serde_json::json!({
            "phases": self.phases,
            "structures": { "mesh": self.mesh_bytes, "topology": self.topology_bytes },
            "peak_heap": mf_metrics::peak_bytes_since_start(),
            "allocator_installed": mf_metrics::is_installed(),
        });
        let text =
            serde_json::to_string_pretty(&json).map_err(|e| Failure::Runtime(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| Failure::Runtime(format!("writing metrics: {e}")))
    }
}

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

fn output_positional() -> Arg {
    Arg::new("output")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Output mesh (.stl or .obj)")
}

fn cli() -> Command {
    let mut cmd = Command::new("meshfixer")
        .about("Inspect and repair meshes")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg(
            Arg::new("metrics")
                .long("metrics")
                .global(true)
                .value_parser(clap::value_parser!(PathBuf))
                .help("Write timing and memory measurements as JSON to this path"),
        )
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
    cmd = cmd.subcommand(
        Command::new("gen-fixture")
            .about("Generate a deterministic test mesh")
            .arg(
                Arg::new("kind")
                    .required(true)
                    .value_parser(["damaged"])
                    .help("damaged: UV sphere with holes, islands and unwelded corners"),
            )
            .arg(output_positional())
            .arg(
                Arg::new("triangles")
                    .long("triangles")
                    .default_value("1000000")
                    .value_parser(clap::value_parser!(u64).range(100..=200_000_000)),
            )
            .arg(
                Arg::new("seed")
                    .long("seed")
                    .default_value("1")
                    .value_parser(clap::value_parser!(u64)),
            ),
    );
    cmd = cmd
        .subcommand(
            Command::new("run")
                .about("Run a sequence of operations, optionally saving a command log")
                .arg(input_arg())
                .arg(output_arg())
                .arg(
                    Arg::new("op")
                        .long("op")
                        .action(clap::ArgAction::Append)
                        .help("Operation id (repeatable, run in order)"),
                )
                .arg(
                    Arg::new("param")
                        .long("param")
                        .action(clap::ArgAction::Append)
                        .help("name=value for the preceding --op (repeatable)"),
                )
                .arg(
                    Arg::new("save-log")
                        .long("save-log")
                        .value_parser(clap::value_parser!(PathBuf))
                        .help("Write the command log to this directory (<name>.mfx.log)"),
                ),
        )
        .subcommand(
            Command::new("replay")
                .about("Replay a command log and verify every command's output")
                .arg(
                    Arg::new("log")
                        .required(true)
                        .value_parser(clap::value_parser!(PathBuf))
                        .help("Command log directory"),
                )
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_parser(clap::value_parser!(PathBuf))
                        .help("Export the replayed result of the first object"),
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

fn open(path: &Path, metrics: &mut Metrics) -> Result<(Session, ObjectId), Failure> {
    let imported = metrics.record("import", || mf_io::read_path(path))?;
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

fn inspect(m: &ArgMatches, metrics: &mut Metrics) -> Result<(), Failure> {
    let path = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let (mut session, id) = open(path, metrics)?;
    if let Some(mesh) = session.document().object(id).and_then(|o| o.mesh()) {
        let mesh = mesh.clone();
        metrics.record("topology", || mesh.topology());
        metrics.structures(&mesh);
    }
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
            serde_json::to_string_pretty(&report).map_err(|e| Failure::Runtime(e.to_string()))?
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

fn gen_fixture(m: &ArgMatches) -> Result<(), Failure> {
    let output = m
        .get_one::<PathBuf>("output")
        .ok_or_else(|| Failure::Usage("missing output".into()))?;
    let format = Format::from_path(output).ok_or_else(|| {
        Failure::Usage(format!("unsupported output format: {}", output.display()))
    })?;
    let triangles = *m.get_one::<u64>("triangles").unwrap_or(&1_000_000) as usize;
    let seed = *m.get_one::<u64>("seed").unwrap_or(&1);
    let mesh =
        mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(triangles, seed));
    mf_io::write_path(&mesh, output, format)?;
    eprintln!(
        "wrote {} triangles to {}",
        mesh.triangle_count(),
        output.display()
    );
    Ok(())
}

/// `--op` and `--param` values in command-line order, params grouped under
/// the op before them.
/// One `--op` with its `--param`s.
type Step = (String, BTreeMap<String, String>);

fn op_sequence(m: &ArgMatches) -> Result<Vec<Step>, Failure> {
    let ops: Vec<(usize, String)> = match (m.indices_of("op"), m.get_many::<String>("op")) {
        (Some(i), Some(v)) => i.zip(v.cloned()).collect(),
        _ => Vec::new(),
    };
    let params: Vec<(usize, String)> = match (m.indices_of("param"), m.get_many::<String>("param"))
    {
        (Some(i), Some(v)) => i.zip(v.cloned()).collect(),
        _ => Vec::new(),
    };
    if ops.is_empty() {
        return Err(Failure::Usage("run needs at least one --op".into()));
    }
    let mut seq: Vec<Step> = Vec::new();
    for (idx, op) in &ops {
        let next = ops
            .iter()
            .map(|(i, _)| *i)
            .find(|i| i > idx)
            .unwrap_or(usize::MAX);
        let mut raw = BTreeMap::new();
        for (pi, p) in params.iter().filter(|(pi, _)| pi > idx && *pi < next) {
            let (k, v) = p.split_once('=').ok_or_else(|| {
                Failure::Usage(format!("--param '{p}' (position {pi}) must be name=value"))
            })?;
            raw.insert(k.to_owned(), v.to_owned());
        }
        seq.push((op.clone(), raw));
    }
    let first_op = ops[0].0;
    if let Some((_, p)) = params.iter().find(|(pi, _)| *pi < first_op) {
        return Err(Failure::Usage(format!(
            "--param '{p}' comes before any --op"
        )));
    }
    Ok(seq)
}

fn run_sequence(m: &ArgMatches, metrics: &mut Metrics) -> Result<(), Failure> {
    let input = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let output = m
        .get_one::<PathBuf>("output")
        .ok_or_else(|| Failure::Usage("missing output".into()))?;
    let format = Format::from_path(output).ok_or_else(|| {
        Failure::Usage(format!("unsupported output format: {}", output.display()))
    })?;
    let seq = op_sequence(m)?;
    // Validate every step before touching files.
    for (op, raw) in &seq {
        let spec = mf_spec::builtin::get(op)
            .ok_or_else(|| Failure::Usage(format!("unknown operation '{op}'")))?;
        spec.validate(raw)
            .map_err(|e| Failure::Usage(format!("{op}: {e}")))?;
    }
    let (mut session, id) = open(input, metrics)?;
    for (op, raw) in seq {
        let outcome = metrics.record("operation", || {
            session.execute(Invocation {
                op_id: op.clone(),
                target: id,
                raw_params: raw,
            })
        })?;
        match outcome {
            CommitOutcome::NewRevision { report } => eprintln!("{op}: {report}"),
            CommitOutcome::NoChange { report } => eprintln!("{op}: no change ({report})"),
            CommitOutcome::Report(report) => eprintln!("{op}: {report}"),
        }
    }
    if let Some(dir) = m.get_one::<PathBuf>("save-log") {
        session
            .save_log(dir)
            .map_err(|e| Failure::Runtime(e.to_string()))?;
        eprintln!("saved command log to {}", dir.display());
    }
    let mesh = session
        .document()
        .object(id)
        .and_then(|o| o.mesh())
        .ok_or_else(|| Failure::Runtime("only meshes can be exported".into()))?;
    metrics.structures(mesh);
    metrics.record("export", || mf_io::write_path(mesh, output, format))?;
    Ok(())
}

fn replay_log(m: &ArgMatches) -> Result<(), Failure> {
    let dir = m
        .get_one::<PathBuf>("log")
        .ok_or_else(|| Failure::Usage("missing log".into()))?;
    let report = mf_command::replay(dir).map_err(|e| Failure::Runtime(e.to_string()))?;
    for (id, verdict) in &report.results {
        println!("command {}: {verdict:?}", id.0);
    }
    if let Some(out) = m.get_one::<PathBuf>("output") {
        let format = Format::from_path(out).ok_or_else(|| {
            Failure::Usage(format!("unsupported output format: {}", out.display()))
        })?;
        let mesh = report
            .session
            .document()
            .objects()
            .first()
            .and_then(|o| o.mesh())
            .ok_or_else(|| Failure::Runtime("the log produced no mesh".into()))?;
        mf_io::write_path(mesh, out, format)?;
    }
    if report.all_verified() {
        println!("all {} command(s) verified", report.results.len());
        Ok(())
    } else {
        Err(Failure::Runtime("replay did not reproduce the log".into()))
    }
}

fn run_operation(
    spec: &mf_spec::OperationSpec,
    m: &ArgMatches,
    metrics: &mut Metrics,
) -> Result<(), Failure> {
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

    let (mut session, id) = open(input, metrics)?;
    let outcome = metrics.record("operation", || {
        session.execute(Invocation {
            op_id: spec.id.clone(),
            target: id,
            raw_params,
        })
    })?;
    if let CommitOutcome::NewRevision { report } = outcome {
        eprintln!("{}: {report}", spec.id);
    }
    let object = session
        .document()
        .object(id)
        .ok_or_else(|| Failure::Runtime("object disappeared".into()))?;
    let mesh = object
        .mesh()
        .ok_or_else(|| Failure::Runtime("only meshes can be exported".into()))?;
    metrics.structures(mesh);
    metrics.record("export", || mf_io::write_path(mesh, output, format))?;
    Ok(())
}

fn main() -> ExitCode {
    let matches = cli().get_matches();
    let mut metrics = Metrics::default();
    let result = match matches.subcommand() {
        Some(("inspect", m)) => inspect(m, &mut metrics),
        Some(("render", m)) => render(m),
        Some(("gen-fixture", m)) => gen_fixture(m),
        Some(("run", m)) => run_sequence(m, &mut metrics),
        Some(("replay", m)) => replay_log(m),
        Some((name, m)) => match mf_spec::builtin::specs()
            .iter()
            .find(|s| mf_spec::cli::subcommand_name(s) == name)
        {
            Some(spec) => run_operation(spec, m, &mut metrics),
            None => Err(Failure::Usage(format!("unknown command '{name}'"))),
        },
        None => Err(Failure::Usage("no command given".into())),
    };
    let result = result.and_then(|()| {
        let path = matches
            .subcommand()
            .and_then(|(_, m)| m.get_one::<PathBuf>("metrics").cloned());
        match path {
            Some(p) => metrics.write(&p),
            None => Ok(()),
        }
    });
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
