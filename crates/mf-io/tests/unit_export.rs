#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bugs (consolidation plan C01, units-audit.md
//! mf-io/src/lib.rs:425-428): STL and OBJ export ignore the document's
//! `unit` and write the raw doc-unit numbers. A metre document exports
//! 1000x too small for any slicer, which expects millimetres.
//!
//! `write_path`/`write_path_with` take a `unit` parameter for exactly
//! this purpose (3MF honours it; see
//! `io.rs::export_applies_the_objects_transform_and_threemf_takes_the_document_unit`),
//! but `Format::StlBinary`, `Format::StlAscii` and `Format::Obj` never
//! consult it. Fixed by C06 ("Unit-honest IO in mf-io and mf-cli").

use mf_fixtures::cube;
use mf_io::{Format, read_path, write_path};

/// A 10 mm cube modelled in a metres document is `0.01` doc units
/// across. Exporting it to STL must write millimetre coordinates (10.0),
/// which any slicer assumes, not the raw doc-unit numbers (0.01).
#[test]
#[ignore = "red: C06"]
fn stl_export_from_a_metres_document_writes_millimetre_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    // A cube 0.01 doc units across == 10 mm, if the document is metres.
    let mesh = cube(0.01);
    let p = dir.path().join("out.stl");
    write_path(&mesh, &p, Format::StlBinary, mf_io::threemf::Unit::Meter).unwrap();
    let back = read_path(&p).unwrap().mesh;
    let bbox = back.bounding_box().unwrap();
    // Expect the exported STL to measure about 10 mm across each edge
    // (physically correct for a slicer), not 0.01.
    let extent = [
        bbox.max[0] - bbox.min[0],
        bbox.max[1] - bbox.min[1],
        bbox.max[2] - bbox.min[2],
    ];
    for e in extent {
        assert!(
            (e - 10.0).abs() < 1e-3,
            "STL export from a metres document should write mm-scale \
             coordinates (~10 mm edge), but wrote {e} (bbox {bbox:?}): \
             a slicer would treat this as 0.01 mm, 1000x too small"
        );
    }
}

/// Same bug, OBJ path.
#[test]
#[ignore = "red: C06"]
fn obj_export_from_a_metres_document_writes_millimetre_coordinates() {
    let dir = tempfile::tempdir().unwrap();
    let mesh = cube(0.01);
    let p = dir.path().join("out.obj");
    write_path(&mesh, &p, Format::Obj, mf_io::threemf::Unit::Meter).unwrap();
    let back = read_path(&p).unwrap().mesh;
    let bbox = back.bounding_box().unwrap();
    let extent = [
        bbox.max[0] - bbox.min[0],
        bbox.max[1] - bbox.min[1],
        bbox.max[2] - bbox.min[2],
    ];
    for e in extent {
        assert!(
            (e - 10.0).abs() < 1e-3,
            "OBJ export from a metres document should write mm-scale \
             coordinates (~10 mm edge), but wrote {e} (bbox {bbox:?})"
        );
    }
}
