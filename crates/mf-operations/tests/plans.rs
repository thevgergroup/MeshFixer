#![allow(clippy::unwrap_used)] // tests may panic on failure

//! C22 (ADR-0008 part 1): the preview is the operation's own plan.
//! `mesh.split_plane` and `object.align` each expose `plan`, and Apply
//! executes that same plan, so a preview drawn from the plan shows
//! exactly what Apply does. These properties run under random
//! non-uniform transforms, where the old preview's frame used to
//! diverge from Apply's (architecture-review.md §1, §4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::{DMat3, DMat4, DQuat, DVec3};
use mf_document::ObjectId;
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_geometry::affine::{apply as place, from_dmat4};
use mf_operations::{
    DocContext, DocOutput, ObjectChange, OperandInput, RunContext, SplitPlan, Target, TargetPlan,
    execute_align, execute_split, find, plan_align, plan_split,
};
use mf_spec::Params;

/// A small deterministic generator (splitmix64): every case is
/// reproducible from its seed, printed on failure.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[lo, hi)`.
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn unit(&mut self) -> DVec3 {
        loop {
            let v = DVec3::new(
                self.range(-1.0, 1.0),
                self.range(-1.0, 1.0),
                self.range(-1.0, 1.0),
            );
            let l = v.length();
            if l > 0.1 && l <= 1.0 {
                return v / l;
            }
        }
    }

    /// A rotation, then a strongly non-uniform scale (each axis 0.4 to
    /// 3, at least 1.5x apart somewhere), then a translation: row-major
    /// 3x4 (D7).
    fn non_uniform_transform(&mut self) -> [f64; 12] {
        let rotation = DQuat::from_axis_angle(self.unit(), self.range(0.0, std::f64::consts::TAU));
        let mut s = [0.0; 3];
        loop {
            for c in &mut s {
                *c = self.range(0.4, 3.0);
            }
            let (lo, hi) = s.iter().fold((f64::INFINITY, 0.0_f64), |(lo, hi), &c| {
                (lo.min(c), hi.max(c))
            });
            if hi / lo > 1.5 {
                break;
            }
        }
        let linear = DMat3::from_quat(rotation) * DMat3::from_diagonal(DVec3::from(s));
        let translation = DVec3::new(
            self.range(-50.0, 50.0),
            self.range(-50.0, 50.0),
            self.range(-50.0, 50.0),
        );
        from_dmat4(&DMat4::from_mat3_translation(linear, translation))
    }
}

fn vec3_text(v: [f64; 3]) -> String {
    format!("{},{},{}", v[0], v[1], v[2])
}

fn params(op: &str, raw: &[(&str, String)]) -> Params {
    let raw: BTreeMap<String, String> = raw
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    mf_spec::builtin::get(op).unwrap().validate(&raw).unwrap()
}

fn whole<'a>(
    name: &'a str,
    mesh: &'a Mesh,
    transform: [f64; 12],
    slot: &'a str,
) -> OperandInput<'a> {
    OperandInput {
        slot,
        object: ObjectId(1),
        name,
        mesh,
        transform,
        target: Target::WholeObject,
    }
}

/// Everything a mesh is, bit for bit: its faces in order, each with its
/// corners' positions.
fn mesh_bits(m: &Mesh) -> Vec<[[u64; 3]; 3]> {
    m.faces()
        .map(|(_, f)| f.map(|v| m.corner_position(v).map(f64::to_bits)))
        .collect()
}

/// A `DocOutput`, comparable bit for bit (changes in order, then the
/// report).
fn output_bits(out: &DocOutput) -> (Vec<String>, serde_json::Value) {
    let changes = out
        .changes
        .iter()
        .map(|c| match c {
            ObjectChange::Geometry { operand, mesh, .. } => {
                format!("geometry {operand} {:?}", mesh_bits(mesh))
            }
            ObjectChange::Create {
                name,
                mesh,
                transform,
                ..
            } => format!(
                "create {name} {:?} {:?}",
                transform.map(f64::to_bits),
                mesh_bits(mesh)
            ),
            ObjectChange::Transform { operand, transform } => {
                format!("transform {operand} {:?}", transform.map(f64::to_bits))
            }
            other => format!("{other:?}"),
        })
        .collect();
    (changes, out.report.clone())
}

/// Distance of world point `p` from the world plane through `point`
/// with unit `normal`.
fn off_plane(p: [f64; 3], point: DVec3, normal: DVec3) -> f64 {
    (DVec3::from(p) - point).dot(normal).abs()
}

/// For random non-uniform transforms, random planes through the model,
/// every alignment kind and a far-away second target the plane misses:
/// - Apply's output is exactly `execute_split` of the plan a preview
///   would have computed (bit for bit: changes and report);
/// - the plan's regions and hole bases, carried to world space by the
///   target's own transform, lie on the world cut planes the user placed
///   (the check the old `M⁻¹·n` preview failed);
/// - the plan's joints, missed targets and hole count are the report's.
#[test]
fn preview_equals_apply_for_split() {
    let sphere = scaled(&uv_sphere(12, 24), 30.0);
    let ctx = RunContext::none();
    let op = find("mesh.split_plane").unwrap();
    let kinds = ["none", "dowels", "attached", "dovetail"];
    let mut cases = 0;
    for seed in 0..24u64 {
        let mut rng = Rng(seed);
        let t = rng.non_uniform_transform();
        // A plane through a point well inside the sphere, any direction.
        let object_point = rng.unit() * rng.range(0.0, 8.0);
        let world_point = DVec3::from(place(&t, object_point.to_array()));
        let normal = rng.unit();
        let gap = if seed % 3 == 0 {
            0.0
        } else {
            rng.range(0.1, 1.0)
        };
        let kind = kinds[(seed % 4) as usize];
        // The same sphere, pushed far along the plane's normal: the plane
        // misses it, so it's listed, not refused.
        let mut far = t;
        for (i, c) in (normal * 10_000.0).to_array().into_iter().enumerate() {
            far[9 + i] += c;
        }
        let inputs = [
            whole("near", &sphere, t, "target"),
            whole("far", &sphere, far, "target"),
        ];
        let p = params(
            "mesh.split_plane",
            &[
                ("plane_point", vec3_text(world_point.to_array())),
                ("plane_normal", vec3_text(normal.to_array())),
                ("cut_gap", gap.to_string()),
                ("pegs", kind.to_owned()),
                ("hole_diameter", "3".to_owned()),
                ("hole_depth_above", "4".to_owned()),
                ("hole_depth_below", "5".to_owned()),
                ("min_wall", "1".to_owned()),
            ],
        );
        let preview: SplitPlan = match plan_split(&inputs, &p, &ctx) {
            Ok(plan) => plan,
            // A random plane can make a section a dovetail doesn't fit;
            // Apply must refuse it the same way.
            Err(e) => {
                let apply = op.execute_scene(&inputs, &[], &p, &DocContext::default(), &ctx);
                let Err(apply_e) = apply else {
                    panic!("seed {seed}: the plan refused ({e}) but Apply didn't");
                };
                assert_eq!(e.to_string(), apply_e.to_string(), "seed {seed}");
                continue;
            }
        };
        let apply = op
            .execute_scene(&inputs, &[], &p, &DocContext::default(), &ctx)
            .unwrap();
        let executed = execute_split(preview.clone(), &inputs, &[], &p, &ctx).unwrap();
        assert_eq!(
            output_bits(&apply),
            output_bits(&executed),
            "seed {seed}: Apply differs from executing the preview's plan"
        );
        assert_eq!(preview.missed(), ["far"], "seed {seed}");
        assert_eq!(
            apply.report["missed"],
            serde_json::json!(["far"]),
            "seed {seed}"
        );
        let joints: Vec<_> = preview.joints().iter().map(|j| j.report()).collect();
        assert_eq!(
            apply.report["joints"],
            serde_json::json!(joints),
            "seed {seed}"
        );

        let TargetPlan::Cut(cut) = &preview.targets[0] else {
            panic!("seed {seed}: the plane goes through the near sphere");
        };
        let unit = normal.normalize();
        let above_w = world_point + unit * gap / 2.0;
        let below_w = world_point - unit * gap / 2.0;
        let tol = 1e-6;
        let regions = cut.regions();
        assert!(!regions.is_empty(), "seed {seed}: a section");
        let region_plane = if cut.below.is_some() {
            below_w
        } else {
            above_w
        };
        for r in regions {
            for q in &r.outer {
                let w = place(&t, r.frame.to_3d(*q));
                assert!(
                    off_plane(w, region_plane, unit) < tol,
                    "seed {seed}: a region's outline is {} off the world cut plane",
                    off_plane(w, region_plane, unit)
                );
            }
        }
        if let Some(pegs) = &cut.pegs {
            assert_eq!(
                apply.report["pegs"]["holes"],
                serde_json::json!(pegs.holes.len()),
                "seed {seed}"
            );
            for h in &pegs.holes {
                assert!(off_plane(h.base_below, below_w, unit) < tol, "seed {seed}");
                if cut.above.is_some() && cut.below.is_some() {
                    assert!(off_plane(h.base_above, above_w, unit) < tol, "seed {seed}");
                }
            }
        }
        cases += 1;
    }
    assert!(cases >= 12, "only {cases} of 24 random cases cut");
}

/// World bounds (min, max) of `mesh` placed by `t`, by `object.align`'s
/// rule: the box around its object-space bounding box's eight corners,
/// placed.
fn world_bounds(mesh: &Mesh, t: &[f64; 12]) -> ([f64; 3], [f64; 3]) {
    let bb = mesh.bounding_box().unwrap();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in 0..8 {
        let p = [0, 1, 2].map(|a| {
            if c & (1 << a) != 0 {
                bb.max[a]
            } else {
                bb.min[a]
            }
        });
        let w = place(t, p);
        for a in 0..3 {
            lo[a] = lo[a].min(w[a]);
            hi[a] = hi[a].max(w[a]);
        }
    }
    (lo, hi)
}

/// For random non-uniform transforms, every axis and the edge/centre
/// modes: Apply's transforms are exactly the plan's, and the plan's
/// transforms really do line the objects' world bounds up.
#[test]
fn preview_equals_apply_for_align() {
    let ctx = RunContext::none();
    let op = find("object.align").unwrap();
    let meshes = [cube(10.0), cube(4.0), scaled(&uv_sphere(8, 16), 6.0)];
    for seed in 0..48u64 {
        let mut rng = Rng(1000 + seed);
        let transforms: Vec<[f64; 12]> = (0..3).map(|_| rng.non_uniform_transform()).collect();
        let inputs: Vec<OperandInput> = ["a", "b", "c"]
            .iter()
            .zip(&meshes)
            .zip(&transforms)
            .map(|((name, mesh), t)| whole(name, mesh, *t, "target"))
            .collect();
        let axis = ["x", "y", "z"][(seed % 3) as usize];
        let mode = ["min", "centre", "max", "distribute_centres"][(seed / 3 % 4) as usize];
        let p = params(
            "object.align",
            &[
                ("axis", axis.to_owned()),
                ("mode", mode.to_owned()),
                ("leaders", "[]".to_owned()),
            ],
        );
        let preview = plan_align(&inputs, &p, &ctx).unwrap();
        let apply = op
            .execute_doc(&inputs, &p, &DocContext::default(), &ctx)
            .unwrap();
        assert_eq!(
            output_bits(&apply),
            output_bits(&execute_align(&preview)),
            "seed {seed}: Apply differs from executing the preview's plan"
        );
        let a = match axis {
            "x" => 0,
            "y" => 1,
            _ => 2,
        };
        let placed: Vec<_> = meshes
            .iter()
            .zip(&preview.transforms)
            .map(|(m, t)| world_bounds(m, t))
            .collect();
        let value = |b: &([f64; 3], [f64; 3])| match mode {
            "min" => b.0[a],
            "max" => b.1[a],
            _ => (b.0[a] + b.1[a]) / 2.0,
        };
        if mode != "distribute_centres" {
            for b in &placed[1..] {
                assert!(
                    (value(b) - value(&placed[0])).abs() < 1e-6,
                    "seed {seed}: {mode} on {axis} didn't line up: {placed:?}"
                );
            }
        }
    }
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// C22: there is one affine module, `mf_geometry::affine`. No other file
/// in the workspace's crates (sources or tests) is an affine module or
/// re-implements its inverse, composition, determinant, direction or
/// plane-normal rules for the 12-value 3MF transform.
///
/// mf-desktop's `split_flow.rs` still carries the region preview's own
/// `inverse_transform` until C23 moves that preview onto the Split plan
/// and deletes it; it is the one allowed exception, by name.
#[test]
fn affine_is_implemented_once() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let the_module = crates.join("mf-geometry/src/affine.rs");
    let this_test = crates.join("mf-operations/tests/plans.rs");
    let c23 = crates.join("mf-desktop/src/split_flow.rs");
    let forbidden = [
        "fn inverse(t: &[f64; 12]",
        "fn compose(a: &[f64; 12]",
        "fn determinant(t: &[f64; 12]",
        "fn apply_linear(",
        "fn normal_to_object(",
        "fn world_normal_to_object(",
        "fn inverse_transform(",
        "let cof = |r: usize, c: usize|",
        "t[3 * r] * n[0]",
        "mod affine",
    ];
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let krate = entry.unwrap().path();
        for sub in ["src", "tests"] {
            let dir = krate.join(sub);
            if dir.is_dir() {
                rust_files(&dir, &mut files);
            }
        }
    }
    assert!(files.len() > 100, "the scan found the workspace's sources");
    let mut found = Vec::new();
    for file in files {
        if file == the_module || file == this_test {
            continue;
        }
        if file.file_name().is_some_and(|n| n == "affine.rs") {
            found.push(format!("{}: a second affine module", file.display()));
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for pattern in forbidden {
                if !line.contains(pattern) {
                    continue;
                }
                let allowed = (file == c23 && pattern == "fn inverse_transform(")
                    // mf-geometry's own `pub mod affine;`.
                    || (pattern == "mod affine"
                        && file == crates.join("mf-geometry/src/lib.rs"));
                if !allowed {
                    found.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "affine math belongs in mf_geometry::affine only; use it instead of:\n{}",
        found.join("\n")
    );
}
