#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Golden topology digests (M4 Task 9 review I1). The topology build was
//! rewritten for memory (MF-ISSUE-019); these digests pin its output to the
//! previous implementation's on 92 meshes chosen to hit every branch:
//! damaged spheres, the plate and demo scene, random triangle soups (heavily
//! non-manifold, with collapsed and duplicate faces and pinched vertices),
//! grids with duplicated, flipped, fin and collapsed faces, tombstoned
//! meshes, and a vertex shared by many fans.
//!
//! A digest is the first 16 hex digits of the BLAKE3 hash of the
//! topology's `Debug` text, so it covers every field. How the goldens were
//! produced: `topology.rs` from commit d2acada (the 16-byte half-edge sort
//! with corner union-find, before Task 9) was checked out temporarily and
//! `cargo test -p mf-geometry --test topology_golden -- --ignored
//! --nocapture print_topology_digests` printed the table below. A change to
//! `Topology`'s fields changes every digest: regenerate them the same way
//! from the implementation being replaced, never from the new one.

use mf_fixtures::*;
use mf_geometry::{Mesh, Topology};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random triangles over a few vertices: non-manifold edges and vertices,
/// collapsed and duplicate faces, pinched boundaries.
fn soup(seed: u64, verts: u32, tris: usize) -> Mesh {
    let mut r = Rng(seed);
    let positions: Vec<[f64; 3]> = (0..verts)
        .map(|_| [0, 1, 2].map(|_| r.below(1000) as f64))
        .collect();
    let triangles: Vec<[u32; 3]> = (0..tris)
        .map(|_| [0, 1, 2].map(|_| r.below(verts as u64) as u32))
        .collect();
    Mesh::new(positions, triangles).unwrap()
}

/// Remove about one face in `every`.
fn tombstone(m: &Mesh, seed: u64, every: u64) -> Mesh {
    let mut r = Rng(seed);
    let mut e = m.edit();
    for (f, _) in m.faces() {
        if r.below(every) == 0 {
            e.remove_face(f).unwrap();
        }
    }
    e.finish().unwrap().0
}

/// A grid with faces duplicated, flipped, fins and collapsed faces added.
fn grid_soup(seed: u64) -> Mesh {
    let (p, mut t) = grid(30).to_arrays();
    let mut r = Rng(seed);
    let n = t.len();
    for _ in 0..200 {
        let f = t[r.below(n as u64) as usize];
        match r.below(4) {
            0 => t.push(f),
            1 => t.push([f[0], f[2], f[1]]),
            2 => t.push([f[0], f[1], r.below(p.len() as u64) as u32]),
            _ => t.push([f[0], f[0], f[1]]),
        }
    }
    Mesh::new(p, t).unwrap()
}

/// Eight separate triangles sharing one hub vertex: eight fans.
fn star() -> Mesh {
    let mut p = vec![[0.0, 0.0, 0.0]];
    let mut t = Vec::new();
    for k in 0..8u32 {
        let a = k as f64;
        p.push([a.cos(), a.sin(), 0.0]);
        p.push([(a + 0.3).cos(), (a + 0.3).sin(), 0.1 * a]);
        t.push([0, 1 + 2 * k, 2 + 2 * k]);
    }
    Mesh::new(p, t).unwrap()
}

fn meshes() -> Vec<(String, Mesh)> {
    let mut meshes: Vec<(String, Mesh)> = Vec::new();
    for s in 1..4 {
        meshes.push((
            format!("damaged{s}"),
            damaged_sphere(&DamageSpec::for_triangles(200_000, s)),
        ));
    }
    meshes.push(("plate".into(), plate_with_defects()));
    for (name, m, _) in demo_scene() {
        meshes.push((name, m));
    }
    meshes.push(("cube_missing".into(), cube_with_missing_face()));
    meshes.push(("tet".into(), tetrahedron()));
    meshes.push(("flipped".into(), flipped_tetrahedron()));
    meshes.push((
        "merge".into(),
        merge(&[cube(1.0), flipped_tetrahedron(), tetrahedron()]),
    ));
    meshes.push(("sphere".into(), uv_sphere(20, 40)));
    for s in 0..40 {
        let verts = 10 + (s as u32 % 7) * 10;
        meshes.push((format!("soup{s}"), soup(s, verts, 50 + 20 * s as usize)));
    }
    for s in 0..20 {
        meshes.push((format!("gridsoup{s}"), grid_soup(s)));
    }
    for s in 0..10 {
        let base = damaged_sphere(&DamageSpec::for_triangles(20_000, s));
        meshes.push((format!("tomb{s}"), tombstone(&base, s, 3 + s)));
        meshes.push((format!("gridtomb{s}"), tombstone(&grid(40), s + 100, 2 + s)));
    }
    meshes.push(("star".into(), star()));
    meshes
}

fn digest(topo: &Topology) -> String {
    blake3::hash(format!("{topo:?}").as_bytes()).to_hex()[..16].to_owned()
}

/// Prints the golden table (see the module comment for when to run it).
#[test]
#[ignore = "regenerates the goldens; run against the implementation being replaced"]
fn print_topology_digests() {
    for (name, m) in meshes() {
        println!("    (\"{name}\", \"{}\"),", digest(&Topology::compute(&m)));
    }
}

#[test]
fn topology_matches_the_previous_implementation_on_92_meshes() {
    let meshes = meshes();
    assert_eq!(meshes.len(), GOLDEN.len());
    let mut wrong = Vec::new();
    for ((name, m), (golden_name, golden)) in meshes.iter().zip(GOLDEN) {
        assert_eq!(name, golden_name);
        let d = digest(&Topology::compute(m));
        if d != *golden {
            wrong.push(format!("{name}: {d} != {golden}"));
        }
    }
    assert!(wrong.is_empty(), "topology changed on: {wrong:#?}");
}

/// Produced by the pre-Task-9 implementation (d2acada); see the module comment.
const GOLDEN: &[(&str, &str)] = &[
    ("damaged1", "a8630c020edfad4d"),
    ("damaged2", "6372cbff5ab164f6"),
    ("damaged3", "2b7745f13d08600a"),
    ("plate", "c62774821dbdde17"),
    ("scan", "4d400b1a60ee1deb"),
    ("plate", "c62774821dbdde17"),
    ("cube_missing", "df740a1cc588d1f3"),
    ("tet", "e09462cdf4768e26"),
    ("flipped", "0c578809c97a5290"),
    ("merge", "9e7070660c55390b"),
    ("sphere", "8349ab093db5d6f4"),
    ("soup0", "ebfbe5a385c6837e"),
    ("soup1", "629b9f7a2c7a9463"),
    ("soup2", "80ebd4988a54757b"),
    ("soup3", "715bc6cbd451c0c3"),
    ("soup4", "ec8b40156a011081"),
    ("soup5", "b1107fbb58e5784b"),
    ("soup6", "0fdd2e094524a1a3"),
    ("soup7", "bf3d6a4e8bbd2ef2"),
    ("soup8", "b8a40fe9f7aba617"),
    ("soup9", "ea6841119bc2bf59"),
    ("soup10", "b462e96a637a20c9"),
    ("soup11", "845aaa1a7debe066"),
    ("soup12", "36375bdf945332e2"),
    ("soup13", "3cb592034cd4fbc9"),
    ("soup14", "1f9f89fe716dcdbf"),
    ("soup15", "af76e52b2fe246fc"),
    ("soup16", "944f13dde488cfe0"),
    ("soup17", "9f8fb7a1f6f4cc13"),
    ("soup18", "0b640bfacc951acf"),
    ("soup19", "c7b8b862dccc0e0a"),
    ("soup20", "534898d8d42ec582"),
    ("soup21", "d2cdf19bd94b2ab8"),
    ("soup22", "d4fe0afb3c46cc75"),
    ("soup23", "2dc521ef88ebaf52"),
    ("soup24", "46da772455518816"),
    ("soup25", "ec7be23a7e87cd10"),
    ("soup26", "fd4b472da62bbd67"),
    ("soup27", "33b62dca19c3a6d7"),
    ("soup28", "301a3674b1e72947"),
    ("soup29", "76f12db102563832"),
    ("soup30", "00dbec1e7d4954f2"),
    ("soup31", "b19493c060e5ce32"),
    ("soup32", "ac197ec519817d6e"),
    ("soup33", "248514a5e077fb39"),
    ("soup34", "011fcd5a1ea9b1ee"),
    ("soup35", "af0f29aa8fab6951"),
    ("soup36", "bf8caf9a715abb6e"),
    ("soup37", "2de3d370e3b76df4"),
    ("soup38", "3ec37b22751af0c2"),
    ("soup39", "4e258ec37a4e078d"),
    ("gridsoup0", "fb8fe01a119fa4eb"),
    ("gridsoup1", "183e9959f067deeb"),
    ("gridsoup2", "66274648a7d29e61"),
    ("gridsoup3", "22ea8df507a1ea6c"),
    ("gridsoup4", "afe05ebcd90fe34a"),
    ("gridsoup5", "7c7fbac250d897fb"),
    ("gridsoup6", "a9c37b2cdad4ed5d"),
    ("gridsoup7", "d1614a90288fee34"),
    ("gridsoup8", "2313c15c4f86ae90"),
    ("gridsoup9", "5ec9f5faf2364746"),
    ("gridsoup10", "ee05a620d4a3da63"),
    ("gridsoup11", "e0383e75bd13562b"),
    ("gridsoup12", "a5b6d01a2d2257be"),
    ("gridsoup13", "ca97f1c5f579729e"),
    ("gridsoup14", "0baaf7286d782d6f"),
    ("gridsoup15", "a1d384d7d545d6b8"),
    ("gridsoup16", "821c63f8a5b4d543"),
    ("gridsoup17", "cce3dd330a5afee8"),
    ("gridsoup18", "676b47378f29efaa"),
    ("gridsoup19", "2295e6c403c7ac13"),
    ("tomb0", "4a82a9fb729d6731"),
    ("gridtomb0", "30905215659b9232"),
    ("tomb1", "41ae6bafa8d9bd70"),
    ("gridtomb1", "99f27b6ad10bedcb"),
    ("tomb2", "627dda88480807ff"),
    ("gridtomb2", "0f5afb040be979b8"),
    ("tomb3", "a0900863c7a81aab"),
    ("gridtomb3", "f384eec4f0d005ba"),
    ("tomb4", "870572876ca90b0d"),
    ("gridtomb4", "9e3233d8069885dd"),
    ("tomb5", "1aa14dfa2438cfad"),
    ("gridtomb5", "cfe02886d518d51c"),
    ("tomb6", "becf6c8d2db18c8b"),
    ("gridtomb6", "9c7602a4ad8cf458"),
    ("tomb7", "28dbf146c4f82efa"),
    ("gridtomb7", "ea024b684ed07663"),
    ("tomb8", "192e04d16ee205fe"),
    ("gridtomb8", "60c2c9749426cf87"),
    ("tomb9", "e3936ce1135a2d88"),
    ("gridtomb9", "ef9a4fb5f49e7ee1"),
    ("star", "5adaaa080949ab05"),
];
