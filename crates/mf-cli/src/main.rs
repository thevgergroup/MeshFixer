//! `meshfixer` command-line interface.
//!
//! Operation subcommands are generated from the built-in operation specs and
//! run through the same [`Session`] path as the desktop app (MF-SPEC-003,
//! MF-CMD-001). Exit codes: 0 success, 1 I/O or operation error, 2 usage or
//! parameter error, 130 cancelled (Ctrl-C / SIGINT: the running job stops
//! and no output file is written).
//!
//! Work runs on one job pool (`--threads`, default all cores but one;
//! MF-RT-004). `--progress` prints `progress <stage> <fraction>` lines to
//! stderr (MF-RT-003).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::{Arg, ArgMatches, Command};
use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, RevisionId, Session};
use mf_io::{Format, IoError};
use mf_metrics::{CountingAlloc, GpuBufferBytes, MemoryBreakdown, Phase, measure};
use mf_runtime::{JobPool, PoolChoice, RunContext};
use mf_selection::{Selection, SelectionKind, Translation};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Per-run measurements written by `--metrics` (MF-RT-005, MF-ISSUE-013).
#[derive(Default)]
struct Metrics {
    phases: Vec<Phase>,
    mesh_bytes: u64,
    topology_bytes: u64,
    /// The job pool's size: requested, chosen, cores and why (MF-RT-004).
    threads: Option<(Option<usize>, PoolChoice)>,
    /// GPU allocations by kind, when the command used a GPU (`bench orbit`).
    gpu: Option<GpuBufferBytes>,
    /// Frame-time percentiles (`frame_ms`, `cpu_ms`, `gpu_ms`, `fps`),
    /// when the command rendered frames (`bench orbit`).
    frames: Option<serde_json::Map<String, serde_json::Value>>,
    /// The process RSS measured by the command at its busiest (`bench
    /// orbit`: while the GPU mesh is held); otherwise sampled at the end.
    rss: Option<Result<mf_metrics::ProcessMemory, String>>,
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

    /// Where memory went (MF-RT-005): counted storage, the last
    /// operation's peak, GPU buffers and the process RSS side by side.
    fn memory(&self) -> MemoryBreakdown {
        let operation = self.phases.iter().rev().find(|p| p.name == "operation");
        let (scratch_peak_bytes, scratch_peak_note) = match operation {
            None => (None, Some("no operation run".to_owned())),
            Some(_) if !mf_metrics::is_installed() => {
                (None, Some("no counting allocator".to_owned()))
            }
            Some(p) => (Some(p.peak_bytes), None),
        };
        let mut memory = MemoryBreakdown {
            mesh_storage_bytes: self.mesh_bytes,
            topology_bytes: (self.topology_bytes > 0).then_some(self.topology_bytes),
            scratch_peak_bytes,
            scratch_peak_note,
            gpu: self.gpu,
            ..MemoryBreakdown::default()
        };
        memory.set_process(self.rss.clone().unwrap_or_else(mf_metrics::process_memory));
        memory
    }

    fn write(&self, path: &Path) -> Result<(), Failure> {
        let mut json = serde_json::json!({
            "phases": self.phases,
            "structures": { "mesh": self.mesh_bytes, "topology": self.topology_bytes },
            "memory": self.memory(),
            "gpu_buffer_bytes": self.gpu,
            "peak_heap": mf_metrics::peak_bytes_since_start(),
            "threads": self.threads.map(|(requested, choice)| serde_json::json!({
                "requested": requested,
                "threads": choice.threads,
                "cores": choice.cores,
                "reason": choice.reason,
            })),
            "allocator_installed": mf_metrics::is_installed(),
        });
        if let (Some(frames), Some(obj)) = (&self.frames, json.as_object_mut()) {
            obj.extend(frames.clone());
        }
        let text =
            serde_json::to_string_pretty(&json).map_err(|e| Failure::Runtime(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| Failure::Runtime(format!("writing metrics: {e}")))
    }
}

enum Failure {
    Usage(String),
    Runtime(String),
    /// Ctrl-C: the job stopped and nothing was written.
    Cancelled,
}

impl From<IoError> for Failure {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Cancelled => Failure::Cancelled,
            e => Failure::Runtime(e.to_string()),
        }
    }
}

impl From<CommandError> for Failure {
    fn from(e: CommandError) -> Self {
        match e {
            CommandError::Spec(_) | CommandError::UnknownOperation(_) => {
                Failure::Usage(e.to_string())
            }
            CommandError::Cancelled => Failure::Cancelled,
            _ => Failure::Runtime(e.to_string()),
        }
    }
}

impl From<mf_runtime::Cancelled> for Failure {
    fn from(_: mf_runtime::Cancelled) -> Self {
        Failure::Cancelled
    }
}

/// Run `inv` with `ctx` (cancellable, reporting progress) and commit it.
fn execute(
    session: &mut Session,
    inv: Invocation,
    ctx: &RunContext,
) -> Result<CommitOutcome, CommandError> {
    let completed = session.prepare(inv)?.run(ctx)?;
    session.commit(completed)
}

fn input_arg() -> Arg {
    Arg::new("input")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Input mesh (.stl, .obj or .3mf)")
}

/// `--units`: what a unitless input's (STL, OBJ) numbers measure
/// (MF-ISSUE-045). Without it they're read as millimetres, with a warning.
fn units_arg() -> Arg {
    Arg::new("units")
        .long("units")
        .value_parser(["mm", "cm", "m", "in"])
        .help("Units of an STL or OBJ input (default: mm, with a warning)")
}

/// The unit `--units` names, if given.
fn units_of(m: &ArgMatches) -> Option<mf_io::threemf::Unit> {
    use mf_io::threemf::Unit;
    m.get_one::<String>("units").map(|u| match u.as_str() {
        "cm" => Unit::Centimeter,
        "m" => Unit::Meter,
        "in" => Unit::Inch,
        "mm" => Unit::Millimeter,
        other => unreachable!("--units only accepts mm, cm, m or in, not {other}"),
    })
}

fn output_arg() -> Arg {
    Arg::new("output")
        .short('o')
        .long("output")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Output mesh (.stl, .obj or .3mf)")
}

fn output_positional() -> Arg {
    Arg::new("output")
        .required(true)
        .value_parser(clap::value_parser!(PathBuf))
        .help("Output mesh (.stl, .obj or .3mf)")
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
        .arg(
            Arg::new("progress")
                .long("progress")
                .global(true)
                .action(clap::ArgAction::SetTrue)
                .help(
                    "Print 'progress <stage> <0.000-1.000>' lines to stderr (at most 10 a second)",
                ),
        )
        .arg(
            Arg::new("threads")
                .long("threads")
                .global(true)
                .value_parser(clap::value_parser!(usize))
                .help("Worker threads (default: all cores but one)"),
        )
        .subcommand(
            Command::new("inspect")
                .about("Report mesh diagnostics")
                .arg(input_arg())
                .arg(units_arg())
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
                    .value_parser(["damaged", "scene"])
                    .help(
                        "damaged: UV sphere with holes, islands and unwelded corners; \
                         scene: the two-object demo (a damaged scan and a plate with a \
                         hole, a crack and a tangled edge, each at its own transform), 3MF only",
                    ),
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
    cmd = cmd.subcommand(
        Command::new("bench")
            .about("Measure the engine on generated meshes")
            .subcommand_required(true)
            .subcommand(
                Command::new("memory")
                    .about(
                        "Heap kept by a mesh and its cached topology (the working set), \
                         and the topology build's peak, counted by the allocator",
                    )
                    .arg(
                        Arg::new("triangles")
                            .long("triangles")
                            .default_value("5000000")
                            .value_parser(clap::value_parser!(u64).range(100..=200_000_000)),
                    )
                    .arg(
                        Arg::new("seed")
                            .long("seed")
                            .default_value("11")
                            .value_parser(clap::value_parser!(u64)),
                    )
                    .arg(
                        Arg::new("json")
                            .long("json")
                            .action(clap::ArgAction::SetTrue)
                            .help("Print the measurements as JSON"),
                    ),
            )
            .subcommand(
                Command::new("orbit")
                    .about(
                        "Frame times of a full 360° orbit rendered offscreen with the desktop's \
                         render path, each frame waited for on the GPU, tagged with the device",
                    )
                    .arg(
                        Arg::new("input")
                            .value_parser(clap::value_parser!(PathBuf))
                            .help("Mesh to orbit (default: the damaged fixture, generated in memory)"),
                    )
                    .arg(
                        Arg::new("triangles")
                            .long("triangles")
                            .default_value("1000000")
                            .value_parser(clap::value_parser!(u64).range(100..=200_000_000))
                            .help("Requested size of the generated fixture (with an input: the size it was generated at, for the report)"),
                    )
                    .arg(
                        Arg::new("seed")
                            .long("seed")
                            .default_value("11")
                            .value_parser(clap::value_parser!(u64)),
                    )
                    .arg(
                        Arg::new("frames")
                            .long("frames")
                            .default_value("600")
                            .value_parser(clap::value_parser!(u32).range(1..=100_000))
                            .help("Frames in the orbit (CPU and GPU percentiles cover the last 1000)"),
                    )
                    .arg(
                        Arg::new("size")
                            .long("size")
                            .default_value("1920x1080")
                            .help("Render size, WIDTHxHEIGHT pixels"),
                    )
                    .arg(
                        Arg::new("skip-if-short-of-memory")
                            .long("skip-if-short-of-memory")
                            .action(clap::ArgAction::SetTrue)
                            .help(
                                "Report the run as skipped, with the reason, instead of running \
                                 when the estimated memory need exceeds the memory available",
                            ),
                    )
                    .arg(
                        Arg::new("only-check-memory")
                            .long("only-check-memory")
                            .action(clap::ArgAction::SetTrue)
                            .help(
                                "Only say whether the run would be skipped for memory (as \
                                 --skip-if-short-of-memory decides), without running it",
                            ),
                    )
                    .arg(
                        Arg::new("json")
                            .long("json")
                            .action(clap::ArgAction::SetTrue)
                            .help("Print the measurements as JSON"),
                    ),
            ),
    );
    cmd = cmd
        .subcommand(
            Command::new("run")
                .about("Run a sequence of operations, optionally saving a command log")
                .arg(input_arg())
                .arg(units_arg())
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
                .arg(select_arg(true))
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
                .arg(units_arg())
                .arg(output_arg())
                .arg(select_arg(false)),
        );
    }
    cmd
}

/// Import every object of the file; commands act on the first one.
fn open(
    path: &Path,
    units: Option<mf_io::threemf::Unit>,
    metrics: &mut Metrics,
    ctx: &RunContext,
) -> Result<(Session, ObjectId), Failure> {
    let unitless = !matches!(Format::from_path(path), Some(Format::ThreeMf) | None);
    if units.is_some() && !unitless {
        return Err(Failure::Usage(format!(
            "--units is for STL and OBJ files; {} says its own unit",
            path.display()
        )));
    }
    let mut model = metrics.record("import", || mf_io::read_model_with(path, ctx))?;
    match units {
        Some(u) => model.unit = u,
        None if unitless => eprintln!(
            "warning: {} has no units; read as millimetres (use --units mm|cm|m|in to say otherwise)",
            path.display()
        ),
        None => {}
    }
    let mut session = Session::new();
    let (ids, warnings) = session.import_model(model);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let first = *ids
        .first()
        .ok_or_else(|| Failure::Runtime(format!("{} contains no mesh objects", path.display())))?;
    if ids.len() > 1 {
        eprintln!(
            "note: {} has {} objects; commands act on the first ('{}'), 3MF output keeps them all",
            path.display(),
            ids.len(),
            session
                .document()
                .object(first)
                .map_or("", |o| o.name.as_str())
        );
    }
    Ok((session, first))
}

/// Write the result: every object (with transforms and units) for 3MF,
/// otherwise the target object's mesh.
fn export(
    session: &Session,
    id: ObjectId,
    output: &Path,
    format: Format,
    metrics: &mut Metrics,
    ctx: &RunContext,
) -> Result<(), Failure> {
    let doc = session.document();
    let object = doc
        .object(id)
        .ok_or_else(|| Failure::Runtime("only meshes can be exported".into()))?;
    let mesh = object
        .mesh()
        .ok_or_else(|| Failure::Runtime("only meshes can be exported".into()))?;
    metrics.structures(mesh);
    if format == Format::ThreeMf {
        let model = threemf_model(doc)?;
        metrics.record("export", || write_threemf(&model, output, ctx))?;
    } else {
        // MF-ISSUE-035: STL/OBJ write a single mesh with no transform of
        // its own, so apply the object's here (mirrors
        // mf-desktop's App::export_to) and say so, like the desktop's
        // status note.
        let transformed = mf_io::transform_mesh(mesh, &object.transform);
        metrics.record("export", || {
            mf_io::write_path_with(&transformed, output, format, doc.units(), ctx)
        })?;
        eprintln!(
            "exported with the object's position applied: {}",
            output.display()
        );
    }
    Ok(())
}

/// Every object of `doc` as a 3MF model: object-space geometry, each with
/// its own transform, in the document's unit.
fn threemf_model(doc: &mf_command::Document) -> Result<mf_io::threemf::Model, Failure> {
    let objects = doc
        .objects()
        .iter()
        .map(|o| {
            let mesh = o
                .mesh()
                .ok_or_else(|| Failure::Runtime(format!("object '{}' is not a mesh", o.name)))?;
            Ok(mf_io::threemf::ModelObject {
                name: o.name.clone(),
                mesh: mesh.clone(),
                transform: o.transform,
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    Ok(mf_io::threemf::Model {
        unit: doc.units(),
        objects,
    })
}

/// Write (and verify) a 3MF model, printing its warnings.
fn write_threemf(
    model: &mf_io::threemf::Model,
    output: &Path,
    ctx: &RunContext,
) -> Result<(), Failure> {
    let warnings = mf_io::write_model_path_with(model, output, ctx)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn inspect(m: &ArgMatches, metrics: &mut Metrics, ctx: &RunContext) -> Result<(), Failure> {
    let path = m
        .get_one::<PathBuf>("input")
        .ok_or_else(|| Failure::Usage("missing input".into()))?;
    let (mut session, id) = open(path, units_of(m), metrics, ctx)?;
    if let Some(mesh) = session.document().object(id).and_then(|o| o.mesh()) {
        let mesh = mesh.clone();
        metrics.record("topology", || mesh.topology_with(ctx))?;
        metrics.structures(&mesh);
    }
    let inv = Invocation {
        selection: None,
        op_id: "mesh.inspect".into(),
        target: id,
        raw_params: Default::default(),
    };
    let CommitOutcome::Report(report) = execute(&mut session, inv, ctx)? else {
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

fn render(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
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
    ctx.checkpoint()?;
    image
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|e| Failure::Runtime(e.to_string()))?;
    Ok(())
}

fn gen_fixture(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
    let output = m
        .get_one::<PathBuf>("output")
        .ok_or_else(|| Failure::Usage("missing output".into()))?;
    let format = Format::from_path(output).ok_or_else(|| {
        Failure::Usage(format!("unsupported output format: {}", output.display()))
    })?;
    if m.get_one::<String>("kind").map(String::as_str) == Some("scene") {
        if format != Format::ThreeMf {
            return Err(Failure::Usage(
                "the scene has several objects: write it as .3mf".into(),
            ));
        }
        let model = mf_io::threemf::Model {
            unit: mf_io::threemf::Unit::Millimeter,
            objects: mf_fixtures::demo_scene()
                .into_iter()
                .map(|(name, mesh, transform)| mf_io::threemf::ModelObject {
                    name,
                    mesh,
                    transform,
                })
                .collect(),
        };
        let warnings = mf_io::write_model_path_with(&model, output, ctx)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        eprintln!(
            "wrote {} objects to {}",
            model.objects.len(),
            output.display()
        );
        return Ok(());
    }
    let triangles = *m.get_one::<u64>("triangles").unwrap_or(&1_000_000) as usize;
    let seed = *m.get_one::<u64>("seed").unwrap_or(&1);
    let mesh =
        mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(triangles, seed));
    ctx.checkpoint()?;
    mf_io::write_path_with(&mesh, output, format, mf_io::threemf::Unit::Millimeter, ctx)?;
    eprintln!(
        "wrote {} triangles to {}",
        mesh.triangle_count(),
        output.display()
    );
    Ok(())
}

/// `bench memory`: what a mesh and its cached topology keep on the heap
/// (MF-GEO-002, gated at 400 MB in CI for 5M requested triangles, which
/// the damaged-sphere fixture turns into ≈4.8M) and how far the
/// topology build peaks above the mesh (MF-ISSUE-019). Bytes are counted by
/// the allocator, not estimated. The mesh is the damaged sphere fixture,
/// generated in memory.
fn bench_memory(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
    let requested = *m.get_one::<u64>("triangles").unwrap_or(&5_000_000) as usize;
    let seed = *m.get_one::<u64>("seed").unwrap_or(&11);
    if !mf_metrics::is_installed() {
        return Err(Failure::Runtime(
            "the counting allocator isn't installed: nothing to measure with".into(),
        ));
    }
    let (mesh, mesh_phase) = measure("mesh", || {
        mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(requested, seed))
    });
    ctx.checkpoint()?;
    let (topology, topology_phase) = measure("topology", || mesh.topology_with(ctx));
    let topology = topology?;
    let working_set = mesh_phase.retained_bytes + topology_phase.retained_bytes;
    let json = serde_json::json!({
        "requested_triangles": requested,
        "triangles": mesh.triangle_count(),
        "vertices": mesh.vertex_count(),
        "mesh_bytes": mesh_phase.retained_bytes,
        "topology_bytes": topology_phase.retained_bytes,
        "working_set_bytes": working_set,
        "topology_peak_bytes": topology_phase.peak_bytes,
        "topology_s": topology_phase.wall_s,
        "peak_heap": mf_metrics::peak_bytes_since_start(),
        "structures": { "mesh": mesh.heap_bytes(), "topology": topology.heap_bytes() },
        "allocator_installed": mf_metrics::is_installed(),
    });
    if m.get_flag("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&json).map_err(|e| Failure::Runtime(e.to_string()))?
        );
    } else {
        let mb = |b: u64| format!("{:.1} MB", b as f64 / (1u64 << 20) as f64);
        println!(
            "{} triangles, {} vertices",
            mesh.triangle_count(),
            mesh.vertex_count()
        );
        println!("{:>24}: {}", "mesh", mb(mesh_phase.retained_bytes));
        println!(
            "{:>24}: {}",
            "cached topology",
            mb(topology_phase.retained_bytes)
        );
        println!("{:>24}: {}", "working set", mb(working_set));
        println!(
            "{:>24}: {} above the mesh ({:.2} s)",
            "topology build peak",
            mb(topology_phase.peak_bytes),
            topology_phase.wall_s
        );
    }
    Ok(())
}

/// `--size WIDTHxHEIGHT`.
fn parse_size(s: &str) -> Result<(u32, u32), Failure> {
    let usage = || Failure::Usage(format!("--size '{s}' must be WIDTHxHEIGHT, e.g. 1920x1080"));
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(usage)?;
    let w: u32 = w.trim().parse().map_err(|_| usage())?;
    let h: u32 = h.trim().parse().map_err(|_| usage())?;
    if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
        return Err(usage());
    }
    Ok((w, h))
}

/// What `bench orbit` holds per requested triangle, at its peak: the mesh
/// and its topology, the GPU mesh (72 bytes of surface vertices and a
/// face-state byte; unified memory counts it in the footprint) and the
/// import's transient buffers. Calibrated on an M2 Max from the physical
/// footprint's lifetime peak (M4 Task 10 fix round 1, P2): 10M requested
/// (9,618,698 actual) from an STL peaked at 2,176 MB (≈218 bytes per
/// requested triangle above the ≈40 MB baseline; 5M: 1,225 MB). Rounded
/// up by ~10%. (The first estimate, 400, came from a 1M run, where the
/// baseline dominates.)
const ORBIT_BYTES_PER_TRIANGLE: u64 = 240;
/// Where [`ORBIT_BYTES_PER_TRIANGLE`] comes from (in the JSON).
const ORBIT_ESTIMATE_BASIS: &str = "M2 Max, 10M requested (9,618,698 actual) from an STL: \
     2,176 MB peak physical footprint (≈218 B per requested triangle above baseline), +10%";

/// One figure of the OS's process memory; `None` if the OS gave none (the
/// error itself is reported in `memory.rss_error`).
fn process_figure(
    m: &Result<mf_metrics::ProcessMemory, String>,
    figure: impl Fn(&mf_metrics::ProcessMemory) -> Option<u64>,
) -> Option<u64> {
    match m {
        Ok(m) => figure(m),
        Err(_) => None,
    }
}

fn memory_estimate_json() -> serde_json::Value {
    serde_json::json!({
        "bytes_per_requested_triangle": ORBIT_BYTES_PER_TRIANGLE,
        "basis": ORBIT_ESTIMATE_BASIS,
        "compared_with": "sysinfo available memory",
    })
}

/// The machine `bench orbit` ran on (MF-RENDER-002): CPU, RAM and OS from
/// `sysinfo`, the GPU from wgpu's adapter info. A figure the OS doesn't
/// give is `null`, never a guess.
fn device_json(adapter: &wgpu::AdapterInfo) -> serde_json::Value {
    use sysinfo::{CpuRefreshKind, System};
    let mut sys = System::new();
    sys.refresh_cpu_list(CpuRefreshKind::nothing());
    sys.refresh_memory();
    let model = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .filter(|b| !b.is_empty());
    let os = match (System::long_os_version(), System::kernel_version()) {
        (Some(os), Some(kernel)) => Some(format!("{os} (kernel {kernel})")),
        (os, kernel) => os.or(kernel),
    };
    serde_json::json!({
        "cpu": {
            "model": model,
            "physical_cores": System::physical_core_count(),
            "logical_cores": sys.cpus().len(),
        },
        "gpu": {
            "name": adapter.name,
            "backend": adapter.backend.to_string(),
            "device_type": format!("{:?}", adapter.device_type),
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "vendor_id": adapter.vendor,
            "device_id": adapter.device,
        },
        "memory_bytes": sys.total_memory(),
        "available_memory_bytes": sys.available_memory(),
        "swap_total_bytes": sys.total_swap(),
        "swap_used_bytes": sys.used_swap(),
        "os": os,
    })
}

/// `bench orbit` (MF-RENDER-002, MF-RT-005): a full 360° orbit of the
/// damaged fixture (or `input`) rendered offscreen at `--size` with the
/// desktop's render path, every frame waited for on the GPU
/// (`mf_render::bench`). Reports whole-frame p50/p95/p99 and the worst
/// frame, fps (1 / p50), CPU time and GPU timestamps (with their counts),
/// the GPU buffers and the process RSS, tagged with the device.
fn bench_orbit(m: &ArgMatches, metrics: &mut Metrics, ctx: &RunContext) -> Result<(), Failure> {
    let input = m.get_one::<PathBuf>("input");
    let triangles = *m.get_one::<u64>("triangles").unwrap_or(&1_000_000);
    let seed = *m.get_one::<u64>("seed").unwrap_or(&11);
    let frames = *m.get_one::<u32>("frames").unwrap_or(&600);
    let size = m
        .get_one::<String>("size")
        .map_or("1920x1080", String::as_str);
    let (width, height) = parse_size(size)?;
    // With an input, --triangles only labels it (what it was generated at).
    let requested = (input.is_none()
        || m.value_source("triangles") == Some(clap::parser::ValueSource::CommandLine))
    .then_some(triangles);
    let print = |json: &serde_json::Value| -> Result<(), Failure> {
        println!(
            "{}",
            serde_json::to_string_pretty(json).map_err(|e| Failure::Runtime(e.to_string()))?
        );
        Ok(())
    };

    let only_check = m.get_flag("only-check-memory");
    if only_check || m.get_flag("skip-if-short-of-memory") {
        let need = triangles * ORBIT_BYTES_PER_TRIANGLE;
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let available = sys.available_memory();
        let short = need > available;
        if short || only_check {
            let (info, _, _) =
                mf_render::bench::timing_device().map_err(|e| Failure::Runtime(e.to_string()))?;
            let gb = |b: u64| b as f64 / f64::from(1u32 << 30);
            let reason = format!(
                "needs ≈{:.1} GB ({ORBIT_BYTES_PER_TRIANGLE} bytes per requested triangle), \
                 {:.1} GB available",
                gb(need),
                gb(available)
            );
            if short {
                eprintln!("skipped: {reason}");
            }
            return print(&serde_json::json!({
                "skipped": short,
                "reason": reason,
                "requested_triangles": triangles,
                "memory_estimate": memory_estimate_json(),
                "frames": frames,
                "resolution": { "width": width, "height": height },
                "device": device_json(&info),
            }));
        }
    }

    let mesh = match input {
        Some(path) => {
            metrics
                .record("import", || mf_io::read_model_with(path, ctx))?
                .objects
                .into_iter()
                .next()
                .ok_or_else(|| {
                    Failure::Runtime(format!("{} contains no mesh objects", path.display()))
                })?
                .mesh
        }
        None => metrics.record("mesh", || {
            mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(
                triangles as usize,
                seed,
            ))
        }),
    };
    ctx.checkpoint()?;
    metrics.record("topology", || mesh.topology_with(ctx))?;
    metrics.structures(&mesh);
    let spec = mf_render::bench::OrbitSpec {
        frames,
        width,
        height,
    };
    let run = metrics
        .record("orbit", || mf_render::bench::orbit(&mesh, spec, ctx))
        .map_err(|e| match e {
            mf_render::bench::OrbitError::Cancelled => Failure::Cancelled,
            e => Failure::Runtime(e.to_string()),
        })?;

    let frame = mf_metrics::Percentiles::of(run.frames.clone())
        .ok_or_else(|| Failure::Runtime("no frames were rendered".into()))?;
    let summary = run.times.summary();
    let mut percentiles = serde_json::Map::new();
    percentiles.insert(
        "frame_ms".into(),
        serde_json::json!(mf_metrics::DurationStats::from(&frame)),
    );
    percentiles.insert(
        "fps".into(),
        serde_json::json!(mf_metrics::fps(frame.p50())),
    );
    percentiles.insert("cpu_ms".into(), serde_json::json!(summary.cpu));
    percentiles.insert("gpu_ms".into(), serde_json::json!(summary.gpu));
    metrics.gpu = Some(run.buffers);
    metrics.frames = Some(percentiles.clone());
    metrics.rss = Some(run.process.clone());

    let mut json = serde_json::json!({
        "requested_triangles": requested,
        "triangles": mesh.triangle_count(),
        "input": input.map(|p| p.display().to_string()),
        "frames": frames,
        "resolution": { "width": width, "height": height },
        "measures": mf_render::bench::MEASURES,
        "msaa_samples": mf_render::bench::SAMPLES,
        "overlay_layers": "none (the boundary outlines in the mesh's own buffers are drawn; \
                           selection, hover and defect overlay layers are not)",
        "fps_basis": "1 / p50 frame time (render only, see measures)",
        // A process-memory error is reported in memory.rss_error; these are
        // null then (and off macOS / Linux), never a guess.
        "peak_phys_footprint_bytes": process_figure(&run.process, |p| p.peak_phys_footprint_bytes),
        "page_ins": process_figure(&run.process, |p| p.page_ins),
        "memory_estimate": memory_estimate_json(),
        "device": device_json(&run.adapter),
        "gpu_timestamps": run.timestamps,
        "gpu_time": summary.gpu.describe(),
        "first_frame_ms": mf_metrics::duration_ms(run.first_frame),
        "upload_s": run.upload.as_secs_f64(),
        "gpu_buffer_bytes": run.buffers,
        "memory": metrics.memory(),
        "allocator_installed": mf_metrics::is_installed(),
    });
    if let Some(obj) = json.as_object_mut() {
        obj.extend(percentiles);
    }
    if m.get_flag("json") {
        return print(&json);
    }
    let ms = |p: &mf_metrics::Percentiles| {
        let d = mf_metrics::DurationStats::from(p);
        format!(
            "p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, worst {:.2} ms",
            d.p50_ms, d.p95_ms, d.p99_ms, d.max_ms
        )
    };
    println!(
        "{} triangles{}, {frames} frames at {width}x{height} on {} ({})",
        mesh.triangle_count(),
        requested.map_or(String::new(), |r| format!(" (requested {r})")),
        run.adapter.name,
        run.adapter.backend
    );
    println!("{:>12}: {}", "frame", ms(&frame));
    println!(
        "{:>12}: {:.1}",
        "fps (p50)",
        mf_metrics::fps(frame.p50()).unwrap_or(f64::INFINITY)
    );
    if let Some(cpu) = run.times.cpu() {
        println!("{:>12}: {}", "CPU", ms(&cpu));
    }
    println!("{:>12}: {}", "GPU", summary.gpu.describe());
    Ok(())
}

/// A `--select kind:ids` value: `loop:3,4`, `component:1`, `faces:10,11`.
type Select = (SelectionKind, Vec<u32>);

fn parse_select(s: &str) -> Result<Select, Failure> {
    let usage = || {
        Failure::Usage(format!(
            "--select '{s}' must be loop:<ids>, component:<ids> or faces:<ids> (ids comma-separated)"
        ))
    };
    let (kind, ids) = s.split_once(':').ok_or_else(usage)?;
    let kind = match kind {
        "loop" => SelectionKind::BoundaryLoop,
        "component" => SelectionKind::Component,
        "faces" => SelectionKind::Faces,
        _ => return Err(usage()),
    };
    let ids = ids
        .split(',')
        .map(|t| t.trim().parse::<u32>().map_err(|_| usage()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((kind, ids))
}

fn select_arg(repeatable: bool) -> Arg {
    let arg = Arg::new("select")
        .long("select")
        .help("Target part of the object: loop:<ids>, component:<ids> or faces:<ids> (ids from the imported mesh)");
    if repeatable {
        arg.action(clap::ArgAction::Append)
    } else {
        arg
    }
}

/// Bind a parsed `--select` to the session's current revision.
/// A `--select`'s IDs name elements of the imported mesh (`import`). Later
/// steps run on edited revisions, where IDs may have changed (loops are
/// renumbered when one is filled), so the selection is translated to the
/// current revision. Anything short of a full mapping is an error: running
/// on part of the selection, or on a guess, would edit the wrong place.
fn selection_for(
    session: &Session,
    object: ObjectId,
    import: RevisionId,
    select: &Option<Select>,
) -> Result<Option<Selection>, Failure> {
    let Some((kind, ids)) = select else {
        return Ok(None);
    };
    let sel = Selection {
        object,
        revision: import,
        kind: *kind,
        ids: ids.clone(),
    };
    if import == session.current() {
        return Ok(Some(sel));
    }
    match session.translate(&sel, session.current()) {
        Translation::Mapped(s) => Ok(Some(s)),
        other => Err(Failure::Runtime(format!(
            "--select {}: after the earlier steps, the selected elements can't be found unchanged ({other:?})",
            ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
        ))),
    }
}

/// One `--op` with the `--param`s and optional `--select` that follow it.
struct Step {
    op: String,
    raw: BTreeMap<String, String>,
    select: Option<Select>,
}

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
    let selects: Vec<(usize, String)> =
        match (m.indices_of("select"), m.get_many::<String>("select")) {
            (Some(i), Some(v)) => i.zip(v.cloned()).collect(),
            _ => Vec::new(),
        };
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
        let mine: Vec<&String> = selects
            .iter()
            .filter(|(si, _)| si > idx && *si < next)
            .map(|(_, v)| v)
            .collect();
        if mine.len() > 1 {
            return Err(Failure::Usage(format!(
                "--op {op} has more than one --select"
            )));
        }
        let select = mine.first().map(|v| parse_select(v)).transpose()?;
        seq.push(Step {
            op: op.clone(),
            raw,
            select,
        });
    }
    let first_op = ops[0].0;
    if let Some((_, p)) = params
        .iter()
        .chain(selects.iter())
        .find(|(pi, _)| *pi < first_op)
    {
        return Err(Failure::Usage(format!("'{p}' comes before any --op")));
    }
    Ok(seq)
}

fn run_sequence(m: &ArgMatches, metrics: &mut Metrics, ctx: &RunContext) -> Result<(), Failure> {
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
    for Step { op, raw, .. } in &seq {
        let spec = mf_spec::builtin::get(op)
            .ok_or_else(|| Failure::Usage(format!("unknown operation '{op}'")))?;
        spec.validate(raw)
            .map_err(|e| Failure::Usage(format!("{op}: {e}")))?;
    }
    let (mut session, id) = open(input, units_of(m), metrics, ctx)?;
    let import = session.current();
    for Step { op, raw, select } in seq {
        let selection = selection_for(&session, id, import, &select)?;
        let outcome = metrics.record("operation", || {
            execute(
                &mut session,
                Invocation {
                    selection,
                    op_id: op.clone(),
                    target: id,
                    raw_params: raw,
                },
                ctx,
            )
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
    export(&session, id, output, format, metrics, ctx)
}

fn replay_log(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
    let dir = m
        .get_one::<PathBuf>("log")
        .ok_or_else(|| Failure::Usage("missing log".into()))?;
    let report = mf_command::replay(dir).map_err(|e| Failure::Runtime(e.to_string()))?;
    for (id, verdict) in &report.results {
        println!("command {}: {verdict:?}", id.0);
    }
    // MF-ISSUE-046: byte-identical and within-tolerance results are counted
    // apart, so a cross-platform run shows which it got.
    let count =
        |f: fn(&mf_command::Verdict) -> bool| report.results.iter().filter(|(_, v)| f(v)).count();
    println!(
        "verdicts: {} exact, {} equivalent, {} not verified",
        count(|v| matches!(v, mf_command::Verdict::Exact)),
        count(|v| matches!(v, mf_command::Verdict::Equivalent { .. })),
        count(|v| !v.is_verified()),
    );
    if let Some(out) = m.get_one::<PathBuf>("output") {
        let format = Format::from_path(out).ok_or_else(|| {
            Failure::Usage(format!("unsupported output format: {}", out.display()))
        })?;
        let doc = report.session.document();
        let object = doc
            .objects()
            .first()
            .ok_or_else(|| Failure::Runtime("the log produced no mesh".into()))?;
        let mesh = object
            .mesh()
            .ok_or_else(|| Failure::Runtime("the log produced no mesh".into()))?;
        // MF-ISSUE-035: apply the object's transform before an STL/OBJ
        // write, as `export` does. A 3MF goes through `export`'s path too:
        // object-space geometry with each object's own transform, so the
        // transform is kept and applied once (by whatever reads it).
        if format == Format::ThreeMf {
            write_threemf(&threemf_model(doc)?, out, ctx)?;
        } else {
            let transformed = mf_io::transform_mesh(mesh, &object.transform);
            mf_io::write_path_with(&transformed, out, format, doc.units(), ctx)?;
            eprintln!(
                "exported with the object's position applied: {}",
                out.display()
            );
        }
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
    ctx: &RunContext,
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
    // Validate parameters and selection before touching the filesystem.
    let select = m
        .get_one::<String>("select")
        .map(|s| parse_select(s))
        .transpose()?;
    let raw_params = mf_spec::cli::raw_params(spec, m);
    spec.validate(&raw_params)
        .map_err(|e| Failure::Usage(e.to_string()))?;

    let (mut session, id) = open(input, units_of(m), metrics, ctx)?;
    let selection = selection_for(&session, id, session.current(), &select)?;
    let outcome = metrics.record("operation", || {
        execute(
            &mut session,
            Invocation {
                selection,
                op_id: spec.id.clone(),
                target: id,
                raw_params,
            },
            ctx,
        )
    })?;
    if let CommitOutcome::NewRevision { report } = outcome {
        eprintln!("{}: {report}", spec.id);
    }
    export(&session, id, output, format, metrics, ctx)
}

/// Progress lines are at most this often (10 a second).
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Prints `progress <stage> <fraction>` to stderr every [`PROGRESS_EVERY`]
/// while the stage or fraction changed, until told how the run ended; a
/// successful run ends with a line at 1.000. Never more than one line per
/// [`PROGRESS_EVERY`], the last one included.
fn progress_reporter(
    progress: mf_runtime::Progress,
) -> (mpsc::Sender<bool>, std::thread::JoinHandle<()>) {
    let (done_tx, done_rx) = mpsc::channel::<bool>();
    let thread = std::thread::Builder::new()
        .name("mf-progress".into())
        .spawn(move || {
            let mut printed: Option<(&'static str, String)> = None;
            let mut last_line: Option<Instant> = None;
            let mut print = |printed: &mut Option<(&'static str, String)>,
                             stage: &'static str,
                             fraction: String| {
                if let Some(t) = last_line {
                    std::thread::sleep(PROGRESS_EVERY.saturating_sub(t.elapsed()));
                }
                eprintln!("progress {stage} {fraction}");
                last_line = Some(Instant::now());
                *printed = Some((stage, fraction));
            };
            loop {
                let ended = match done_rx.recv_timeout(PROGRESS_EVERY) {
                    Ok(success) => Some(success),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    // The sender only goes away once main is done.
                    Err(mpsc::RecvTimeoutError::Disconnected) => Some(false),
                };
                let (stage, fraction) = progress.snapshot();
                match ended {
                    None => {
                        let fraction = format!("{fraction:.3}");
                        let changed = printed
                            .as_ref()
                            .is_none_or(|(s, f)| *s != stage || *f != fraction);
                        if !stage.is_empty() && changed {
                            print(&mut printed, stage, fraction);
                        }
                    }
                    Some(true) => {
                        let stage = if stage.is_empty() { "done" } else { stage };
                        let finished = printed
                            .as_ref()
                            .is_some_and(|(s, f)| *s == stage && f == "1.000");
                        if !finished {
                            print(&mut printed, stage, "1.000".to_owned());
                        }
                        return;
                    }
                    Some(false) => return,
                }
            }
        })
        .expect("failed to spawn the progress thread");
    (done_tx, thread)
}

fn main() -> ExitCode {
    let matches = cli().get_matches();
    let sub = matches.subcommand().map(|(_, m)| m);
    let requested = sub.and_then(|m| m.get_one::<usize>("threads").copied());
    let (pool, choice) = JobPool::new(requested);
    let mut metrics = Metrics {
        threads: Some((requested, choice)),
        ..Metrics::default()
    };
    let ctx = RunContext::none();
    let cancel = ctx.cancel.clone();
    if let Err(e) = ctrlc::set_handler(move || cancel.cancel()) {
        eprintln!("error: can't handle Ctrl-C: {e}");
        return ExitCode::from(1);
    }
    let reporter = sub
        .is_some_and(|m| m.get_flag("progress"))
        .then(|| progress_reporter(ctx.progress.clone()));
    let result = pool.install(|| match matches.subcommand() {
        Some(("inspect", m)) => inspect(m, &mut metrics, &ctx),
        Some(("render", m)) => render(m, &ctx),
        Some(("gen-fixture", m)) => gen_fixture(m, &ctx),
        Some(("bench", m)) => match m.subcommand() {
            Some(("memory", m)) => bench_memory(m, &ctx),
            Some(("orbit", m)) => bench_orbit(m, &mut metrics, &ctx),
            _ => Err(Failure::Usage(
                "bench needs a measurement: memory or orbit".into(),
            )),
        },
        Some(("run", m)) => run_sequence(m, &mut metrics, &ctx),
        Some(("replay", m)) => replay_log(m, &ctx),
        Some((name, m)) => match mf_spec::builtin::specs()
            .iter()
            .find(|s| mf_spec::cli::subcommand_name(s) == name)
        {
            Some(spec) => run_operation(spec, m, &mut metrics, &ctx),
            None => Err(Failure::Usage(format!("unknown command '{name}'"))),
        },
        None => Err(Failure::Usage("no command given".into())),
    });
    let result = result.and_then(|()| {
        let path = sub.and_then(|m| m.get_one::<PathBuf>("metrics").cloned());
        match path {
            Some(p) => metrics.write(&p),
            None => Ok(()),
        }
    });
    if let Some((done, thread)) = reporter {
        // allow-swallow: the reporter only stops listening once it has returned
        let _ = done.send(result.is_ok());
        if thread.join().is_err() {
            eprintln!("error: the progress thread panicked");
        }
    }
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
        Err(Failure::Cancelled) => {
            eprintln!("cancelled");
            ExitCode::from(130)
        }
    }
}
