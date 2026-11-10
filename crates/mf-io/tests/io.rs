#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::{cube, uv_sphere};
use mf_geometry::Mesh;
use mf_io::{Format, IoError, SourcePrecision, obj, read_model, read_path, stl, write_path};

#[test]
fn stl_binary_roundtrip() {
    let bytes = stl::write_binary(&cube(1.0)).unwrap();
    assert_eq!(bytes.len(), 84 + 50 * 12);
    let (m, prec, _) = stl::read(&bytes).unwrap();
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
    let (m, _, _) = stl::read(text.as_bytes()).unwrap();
    assert_eq!(m.triangle_count(), 1);
    assert_eq!(m.to_arrays().0[1], [1.0, 0.0, 0.0]);
}

#[test]
fn stl_ascii_write_parse() {
    let text = stl::write_ascii(&cube(2.0));
    let (m, _, _) = stl::read(text.as_bytes()).unwrap();
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.vertex_count(), 8);
}

#[test]
fn binary_with_solid_header() {
    let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
    bytes[..5].copy_from_slice(b"solid");
    let (m, _, _) = stl::read(&bytes).unwrap();
    assert_eq!(m.triangle_count(), 12);
}

#[test]
fn zero_facets() {
    let mut bytes = vec![0u8; 84];
    bytes[..4].copy_from_slice(b"mesh");
    let (m, _, _) = stl::read(&bytes).unwrap();
    assert!(m.is_empty());
}

#[test]
fn truncated_errors() {
    let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
    bytes[80..84].copy_from_slice(&10_000u32.to_le_bytes());
    bytes.truncate(84 + 50 * 3);
    match stl::read(&bytes) {
        Err(IoError::Truncated {
            expected, actual, ..
        }) => {
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
        write_path(&mesh, &p, fmt, mf_io::threemf::Unit::Millimeter).unwrap();
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
    let err = write_path(
        &bad,
        &p,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap_err();
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
    write_path(
        &cube(1.0),
        &p,
        Format::Obj,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    assert_eq!(read_path(&p).unwrap().mesh.triangle_count(), 12);
    assert_eq!(files_in(dir.path()), vec!["out.obj"]);
}

#[test]
fn stl_weld_identical_at_1_and_8_threads() {
    let mesh = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(50_000, 4));
    let bytes = stl::write_binary(&mesh).unwrap();
    let run = |threads| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
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
    for (name, fmt) in [
        ("t.stl", Format::StlBinary),
        ("t.obj", Format::Obj),
        ("a.stl", Format::StlAscii),
    ] {
        let p = dir.path().join(name);
        write_path(&open, &p, fmt, mf_io::threemf::Unit::Millimeter).unwrap();
        let back = read_path(&p).unwrap().mesh;
        assert_eq!(back.triangle_count(), 10, "{name}");
        assert_eq!(
            mf_geometry::inspect(&back).boundary_loops.len(),
            1,
            "{name}"
        );
    }
}

mod stl_header_cases {
    use super::*;

    fn with_count(n: u32) -> Vec<u8> {
        let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
        bytes[80..84].copy_from_slice(&n.to_le_bytes());
        bytes
    }

    #[test]
    fn zero_facet_count_reads_by_size_with_warning() {
        let (m, _, warnings) = stl::read(&with_count(0)).unwrap();
        assert_eq!(m.triangle_count(), 12);
        assert!(
            warnings.iter().any(|w| w.contains("facet count is 0")),
            "{warnings:?}"
        );
    }

    #[test]
    fn trailing_bytes_are_ignored_with_warning() {
        let mut bytes = stl::write_binary(&cube(1.0)).unwrap();
        bytes.extend_from_slice(&[0u8; 7]);
        let (m, _, warnings) = stl::read(&bytes).unwrap();
        assert_eq!(m.triangle_count(), 12);
        assert!(
            warnings.iter().any(|w| w.contains("7 trailing bytes")),
            "{warnings:?}"
        );
    }

    #[test]
    fn shorter_than_header_claims_is_an_error_naming_the_ambiguity() {
        let mut bytes = with_count(20);
        bytes.truncate(84 + 50 * 12);
        let err = stl::read(&bytes).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, IoError::Truncated { .. }), "{err:?}");
        assert!(msg.contains("20 facets") && msg.contains("12"), "{msg}");
    }

    #[test]
    fn solid_header_with_size_mismatch_names_the_ambiguity() {
        let mut bytes = with_count(20);
        bytes[..5].copy_from_slice(b"solid");
        bytes.push(0xFF); // not UTF-8, not a valid binary size either
        let msg = stl::read(&bytes).unwrap_err().to_string();
        assert!(msg.contains("binary STL") && msg.contains("solid"), "{msg}");
        assert!(!msg.contains("UTF-8"), "{msg}");
    }
}

/// MF-ISSUE-035: STL/OBJ export applies an object's transform (positions in
/// f64, winding flipped for a mirrored transform so normals stay outward),
/// and `write_path(ThreeMf)` takes the document's unit instead of hard-coding
/// millimetres.
#[test]
fn export_applies_the_objects_transform_and_threemf_takes_the_document_unit() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = cube(1.0);

    // A rotation + translation: STL export must write the transformed
    // positions, not the object-space ones.
    let transform = mf_fixtures::rotation_translation(2, 25.0, [10.0, -2.0, 3.0]);
    let moved = mf_io::transform_mesh(&mesh, &transform);
    let p = dir.path().join("moved.stl");
    mf_io::write_path(
        &moved,
        &p,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let back = read_path(&p).unwrap().mesh;
    let mut back_pos = back.to_arrays().0;
    let mut want_pos = moved.to_arrays().0;
    back_pos.sort_by(|a, b| a.partial_cmp(b).unwrap());
    want_pos.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for (b, w) in back_pos.iter().zip(&want_pos) {
        for i in 0..3 {
            // Binary STL stores f32: tolerance matches that precision, not
            // f64 exactness.
            assert!((b[i] - w[i]).abs() < 1e-5, "{b:?} vs {w:?}");
        }
    }
    // The transform actually moved it: object-space positions aren't a match.
    let mut orig_pos = mesh.to_arrays().0;
    orig_pos.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_ne!(back_pos, orig_pos);

    // A mirrored transform (negative determinant) flips winding so normals
    // stay outward: the transformed cube's signed volume stays positive.
    let mirror: [f64; 12] = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let mirrored = mf_io::transform_mesh(&mesh, &mirror);
    let vol = mf_geometry::inspect(&mirrored).signed_volume.unwrap();
    assert!(vol > 0.0, "mirrored mesh's winding wasn't flipped: {vol}");

    // `write_path(ThreeMf)` takes the document's unit, not a hard-coded mm.
    let p3 = dir.path().join("out.3mf");
    mf_io::write_path(&mesh, &p3, Format::ThreeMf, mf_io::threemf::Unit::Inch).unwrap();
    let model = read_model(&p3).unwrap();
    assert_eq!(model.unit, mf_io::threemf::Unit::Inch);
}

/// A cancelled export writes nothing: no file at the path (an existing one
/// is unchanged) and no temp file (MF-RT-002).
#[test]
fn cancelled_export_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.stl");
    let mesh = mf_fixtures::cube(1.0);
    let ctx = mf_runtime::RunContext::none();
    ctx.cancel.cancel();
    for format in [Format::StlBinary, Format::Obj, Format::ThreeMf] {
        let r =
            mf_io::write_path_with(&mesh, &path, format, mf_io::threemf::Unit::Millimeter, &ctx);
        assert!(matches!(r, Err(IoError::Cancelled)), "{format:?}: {r:?}");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    // An existing file is left as it was.
    std::fs::write(&path, b"keep").unwrap();
    let r = mf_io::write_path_with(
        &mesh,
        &path,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
        &ctx,
    );
    assert!(matches!(r, Err(IoError::Cancelled)));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    // Not cancelled: written, and the progress ends at "writing" 1.0.
    let ctx = mf_runtime::RunContext::none();
    mf_io::write_path_with(
        &mesh,
        &path,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
        &ctx,
    )
    .unwrap();
    assert_eq!(ctx.progress.snapshot(), (mf_io::WRITING, 1.0));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
