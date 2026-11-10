#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.intersections` (Phase 1 Task 9): read-only, with or without the
//! optional `other` operand.

use std::collections::BTreeMap;

use mf_document::ObjectId;
use mf_fixtures::{cube, merge, translated};
use mf_geometry::Mesh;
use mf_operations::{OperandInput, RunContext, Target, find};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn input(mesh: &Mesh, id: u64, transform: [f64; 12]) -> OperandInput<'_> {
    OperandInput {
        object: ObjectId(id),
        mesh,
        transform,
        target: Target::WholeObject,
    }
}

#[test]
fn intersections_with_another_object_and_with_itself() {
    let op = find("mesh.intersections").unwrap();
    let params = mf_spec::builtin::get("mesh.intersections")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap();
    let (a, b) = (cube(2.0), cube(1.0));
    let mut at = IDENTITY;
    at[9..].copy_from_slice(&[1.5, 0.5, 0.5]);
    let out = op
        .execute_doc(
            &[input(&a, 1, IDENTITY), input(&b, 2, at)],
            &params,
            &RunContext::none(),
        )
        .unwrap();
    assert!(out.changes.is_empty(), "read-only");
    assert!(out.report["pair_count"].as_u64().unwrap() > 0);
    assert_eq!(out.report["regions"].as_array().unwrap().len(), 1);
    // Moved apart: none.
    at[9] = 5.0;
    let apart = op
        .execute_doc(
            &[input(&a, 1, IDENTITY), input(&b, 2, at)],
            &params,
            &RunContext::none(),
        )
        .unwrap();
    assert_eq!(apart.report["pair_count"], 0);
    // Alone: its own crossings.
    let crossing = merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]);
    let own = op
        .execute_doc(
            &[input(&crossing, 1, IDENTITY)],
            &params,
            &RunContext::none(),
        )
        .unwrap();
    assert!(own.report["pair_count"].as_u64().unwrap() > 0);
    let clean = op
        .execute_doc(&[input(&a, 1, IDENTITY)], &params, &RunContext::none())
        .unwrap();
    assert_eq!(clean.report["pair_count"], 0);
}
