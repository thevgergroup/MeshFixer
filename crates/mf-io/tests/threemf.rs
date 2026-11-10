#![allow(clippy::unwrap_used)] // tests may panic on failure

//! 3MF core: objects, build-item transforms, units (MF-IO-001, MF-DOC-004).

use mf_fixtures::{cube, tetrahedron, uv_sphere};
use mf_geometry::FaceId;
use mf_io::threemf::{self, Model, ModelObject, Unit};
use mf_io::{Format, IoError, read_model, write_model_path, write_path};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn obj(name: &str, mesh: mf_geometry::Mesh, transform: [f64; 12]) -> ModelObject {
    ModelObject {
        metadata: Default::default(),
        name: name.into(),
        mesh,
        transform,
    }
}

#[test]
fn threemf_roundtrip_three_objects() {
    let shifted = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 10.0, -2.5, 3.0];
    let model = Model {
        unit: Unit::Inch,
        objects: vec![
            obj("box", cube(1.0), IDENTITY),
            obj("pyramid", tetrahedron(), shifted),
            obj("ball", uv_sphere(8, 16), IDENTITY),
        ],
    };
    let bytes = threemf::write(&model).unwrap();
    let (back, warnings) = threemf::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.unit, Unit::Inch);
    assert_eq!(back.objects.len(), 3);
    for (a, b) in model.objects.iter().zip(&back.objects) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.transform, b.transform);
        assert_eq!(a.mesh.to_arrays(), b.mesh.to_arrays(), "{}", a.name);
    }
}

fn zip_model(model_xml: &str, with_rels: bool) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let opt = zip::write::SimpleFileOptions::default();
    z.start_file("[Content_Types].xml", opt).unwrap();
    z.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#).unwrap();
    if with_rels {
        z.start_file("_rels/.rels", opt).unwrap();
        z.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#).unwrap();
    }
    z.start_file("3D/3dmodel.model", opt).unwrap();
    z.write_all(model_xml.as_bytes()).unwrap();
    z.finish().unwrap();
    buf.into_inner()
}

const TRI: &str = r#"<mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh>"#;

#[test]
fn threemf_edge_cases() {
    // No unit attribute means millimetre; a non-identity transform is kept.
    let xml = format!(
        r#"<?xml version="1.0"?><model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model">{TRI}</object><object id="2" type="model"><mesh><vertices/><triangles/></mesh></object></resources><build><item objectid="1" transform="2 0 0 0 2 0 0 0 2 5 6 7"/><item objectid="2"/></build></model>"#
    );
    let (m, warnings) = threemf::read(&zip_model(&xml, true)).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(m.unit, Unit::Millimeter);
    assert_eq!(
        m.objects[0].transform,
        [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 5.0, 6.0, 7.0]
    );
    assert_eq!(m.objects[0].mesh.triangle_count(), 1);
    assert!(
        m.objects[1].mesh.is_empty(),
        "an empty mesh object is allowed"
    );

    // Unsupported <components> produce a warning, never silent loss.
    let xml = format!(
        r#"<?xml version="1.0"?><model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model">{TRI}</object><object id="2" type="model"><components><component objectid="1"/></components></object></resources><build><item objectid="1"/><item objectid="2"/></build></model>"#
    );
    let (m, warnings) = threemf::read(&zip_model(&xml, true)).unwrap();
    assert_eq!(m.objects.len(), 1);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("components") && w.contains("object 2")),
        "{warnings:?}"
    );

    // A missing _rels/.rels falls back to the standard model path, with a warning.
    let (_, warnings) = threemf::read(&zip_model(&xml, false)).unwrap();
    assert!(warnings.iter().any(|w| w.contains(".rels")), "{warnings:?}");
}

#[test]
fn threemf_rejects_malformed_xml_with_location() {
    let xml = r#"<?xml version="1.0"?>
<model unit="millimeter">
  <resources>
    <object id="1" type="model"><mesh><vertices><vertex x="0" y="zero" z="0"/></vertices></mesh></object>
  </resources>
</model>"#;
    match threemf::read(&zip_model(xml, true)) {
        Err(IoError::Parse { line, message }) => {
            assert_eq!(line, 4, "{message}");
            assert!(message.contains("zero"), "{message}");
        }
        other => panic!("{:?}", other.map(|(m, _)| m.objects.len())),
    }
    assert!(matches!(
        threemf::read(b"not a zip"),
        Err(IoError::Parse { .. })
    ));
}

#[test]
fn export_3mf_skips_tombstones_and_round_trips_through_read_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = cube(1.0).edit();
    e.remove_face(FaceId(2)).unwrap();
    let (m, _) = e.finish().unwrap();
    let p = dir.path().join("part.3mf");
    write_path(&m, &p, Format::ThreeMf, Unit::Millimeter).unwrap();
    let model = read_model(&p).unwrap();
    assert_eq!(model.objects.len(), 1);
    assert_eq!(model.objects[0].mesh.triangle_count(), 11);
    assert_eq!(model.source.format, Format::ThreeMf);
    assert_eq!(Format::from_path("X.3MF".as_ref()), Some(Format::ThreeMf));
}

/// MF-ISSUE-033: `write_model_path` catches a corrupted export even when
/// face count and vertex positions still match (different triangle
/// indices, or a swapped object name), not just counts and positions.
/// `cube_with_rewound_face` has the same vertex positions and the same
/// face count as `cube(1.0)`, but triangle 0's indices are permuted
/// (`[a,b,c]` -> `[b,c,a]`): the old position/count-only check can't see
/// this, since the *set* of positions used by every triangle is unchanged.
#[test]
fn export_verification_compares_triangle_indices_and_object_names() {
    let (positions, mut triangles) = cube(1.0).to_arrays();
    // Rotate triangle 0's indices: same 3 positions, different winding/order.
    let [a, b, c] = triangles[0];
    triangles[0] = [b, c, a];
    let rewound = mf_geometry::Mesh::new(positions, triangles).unwrap();

    // Sanity check the fixture actually isolates the "indices differ, but
    // positions-as-a-set and face count agree" case this issue is about.
    assert_eq!(rewound.face_count(), cube(1.0).face_count());
    let mut a_pos = cube(1.0).to_arrays().0;
    let mut b_pos = rewound.to_arrays().0;
    a_pos.sort_by(|x, y| x.partial_cmp(y).unwrap());
    b_pos.sort_by(|x, y| x.partial_cmp(y).unwrap());
    assert_eq!(a_pos, b_pos, "fixture must keep the same position set");
    assert_ne!(
        cube(1.0).to_arrays().1,
        rewound.to_arrays().1,
        "fixture must actually change the triangle indices"
    );

    // `write_model_path`'s own round-trip (write then read back what it
    // just wrote) can't reproduce a disk-level corruption directly, so this
    // exercises the comparison `write_model_path_with` runs internally by
    // round-tripping through `threemf::write`/`read` the same way it does,
    // and checking the comparison it must perform would reject a model
    // whose indices (or name) changed between what was asked for and what
    // comes back.
    let asked = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("part", cube(1.0), IDENTITY)],
    };
    let got_indices = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("part", rewound, IDENTITY)],
    };
    let got_name = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("other", cube(1.0), IDENTITY)],
    };
    assert!(
        !mf_io::models_match_for_verification(&asked, &got_indices),
        "different triangle indices must be caught"
    );
    assert!(
        !mf_io::models_match_for_verification(&asked, &got_name),
        "a different object name must be caught"
    );
    assert!(mf_io::models_match_for_verification(
        &asked,
        &Model {
            unit: Unit::Millimeter,
            objects: vec![obj("part", cube(1.0), IDENTITY)],
        }
    ));

    // End-to-end: a normal export still succeeds and round-trips exactly.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.3mf");
    write_model_path(&asked, &p).unwrap();
    let (back, _) = threemf::read(&std::fs::read(&p).unwrap()).unwrap();
    assert_eq!(back.objects[0].mesh.to_arrays(), cube(1.0).to_arrays());
    assert_eq!(back.objects[0].name, "part");
}

#[test]
fn malformed_attribute_is_an_error_not_a_default() {
    // A broken entity in `unit` must not silently fall back to millimetres.
    let xml = format!(
        r#"<?xml version="1.0"?><model unit="inch&bogus;" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model">{TRI}</object></resources><build><item objectid="1"/></build></model>"#
    );
    match threemf::read(&zip_model(&xml, true)) {
        Err(IoError::Parse { message, .. }) => assert!(message.contains("unit"), "{message}"),
        other => panic!(
            "expected a parse error, got {:?}",
            other.map(|(m, _)| m.unit)
        ),
    }
}

#[test]
fn threemf_read_is_linear_in_file_size() {
    // Locating each XML event by counting newlines from the start of the
    // file made reading quadratic: minutes for a 200k-triangle export.
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("sphere", uv_sphere(150, 300), threemf::IDENTITY)],
    };
    let bytes = threemf::write(&model).unwrap();
    let start = std::time::Instant::now();
    let (back, _) = threemf::read(&bytes).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(
        back.objects[0].mesh.face_count(),
        model.objects[0].mesh.face_count()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "reading {} triangles took {elapsed:?}",
        model.objects[0].mesh.face_count()
    );
}

#[test]
fn threemf_extensions_and_properties_are_warned_never_silently_lost() {
    let xml = format!(
        r##"<?xml version="1.0"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
       xmlns:s="http://schemas.microsoft.com/3dmanufacturing/slice/2015/07"
       requiredextensions="s">
  <resources>
    <basematerials id="5"><base name="red" displaycolor="#FF0000"/></basematerials>
    <s:slicestack id="9"><s:slice ztop="1"><s:vertices><s:vertex x="0" y="0"/></s:vertices></s:slice></s:slicestack>
    <object id="1" type="model" pid="5" pindex="0"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2" pid="5" p1="0"/></triangles></mesh></object>
    <object id="2" type="support">{TRI}</object>
  </resources>
  <build><item objectid="1"/><item objectid="2"/></build>
</model>"##
    );
    let (m, warnings) = threemf::read(&zip_model(&xml, true)).unwrap();
    assert_eq!(m.objects.len(), 2, "core geometry still imports");
    assert_eq!(m.objects[0].mesh.triangle_count(), 1);
    let has = |needle: &str| warnings.iter().any(|w| w.contains(needle));
    assert!(has("requires extension"), "{warnings:?}");
    assert!(
        has("s:slicestack"),
        "non-core elements are named: {warnings:?}"
    );
    // Materials import now (MF-ISSUE-091): no "ignored" warning.
    assert!(
        !has("ignored; MeshFixer imports geometry only"),
        "{warnings:?}"
    );
    assert_eq!(m.objects[0].mesh.materials()[0].name, "red");
    assert_eq!(
        m.objects[0].mesh.face_material(mf_geometry::FaceId(0)),
        Some(0)
    );
    assert!(has("object 2") && has("support"), "{warnings:?}");
}

/// MF-ISSUE-091: base materials and the Materials extension's colour
/// groups import as per-face materials (object default plus per-triangle
/// overrides), and survive an export and re-import.
#[test]
fn threemf_materials_and_colours_round_trip() {
    let xml = r##"<?xml version="1.0"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
       xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02">
  <resources>
    <basematerials id="5"><base name="PLA white" displaycolor="#FFFFFF"/><base name="PLA red" displaycolor="#FF0000"/></basematerials>
    <m:colorgroup id="6"><m:color color="#00FF0080"/><m:color color="#0000FF"/></m:colorgroup>
    <object id="1" type="model" pid="5" pindex="0"><mesh>
      <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/></vertices>
      <triangles>
        <triangle v1="0" v2="2" v3="1"/>
        <triangle v1="0" v2="1" v3="3" p1="1"/>
        <triangle v1="1" v2="2" v3="3" pid="6" p1="0"/>
        <triangle v1="0" v2="3" v3="2" pid="6" p1="1" p2="0"/>
      </triangles></mesh></object>
  </resources>
  <build><item objectid="1"/></build>
</model>"##;
    let (m, warnings) = threemf::read(&zip_model(xml, true)).unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("corner")),
        "{warnings:?}"
    );
    let mesh = &m.objects[0].mesh;
    let names: Vec<(String, [u8; 4])> = (0..4)
        .map(|f| {
            let mat =
                &mesh.materials()[mesh.face_material(mf_geometry::FaceId(f)).unwrap() as usize];
            (mat.name.clone(), mat.color)
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("PLA white".into(), [255, 255, 255, 255]),
            ("PLA red".into(), [255, 0, 0, 255]),
            (String::new(), [0, 255, 0, 128]),
            (String::new(), [0, 0, 255, 255]),
        ]
    );
    assert_eq!(mesh.default_material(), Some(0));
    // Export and read back: the same colour on every face.
    let bytes = threemf::write(&m).unwrap();
    let (back, warnings) = threemf::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let b = &back.objects[0].mesh;
    for f in 0..4 {
        let f = mf_geometry::FaceId(f);
        let color =
            |x: &mf_geometry::Mesh| x.materials()[x.face_material(f).unwrap() as usize].color;
        assert_eq!(color(b), color(mesh), "{f:?}");
    }
    // A colour-free model still writes no properties at all.
    let plain = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("plain", cube(1.0), IDENTITY)],
    };
    let (plain_back, _) = threemf::read(&threemf::write(&plain).unwrap()).unwrap();
    assert!(!plain_back.objects[0].mesh.has_materials());
    assert_eq!(
        plain_back.objects[0].mesh.content_hash(),
        cube(1.0).content_hash(),
        "no materials: the same hash as before MF-ISSUE-091"
    );
    // A missing property group is a named error, not a silent default.
    let bad = xml.replace(r#"pid="6" p1="0""#, r#"pid="9" p1="0""#);
    assert!(threemf::read(&zip_model(&bad, true)).is_err());
}

/// MF-ISSUE-034: an empty mesh object (no vertices/triangles) is left out of
/// a 3MF export, with a warning naming it, instead of being written as an
/// invalid 3MF object (the core schema needs at least 3 vertices and 1
/// triangle).
#[test]
fn empty_objects_are_left_out_of_the_export_with_a_warning_naming_them() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            obj("solid", cube(1.0), IDENTITY),
            obj("hollow", mf_geometry::Mesh::empty(), IDENTITY),
        ],
    };
    let warnings = threemf::write_checked(&model, &p).unwrap();
    assert!(
        warnings.iter().any(|w| w.contains("hollow")),
        "{warnings:?}"
    );
    let (back, read_warnings) = threemf::read(&std::fs::read(&p).unwrap()).unwrap();
    assert!(read_warnings.is_empty(), "{read_warnings:?}");
    assert_eq!(back.objects.len(), 1, "the empty object was left out");
    assert_eq!(back.objects[0].name, "solid");

    // write_model_path surfaces the same warning rather than failing.
    let p2 = dir.path().join("out2.3mf");
    let outcome = write_model_path(&model, &p2);
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// MF-ISSUE-039: only a *missing* `_rels/.rels` falls back to the standard
/// model path (with the existing warning). A present but corrupt/unreadable
/// one is an `IoError::Parse` naming it — never a silent fallback.
#[test]
fn only_a_missing_rels_falls_back_a_corrupt_one_is_a_named_error() {
    let xml = format!(
        r#"<?xml version="1.0"?><model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model">{TRI}</object></resources><build><item objectid="1"/></build></model>"#
    );

    // Missing entirely: falls back to the standard path, with a warning.
    let (_, warnings) = threemf::read(&zip_model(&xml, false)).unwrap();
    assert!(warnings.iter().any(|w| w.contains(".rels")), "{warnings:?}");

    // Present but corrupt (not valid XML): a named Parse error, not a
    // silent fallback to the standard path.
    let bytes = {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("[Content_Types].xml", opt).unwrap();
        z.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#).unwrap();
        z.start_file("_rels/.rels", opt).unwrap();
        z.write_all(b"<not valid xml").unwrap();
        z.start_file("3D/3dmodel.model", opt).unwrap();
        z.write_all(xml.as_bytes()).unwrap();
        z.finish().unwrap();
        buf.into_inner()
    };
    match threemf::read(&bytes) {
        Err(IoError::Parse { message, .. }) => {
            assert!(message.contains(".rels"), "{message}");
        }
        other => panic!(
            "expected a named Parse error, got {:?}",
            other.map(|(m, _)| m.unit)
        ),
    }
}

/// Object metadata (MeshFixer's `meshfixer:role`, and any other entry)
/// round-trips through `<metadatagroup>`, escaped, with no warning.
#[test]
fn object_metadata_round_trips() {
    let mut hole = obj("hole", cube(1.0), IDENTITY);
    hole.metadata.insert("meshfixer:role".into(), "hole".into());
    hole.metadata
        .insert("meshfixer:note".into(), "a < b & \"c\"".into());
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![obj("solid", tetrahedron(), IDENTITY), hole],
    };
    let bytes = threemf::write(&model).unwrap();
    let (back, warnings) = threemf::read(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(back.objects[0].metadata.is_empty());
    assert_eq!(back.objects[1].metadata, model.objects[1].metadata);
}
