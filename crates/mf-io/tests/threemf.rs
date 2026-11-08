#![allow(clippy::unwrap_used)] // tests may panic on failure

//! 3MF core: objects, build-item transforms, units (MF-IO-001, MF-DOC-004).

use mf_fixtures::{cube, tetrahedron, uv_sphere};
use mf_geometry::FaceId;
use mf_io::threemf::{self, Model, ModelObject, Unit};
use mf_io::{Format, IoError, read_model, write_path};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn obj(name: &str, mesh: mf_geometry::Mesh, transform: [f64; 12]) -> ModelObject {
    ModelObject {
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
    write_path(&m, &p, Format::ThreeMf).unwrap();
    let model = read_model(&p).unwrap();
    assert_eq!(model.objects.len(), 1);
    assert_eq!(model.objects[0].mesh.triangle_count(), 11);
    assert_eq!(model.source.format, Format::ThreeMf);
    assert_eq!(Format::from_path("X.3MF".as_ref()), Some(Format::ThreeMf));
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
