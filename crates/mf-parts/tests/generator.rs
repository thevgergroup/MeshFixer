#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The one generator registry (parts-architecture-review.md): every
//! built-in shape and connector is a `Generator`, with a unique id, and
//! builds its parts from its own schema's defaults.

use std::collections::{BTreeMap, BTreeSet};

use mf_parts::{GenContext, TargetSlot, find_generator, generators};
use mf_runtime::RunContext;

#[test]
fn every_generator_has_a_unique_id_and_builds_from_its_defaults() {
    let all = generators();
    let ids: BTreeSet<&str> = all.iter().map(|g| g.id()).collect();
    assert_eq!(ids.len(), all.len(), "ids are unique");
    let run = RunContext::none();
    for g in all {
        let params = g.spec().validate(&BTreeMap::new()).unwrap();
        let parts = g
            .generate(&params, &GenContext::new(&run))
            .unwrap_or_else(|e| panic!("{}: {e}", g.id()));
        assert!(!parts.is_empty(), "{}", g.id());
        for p in &parts {
            assert!(!p.mesh.is_empty(), "{} {}", g.id(), p.name);
        }
        if g.is_shape() {
            assert_eq!(parts.len(), 1, "{}", g.id());
            assert_eq!(parts[0].attach_to, TargetSlot::Neither);
        }
    }
}

#[test]
fn shapes_and_connectors_are_found_by_id_in_one_registry() {
    for id in [
        "shape.box",
        "joint.dowel",
        "hinge.leaf",
        "thread",
        "dovetail.slide",
    ] {
        assert_eq!(find_generator(id).map(|g| g.id()), Some(id));
    }
    assert!(find_generator("nope").is_none());
}
