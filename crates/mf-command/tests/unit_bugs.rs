#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bug (consolidation plan C01, units-audit.md
//! mf-command/src/session.rs:811): importing a second file in another
//! unit only warns "coordinates are not converted" — it never actually
//! converts them. A 10 mm part imported into an existing metres document
//! lands at its raw mm coordinates (10 doc units, 1000x too big), not at
//! its correct physical size (0.01 doc units). Fixed by C07 ("DocContext
//! and cross-unit import in mf-command").

use mf_command::Session;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn model_at(unit: Unit, name: &str, mesh: mf_geometry::Mesh) -> Model {
    Model {
        metadata: Default::default(),
        unit,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: IDENTITY,
        }],
    }
}

/// Open a metres document (a 1 m cube), then import a second file that
/// is itself in millimetres (a 10 mm cube): the second object's physical
/// size, once it's in the document, must be the same 10 mm — 0.01 doc
/// units — not the raw mm number (10.0) taken as if it were already in
/// doc units.
#[test]
#[ignore = "red: C07"]
fn a_second_import_in_another_unit_converts_to_the_document_unit() {
    let dir = tempfile::tempdir().unwrap();

    let base_path = dir.path().join("base.3mf");
    write_model_path(
        &model_at(Unit::Meter, "base", mf_fixtures::cube(1.0)),
        &base_path,
    )
    .unwrap();

    let part_path = dir.path().join("part.3mf");
    // A 10 mm cube, saved in a millimetre file.
    write_model_path(
        &model_at(Unit::Millimeter, "part", mf_fixtures::cube(10.0)),
        &part_path,
    )
    .unwrap();

    let mut s = Session::new();
    s.import_model(read_model(&base_path).unwrap());
    assert_eq!(s.document().units(), Unit::Meter, "set by the first import");

    let (ids, _warnings) = s.import_model(read_model(&part_path).unwrap());
    let part = s.document().object(ids[0]).unwrap();
    let mesh = part.mesh().unwrap();
    let bbox = mesh.bounding_box().unwrap();
    let edge = bbox.max[0] - bbox.min[0];
    // Physically correct: 10 mm == 0.01 m == 0.01 doc units.
    assert!(
        (edge - 0.01).abs() < 1e-6,
        "a 10 mm part imported into a metres document should measure \
         0.01 doc units across, but measured {edge} (bbox {bbox:?}): \
         the cross-unit import left the raw mm coordinates unconverted"
    );
}
