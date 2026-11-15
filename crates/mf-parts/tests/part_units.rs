#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bugs (consolidation plan C01, units-audit.md
//! mf-operations/src/add_part.rs:65-110 and mf-parts/src/library.rs:
//! 288-291): library parts are mm meshes, but `add_part` placed them
//! with an unscaled transform, so a 10 mm built-in became 10 m on a
//! metre document; and `save_object_as_part` always labelled the saved
//! mesh `unit: Millimeter`, so a part saved from a metre model came
//! back 1000x smaller when reloaded. Fixed by C10 ("Unit-correct
//! library parts"): the manifest now carries the real `unit`, and
//! `object.add_part` scales the mm-internal mesh by `DocContext`.

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

fn manifest(id: &str, unit: Unit) -> PartManifest {
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
        unit,
        mesh: String::new(),
        category: String::new(),
        description: String::new(),
    }
}

/// Save-as-part from a metres document, then reload the same part into
/// an mm document: the physical size must round-trip. `save_object_as_
/// part` used to always label the saved mesh `unit: Millimeter`
/// regardless of the source document's unit, so a part saved from a
/// 0.02 m (20 mm) cube was read back as a 0.02 mm cube — 1000x smaller
/// than the 20 mm cube that was actually saved.
#[test]
fn save_as_part_from_a_metres_document_round_trips_the_physical_size() {
    let dir = tempfile::tempdir().unwrap();
    // A 20 mm cube, modelled at 0.02 doc units in a metres document.
    let cube_m = mf_fixtures::cube(0.02);
    let saved =
        save_object_as_part(&cube_m, &manifest("saved-cube", Unit::Meter), dir.path()).unwrap();
    assert!(saved.join("part.yaml").is_file());

    let (lib, warnings) = Library::load(Some(dir.path()), &ctx());
    assert!(warnings.is_empty(), "{warnings:?}");
    let part = lib.get("saved-cube").unwrap();
    let reloaded = part.mesh(&ctx()).unwrap();
    let bbox = reloaded.bounding_box().unwrap();
    let edge = bbox.max[0] - bbox.min[0];
    // Reloading this part (its mesh file is read raw, labelled with the
    // source document's real unit) should give back the same physical
    // 20 mm edge the part was saved at, not the 0.02 its stored number
    // actually is.
    assert!(
        (edge - 20.0).abs() < 1e-3,
        "a part saved from a metres document (20 mm/0.02 doc-unit cube) \
         should reload with a 20 mm edge, but reloaded at {edge} \
         (saved mislabelled as millimetres with no scaling)"
    );
}

/// `part_roundtrip_preserves_physical_size` (C10 contract), the
/// `add_part`/`DocContext` half: the built-in "sphere" (10 mm across,
/// `builtin.rs:230`) is placed by `object.add_part` into a fresh
/// document in each of mm, m and in. Its placed bbox diagonal must
/// measure the same 10 mm physically every time, whatever the
/// document's own unit — `LibraryPart::mesh` hands back millimetres
/// (D16), and `add_part` scales by `DocContext`'s unit.
#[test]
fn part_roundtrip_preserves_physical_size() {
    for place_unit in [Unit::Millimeter, Unit::Meter, Unit::Inch] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.3mf");
        let model = Model {
            metadata: Default::default(),
            unit: place_unit,
            objects: vec![ModelObject {
                metadata: Default::default(),
                name: "base".into(),
                // A 1 m cube (in the document's own unit), so the part
                // is placed well inside it regardless of `place_unit`.
                mesh: mf_fixtures::cube(1000.0 / place_unit.mm_per()),
                transform: IDENTITY,
            }],
        };
        mf_io::write_model_path(&model, &path).unwrap();
        let mut s = Session::new();
        let (ids, _) = s.import_model(mf_io::read_model(&path).unwrap());
        assert_eq!(s.document().units(), place_unit);

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
            .unwrap_or_else(|| panic!("{place_unit:?}: the part wasn't placed"));
        let mesh = placed.mesh().unwrap();
        let bbox = mesh.bounding_box().unwrap();
        let diag_doc_units = bbox.diagonal();
        let diag_mm = diag_doc_units * place_unit.mm_per();
        // A 10 mm sphere's bbox diagonal is 10 mm (it's a cube-ish bbox
        // around a sphere of diameter 10, so the diagonal is 10 * sqrt(3)
        // — what matters is that it scales the *same* way, in mm, in
        // every document unit).
        let expected_mm = 10.0 * 3.0_f64.sqrt();
        assert!(
            (diag_mm - expected_mm).abs() < 1e-6,
            "a 10 mm library part placed into a {place_unit:?} document \
             should have a bbox diagonal of {expected_mm} mm physically, \
             not {diag_mm} mm"
        );
    }
}

/// `part_roundtrip_preserves_physical_size`, the save/reload half: a
/// part saved from documents in each of mm, m and in reloads at the
/// same physical 20 mm edge every time — `save_object_as_part` records
/// the real source unit, and `LibraryPart::mesh` scales back to mm from
/// it.
#[test]
fn save_and_reload_preserves_physical_size_across_units() {
    for save_unit in [Unit::Millimeter, Unit::Meter, Unit::Inch] {
        let dir = tempfile::tempdir().unwrap();
        // A 20 mm cube, modelled in the source document's own unit.
        let edge_doc_units = 20.0 / save_unit.mm_per();
        let cube = mf_fixtures::cube(edge_doc_units);
        let part_id = format!("rt-{}", save_unit.symbol().replace('µ', "u"));
        save_object_as_part(&cube, &manifest(&part_id, save_unit), dir.path()).unwrap();

        let (lib, warnings) = Library::load(Some(dir.path()), &ctx());
        assert!(warnings.is_empty(), "{save_unit:?}: {warnings:?}");
        let part = lib.get(&part_id).unwrap();
        let reloaded = part.mesh(&ctx()).unwrap();
        let bbox = reloaded.bounding_box().unwrap();
        let edge_mm = bbox.max[0] - bbox.min[0];
        assert!(
            (edge_mm - 20.0).abs() < 1e-6,
            "a part saved from a {save_unit:?} document (a 20 mm cube) \
             should reload at a 20 mm edge, not {edge_mm} mm"
        );
    }
}
