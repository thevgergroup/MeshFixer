#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bugs (consolidation plan C01, units-audit.md
//! mf-operations/src/add_part.rs:65-110 and mf-parts/src/library.rs:
//! 288-291): library parts are mm meshes, but `add_part` places them
//! with an unscaled transform, so a 10 mm built-in becomes 10 m on a
//! metre document; and `save_object_as_part` always labels the saved
//! mesh `unit: Millimeter`, so a part saved from a metre model comes
//! back 1000x smaller when reloaded. Fixed by C10 ("Unit-correct
//! library parts").

use std::collections::BTreeMap;

use mf_command::{Invocation, Session};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_parts::{Attach, Library, PartKind, PartManifest, save_object_as_part};

fn ctx() -> mf_runtime::RunContext {
    mf_runtime::RunContext::none()
}

/// The built-in "sphere" part is 10 mm across (radius 5 mm,
/// `builtin.rs:230`). Placed by `object.add_part` into a *metres*
/// document, its bounding box should measure about 0.01 doc units
/// (10 mm), the physically correct size — not 10 doc units (10 m), what
/// an unscaled mm mesh dropped straight into a metres document would
/// give.
#[test]
#[ignore = "red: C10"]
fn library_part_placed_into_a_metres_document_has_a_physically_correct_bbox() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Meter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "base".into(),
            // A 1 m cube, so the part is placed well inside it.
            mesh: mf_fixtures::cube(1.0),
            transform: IDENTITY,
        }],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(mf_io::read_model(&path).unwrap());
    assert_eq!(s.document().units(), Unit::Meter);

    let (lib, _) = Library::load(None, &ctx());
    let sphere = lib.get("sphere").unwrap();
    let csv: Vec<String> = IDENTITY.iter().map(f64::to_string).collect();
    s.execute(Invocation {
        op_id: "object.add_part".into(),
        target: ids[0],
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            ("part_id".to_string(), "sphere".to_string()),
            ("part_hash".to_string(), sphere.content_hash().unwrap()),
            ("transform".to_string(), csv.join(",")),
        ]),
    })
    .unwrap();

    let placed = s
        .document()
        .objects()
        .iter()
        .find(|o| o.name == "Sphere")
        .expect("the part was placed as a new object");
    let mesh = placed.mesh().unwrap();
    let bbox = mesh.bounding_box().unwrap();
    let diag = bbox.diagonal();
    // The sphere is 10 mm across. In a metres document that's 0.01 doc
    // units. Give it a generous tolerance either side, but the bug
    // reproduces as a full 1000x (diag near 10.0, not 0.01).
    assert!(
        diag < 0.1,
        "a 10 mm library part placed into a metres document should have \
         a bbox diagonal of about 0.01 doc units, not {diag} (mesh placed \
         unscaled, as raw millimetres, into the metres document)"
    );
}

fn manifest(id: &str) -> PartManifest {
    PartManifest {
        id: id.into(),
        name: "Saved part".into(),
        version: 1,
        licence: "CC0-1.0".into(),
        source: "test".into(),
        kind: PartKind::Solid,
        attach: Attach {
            point: [0.5, 0.5, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        },
        default_scale: 1.0,
        mesh: String::new(),
        category: String::new(),
        description: String::new(),
    }
}

/// Save-as-part from a metres document, then reload the same part into
/// an mm document: the physical size must round-trip. `save_object_as_
/// part` records the doc-unit mesh but always labels it `unit:
/// Millimeter`, so a part saved from a 0.02 m (20 mm) cube is read back
/// as a 0.02 mm cube — 1000x smaller than the 20 mm cube that was
/// actually saved.
#[test]
#[ignore = "red: C10"]
fn save_as_part_from_a_metres_document_round_trips_the_physical_size() {
    let dir = tempfile::tempdir().unwrap();
    // A 20 mm cube, modelled at 0.02 doc units in a metres document.
    let cube_m = mf_fixtures::cube(0.02);
    let saved = save_object_as_part(&cube_m, &manifest("saved-cube"), dir.path()).unwrap();
    assert!(saved.join("part.yaml").is_file());

    let (lib, warnings) = Library::load(Some(dir.path()), &ctx());
    assert!(warnings.is_empty(), "{warnings:?}");
    let part = lib.get("saved-cube").unwrap();
    let reloaded = part.mesh(&ctx()).unwrap();
    let bbox = reloaded.bounding_box().unwrap();
    let edge = bbox.max[0] - bbox.min[0];
    // Reloading this part (its mesh file is read raw, and it was saved
    // labelled mm regardless of the source document's unit) should give
    // back the same physical 20 mm edge the part was saved at, not the
    // 0.02 its stored number actually is.
    assert!(
        (edge - 20.0).abs() < 1e-3,
        "a part saved from a metres document (20 mm/0.02 doc-unit cube) \
         should reload with a 20 mm edge, but reloaded at {edge} \
         (saved mislabelled as millimetres with no scaling)"
    );
}
