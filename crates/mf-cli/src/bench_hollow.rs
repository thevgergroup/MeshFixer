//! `bench hollow`: time, CPU and memory of one `mesh.hollow` on a
//! generated sphere or a real model (optionally subdivided to reach a
//! size), as the app runs it: the model's topology is computed first (the
//! app's analysis already has it when the Hollow tool opens), then the
//! hollow alone is measured. Same measures as `bench split`
//! ([`crate::bench_split`]): wall time, average and peak cores, and the
//! physical footprint before and at its peak.
//!
//! Defaults: a 2 mm wall, the automatic cell size, one auto drain hole.
//! One run per process: `scripts/bench-hollow.sh` runs the table.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use clap::{Arg, ArgMatches, Command};
use mf_operations::{ObjectChange, OperandInput, Target};
use mf_runtime::RunContext;

use crate::Failure;
use crate::bench_split::{cpu, footprint, sampler, subdivide};

pub fn command() -> Command {
    Command::new("hollow")
        .about(
            "Time, CPU and memory of one mesh.hollow (2 mm wall, automatic cell, one auto drain \
             hole) on a generated sphere or a model file",
        )
        .arg(
            Arg::new("input")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Model to hollow (default: a generated UV sphere of --triangles)"),
        )
        .arg(
            Arg::new("triangles")
                .long("triangles")
                .default_value("1000000")
                .value_parser(clap::value_parser!(u64).range(100..=200_000_000))
                .help("Size of the generated sphere (ignored with an input)"),
        )
        .arg(
            Arg::new("radius")
                .long("radius")
                .default_value("50")
                .value_parser(clap::value_parser!(f64))
                .help("Radius of the generated sphere, mm"),
        )
        .arg(
            Arg::new("subdivide")
                .long("subdivide")
                .default_value("0")
                .value_parser(clap::value_parser!(u32).range(0..=6))
                .help("Split every triangle of the input in four this many times (same surface)"),
        )
        .arg(
            Arg::new("wall")
                .long("wall")
                .default_value("2")
                .value_parser(clap::value_parser!(f64))
                .help("Wall thickness, mm"),
        )
        .arg(
            Arg::new("cell")
                .long("cell")
                .default_value("0")
                .value_parser(clap::value_parser!(f64))
                .help("Cell size, mm (0: automatic)"),
        )
        .arg(
            Arg::new("holes")
                .long("holes")
                .default_value("auto")
                .help("Drain holes: none or auto"),
        )
        .arg(
            Arg::new("label")
                .long("label")
                .help("A name for the model in the report"),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Print the measurements as JSON"),
        )
}

pub fn run(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
    let rounds = *m.get_one::<u32>("subdivide").unwrap_or(&0);
    let load = Instant::now();
    let (mut mesh, mut label) = match m.get_one::<PathBuf>("input") {
        Some(path) => {
            let model = mf_io::read_model_with(path, ctx)
                .map_err(|e| Failure::Runtime(format!("reading {}: {e}", path.display())))?;
            let mut objects = model.objects.into_iter();
            let first = objects
                .next()
                .ok_or_else(|| Failure::Runtime(format!("{} has no object", path.display())))?;
            if objects.next().is_some() {
                return Err(Failure::Usage(format!(
                    "{} has more than one object; bench hollow takes one",
                    path.display()
                )));
            }
            let name = path
                .file_stem()
                .map_or_else(|| "model".into(), |s| s.to_string_lossy().into_owned());
            (first.mesh, name)
        }
        None => {
            let requested = *m.get_one::<u64>("triangles").unwrap_or(&1_000_000) as f64;
            let radius = *m.get_one::<f64>("radius").unwrap_or(&50.0);
            let rings = (requested / 2.0).sqrt().round().max(3.0) as u32;
            (
                mf_fixtures::scaled(&mf_fixtures::uv_sphere(rings, rings), radius),
                format!("uv sphere r{radius}"),
            )
        }
    };
    for _ in 0..rounds {
        mesh = subdivide(&mesh)?;
    }
    if rounds > 0 {
        label = format!("{label} (subdivided x{})", 4u32.pow(rounds));
    }
    if let Some(l) = m.get_one::<String>("label") {
        label = l.clone();
    }
    mesh.topology_with(ctx)?;
    let load_s = load.elapsed().as_secs_f64();
    let wall = *m.get_one::<f64>("wall").unwrap_or(&2.0);
    let cell = *m.get_one::<f64>("cell").unwrap_or(&0.0);
    let holes = m.get_one::<String>("holes").map_or("auto", |s| s.as_str());
    let spec = mf_spec::builtin::get("mesh.hollow")
        .ok_or_else(|| Failure::Runtime("no mesh.hollow spec".into()))?;
    let raw = [
        ("wall_thickness", wall.to_string()),
        ("inner_detail", cell.to_string()),
        ("drain_holes", holes.to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let params = spec
        .validate(&raw)
        .map_err(|e| Failure::Usage(e.to_string()))?;
    let op = mf_operations::find("mesh.hollow")
        .ok_or_else(|| Failure::Runtime("mesh.hollow isn't registered".into()))?;

    let before = footprint()?;
    let stop = Arc::new(AtomicBool::new(false));
    let samples = sampler(Arc::clone(&stop));
    let cpu0 = cpu()?;
    let start = Instant::now();
    let out = op.execute_doc(
        &[OperandInput {
            slot: "target",
            object: mf_document::ObjectId(1),
            name: "model",
            mesh: &mesh,
            transform: mf_io::threemf::IDENTITY,
            target: Target::WholeObject,
        }],
        &params,
        ctx,
    );
    let hollow_s = start.elapsed().as_secs_f64();
    let cpu_s = cpu()? - cpu0;
    stop.store(true, Ordering::Relaxed);
    let samples = samples
        .join()
        .map_err(|_| Failure::Runtime("the sampler thread panicked".into()))??;
    let (outcome, faces, report) = match &out {
        Ok(out) => (
            "ok".to_owned(),
            out.changes
                .iter()
                .find_map(|c| match c {
                    ObjectChange::Geometry { mesh, .. } => Some(mesh.face_count()),
                    _ => None,
                })
                .unwrap_or(0),
            out.report.clone(),
        ),
        Err(e) => (format!("refused: {e}"), 0, serde_json::Value::Null),
    };
    let mb = |b: u64| b as f64 / (1u64 << 20) as f64;
    let json = serde_json::json!({
        "model": label,
        "triangles": mesh.face_count(),
        "threads": rayon::current_num_threads(),
        "load_s": load_s,
        "hollow_s": hollow_s,
        "cpu_s": cpu_s,
        "avg_cores": if hollow_s > 0.0 { cpu_s / hollow_s } else { 0.0 },
        "peak_cores": samples.peak_cores,
        "footprint_before_mb": mb(before),
        "peak_footprint_mb": mb(samples.peak_footprint),
        "peak_above_loaded_mb": mb(samples.peak_footprint.saturating_sub(before)),
        "outcome": outcome,
        "result_faces": faces,
        "cell_size": report["cell_size"],
        "band_points": report["band_points"],
        "inner_faces": report["inner_faces"],
        "holes": report["holes"].as_array().map_or(0, Vec::len),
    });
    if m.get_flag("json") {
        println!(
            "{}",
            serde_json::to_string(&json).map_err(|e| Failure::Runtime(e.to_string()))?
        );
    } else {
        println!(
            "{label}: {} triangles, hollow {:.2} s ({:.1} cores avg, {:.1} peak), footprint \
             {:.0} MB -> peak {:.0} MB (+{:.0} MB), cell {}, {} band points, {} inner faces, \
             {outcome}",
            mesh.face_count(),
            hollow_s,
            cpu_s / hollow_s.max(1e-9),
            samples.peak_cores,
            mb(before),
            mb(samples.peak_footprint),
            mb(samples.peak_footprint.saturating_sub(before)),
            report["cell_size"],
            report["band_points"],
            report["inner_faces"],
        );
    }
    if out.is_err() {
        return Err(Failure::Runtime(outcome));
    }
    Ok(())
}
