//! `bench split`: time, CPU and memory of one `mesh.split_plane` on a
//! generated sphere or a real model (optionally subdivided to reach a
//! size), as the app runs it: the model's topology is computed first (the
//! app's analysis already has it when the Split tool opens), then the
//! split alone is measured.
//!
//! - **Time**: the split's wall time.
//! - **CPU**: user plus system CPU time over the split, so the average
//!   number of cores it kept busy; and the peak, the busiest 50 ms sample.
//! - **Memory**: the process's physical footprint (what Activity Monitor
//!   shows) before the split, and the highest sampled every 10 ms during
//!   it; the difference is what the split itself needed on top of the
//!   loaded model.
//!
//! The plane goes through the model's bounding-box centre, tilted
//! (`--normal`, default (0.3, 0.1, 1)), keeping both halves, capped. One
//! run per process: `scripts/bench-split.sh` runs the table, one case at a
//! time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::{Arg, ArgMatches, Command};
use mf_geometry::Mesh;
use mf_operations::{DocContext, ObjectChange, OperandInput, Target};
use mf_runtime::RunContext;

use crate::Failure;

pub fn command() -> Command {
    Command::new("split")
        .about(
            "Time, CPU and memory of one mesh.split_plane (keep both, capped) through the \
             model's centre, on a generated sphere or a model file",
        )
        .arg(
            Arg::new("input")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Model to split (default: a generated UV sphere of --triangles)"),
        )
        .arg(
            Arg::new("triangles")
                .long("triangles")
                .default_value("1000000")
                .value_parser(clap::value_parser!(u64).range(100..=200_000_000))
                .help("Size of the generated sphere (ignored with an input)"),
        )
        .arg(
            Arg::new("subdivide")
                .long("subdivide")
                .default_value("0")
                .value_parser(clap::value_parser!(u32).range(0..=6))
                .help("Split every triangle of the input in four this many times (same surface)"),
        )
        .arg(
            Arg::new("normal")
                .long("normal")
                .default_value("0.3,0.1,1")
                .help("The plane's normal, x,y,z"),
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

/// Each triangle into four by its edge midpoints, shared between the two
/// faces of each edge, so a closed mesh stays closed.
pub(crate) fn subdivide(mesh: &Mesh) -> Result<Mesh, Failure> {
    let (mut positions, triangles) = mesh.to_arrays();
    let mut mid: HashMap<(u32, u32), u32> = HashMap::with_capacity(triangles.len() * 3 / 2);
    let mut out = Vec::with_capacity(triangles.len() * 4);
    for t in &triangles {
        let mut m = [0u32; 3];
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            m[i] = *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let (p, q) = (positions[a as usize], positions[b as usize]);
                positions.push(std::array::from_fn(|k| (p[k] + q[k]) * 0.5));
                positions.len() as u32 - 1
            });
        }
        out.push([t[0], m[0], m[2]]);
        out.push([t[1], m[1], m[0]]);
        out.push([t[2], m[2], m[1]]);
        out.push(m);
    }
    Mesh::new(positions, out).map_err(|e| Failure::Runtime(format!("subdividing: {e}")))
}

fn parse_normal(s: &str) -> Result<[f64; 3], Failure> {
    let v: Vec<f64> = s
        .split(',')
        .map(|c| c.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|e| Failure::Usage(format!("--normal '{s}': {e}")))?;
    let [x, y, z] = v[..] else {
        return Err(Failure::Usage(format!("--normal '{s}' must be x,y,z")));
    };
    Ok([x, y, z])
}

/// What a sampler thread saw while the split ran.
pub(crate) struct Samples {
    pub(crate) peak_footprint: u64,
    pub(crate) peak_cores: f64,
}

pub(crate) fn footprint() -> Result<u64, Failure> {
    let m = mf_metrics::process_memory().map_err(Failure::Runtime)?;
    Ok(m.phys_footprint_bytes.unwrap_or(m.resident_bytes))
}

pub(crate) fn cpu() -> Result<f64, Failure> {
    mf_metrics::process_cpu_seconds().map_err(Failure::Runtime)
}

/// Samples the footprint every 10 ms and the CPU every 50 ms until
/// `stop`.
pub(crate) fn sampler(stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Result<Samples, Failure>> {
    std::thread::spawn(move || {
        let mut s = Samples {
            peak_footprint: footprint()?,
            peak_cores: 0.0,
        };
        let (mut t0, mut c0) = (Instant::now(), cpu()?);
        let mut tick = 0u32;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            s.peak_footprint = s.peak_footprint.max(footprint()?);
            tick += 1;
            if tick.is_multiple_of(5) {
                let (t1, c1) = (Instant::now(), cpu()?);
                let wall = (t1 - t0).as_secs_f64();
                if wall > 0.0 {
                    s.peak_cores = s.peak_cores.max((c1 - c0) / wall);
                }
                (t0, c0) = (t1, c1);
            }
        }
        s.peak_footprint = s.peak_footprint.max(footprint()?);
        Ok(s)
    })
}

pub fn run(m: &ArgMatches, ctx: &RunContext) -> Result<(), Failure> {
    let normal = parse_normal(m.get_one::<String>("normal").map_or("0.3,0.1,1", |s| s))?;
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
                    "{} has more than one object; bench split takes one",
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
            let rings = (requested / 2.0).sqrt().round().max(3.0) as u32;
            (mf_fixtures::uv_sphere(rings, rings), "uv sphere".into())
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
    let bb = mesh
        .bounding_box()
        .ok_or_else(|| Failure::Runtime("the model is empty".into()))?;
    let centre = bb.center();
    let spec = mf_spec::builtin::get("mesh.split_plane")
        .ok_or_else(|| Failure::Runtime("no mesh.split_plane spec".into()))?;
    let raw = [
        (
            "plane_point",
            format!("{},{},{}", centre[0], centre[1], centre[2]),
        ),
        (
            "plane_normal",
            format!("{},{},{}", normal[0], normal[1], normal[2]),
        ),
        ("keep", "both".into()),
        ("cap", "true".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let params = spec
        .validate(&raw)
        .map_err(|e| Failure::Usage(e.to_string()))?;
    let op = mf_operations::find("mesh.split_plane")
        .ok_or_else(|| Failure::Runtime("mesh.split_plane isn't registered".into()))?;

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
        &DocContext::default(),
        ctx,
    );
    let split_s = start.elapsed().as_secs_f64();
    let cpu_s = cpu()? - cpu0;
    stop.store(true, Ordering::Relaxed);
    let samples = samples
        .join()
        .map_err(|_| Failure::Runtime("the sampler thread panicked".into()))??;
    let (outcome, parts) = match &out {
        Ok(out) => (
            "ok".to_owned(),
            out.changes
                .iter()
                .filter_map(|c| match c {
                    ObjectChange::Geometry { mesh, .. } | ObjectChange::Create { mesh, .. } => {
                        Some(mesh.face_count())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        ),
        Err(e) => (format!("refused: {e}"), Vec::new()),
    };
    let mb = |b: u64| b as f64 / (1u64 << 20) as f64;
    let json = serde_json::json!({
        "model": label,
        "triangles": mesh.face_count(),
        "threads": rayon::current_num_threads(),
        "load_s": load_s,
        "split_s": split_s,
        "cpu_s": cpu_s,
        "avg_cores": if split_s > 0.0 { cpu_s / split_s } else { 0.0 },
        "peak_cores": samples.peak_cores,
        "footprint_before_mb": mb(before),
        "peak_footprint_mb": mb(samples.peak_footprint),
        "peak_above_loaded_mb": mb(samples.peak_footprint.saturating_sub(before)),
        "outcome": outcome,
        "parts_faces": parts,
    });
    if m.get_flag("json") {
        println!(
            "{}",
            serde_json::to_string(&json).map_err(|e| Failure::Runtime(e.to_string()))?
        );
    } else {
        println!(
            "{label}: {} triangles, split {:.2} s ({:.1} cores avg, {:.1} peak), footprint \
             {:.0} MB -> peak {:.0} MB (+{:.0} MB), {outcome}",
            mesh.face_count(),
            split_s,
            cpu_s / split_s.max(1e-9),
            samples.peak_cores,
            mb(before),
            mb(samples.peak_footprint),
            mb(samples.peak_footprint.saturating_sub(before)),
        );
    }
    if out.is_err() {
        return Err(Failure::Runtime(outcome));
    }
    Ok(())
}
