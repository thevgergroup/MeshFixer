#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Cooperative cancellation and progress through import, topology and
//! operations (MF-RT-002, MF-RT-003, MF-CMD-006).
//!
//! The 5M-triangle tests run on code built at opt-level 2 even in the dev
//! profile (root `Cargo.toml`), so their timings mean something. They hold
//! a lock so they never time each other.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use mf_command::{CommandError, CommitOutcome, Completed, Invocation, ObjectId, Prepared, Session};
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, Imported, IoError, SourceInfo, SourcePrecision};
use mf_runtime::{CheckpointProbe, RunContext};

const BIG: usize = 5_000_000;
/// MF-RT-002 via the M4 plan: cancel returns within 250 ms.
const CANCEL_BUDGET: Duration = Duration::from_millis(250);
/// MF-RT-002: a checkpoint at least every 100 ms of work.
const CHECKPOINT_BUDGET: Duration = Duration::from_millis(100);

/// One test at a time, so timings aren't skewed by a neighbour.
fn heavy() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The 5M damaged sphere, generated once per test binary. Callers that
/// need topology computed from scratch release it first.
fn big_mesh() -> Mesh {
    static MESH: OnceLock<Mesh> = OnceLock::new();
    MESH.get_or_init(|| damaged_sphere(&DamageSpec::for_triangles(BIG, 11)))
        .clone()
}

fn imported(mesh: Mesh) -> Imported {
    Imported {
        mesh,
        source: SourceInfo {
            path: "big.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

fn fill(target: ObjectId) -> Invocation {
    Invocation {
        op_id: "mesh.fill_holes".into(),
        target,
        selection: None,
        raw_params: BTreeMap::new(),
    }
}

fn session_with(mesh: Mesh) -> (Session, ObjectId) {
    let mut s = Session::new();
    let id = s.import(imported(mesh), "big".into());
    (s, id)
}

fn hash_of(s: &Session, id: ObjectId) -> ContentHash {
    s.document().object(id).unwrap().geometry.content_hash()
}

type Outcome<T> = (T, Instant);

/// Run `job` on a thread with `ctx`; return a receiver for its result and
/// the instant it returned.
fn spawn<T: Send + 'static>(
    ctx: &RunContext,
    job: impl FnOnce(&RunContext) -> T + Send + 'static,
) -> mpsc::Receiver<Outcome<T>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let out = job(&ctx);
        tx.send((out, Instant::now())).unwrap();
    });
    rx
}

/// Poll progress until `when(stage, fraction)` holds (or the job finished
/// first, which is a test failure), then cancel; returns the time from
/// `cancel()` to the job's return, and its result.
fn cancel_when<T>(
    ctx: &RunContext,
    rx: &mpsc::Receiver<Outcome<T>>,
    what: &str,
    when: impl Fn(&str, f64) -> bool,
) -> (T, Duration) {
    let mut last = ("", 0.0);
    loop {
        if let Ok((_, _)) = rx.try_recv() {
            panic!("{what}: the job finished before the cancel point (last saw {last:?})");
        }
        let (stage, f) = ctx.progress.snapshot();
        if when(stage, f) {
            break;
        }
        last = (stage, f);
        std::thread::sleep(Duration::from_micros(200));
    }
    let cancelled_at = Instant::now();
    ctx.cancel.cancel();
    let (out, returned_at) = rx.recv().unwrap();
    (out, returned_at.saturating_duration_since(cancelled_at))
}

fn write_big_stl(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("big.stl");
    mf_io::write_path(&big_mesh(), &path, Format::StlBinary).unwrap();
    path
}

fn describe(probe: &CheckpointProbe) -> String {
    match probe.worst() {
        Some(w) => format!(
            "largest gap {:?} from {} to {} ({} checkpoints)",
            w.gap,
            w.from.map_or("the start".to_string(), |l| l.to_string()),
            w.to,
            probe.checkpoints()
        ),
        None => "no checkpoints at all".into(),
    }
}

#[test]
fn cancel_during_fill_holes_leaves_document_unchanged() {
    let _guard = heavy();
    let mesh = big_mesh();
    mesh.release_topology();
    let (s, id) = session_with(mesh);
    let before_hash = hash_of(&s, id);
    let before_revisions = s.revisions().len();
    let before_current = s.current();

    let prepared = s.prepare(fill(id)).unwrap();
    let ctx = RunContext::none();
    let rx = spawn(&ctx, move |ctx| prepared.run(ctx));
    // 30% of the way through filling.
    let (result, _) = cancel_when(&ctx, &rx, "fill", |stage, f| {
        stage == "triangulating" && f >= 0.3
    });

    assert!(
        matches!(result, Err(CommandError::Cancelled)),
        "{:?}",
        result.err()
    );
    assert_eq!(hash_of(&s, id), before_hash);
    assert_eq!(s.revisions().len(), before_revisions);
    assert_eq!(s.current(), before_current);
}

#[test]
fn cancel_to_stop_within_250ms_on_5m_import_and_fill() {
    let _guard = heavy();
    let dir = tempfile::tempdir().unwrap();
    let path = write_big_stl(dir.path());

    let mut measured = Vec::new();
    // Import: while reading, and twice while welding.
    let import_points: [(&str, &str, f64); 3] = [
        ("import/reading", "reading", 0.2),
        ("import/welding", "welding", 0.0),
        ("import/welding late", "welding", 0.6),
    ];
    for (what, at_stage, at) in import_points {
        let ctx = RunContext::none();
        let p = path.clone();
        let rx = spawn(&ctx, move |ctx| mf_io::read_model_with(&p, ctx));
        let (result, took) = cancel_when(&ctx, &rx, what, |stage, f| stage == at_stage && f >= at);
        assert!(
            matches!(result, Err(IoError::Cancelled)),
            "{what}: {:?}",
            result.err()
        );
        measured.push((what, took));
    }

    // Fill: in topology, triangulating and refining.
    let fill_points: [(&str, &str, f64); 4] = [
        ("fill/topology", "topology", 0.2),
        ("fill/topology late", "topology", 0.7),
        ("fill/triangulating", "triangulating", 0.5),
        ("fill/refining", "refining", 0.3),
    ];
    for (what, at_stage, at) in fill_points {
        let mesh = big_mesh();
        mesh.release_topology();
        let (s, id) = session_with(mesh);
        let prepared = s.prepare(fill(id)).unwrap();
        let ctx = RunContext::none();
        let rx = spawn(&ctx, move |ctx| prepared.run(ctx));
        let (result, took) = cancel_when(&ctx, &rx, what, |stage, f| stage == at_stage && f >= at);
        assert!(
            matches!(result, Err(CommandError::Cancelled)),
            "{what}: {:?}",
            result.err()
        );
        measured.push((what, took));
    }
    eprintln!("cancel-to-stop: {measured:?}");
    for (what, took) in &measured {
        assert!(
            *took <= CANCEL_BUDGET,
            "{what}: {took:?} > {CANCEL_BUDGET:?}"
        );
    }
}

#[test]
fn checkpoints_at_least_every_100ms() {
    let _guard = heavy();
    // fill_holes on 5M triangles, topology included.
    let mesh = big_mesh();
    mesh.release_topology();
    let (s, id) = session_with(mesh);
    let prepared = s.prepare(fill(id)).unwrap();
    let (ctx, probe) = RunContext::instrumented();
    ctx.checkpoint().unwrap();
    let completed = prepared.run(&ctx).unwrap();
    ctx.checkpoint().unwrap();
    eprintln!("fill_holes: {}", describe(&probe));
    assert!(
        probe.max_gap() <= CHECKPOINT_BUDGET,
        "fill_holes: {}",
        describe(&probe)
    );
    drop(completed);

    // Import of the same mesh as a 5M binary STL.
    let dir = tempfile::tempdir().unwrap();
    let path = write_big_stl(dir.path());
    let (ctx, probe) = RunContext::instrumented();
    ctx.checkpoint().unwrap();
    let model = mf_io::read_model_with(&path, &ctx).unwrap();
    ctx.checkpoint().unwrap();
    eprintln!("import: {}", describe(&probe));
    assert!(
        probe.max_gap() <= CHECKPOINT_BUDGET,
        "import: {}",
        describe(&probe)
    );
    assert_eq!(model.objects[0].mesh.face_count(), big_mesh().face_count());
}

/// Every update a context's progress sends, in order.
fn stages_seen(updates: &[(&'static str, f64)]) -> Vec<&'static str> {
    let mut stages: Vec<&'static str> = Vec::new();
    for &(stage, _) in updates {
        if stages.last() != Some(&stage) {
            stages.push(stage);
        }
    }
    stages
}

fn assert_monotonic(what: &str, updates: &[(&'static str, f64)]) {
    for w in updates.windows(2) {
        if w[0].0 == w[1].0 {
            assert!(w[1].1 >= w[0].1, "{what}: {w:?} went backwards");
        }
    }
}

#[test]
fn progress_reaches_one_on_success() {
    let _guard = heavy();
    let mesh = damaged_sphere(&DamageSpec::for_triangles(40_000, 3));

    // Operations, through the session.
    let ops: [(&str, &[&str]); 3] = [
        (
            "mesh.fill_holes",
            &["topology", "triangulating", "refining"],
        ),
        ("mesh.remove_small_components", &["topology", "removing"]),
        ("mesh.inspect", &["topology", "inspecting"]),
    ];
    for (op, expected) in ops {
        let mesh = mesh.clone();
        mesh.release_topology();
        let (s, id) = session_with(mesh);
        let prepared: Prepared = s
            .prepare(Invocation {
                op_id: op.into(),
                target: id,
                selection: None,
                raw_params: BTreeMap::new(),
            })
            .unwrap();
        let ctx = RunContext::none();
        let (progress, reader) = ctx.progress.channel();
        let ctx = RunContext {
            cancel: ctx.cancel,
            progress,
        };
        let _completed: Completed = prepared.run(&ctx).unwrap();
        let updates: Vec<_> = std::iter::from_fn(|| reader.try_recv()).collect();
        assert_eq!(stages_seen(&updates), expected, "{op}");
        assert_monotonic(op, &updates);
        assert_eq!(
            ctx.progress.snapshot(),
            (*expected.last().unwrap(), 1.0),
            "{op}"
        );
    }

    // Imports, in every format. STL welds corners into vertices; OBJ and
    // 3MF are indexed already, so they end with reading.
    let dir = tempfile::tempdir().unwrap();
    let welded: &[&str] = &["reading", "welding"];
    let indexed: &[&str] = &["reading"];
    for (name, format, expected) in [
        ("m.stl", Format::StlBinary, welded),
        ("a.stl", Format::StlAscii, welded),
        ("m.obj", Format::Obj, indexed),
        ("m.3mf", Format::ThreeMf, indexed),
    ] {
        let path = dir.path().join(name);
        mf_io::write_path(&mesh, &path, format).unwrap();
        let ctx = RunContext::none();
        let (progress, reader) = ctx.progress.channel();
        let ctx = RunContext {
            cancel: ctx.cancel,
            progress,
        };
        let model = mf_io::read_model_with(&path, &ctx).unwrap();
        assert_eq!(
            model.objects[0].mesh.face_count(),
            mesh.face_count(),
            "{name}"
        );
        let updates: Vec<_> = std::iter::from_fn(|| reader.try_recv()).collect();
        assert_eq!(stages_seen(&updates), expected, "{name}");
        assert_monotonic(name, &updates);
        assert_eq!(
            ctx.progress.snapshot(),
            (*expected.last().unwrap(), 1.0),
            "{name}"
        );
    }
}

/// A small splitmix64, so the cancel offsets are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Review Focus 1: a cancel racing completion has exactly one outcome.
/// Either `run` noticed the cancel (an error, nothing to commit, the
/// document untouched) or it returned a result, which a later cancel can't
/// affect: committing it creates exactly one revision.
#[test]
fn cancel_racing_completion_has_one_outcome() {
    let _guard = heavy();
    let mesh = damaged_sphere(&DamageSpec::for_triangles(20_000, 5));
    let (mut s, id) = session_with(mesh);
    let base = s.current();
    let base_hash = hash_of(&s, id);

    // How long an uncancelled run takes, to aim the cancels around its end.
    let mut runs = Vec::new();
    for _ in 0..5 {
        let prepared = s.prepare(fill(id)).unwrap();
        let start = Instant::now();
        prepared.run(&RunContext::none()).unwrap();
        runs.push(start.elapsed());
    }
    runs.sort();
    let typical = runs[runs.len() / 2];

    let mut rng = Rng(0x5EED);
    let (mut cancelled, mut committed) = (0, 0);
    for i in 0..200 {
        assert_eq!(s.current(), base);
        let revisions = s.revisions().len();
        let prepared = s.prepare(fill(id)).unwrap();
        // Uniform in [0.5, 1.5] × the typical run time.
        let offset = typical.mul_f64(0.5 + (rng.next() % 1000) as f64 / 1000.0);
        let ctx = RunContext::none();
        let worker = {
            let ctx = ctx.clone();
            std::thread::spawn(move || prepared.run(&ctx))
        };
        let start = Instant::now();
        while start.elapsed() < offset {
            std::hint::spin_loop();
        }
        ctx.cancel.cancel();
        match worker.join().unwrap() {
            Err(CommandError::Cancelled) => {
                cancelled += 1;
                assert_eq!(hash_of(&s, id), base_hash, "iteration {i}");
                assert_eq!(s.revisions().len(), revisions, "iteration {i}");
            }
            Ok(completed) => {
                committed += 1;
                // The cancel came too late to matter: the result commits.
                let outcome = s.commit(completed).unwrap();
                assert!(
                    matches!(outcome, CommitOutcome::NewRevision { .. }),
                    "iteration {i}"
                );
                assert_eq!(s.revisions().len(), revisions + 1, "iteration {i}");
                assert_ne!(hash_of(&s, id), base_hash, "iteration {i}");
                assert!(s.undo());
            }
            Err(other) => panic!("iteration {i}: a third outcome: {other:?}"),
        }
    }
    eprintln!("typical run {typical:?}: {cancelled} cancelled, {committed} committed");
    assert_eq!(cancelled + committed, 200);
    assert!(
        cancelled > 0 && committed > 0,
        "{cancelled} cancelled, {committed} committed"
    );
}
