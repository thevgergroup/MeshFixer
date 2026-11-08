#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::{cube, uv_sphere};
use mf_geometry::Mesh;
use mf_io::{Format, IoError, SourcePrecision, obj, read_path, stl, write_path};

#[test]
fn stl_binary_roundtrip() {
    let bytes = stl::write_binary(&cube(1.0)).unwrap();
    assert_eq!(bytes.len(), 84 + 50 * 12);
    let (m, prec) = stl::read(&bytes).unwrap();
    assert_eq!(m.vertex_count(), 8);
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(prec, SourcePrecision::F32);
    assert_eq!(
        mf_geometry::inspect(&m)
            .signed_volume
            .map(|v| (v - 1.0).abs() < 1e-6),
        Some(true)
    );
}

#[test]
fn stl_ascii_parse() {
    let text = "solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid t\n";
    let (m, _) = stl::read(text.as_bytes()).unwrap();
    assert_eq!(m.triangle_count(), 1);
    assert_eq!(m.to_arrays().0[1], [1.0, 0.0, 0.0]);
}

#[test]
fn stl_ascii_write_parse() {
    let text = stl::write_ascii(&cube(2.0));
    let (m, _) = stl::read(text.as_bytes()).unwrap();
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.vertex_count(), 8);
}

#[test]
fn binary_with_solid_header() {
    let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
    bytes[..5].copy_from_slice(b"solid");
    let (m, _) = stl::read(&bytes).unwrap();
    assert_eq!(m.triangle_count(), 12);
}

#[test]
fn zero_facets() {
    let mut bytes = vec![0u8; 84];
    bytes[..4].copy_from_slice(b"mesh");
    let (m, _) = stl::read(&bytes).unwrap();
    assert!(m.is_empty());
}

#[test]
fn truncated_errors() {
    let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
    bytes[80..84].copy_from_slice(&10_000u32.to_le_bytes());
    bytes.truncate(84 + 50 * 3);
    match stl::read(&bytes) {
        Err(IoError::Truncated { expected, actual }) => {
            assert_eq!(expected, 84 + 50 * 10_000);
            assert_eq!(actual, 84 + 50 * 3);
        }
        other => panic!("expected Truncated, got {other:?}"),
    }
}

#[test]
fn garbage_is_parse_error() {
    assert!(matches!(
        stl::read(b"solid x\n facet normal nope\n"),
        Err(IoError::Parse { .. })
    ));
    assert!(matches!(
        stl::read(b"abc"),
        Err(IoError::Truncated { .. }) | Err(IoError::Parse { .. })
    ));
}

#[test]
fn obj_roundtrip_precision() {
    let src = "v 0.123456789012345 1 2\nv 3 4 5\nv 6 7 8.000000000000001\nf 1 2 3\n";
    let (m, prec, _) = obj::read(src).unwrap();
    assert_eq!(
        prec,
        SourcePrecision::Decimal {
            max_significant_digits: 16
        }
    );
    let out = obj::write(&m);
    let (m2, _, _) = obj::read(&out).unwrap();
    assert_eq!(m2.to_arrays().0, m.to_arrays().0);
    assert_eq!(m.to_arrays().0[0][0], 0.123456789012345);
}

#[test]
fn relative_and_slash_indices() {
    let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvn 0 0 1\nf -3/1/1 -2/1/1 -1/1/1\nf 1//1 2//1 3//1\nf 1/1 2/1 3/1\n";
    let (m, _, warnings) = obj::read(src).unwrap();
    assert_eq!(m.triangle_count(), 3);
    assert!(m.to_arrays().1.iter().all(|t| *t == [0, 1, 2]));
    assert_eq!(warnings.len(), 2, "{warnings:?}"); // one each for vt, vn
}

#[test]
fn out_of_range_index_errors() {
    match obj::read("v 0 0 0\nv 1 0 0\nf 1 2 9\n") {
        Err(IoError::Parse { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected Parse error, got {other:?}"),
    }
}

#[test]
fn quad_triangulated() {
    let (m, _, _) = obj::read("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n").unwrap();
    assert_eq!(m.to_arrays().1, vec![[0, 1, 2], [0, 2, 3]]);
}

#[test]
fn obj_vertices_only_is_empty_mesh() {
    let (m, _, _) = obj::read("v 0 0 0\nv 1 0 0\n").unwrap();
    assert!(m.is_empty());
    assert_eq!(m.vertex_count(), 2);
}

#[test]
fn read_write_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = uv_sphere(8, 16);
    for (name, fmt) in [("a.stl", Format::StlBinary), ("b.obj", Format::Obj)] {
        let p = dir.path().join(name);
        write_path(&mesh, &p, fmt).unwrap();
        let imp = read_path(&p).unwrap();
        assert_eq!(imp.mesh.triangle_count(), mesh.triangle_count());
        assert_eq!(imp.source.format, fmt);
        assert_eq!(
            imp.source.content_hash.0,
            *blake3::hash(&std::fs::read(&p).unwrap()).as_bytes()
        );
    }
    assert!(matches!(
        read_path(&dir.path().join("x.xyz")),
        Err(IoError::UnsupportedFormat(_))
    ));
    assert_eq!(Format::from_path("A.STL".as_ref()), Some(Format::StlBinary));
    let _ = Mesh::empty();
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn failed_export_leaves_existing_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("scan.stl");
    std::fs::write(&p, b"ORIGINAL").unwrap();
    // 1e300 doesn't fit in f32: a binary STL of this would contain inf.
    let bad = Mesh::new(
        vec![[0.0; 3], [1e300, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 2]],
    )
    .unwrap();
    let err = write_path(&bad, &p, Format::StlBinary).unwrap_err();
    assert!(matches!(err, IoError::Export { .. }), "{err:?}");
    assert_eq!(std::fs::read(&p).unwrap(), b"ORIGINAL");
    assert_eq!(
        files_in(dir.path()),
        vec!["scan.stl"],
        "no temp files left behind"
    );
}

#[test]
fn export_replaces_existing_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.obj");
    std::fs::write(&p, b"OLD").unwrap();
    write_path(&cube(1.0), &p, Format::Obj).unwrap();
    assert_eq!(read_path(&p).unwrap().mesh.triangle_count(), 12);
    assert_eq!(files_in(dir.path()), vec!["out.obj"]);
}

#[test]
fn stl_weld_identical_at_1_and_8_threads() {
    let mesh = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(50_000, 4));
    let bytes = stl::write_binary(&mesh).unwrap();
    let run = |threads| {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        pool.install(|| stl::read(&bytes).unwrap().0.content_hash())
    };
    assert_eq!(run(1), run(8));
}

#[test]
fn export_skips_tombstoned_faces() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = cube(1.0).edit();
    e.remove_face(mf_geometry::FaceId(2)).unwrap();
    e.remove_face(mf_geometry::FaceId(3)).unwrap();
    let (open, _) = e.finish().unwrap();
    for (name, fmt) in [("t.stl", Format::StlBinary), ("t.obj", Format::Obj), ("a.stl", Format::StlAscii)] {
        let p = dir.path().join(name);
        write_path(&open, &p, fmt).unwrap();
        let back = read_path(&p).unwrap().mesh;
        assert_eq!(back.triangle_count(), 10, "{name}");
        assert_eq!(mf_geometry::inspect(&back).boundary_loops.len(), 1, "{name}");
    }
}
