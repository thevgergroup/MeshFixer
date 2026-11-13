#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Split through the command layer on real closed manifold input that
//! puts vertices on, or within an ulp of, the cut plane (owner
//! 2026-11-13, the third Split refusal):
//!
//! - the owner's own steps, on the owner's own demo file: (A) 'scan' and
//!   'part' split together at the default plane, and (B) the same two
//!   combined, then split there;
//! - a randomized property: unions of primitives built by `mesh.combine`,
//!   cut at planes through vertex rings, single vertices and faces, are
//!   never refused, and every part is closed and manifold.
//!
//! The property runs `MF_SPLIT_PROPERTY_CASES` scenes (default 24); set
//! it higher for an on-demand soak, with `MF_SPLIT_PROPERTY_SEED` to
//! start elsewhere.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommandError, Invocation, ObjectId, Session};
use mf_fixtures::{cube, rotation_translation, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::RunContext;

/// The owner's demo file (`artifacts/phase1/demo/boolean-demo.3mf`,
/// copied byte for byte): a fresh `gen-fixture boolean-demo` differs
/// from it in the last bit of some coordinates — the level this bug
/// lives at — so the owner's own file is what's opened.
fn owner_demo() -> (Session, Vec<ObjectId>) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/owner/boolean-demo.3mf"
    );
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(Path::new(path)).unwrap());
    (s, ids)
}

fn names(s: &Session) -> Vec<String> {
    s.document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

fn split(target: ObjectId, also: &[ObjectId], params: &[(&str, String)]) -> Invocation {
    Invocation {
        op_id: "mesh.split_plane".into(),
        target,
        selection: None,
        operands: also
            .iter()
            .map(|&o| ("target".to_string(), o, None))
            .collect(),
        raw_params: params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    }
}

fn combine(target: ObjectId, solids: &[ObjectId]) -> Invocation {
    Invocation {
        op_id: "mesh.combine".into(),
        target,
        selection: None,
        operands: solids
            .iter()
            .map(|&o| ("solids".to_string(), o, None))
            .collect(),
        raw_params: BTreeMap::new(),
    }
}

/// Closed, manifold, consistently oriented, no degenerate face and no
/// two faces crossing (exact): `mf_boolean::validate_solid`.
fn assert_solid(mesh: &Mesh, what: &str) {
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &RunContext::none())
        .unwrap_or_else(|e| panic!("{what}: not a closed solid: {e}"));
}

/// Closed (no open edge), manifold (no edge with more than two faces)
/// and consistently oriented: the property every cut must keep. Not
/// the stricter [`assert_solid`]: where the kept side lies along the
/// plane within rounding (a face that is flat in world space but
/// ulp-noisy in a tilted object's frame, cut along it), the exact part
/// is an ε-thin slab, which f64 coordinates can't hold without
/// ulp-level crossings.
fn assert_closed_manifold(mesh: &Mesh, what: &str) {
    let topo = mesh.topology_with(&RunContext::none()).unwrap();
    assert_eq!(topo.boundary_edges, 0, "{what}: open edges");
    assert!(
        topo.non_manifold_edges.is_empty(),
        "{what}: {} non-manifold edges",
        topo.non_manifold_edges.len()
    );
    assert_eq!(topo.inconsistent_edges, 0, "{what}: misoriented edges");
}

/// Manifold and consistently oriented, and every open edge's ends on the
/// world plane `(point, normal)` (to rounding): an uncapped cut is open
/// only where it was cut.
fn assert_open_only_along(
    mesh: &Mesh,
    transform: &[f64; 12],
    point: [f64; 3],
    normal: [f64; 3],
    what: &str,
) {
    let topo = mesh.topology_with(&RunContext::none()).unwrap();
    assert!(
        topo.non_manifold_edges.is_empty(),
        "{what}: {} non-manifold edges",
        topo.non_manifold_edges.len()
    );
    assert_eq!(topo.inconsistent_edges, 0, "{what}: misoriented edges");
    let len = normal.iter().map(|c| c * c).sum::<f64>().sqrt();
    let n = normal.map(|c| c / len);
    let diagonal = mesh.bounding_box().map_or(1.0, |b| b.diagonal().max(1.0));
    for l in &topo.boundary_loops {
        for &v in &l.vertices {
            let p = apply(transform, mesh.corner_position(v));
            let d: f64 = (0..3).map(|i| (p[i] - point[i]) * n[i]).sum();
            assert!(
                d.abs() <= 1e-9 * diagonal,
                "{what}: an open edge {d} off the plane"
            );
        }
    }
}

fn mesh_of(s: &Session, name: &str) -> Mesh {
    s.document()
        .objects()
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no {name:?} in {:?}", names(s)))
        .mesh()
        .unwrap()
        .clone()
}

/// Owner step (A): 'scan' and 'part' selected together, Split at the
/// default plane for that selection (horizontal through its bounding-
/// box centre, (2.5, 0, 20)), Keep Top. Both cross the plane; both are
/// cut. Before the fix: `split_section_self_intersects` on 'scan'.
#[test]
fn owner_a_split_scan_and_part_together_at_the_default_plane() {
    let (mut s, ids) = owner_demo();
    s.execute(split(
        ids[0],
        &[ids[1]],
        &[("plane_point", "2.5,0,20".into()), ("keep", "above".into())],
    ))
    .unwrap();
    assert_eq!(
        names(&s),
        ["scan (top)", "part (top)", "crossed scan", "probe"]
    );
    assert_solid(&mesh_of(&s, "scan (top)"), "scan (top)");
    assert_solid(&mesh_of(&s, "part (top)"), "part (top)");
}

/// Owner step (B): Combine scan + part (one object 'scan' left), then
/// Split it at the same default plane, Keep Top, Cap on. Before the fix:
/// `split_section_self_intersects`, "The cut grazes the surface here
/// (near -7.045, 18.718, 20.000)".
#[test]
fn owner_b_combine_then_split_at_the_default_plane() {
    let (mut s, ids) = owner_demo();
    s.execute(combine(ids[0], &[ids[1]])).unwrap();
    assert_eq!(names(&s), ["scan", "crossed scan", "probe"]);
    for keep in ["above", "below", "both"] {
        s.execute(split(
            ids[0],
            &[],
            &[
                ("plane_point", "2.5,0,20".into()),
                ("keep", keep.into()),
                ("cap", "true".into()),
            ],
        ))
        .unwrap_or_else(|e| panic!("keep {keep}: {e}"));
        for o in s.document().objects() {
            if o.name.starts_with("scan") {
                assert_solid(o.mesh().unwrap(), &o.name);
            }
        }
        assert!(s.undo());
    }
}

/// The owner's scan alone, at the default plane's height with the
/// plane's point anywhere along it: the point's x and y never change
/// the cut (they used to, through the section's rounded 2D frame).
#[test]
fn owner_scan_split_at_its_equator_from_any_plane_point() {
    let (mut s, ids) = owner_demo();
    for x in ["0", "2.5", "-7.045", "0.1", "1e-9", "123.456"] {
        for y in ["0", "0.3", "-19"] {
            s.execute(split(
                ids[0],
                &[],
                &[
                    ("plane_point", format!("{x},{y},20")),
                    ("keep", "both".into()),
                ],
            ))
            .unwrap_or_else(|e| panic!("({x}, {y}, 20): {e}"));
            assert_solid(&mesh_of(&s, "scan (top)"), "top");
            assert_solid(&mesh_of(&s, "scan (bottom)"), "bottom");
            assert!(s.undo());
        }
    }
}

/// A genuinely self-intersecting input (the demo's 'crossed scan': a
/// box poking through a sphere's surface, two shells crossing) cut
/// where its two shells cross: the section's loops genuinely cross,
/// which is the one case Split refuses — naming Make solid as the next
/// step, which fixes exactly that.
#[test]
fn a_genuinely_self_intersecting_input_is_refused_with_make_solid_next() {
    let (mut s, ids) = owner_demo();
    let err = s
        .execute(split(
            ids[2],
            &[],
            &[("plane_point", "50,0,10.3".into()), ("keep", "both".into())],
        ))
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("split_section_self_intersects") && text.contains("Make solid"),
        "{text}"
    );
}

// ---------------------------------------------------------------------
// The randomized property.
// ---------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// A multiple of `step` in `[lo, hi]`.
    fn grid(&mut self, lo: f64, hi: f64, step: f64) -> f64 {
        let n = ((hi - lo) / step).round() as u64;
        lo + step * self.below(n + 1) as f64
    }
}

/// One primitive: its mesh and its transform, and for a sphere its
/// centre (world) — its equator ring is a whole ring of vertices on a
/// horizontal plane through it when it isn't tilted.
struct Primitive {
    mesh: Mesh,
    transform: [f64; 12],
    sphere_centre: Option<[f64; 3]>,
}

fn primitive(r: &mut Rng) -> Primitive {
    let t = [
        r.grid(-2.5, 2.5, 0.5),
        r.grid(-2.5, 2.5, 0.5),
        r.grid(0.0, 3.0, 0.5),
    ];
    let tilted = r.below(4) == 0;
    let transform = if tilted {
        rotation_translation(r.below(3) as usize, 15.0 * (1 + r.below(5)) as f64, t)
    } else {
        let mut m = IDENTITY;
        m[9..].copy_from_slice(&t);
        m
    };
    if r.below(2) == 0 {
        let rings = 6 + 2 * r.below(8) as u32;
        let segments = 8 + r.below(20) as u32;
        let radius = r.grid(3.0, 9.0, 0.5);
        Primitive {
            mesh: scaled(&uv_sphere(rings, segments), radius),
            transform,
            sphere_centre: (!tilted).then_some(t),
        }
    } else {
        Primitive {
            mesh: cube(r.grid(3.0, 12.0, 0.5)),
            transform,
            sphere_centre: None,
        }
    }
}

fn apply(m: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i] * p[0] + m[3 + i] * p[1] + m[6 + i] * p[2] + m[9 + i])
}

fn v3(p: [f64; 3]) -> String {
    format!("{},{},{}", p[0], p[1], p[2])
}

/// The planes one scene is cut at (world point, normal): through a
/// sphere's equator ring, through a random vertex (horizontal, along X,
/// and tilted), and through a random face (its own plane).
fn planes(
    r: &mut Rng,
    prims: &[Primitive],
    mesh: &Mesh,
    transform: &[f64; 12],
) -> Vec<([f64; 3], [f64; 3])> {
    let up = [0.0, 0.0, 1.0];
    let mut out = Vec::new();
    for p in prims {
        if let Some(c) = p.sphere_centre {
            out.push((c, up));
        }
    }
    let verts: Vec<u32> = (0..mesh.vertex_slots())
        .filter(|&v| mesh.is_vertex_live(mf_geometry::VertexId(v)))
        .collect();
    let tilts = [
        [1.0, 1.0, 0.0],
        [1.0, 2.0, 3.0],
        [0.0, 1.0, 1.0],
        [-2.0, 1.0, 5.0],
    ];
    for normal in [
        up,
        [1.0, 0.0, 0.0],
        tilts[r.below(tilts.len() as u64) as usize],
    ] {
        let v = verts[r.below(verts.len() as u64) as usize];
        out.push((apply(transform, mesh.corner_position(v)), normal));
    }
    let faces: Vec<_> = mesh.faces().collect();
    let (_, tri) = faces[r.below(faces.len() as u64) as usize];
    let [a, b, c] = tri.map(|v| apply(transform, mesh.corner_position(v)));
    let (u, w) = (
        [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
        [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
    );
    let n = [
        u[1] * w[2] - u[2] * w[1],
        u[2] * w[0] - u[0] * w[2],
        u[0] * w[1] - u[1] * w[0],
    ];
    out.push((a, n));
    out
}

fn session_of(dir: &Path, prims: &[Primitive]) -> (Session, Vec<ObjectId>) {
    let path = dir.join("scene.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: prims
            .iter()
            .enumerate()
            .map(|(k, p)| ModelObject {
                metadata: Default::default(),
                name: format!("p{k}"),
                mesh: p.mesh.clone(),
                transform: p.transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids)
}

/// With `MF_SPLIT_PROPERTY_DUMP` set to a directory, a failing scene's
/// combined input is written there as `<seed>.3mf`, to reproduce it
/// with `meshfixer split-plane`.
fn dump(name: &str, mesh: &Mesh, transform: [f64; 12]) {
    let Ok(dir) = std::env::var("MF_SPLIT_PROPERTY_DUMP") else {
        return;
    };
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "combined".into(),
            mesh: mesh.clone(),
            transform,
        }],
    };
    let path = Path::new(&dir).join(format!("{name}.3mf"));
    write_model_path(&model, &path).unwrap();
    eprintln!("wrote {}", path.display());
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Random closed manifold inputs — unions of spheres and boxes, some
/// tilted, built by `mesh.combine` exactly as the app builds them — cut
/// at planes through vertex rings, single vertices and faces, keeping
/// both, above or below, capped: never refused (only a plane that
/// genuinely leaves nothing on a side, `plane_misses_object`), and every part is
/// closed and manifold ([`assert_closed_manifold`]). Uncapped (both
/// kept, cap off): never refused either, every part manifold and
/// consistently oriented, and open only along the plane.
#[test]
fn random_unions_cut_through_vertices_and_faces_are_never_refused() {
    let cases = env_u64("MF_SPLIT_PROPERTY_CASES", 24);
    let seed0 = env_u64("MF_SPLIT_PROPERTY_SEED", 0x5EED_0001);
    let dir = tempfile::tempdir().unwrap();
    let (mut cuts, mut scenes, mut solid, mut combine_refused) = (0, 0, 0, 0);
    // `MF_SPLIT_PROPERTY_TALLY=1`: carry on past a failure, and count.
    let tally = std::env::var("MF_SPLIT_PROPERTY_TALLY").is_ok();
    let mut failures = 0;
    for case in 0..cases {
        let seed = seed0.wrapping_add(case).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut r = Rng(seed);
        let prims: Vec<Primitive> = (0..2 + r.below(3)).map(|_| primitive(&mut r)).collect();
        let (mut s, ids) = session_of(dir.path(), &prims);
        match s.execute(combine(ids[0], &ids[1..])) {
            Ok(_) => {}
            // A scene `mesh.combine` itself refuses by name (shapes that
            // don't all overlap, or a union its own exact check rejects)
            // isn't an input to cut; the property is about cutting, so
            // draw again — counted, and printed below.
            Err(CommandError::Operation(e)) => {
                if !e.to_string().contains("no_overlap") {
                    eprintln!("seed {seed:#x}: combine refused, scene skipped: {e}");
                }
                combine_refused += 1;
                continue;
            }
            Err(e) => panic!("seed {seed:#x}: combine: {e}"),
        }
        scenes += 1;
        let (mesh, transform) = {
            let o = s.document().object(ids[0]).unwrap();
            (o.mesh().unwrap().clone(), o.transform)
        };
        assert_solid(&mesh, &format!("seed {seed:#x}: the combined input"));
        let mesh_before = mesh.clone();
        for (point, normal) in planes(&mut r, &prims, &mesh, &transform) {
            for (keep, cap) in [
                ("both", true),
                ("above", true),
                ("below", true),
                ("both", false),
            ] {
                let what = format!(
                    "seed {seed:#x}: plane ({}) normal ({}) keep {keep} cap {cap}",
                    v3(point),
                    v3(normal)
                );
                let res = s.execute(split(
                    ids[0],
                    &[],
                    &[
                        ("plane_point", v3(point)),
                        ("plane_normal", v3(normal)),
                        ("keep", keep.into()),
                        ("cap", cap.to_string()),
                    ],
                ));
                match res {
                    Ok(_) => {}
                    // A plane through a hull face (or vertex) can be a
                    // supporting plane, touching the model only within
                    // rounding with all of it on one side: genuinely
                    // nothing to cut, and said so by name.
                    Err(e) if e.to_string().contains("plane_misses_object") => {
                        continue;
                    }
                    Err(e) => {
                        dump(&format!("{seed:x}"), &mesh_before, transform);
                        if tally {
                            eprintln!("FAIL {what}: refused: {e}");
                            failures += 1;
                            continue;
                        }
                        panic!("{what}: refused: {e}")
                    }
                }
                cuts += 1;
                for o in s.document().objects() {
                    let mesh = o.mesh().unwrap();
                    let topo = mesh.topology_with(&RunContext::none()).unwrap();
                    if !cap {
                        assert_open_only_along(mesh, &o.transform, point, normal, &what);
                        continue;
                    }
                    if topo.boundary_edges > 0
                        || !topo.non_manifold_edges.is_empty()
                        || topo.inconsistent_edges > 0
                    {
                        dump(&format!("{seed:x}"), &mesh_before, transform);
                        if tally {
                            eprintln!("FAIL {what}: not closed and manifold");
                            failures += 1;
                            continue;
                        }
                    }
                    assert_closed_manifold(mesh, &format!("{what}: '{}'", o.name));
                    let (p, t) = mesh.to_arrays();
                    if mf_boolean::validate_solid(&p, &t, &RunContext::none()).is_ok() {
                        solid += 1;
                    }
                }
                assert!(s.undo());
            }
        }
    }
    eprintln!(
        "{scenes} scenes ({combine_refused} more refused by combine), {cuts} cuts, {solid} parts \
         also exactly free of crossings"
    );
    assert_eq!(failures, 0, "{failures} failed cuts");
    assert!(
        scenes * 2 >= cases && cuts > scenes * 10,
        "{scenes} scenes, {cuts} cuts"
    );
}
