#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The messy-model collection (M4 Task 11a): real, freely licensed files
//! with the damage people actually bring — open sculpts full of holes,
//! non-manifold ("tangled") edges, flipped faces, hundreds of loose pieces,
//! duplicate and degenerate faces, self-intersecting exports, unwelded
//! triangle soup and a 5.7M-triangle scan. Sources, licences and why each
//! was chosen: `fixtures/SOURCES.md`; fetched by SHA-256 with
//! `scripts/fetch-fixtures.sh`. Skips (per policy) when not fetched.
//!
//! Every model is imported, analysed, filled whole-object (once per fill
//! style the `mesh.fill_holes` spec offers), cleaned of small components,
//! and filled at its largest fillable hole alone. The gate: no panic, no
//! hang and no silent fallback — every step is a success or a named error.
//! Times and peak heap are recorded against a budget, not gated (M4);
//! damage MeshFixer can't repair yet is listed as unsupported with the
//! M5 issue that covers it. The whole collection runs once per process
//! (all four tests share it) and its results go to
//! `artifacts/m4/messy/results.json` (or `$MF_ARTIFACTS/messy/`).
//! Its own binary: the counting allocator is process-wide.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use common::{edge_faces, untouched_outside};
use mf_geometry::{Diagnostics, Mesh, inspect, inspect_with};
use mf_metrics::{CountingAlloc, Phase, measure};
use mf_operations::{OpError, OpOutput, RunContext, Target, find};
use mf_spec::{Params, builtin};
use serde::Serialize;
use serde_json::{Value, json};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// The damage a model was chosen for (the brief's coverage list, plus
/// holes too large for the current fill).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Damage {
    ManyHoles,
    LargeHoles,
    NonManifold,
    FlippedOrientation,
    LoosePieces,
    DuplicateAndDegenerate,
    SelfIntersecting,
    UnweldedSoup,
    Large,
}

/// The brief's coverage list: the collection must show every one.
const REQUIRED: [Damage; 8] = [
    Damage::ManyHoles,
    Damage::NonManifold,
    Damage::FlippedOrientation,
    Damage::LoosePieces,
    Damage::DuplicateAndDegenerate,
    Damage::SelfIntersecting,
    Damage::UnweldedSoup,
    Damage::Large,
];

struct Model {
    /// File name in `fixtures/real` (see `fixtures/manifest.txt`).
    file: &'static str,
    /// Thingi10K file ID.
    thingi10k: u32,
    licence: &'static str,
    damage: &'static [Damage],
    /// Thingi10K's `num_self_intersections` for this file: MeshFixer
    /// doesn't detect self-intersections yet (MF-ISSUE-049), so the
    /// dataset's count is the evidence for that damage.
    dataset_self_intersections: u64,
}

use Damage::*;

const MODELS: &[Model] = &[
    Model {
        file: "messy-inopos-131487.stl",
        thingi10k: 131487,
        licence: "CC BY 3.0",
        damage: &[LargeHoles],
        dataset_self_intersections: 31,
    },
    Model {
        file: "messy-inopos-131443.obj",
        thingi10k: 131443,
        licence: "CC BY 3.0",
        damage: &[NonManifold, LargeHoles],
        dataset_self_intersections: 67,
    },
    Model {
        file: "messy-verlan-dress-285440.obj",
        thingi10k: 285440,
        licence: "CC BY 3.0",
        damage: &[ManyHoles, LoosePieces, SelfIntersecting],
        dataset_self_intersections: 175_314,
    },
    Model {
        file: "messy-aztec-chief-384098.stl",
        thingi10k: 384098,
        licence: "CC BY 3.0",
        damage: &[NonManifold],
        dataset_self_intersections: 6,
    },
    Model {
        file: "messy-moon-shot-45809.stl",
        thingi10k: 45809,
        licence: "CC BY 3.0 (dataset: public domain)",
        damage: &[ManyHoles, NonManifold],
        dataset_self_intersections: 1_651,
    },
    Model {
        file: "messy-walking-man-75275.stl",
        thingi10k: 75275,
        licence: "CC BY 3.0",
        damage: &[FlippedOrientation],
        dataset_self_intersections: 28,
    },
    Model {
        file: "messy-cube-achim-252785.stl",
        thingi10k: 252785,
        licence: "CC BY 3.0",
        damage: &[FlippedOrientation, SelfIntersecting],
        dataset_self_intersections: 322_904,
    },
    Model {
        file: "messy-site3-scan-85538.stl",
        thingi10k: 85538,
        licence: "CC BY 3.0",
        damage: &[ManyHoles, LoosePieces, NonManifold],
        dataset_self_intersections: 13,
    },
    Model {
        file: "messy-duplo-crossing-111959.stl",
        thingi10k: 111959,
        licence: "CC BY 3.0",
        damage: &[ManyHoles, LoosePieces, NonManifold, DuplicateAndDegenerate],
        dataset_self_intersections: 10_253,
    },
    Model {
        file: "messy-bunny-peel-298323.obj",
        thingi10k: 298323,
        licence: "CC BY 3.0",
        damage: &[LoosePieces, SelfIntersecting],
        dataset_self_intersections: 209_830,
    },
    Model {
        file: "messy-cleopatras-needle-63942.stl",
        thingi10k: 63942,
        licence: "CC BY 3.0",
        damage: &[DuplicateAndDegenerate, NonManifold, SelfIntersecting],
        dataset_self_intersections: 225_219,
    },
    Model {
        file: "messy-puritan-91945.stl",
        thingi10k: 91945,
        licence: "CC BY 3.0",
        damage: &[DuplicateAndDegenerate, NonManifold],
        dataset_self_intersections: 28_872,
    },
    Model {
        file: "messy-sullivan-1716279.stl",
        thingi10k: 1716279,
        licence: "CC0 1.0",
        damage: &[SelfIntersecting],
        dataset_self_intersections: 128_435,
    },
    Model {
        file: "messy-openrc-differential-100727.stl",
        thingi10k: 100727,
        licence: "CC BY 3.0",
        damage: &[UnweldedSoup, LoosePieces, ManyHoles],
        dataset_self_intersections: 1_991,
    },
    Model {
        file: "messy-winged-victory-376253.obj",
        thingi10k: 376253,
        licence: "CC BY 3.0",
        damage: &[Large, SelfIntersecting],
        dataset_self_intersections: 707,
    },
];

/// No single step may take longer than this: past it the step is
/// cancelled and recorded as a hang (a failure), not waited on forever.
const HANG: Duration = Duration::from_secs(300);

/// Recorded, not gated, in M4 (gates in M5): one second plus two per
/// million triangles for each step.
fn budget_s(triangles: usize) -> f64 {
    1.0 + 2.0 * triangles as f64 / 1e6
}

/// How a step ended. Anything other than `Ok` or `Refused` fails the gate.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Outcome {
    Ok,
    /// A named error: the operation (or reader) declined, saying why.
    Refused {
        code: String,
        message: String,
    },
    Panicked {
        message: String,
    },
    TimedOut {
        after_s: f64,
    },
}

impl Outcome {
    fn is_named(&self) -> bool {
        matches!(self, Outcome::Ok | Outcome::Refused { .. })
    }
}

#[derive(Debug, Clone, Serialize)]
struct Timing {
    wall_s: f64,
    peak_heap_bytes: u64,
    budget_s: f64,
    within_budget: bool,
}

fn timing(phase: &Phase, triangles: usize) -> Timing {
    let budget = budget_s(triangles);
    Timing {
        wall_s: phase.wall_s,
        peak_heap_bytes: phase.peak_bytes,
        budget_s: budget,
        within_budget: phase.wall_s <= budget,
    }
}

/// Runs `f` with a context a watchdog cancels after [`HANG`], catching
/// panics, measuring wall time and peak heap.
fn guarded<T>(name: &str, f: impl FnOnce(&RunContext) -> T) -> (Result<T, Outcome>, Phase, bool) {
    let ctx = RunContext::none();
    let token = ctx.cancel.clone();
    let (done, wait) = channel::<()>();
    let watchdog = std::thread::spawn(move || match wait.recv_timeout(HANG) {
        Err(RecvTimeoutError::Timeout) => {
            token.cancel();
            true
        }
        _ => false,
    });
    let (result, phase) = measure(name, || catch_unwind(AssertUnwindSafe(|| f(&ctx))));
    done.send(()).unwrap();
    let timed_out = watchdog.join().unwrap();
    let result = result.map_err(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".into());
        Outcome::Panicked { message }
    });
    (result, phase, timed_out)
}

fn io_outcome(e: &mf_io::IoError, timed_out: bool, wall_s: f64) -> Outcome {
    use mf_io::IoError::*;
    if timed_out {
        return Outcome::TimedOut { after_s: wall_s };
    }
    let code = match e {
        Io(_) => "io",
        Parse { .. } => "parse",
        Truncated { .. } => "truncated",
        Geometry(_) => "geometry",
        Export { .. } => "export",
        UnsupportedFormat(_) => "unsupported_format",
        Cancelled => "cancelled",
        LimitExceeded { .. } => "limit_exceeded",
    };
    Outcome::Refused {
        code: code.into(),
        message: e.to_string(),
    }
}

fn op_outcome(e: &OpError, timed_out: bool, wall_s: f64) -> Outcome {
    if timed_out {
        return Outcome::TimedOut { after_s: wall_s };
    }
    let code = match e {
        OpError::Spec(_) => "invalid_parameters".to_string(),
        OpError::Failed { code, .. } => code.to_string(),
        OpError::Cancelled => "cancelled".to_string(),
    };
    Outcome::Refused {
        code,
        message: e.to_string(),
    }
}

/// Diagnostics without the per-loop list (thousands of entries on some
/// models), plus the loop count and the largest loop.
fn summary(d: &Diagnostics) -> Value {
    let mut v = serde_json::to_value(d).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("boundary_loops");
    obj.insert("boundary_loop_count".into(), json!(d.boundary_loops.len()));
    obj.insert(
        "largest_loop_edges".into(),
        json!(d.boundary_loops.iter().map(|l| l.edge_count).max()),
    );
    v
}

/// Whether `d` shows `damage` (self-intersections are taken from the
/// dataset: MeshFixer doesn't detect them yet, MF-ISSUE-049).
fn shows(damage: Damage, d: &Diagnostics, m: &Model, max_loop_edges: usize) -> bool {
    match damage {
        ManyHoles => d.boundary_loops.len() >= 50,
        LargeHoles => d
            .boundary_loops
            .iter()
            .any(|l| l.edge_count > max_loop_edges),
        NonManifold => d.non_manifold_edges > 0 && d.non_manifold_vertices > 0,
        FlippedOrientation => d.inconsistent_orientation_edges > 0,
        LoosePieces => d.components >= 100,
        DuplicateAndDegenerate => d.duplicate_faces > 0 && d.degenerate_triangles > 0,
        SelfIntersecting => m.dataset_self_intersections > 0,
        // Nearly every triangle stands alone: more open edges than
        // triangles, and vertices a hair apart that an exact weld missed.
        UnweldedSoup => {
            m.file.ends_with(".stl")
                && d.boundary_edges > d.triangle_count
                && d.duplicate_vertices_tolerance > 0
        }
        Large => d.triangle_count >= 5_000_000,
    }
}

/// Damage present that MeshFixer has no repair (or detection) for yet,
/// each with the M5 issue that covers it.
fn unsupported(d: &Diagnostics, m: &Model, skipped: &BTreeMap<String, usize>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut add = |what: &str, count: u64, issue: &str| {
        if count > 0 {
            out.push(json!({"damage": what, "count": count, "issue": issue}));
        }
    };
    add(
        "non-manifold (tangled) edges: no repair",
        d.non_manifold_edges as u64,
        "MF-ISSUE-047",
    );
    add(
        "inconsistently oriented (flipped) faces: no reorient operation",
        d.inconsistent_orientation_edges as u64,
        "MF-ISSUE-018",
    );
    add(
        "degenerate faces: no cleanup",
        d.degenerate_triangles as u64,
        "MF-ISSUE-048",
    );
    add(
        "duplicate faces: no cleanup",
        d.duplicate_faces as u64,
        "MF-ISSUE-048",
    );
    add(
        "self-intersections (dataset count): not detected",
        m.dataset_self_intersections,
        "MF-ISSUE-049",
    );
    if m.damage.contains(&UnweldedSoup) {
        add(
            "unwelded near-coincident vertices: no weld/stitch",
            d.duplicate_vertices_tolerance as u64,
            "MF-ISSUE-032",
        );
    }
    let n = |reason: &str| skipped.get(reason).copied().unwrap_or(0) as u64;
    add(
        "holes over max_loop_edges: not filled",
        n("too_large"),
        "MF-ISSUE-043",
    );
    add(
        "slit holes (collinear cracks): not filled",
        n("untriangulable"),
        "MF-ISSUE-032",
    );
    add(
        "holes through a pinched vertex: not filled",
        n("self_touching"),
        "MF-ISSUE-051",
    );
    add(
        "boundary edges at non-manifold vertices: not traced as holes",
        d.untraced_boundary_edges as u64,
        "MF-ISSUE-051",
    );
    out
}

/// The fill styles the `mesh.fill_holes` spec offers: each value of its
/// `fill_style` parameter, read from the spec (Task 12a), or just the
/// defaults while there is no such parameter.
fn fill_styles() -> Vec<(String, Params)> {
    let spec = builtin::get("mesh.fill_holes").unwrap();
    let Some(param) = spec.parameter("fill_style") else {
        return vec![("default".into(), spec.validate(&BTreeMap::new()).unwrap())];
    };
    // Whatever field the spec lists the choices in: every string array.
    let raw = serde_json::to_value(param).unwrap();
    let values: Vec<String> = raw
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(
        !values.is_empty(),
        "fill_holes has a fill_style parameter but its values can't be read from the spec: {raw}"
    );
    values
        .into_iter()
        .map(|style| {
            let params = spec
                .validate(&BTreeMap::from([("fill_style".into(), style.clone())]))
                .unwrap();
            (style, params)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
struct FillResult {
    style: String,
    #[serde(flatten)]
    outcome: Outcome,
    timing: Option<Timing>,
    loops_before: usize,
    filled: usize,
    skipped: BTreeMap<String, usize>,
    created_faces: usize,
    /// Every loop is either filled or skipped with a reason.
    accounted: bool,
    /// `None` when nothing changed; `Ok` counts patch faces laid along an
    /// already non-manifold rim edge (MF-ISSUE-047).
    untouched_outside: Option<Result<usize, String>>,
    after: Option<Value>,
    /// ID and size of the largest loop filled (for the targeted fill).
    #[serde(skip)]
    largest_filled: Option<(u32, usize)>,
}

#[derive(Debug, Clone, Serialize)]
struct RemoveSmallResult {
    #[serde(flatten)]
    outcome: Outcome,
    timing: Option<Timing>,
    removed_components: Option<Value>,
    /// Share of the model's triangles the defaults removed; over 10% is
    /// flagged as MF-ISSUE-052 in `unsupported`.
    removed_triangle_share: Option<f64>,
    /// Every remaining component has at least `max_triangles` triangles or
    /// is the largest.
    postcondition: Option<bool>,
    after: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct TargetedResult {
    /// `None`: no hole was filled whole-object, so none was targeted.
    loop_id: Option<u32>,
    loop_edges: Option<usize>,
    #[serde(flatten)]
    outcome: Outcome,
    timing: Option<Timing>,
    untouched_outside: Option<Result<usize, String>>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelResult {
    file: &'static str,
    thingi10k_file_id: u32,
    licence: &'static str,
    chosen_for: &'static [Damage],
    /// Damage the analysis did not confirm (must be empty).
    damage_not_shown: Vec<Damage>,
    import: Outcome,
    import_timing: Option<Timing>,
    import_warnings: Vec<String>,
    analyse: Option<Outcome>,
    analyse_timing: Option<Timing>,
    before: Option<Value>,
    fills: Vec<FillResult>,
    remove_small_components: Option<RemoveSmallResult>,
    targeted_fill: Option<TargetedResult>,
    unsupported: Vec<Value>,
}

fn fill_whole(
    mesh: &Mesh,
    before_edges: &mut Option<HashMap<(u32, u32), u32>>,
    style: &str,
    params: &Params,
) -> FillResult {
    let n = mesh.face_count();
    let loops_before = mesh.topology().boundary_loops.len();
    let op = find("mesh.fill_holes").unwrap();
    let (r, phase, timed_out) = guarded("fill_holes", |ctx| {
        op.execute(mesh, &Target::WholeObject, params, ctx)
    });
    let mut res = FillResult {
        style: style.into(),
        outcome: Outcome::Ok,
        timing: Some(timing(&phase, n)),
        loops_before,
        filled: 0,
        skipped: BTreeMap::new(),
        created_faces: 0,
        accounted: false,
        untouched_outside: None,
        after: None,
        largest_filled: None,
    };
    let out: OpOutput = match r {
        Err(o) => {
            res.outcome = o;
            return res;
        }
        Ok(Err(e)) => {
            res.outcome = op_outcome(&e, timed_out, phase.wall_s);
            return res;
        }
        Ok(Ok(out)) => out,
    };
    let filled = out.report["filled_loops"].as_array().unwrap();
    let skipped = out.report["skipped_loops"].as_array().unwrap();
    res.filled = filled.len();
    for s in skipped {
        *res.skipped
            .entry(s["reason"].as_str().unwrap_or("<no reason>").to_owned())
            .or_insert(0) += 1;
    }
    res.accounted = filled.len() + skipped.len() == loops_before;
    res.largest_filled = filled
        .iter()
        .map(|f| {
            (
                f["id"].as_u64().unwrap() as u32,
                f["edges"].as_u64().unwrap() as usize,
            )
        })
        .max_by_key(|&(id, edges)| (edges, std::cmp::Reverse(id)));
    if let Some((after, prov)) = &out.change {
        res.created_faces = prov.created_faces.len();
        let edges = before_edges.get_or_insert_with(|| edge_faces(mesh));
        res.untouched_outside = Some(untouched_outside(mesh, edges, after, prov));
        res.after = Some(summary(&inspect(after)));
    }
    res
}

fn remove_small(mesh: &Mesh) -> RemoveSmallResult {
    let n = mesh.face_count();
    let spec = builtin::get("mesh.remove_small_components").unwrap();
    let params = spec.validate(&BTreeMap::new()).unwrap();
    let max_triangles = params.get_int("max_triangles").unwrap() as usize;
    let max_relative = params.get_float("max_relative_size").unwrap();
    let op = find("mesh.remove_small_components").unwrap();
    let (r, phase, timed_out) = guarded("remove_small_components", |ctx| {
        op.execute(mesh, &Target::WholeObject, &params, ctx)
    });
    let mut res = RemoveSmallResult {
        outcome: Outcome::Ok,
        timing: Some(timing(&phase, n)),
        removed_components: None,
        removed_triangle_share: None,
        postcondition: None,
        after: None,
    };
    match r {
        Err(o) => res.outcome = o,
        Ok(Err(e)) => res.outcome = op_outcome(&e, timed_out, phase.wall_s),
        Ok(Ok(out)) => {
            res.removed_components = Some(out.report.clone());
            let removed = out.report["removed_triangles"].as_u64().unwrap_or(0);
            res.removed_triangle_share = Some(removed as f64 / n.max(1) as f64);
            let after = match &out.change {
                Some((after, _)) => after,
                None => mesh,
            };
            let sizes: Vec<usize> = after.topology().components.iter().map(Vec::len).collect();
            let largest = sizes.iter().copied().max().unwrap_or(0);
            // The largest before the run sets the relative limit.
            let before = mesh.topology().components.first().map_or(0, Vec::len);
            res.postcondition = Some(sizes.iter().all(|&s| {
                s >= max_triangles
                    || s as f64 * 100.0 > max_relative * before as f64
                    || s == largest
            }));
            res.after = Some(summary(&inspect(after)));
        }
    }
    res
}

fn targeted(
    mesh: &Mesh,
    before_edges: &mut Option<HashMap<(u32, u32), u32>>,
    largest: Option<(u32, usize)>,
) -> TargetedResult {
    let Some((id, edges)) = largest else {
        return TargetedResult {
            loop_id: None,
            loop_edges: None,
            outcome: Outcome::Ok,
            timing: None,
            untouched_outside: None,
        };
    };
    let params = builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap();
    let op = find("mesh.fill_holes").unwrap();
    let (r, phase, timed_out) = guarded("targeted_fill", |ctx| {
        op.execute(mesh, &Target::Loops(vec![id]), &params, ctx)
    });
    let mut res = TargetedResult {
        loop_id: Some(id),
        loop_edges: Some(edges),
        outcome: Outcome::Ok,
        timing: Some(timing(&phase, mesh.face_count())),
        untouched_outside: None,
    };
    match r {
        Err(o) => res.outcome = o,
        Ok(Err(e)) => res.outcome = op_outcome(&e, timed_out, phase.wall_s),
        Ok(Ok(out)) => {
            res.untouched_outside = Some(match &out.change {
                Some((after, prov)) => {
                    let e = before_edges.get_or_insert_with(|| edge_faces(mesh));
                    untouched_outside(mesh, e, after, prov)
                }
                None => Err("a selected hole produced no change".into()),
            });
        }
    }
    res
}

fn run_model(m: &Model, max_loop_edges: usize) -> ModelResult {
    let path = mf_fixtures::real::dir().join(m.file);
    let mut res = ModelResult {
        file: m.file,
        thingi10k_file_id: m.thingi10k,
        licence: m.licence,
        chosen_for: m.damage,
        damage_not_shown: Vec::new(),
        import: Outcome::Ok,
        import_timing: None,
        import_warnings: Vec::new(),
        analyse: None,
        analyse_timing: None,
        before: None,
        fills: Vec::new(),
        remove_small_components: None,
        targeted_fill: None,
        unsupported: Vec::new(),
    };
    let (r, phase, timed_out) = guarded("import", |ctx| mf_io::read_model_with(&path, ctx));
    let model = match r {
        Err(o) => {
            res.import = o;
            return res;
        }
        Ok(Err(e)) => {
            res.import = io_outcome(&e, timed_out, phase.wall_s);
            return res;
        }
        Ok(Ok(model)) => model,
    };
    res.import_warnings = model.warnings;
    let Some(object) = model.objects.into_iter().next() else {
        res.import = Outcome::Refused {
            code: "no_objects".into(),
            message: format!("{} contains no mesh objects", m.file),
        };
        return res;
    };
    let mesh = object.mesh;
    res.import_timing = Some(timing(&phase, mesh.face_count()));

    let (r, phase, timed_out) = guarded("analyse", |ctx| inspect_with(&mesh, ctx));
    res.analyse_timing = Some(timing(&phase, mesh.face_count()));
    let d = match r {
        Err(o) => {
            res.analyse = Some(o);
            return res;
        }
        Ok(Err(_)) => {
            res.analyse = Some(if timed_out {
                Outcome::TimedOut {
                    after_s: phase.wall_s,
                }
            } else {
                Outcome::Refused {
                    code: "cancelled".into(),
                    message: "cancelled by the user".into(),
                }
            });
            return res;
        }
        Ok(Ok(d)) => d,
    };
    res.analyse = Some(Outcome::Ok);
    res.before = Some(summary(&d));
    res.damage_not_shown = m
        .damage
        .iter()
        .copied()
        .filter(|&dmg| !shows(dmg, &d, m, max_loop_edges))
        .collect();

    let mut before_edges = None;
    for (style, params) in fill_styles() {
        res.fills
            .push(fill_whole(&mesh, &mut before_edges, &style, &params));
    }
    res.remove_small_components = Some(remove_small(&mesh));
    let largest = res.fills.first().and_then(|f| f.largest_filled);
    res.targeted_fill = Some(targeted(&mesh, &mut before_edges, largest));
    let skipped = res
        .fills
        .first()
        .map(|f| f.skipped.clone())
        .unwrap_or_default();
    res.unsupported = unsupported(&d, m, &skipped);
    let share = res
        .remove_small_components
        .as_ref()
        .and_then(|r| r.removed_triangle_share);
    let pieces = res
        .remove_small_components
        .as_ref()
        .and_then(|r| r.removed_components.as_ref())
        .map_or(0, |r| r["removed_components"].as_u64().unwrap_or(0)) as usize;
    let most_pieces = d.components > 1 && 2 * pieces > d.components;
    if let Some(share) = share.filter(|&s| s > 0.1 || most_pieces) {
        res.unsupported.push(json!({
            "damage": format!(
                "remove_small_components defaults delete {pieces} of {} pieces, {:.0}% of the triangles (multi-part design or unwelded soup)",
                d.components,
                100.0 * share
            ),
            "count": res.remove_small_components.as_ref().unwrap().removed_components.as_ref().unwrap()["removed_triangles"],
            "issue": "MF-ISSUE-052",
        }));
    }
    res
}

fn run_all() -> Vec<ModelResult> {
    let max_loop_edges = builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap()
        .get_int("max_loop_edges")
        .unwrap() as usize;
    MODELS
        .iter()
        .map(|m| {
            let r = run_model(m, max_loop_edges);
            eprintln!("{}", line(&r));
            r
        })
        .collect()
}

/// One line of the human-readable table.
fn line(r: &ModelResult) -> String {
    let b = r.before.as_ref();
    let get = |k: &str| b.map_or(json!(null), |v| v[k].clone());
    let fill = r.fills.first();
    format!(
        "{:<40} import={:?} tris={} loops={} nmE={} pieces={} | fill: filled={} skipped={:?} {:.2}s | targeted: {:?} | unsupported: {}",
        r.file,
        r.import,
        get("triangle_count"),
        get("boundary_loop_count"),
        get("non_manifold_edges"),
        get("components"),
        fill.map_or(0, |f| f.filled),
        fill.map(|f| &f.skipped),
        fill.and_then(|f| f.timing.as_ref())
            .map_or(0.0, |t| t.wall_s),
        r.targeted_fill.as_ref().map(|t| (&t.outcome, t.loop_edges)),
        r.unsupported
            .iter()
            .map(|u| u["issue"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(","),
    )
}

static RESULTS: OnceLock<Vec<ModelResult>> = OnceLock::new();

/// The collection's results, run once per process; `None` (after
/// recording a skip) if any model hasn't been fetched.
fn corpus(test: &str) -> Option<&'static [ModelResult]> {
    let missing: Vec<PathBuf> = MODELS
        .iter()
        .filter_map(|m| match mf_fixtures::real::path(m.file, test) {
            Some(_) => None,
            None => Some(mf_fixtures::real::dir().join(m.file)),
        })
        .collect();
    if !missing.is_empty() {
        return None;
    }
    Some(RESULTS.get_or_init(run_all))
}

#[test]
fn every_messy_model_imports_and_analyses() {
    let Some(results) = corpus("every_messy_model_imports_and_analyses") else {
        return;
    };
    assert!((10..=20).contains(&results.len()), "{}", results.len());
    for r in results {
        // A model over a reader limit is a named refusal, which is fine;
        // any other import error on a real file is a bug.
        let imported = r.import == Outcome::Ok
            || matches!(&r.import, Outcome::Refused { code, .. } if code == "limit_exceeded");
        assert!(imported, "{}: import {:?}", r.file, r.import);
        if r.import != Outcome::Ok {
            continue;
        }
        assert_eq!(r.analyse, Some(Outcome::Ok), "{}", r.file);
        assert!(
            r.damage_not_shown.is_empty(),
            "{} doesn't show {:?}: {}",
            r.file,
            r.damage_not_shown,
            r.before.as_ref().unwrap()
        );
    }
    // Between them, the models show every kind of damage on the list.
    for damage in REQUIRED {
        assert!(
            results
                .iter()
                .any(|r| r.chosen_for.contains(&damage) && r.analyse == Some(Outcome::Ok)),
            "no model shows {damage:?}"
        );
    }
}

#[test]
fn every_messy_model_fills_or_refuses_by_name() {
    let Some(results) = corpus("every_messy_model_fills_or_refuses_by_name") else {
        return;
    };
    let styles = fill_styles().len();
    for r in results.iter().filter(|r| r.analyse == Some(Outcome::Ok)) {
        assert_eq!(r.fills.len(), styles, "{}", r.file);
        for f in &r.fills {
            assert!(
                f.outcome.is_named(),
                "{} ({}): {:?}",
                r.file,
                f.style,
                f.outcome
            );
            if f.outcome != Outcome::Ok {
                continue;
            }
            // No silent fallback: every hole is filled or skipped by name.
            assert!(
                f.accounted,
                "{} ({}): {} loops, {} filled, skipped {:?}",
                r.file, f.style, f.loops_before, f.filled, f.skipped
            );
            for reason in f.skipped.keys() {
                assert!(
                    ["too_large", "self_touching", "untriangulable"].contains(&reason.as_str()),
                    "{} ({}): unnamed skip reason {reason}",
                    r.file,
                    f.style
                );
            }
            // A whole-object fill only adds: the input is untouched and no
            // patch edge ends up with more than two faces.
            if let Some(Err(e)) = &f.untouched_outside {
                panic!("{} ({}): {e}", r.file, f.style);
            }
            assert_eq!(f.untouched_outside.is_some(), f.filled > 0, "{}", r.file);
        }
        let rs = r.remove_small_components.as_ref().unwrap();
        assert!(rs.outcome.is_named(), "{}: {:?}", r.file, rs.outcome);
        if rs.outcome == Outcome::Ok {
            assert_eq!(rs.postcondition, Some(true), "{}", r.file);
        }
    }
}

#[test]
fn targeted_fill_on_messy_models_is_untouched_outside() {
    let Some(results) = corpus("targeted_fill_on_messy_models_is_untouched_outside") else {
        return;
    };
    let mut targeted = 0;
    for r in results.iter().filter(|r| r.analyse == Some(Outcome::Ok)) {
        let t = r.targeted_fill.as_ref().unwrap();
        assert!(t.outcome.is_named(), "{}: {:?}", r.file, t.outcome);
        let Some(id) = t.loop_id else {
            // Nothing was fillable whole-object; fine only if every hole
            // was refused by name (checked in the fills test).
            continue;
        };
        // The same hole filled whole-object must fill when targeted alone.
        assert_eq!(t.outcome, Outcome::Ok, "{}: loop {id}", r.file);
        match &t.untouched_outside {
            Some(Ok(_)) => targeted += 1,
            other => panic!("{}: loop {id}: {other:?}", r.file),
        }
    }
    // Most of the collection has holes.
    assert!(targeted >= 6, "{targeted} targeted fills");
}

fn results_path() -> PathBuf {
    match std::env::var_os("MF_ARTIFACTS") {
        Some(dir) => PathBuf::from(dir).join("messy"),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/m4/messy"),
    }
    .join("results.json")
}

#[test]
fn messy_results_are_recorded() {
    let Some(results) = corpus("messy_results_are_recorded") else {
        return;
    };
    let path = results_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let doc = json!({
        "task": "M4 Task 11a: real-world messy-model collection",
        "sources": "fixtures/SOURCES.md",
        "build": if cfg!(debug_assertions) { "test (debug harness; mf-* crates at opt-level 2)" } else { "release" },
        "machine": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "threads": std::thread::available_parallelism().map(|n| n.get()).ok(),
        },
        "hang_limit_s": HANG.as_secs(),
        "budget_rule": "1 s + 2 s per million triangles per step (recorded, not gated, in M4)",
        "fill_styles": fill_styles().into_iter().map(|(s, _)| s).collect::<Vec<_>>(),
        "models": results,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    // Read back: one complete entry per model.
    let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let models = back["models"].as_array().unwrap();
    assert_eq!(models.len(), MODELS.len());
    for (m, v) in MODELS.iter().zip(models) {
        assert_eq!(v["file"], m.file);
        assert!(v["licence"].is_string() && v["import"]["outcome"].is_string());
        if v["import"]["outcome"] != "ok" {
            continue;
        }
        assert!(v["import_timing"]["wall_s"].is_number(), "{}", m.file);
        assert!(
            v["import_timing"]["peak_heap_bytes"].is_number(),
            "{}",
            m.file
        );
        assert!(v["before"]["triangle_count"].is_number(), "{}", m.file);
        for f in v["fills"].as_array().unwrap() {
            assert!(
                f["outcome"].is_string() && f["skipped"].is_object(),
                "{}",
                m.file
            );
            assert!(f["timing"]["budget_s"].is_number(), "{}", m.file);
        }
        assert!(
            v["remove_small_components"]["outcome"].is_string(),
            "{}",
            m.file
        );
        assert!(v["targeted_fill"]["outcome"].is_string(), "{}", m.file);
        assert!(v["unsupported"].is_array(), "{}", m.file);
    }
    eprintln!("messy-model results: {}", path.display());
}

/// Two faces on edge (0, 1) (a third if `tangled`), and two faces that
/// leave edges (0, 4) and (4, 1) open: closing the hole 0-1-4 with one
/// face adds a face on (0, 1) and completes (0, 4) and (4, 1).
fn hinge(tangled: bool) -> Mesh {
    let mut positions = vec![
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.5, 1.0, 0.0],
        [0.5, -1.0, 0.0],
        [0.5, 0.0, 1.0],
        [-0.5, 0.0, 1.0],
        [1.5, 0.0, 1.0],
    ];
    let mut triangles = vec![[0, 1, 2], [1, 0, 3], [0, 4, 5], [4, 1, 6]];
    if tangled {
        positions.push([0.5, 0.0, -1.0]);
        triangles.push([0, 1, 7]);
    }
    Mesh::new(positions, triangles).unwrap()
}

/// Closes hole 0-1-4 with one face.
fn close_hinge(m: &Mesh) -> (Mesh, mf_geometry::Provenance) {
    let mut e = m.edit();
    e.add_face([1, 0, 4], mf_geometry::Sources::default())
        .unwrap();
    e.finish().unwrap()
}

#[test]
fn untouched_outside_rejects_a_clean_edge_made_non_manifold() {
    // 2 → 3 faces: the fill made a clean edge non-manifold. A violation.
    let clean = hinge(false);
    let (after, prov) = close_hinge(&clean);
    let err = untouched_outside(&clean, &edge_faces(&clean), &after, &prov).unwrap_err();
    assert!(
        err.contains("has 3 faces after the fill (2 before)"),
        "{err}"
    );
    // 3 → 4 faces: the rim ran along an edge already tangled in the input
    // (MF-ISSUE-047); counted, not a violation.
    let tangled = hinge(true);
    let (after, prov) = close_hinge(&tangled);
    assert_eq!(
        untouched_outside(&tangled, &edge_faces(&tangled), &after, &prov),
        Ok(1)
    );
}
