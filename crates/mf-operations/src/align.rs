//! `object.align` (Phase 1 modelling tools, Tinkercad-style Align): moves
//! two or more objects by translation only so they share an edge, a
//! centre, or an even spacing, along one world axis.
//!
//! Every target's world bounding box decides its own move, except a
//! target the `leaders` parameter names as a follower of an earlier one
//! (a linked group's other members, D-ALIGN-1): it copies that target's
//! computed translation exactly, so the group keeps its own internal
//! layout and moves as a rigid whole.

use mf_geometry::Mesh;
use mf_spec::Params;

use crate::refusal::RefusalArg;
use crate::{
    DocContext, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

pub struct Align;

/// A malformed invocation (`leaders`/`mode` the UI never sends on its
/// own): never one of `object.align`'s declared refusals. On the
/// `OpError::Failed` allow-list, `docs/failed-refusals.allow`.
fn failed(code: &'static str, message: impl Into<String>) -> OpError {
    OpError::Failed {
        code,
        message: message.into(),
    }
}

/// A declared refusal (ADR-0010), through the one registry helper.
fn refuse_plain(code: &'static str) -> OpError {
    crate::refusal::refuse_plain(code)
}

/// As [`refuse_plain`], naming the object at fault.
fn refuse_object(code: &'static str, name: &str) -> OpError {
    crate::refusal::refuse(
        code,
        std::collections::BTreeMap::from([("name", RefusalArg::Text(name.to_owned()))]),
    )
}

/// A world bounding box, min then max.
type Bounds = ([f64; 3], [f64; 3]);

/// `target`'s world bounding box (min, max), or `None` for an empty mesh.
fn world_bounds(input: &OperandInput, ctx: &RunContext) -> Result<Option<Bounds>, OpError> {
    let Some(bb) = input.mesh.bounding_box_with(ctx)? else {
        return Ok(None);
    };
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in 0..8 {
        let p = [0, 1, 2].map(|a| {
            if c & (1 << a) != 0 {
                bb.max[a]
            } else {
                bb.min[a]
            }
        });
        let w = mf_geometry::affine::apply(&input.transform, p);
        for a in 0..3 {
            lo[a] = lo[a].min(w[a]);
            hi[a] = hi[a].max(w[a]);
        }
    }
    Ok(Some((lo, hi)))
}

fn axis_index(axis: &str) -> usize {
    match axis {
        "x" => 0,
        "y" => 1,
        _ => 2,
    }
}

/// One target's world bounds, keyed by its index among `inputs` (the
/// `leaders` entry for followers is resolved before this is built, so
/// every entry here independently decides its own geometry-based move).
struct Box3 {
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Box3 {
    fn centre(&self, axis: usize) -> f64 {
        (self.lo[axis] + self.hi[axis]) * 0.5
    }
}

/// Parses `leaders` (JSON, one entry per target: `null` or an earlier
/// target's 0-based index). `n` is the number of targets.
fn parse_leaders(text: &str, n: usize) -> Result<Vec<Option<usize>>, OpError> {
    let leaders: Vec<Option<usize>> = serde_json::from_str(text)
        .map_err(|e| failed("invalid_leaders", format!("leaders isn't valid: {e}")))?;
    if leaders.is_empty() {
        return Ok(vec![None; n]);
    }
    if leaders.len() != n {
        return Err(failed(
            "leaders_mismatch",
            format!("{} leaders were given for {n} targets", leaders.len()),
        ));
    }
    for (k, leader) in leaders.iter().enumerate() {
        if let Some(l) = leader {
            if *l >= k {
                return Err(failed(
                    "invalid_leader",
                    format!("target {k}'s leader must be an earlier target"),
                ));
            }
            if leaders[*l].is_some() {
                return Err(failed(
                    "invalid_leader",
                    format!("target {k}'s leader ({l}) is itself a follower"),
                ));
            }
        }
    }
    Ok(leaders)
}

impl Operation for Align {
    fn id(&self) -> &'static str {
        "object.align"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &DocContext,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        _doc: &DocContext,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let plan = plan(inputs, params, ctx)?;
        Ok(execute(&plan))
    }
}

/// What `object.align` will do (C22, ADR-0008): every target's world
/// translation along the axis, decided once. A preview shows these
/// `transforms` and Apply ([`execute`]) commits exactly them.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignPlan {
    /// The axis's name (`x`, `y` or `z`) and the mode, for the report.
    pub axis: String,
    pub mode: String,
    /// Each target's move along the axis (0 for one that stays), in
    /// target order (the `anchor` operand is never a target here).
    pub deltas: Vec<f64>,
    /// Each target's placement after the move.
    pub transforms: Vec<[f64; 12]>,
}

/// `object.align`'s plan for `inputs` (the targets, then an optional
/// trailing `anchor`). A refusal here is Apply's refusal.
pub fn plan(
    inputs: &[OperandInput],
    params: &Params,
    ctx: &RunContext,
) -> Result<AlignPlan, OpError> {
    // The targets are every input except a trailing `anchor` operand
    // (an optional, non-`many` operand always lands after every
    // `many` target, D6).
    let (anchor_input, targets): (Option<&OperandInput>, &[OperandInput]) =
        match inputs.iter().position(|i| i.slot == "anchor") {
            Some(k) => (Some(&inputs[k]), &inputs[..k]),
            None => (None, inputs),
        };
    for t in targets.iter().chain(anchor_input) {
        if t.target != Target::WholeObject {
            return Err(unsupported_target("object.align", &t.target));
        }
    }
    if targets.len() < 2 {
        return Err(refuse_plain("too_few_objects"));
    }
    ctx.checkpoint()?;
    let axis = axis_index(params.get_choice("axis")?);
    let mode = params.get_choice("mode")?.to_owned();
    let leaders = parse_leaders(params.get_text("leaders")?, targets.len())?;

    let mut boxes: Vec<Option<Box3>> = Vec::with_capacity(targets.len());
    for t in targets {
        boxes.push(world_bounds(t, ctx)?.map(|(lo, hi)| Box3 { lo, hi }));
    }
    let empty = |name: &str| refuse_object("empty_object", name);
    for (k, b) in boxes.iter().enumerate() {
        if b.is_none() && leaders[k].is_none() {
            return Err(empty(targets[k].name));
        }
    }

    // The anchor's bounds (named operand), or the overall bounds of
    // every target (no anchor): what min/centre/max land translations
    // on, and what distribute's two extremes leave in place of.
    let overall = |axis: usize| {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for b in boxes.iter().flatten() {
            lo = lo.min(b.lo[axis]);
            hi = hi.max(b.hi[axis]);
        }
        (lo, hi)
    };
    let anchor_box = match anchor_input {
        Some(a) => {
            let (lo, hi) = world_bounds(a, ctx)?.ok_or_else(|| empty(a.name))?;
            Some(Box3 { lo, hi })
        }
        None => None,
    };

    // One world-axis delta per target that decides its own move
    // (leaders and ordinary targets); a follower's delta is copied
    // from its leader afterwards.
    let mut delta = vec![0.0_f64; targets.len()];
    match mode.as_str() {
        "min" | "centre" | "max" => {
            let target_value = match &anchor_box {
                Some(b) => match mode.as_str() {
                    "min" => b.lo[axis],
                    "max" => b.hi[axis],
                    _ => b.centre(axis),
                },
                None => {
                    let (lo, hi) = overall(axis);
                    match mode.as_str() {
                        "min" => lo,
                        "max" => hi,
                        _ => (lo + hi) * 0.5,
                    }
                }
            };
            for (k, b) in boxes.iter().enumerate() {
                let Some(b) = b else { continue };
                // The anchor itself (if it's also a target) never
                // moves: its own value already equals the target
                // value by construction.
                let own = match mode.as_str() {
                    "min" => b.lo[axis],
                    "max" => b.hi[axis],
                    _ => b.centre(axis),
                };
                delta[k] = target_value - own;
            }
        }
        "distribute_gaps" | "distribute_centres" => {
            // Ordered by the targets' own current position on the
            // axis (their centre), ties broken by input order so the
            // result is deterministic (Exact replay).
            let mut order: Vec<usize> =
                (0..targets.len()).filter(|&k| boxes[k].is_some()).collect();
            order.sort_by(|&a, &b| {
                boxes[a]
                    .as_ref()
                    .expect("filtered above")
                    .centre(axis)
                    .total_cmp(&boxes[b].as_ref().expect("filtered above").centre(axis))
                    .then(a.cmp(&b))
            });
            if order.len() < 2 {
                return Err(refuse_plain("too_few_objects"));
            }
            let first = order[0];
            let last = *order.last().expect("at least two");
            if mode == "distribute_gaps" {
                // Equal gaps: the space between the first's max and
                // the last's min, split evenly among the objects
                // between them (each keeping its own width).
                let span_lo = boxes[first].as_ref().expect("filtered").hi[axis];
                let span_hi = boxes[last].as_ref().expect("filtered").lo[axis];
                let between = &order[1..order.len() - 1];
                let widths: f64 = between
                    .iter()
                    .map(|&k| {
                        let b = boxes[k].as_ref().expect("filtered");
                        b.hi[axis] - b.lo[axis]
                    })
                    .sum();
                let gap = if between.is_empty() {
                    0.0
                } else {
                    ((span_hi - span_lo - widths) / (between.len() + 1) as f64).max(0.0)
                };
                let mut cursor = span_lo + gap;
                for &k in between {
                    let b = boxes[k].as_ref().expect("filtered");
                    let width = b.hi[axis] - b.lo[axis];
                    delta[k] = cursor - b.lo[axis];
                    cursor += width + gap;
                }
            } else {
                // Equal centres: the first and last objects' own
                // centres bound the run; the rest land on evenly
                // spaced centres between them.
                let c_lo = boxes[first].as_ref().expect("filtered").centre(axis);
                let c_hi = boxes[last].as_ref().expect("filtered").centre(axis);
                let between = &order[1..order.len() - 1];
                let n = between.len() + 1;
                for (i, &k) in between.iter().enumerate() {
                    let b = boxes[k].as_ref().expect("filtered");
                    let want = c_lo + (c_hi - c_lo) * (i + 1) as f64 / n as f64;
                    delta[k] = want - b.centre(axis);
                }
            }
        }
        other => {
            return Err(failed(
                "unknown_mode",
                format!("'{other}' isn't a known align mode"),
            ));
        }
    }
    // Followers copy their leader's delta (the leader has already
    // been resolved above: `parse_leaders` only allows pointing at
    // an earlier, non-follower target).
    for (k, leader) in leaders.iter().enumerate() {
        if let Some(l) = leader {
            delta[k] = delta[*l];
        }
    }

    let transforms = targets
        .iter()
        .zip(&delta)
        .map(|(t, d)| {
            let mut transform = t.transform;
            transform[9 + axis] += d;
            transform
        })
        .collect();
    Ok(AlignPlan {
        axis: params.get_choice("axis")?.to_owned(),
        mode,
        deltas: delta,
        transforms,
    })
}

/// Commits `plan`: one transform change per target that moves.
pub fn execute(plan: &AlignPlan) -> DocOutput {
    let changes: Vec<ObjectChange> = plan
        .deltas
        .iter()
        .zip(&plan.transforms)
        .enumerate()
        .filter(|(_, (d, _))| **d != 0.0)
        .map(|(k, (_, transform))| ObjectChange::Transform {
            operand: k,
            transform: *transform,
        })
        .collect();
    DocOutput {
        report: serde_json::json!({ "moved": changes.len(), "axis": plan.axis, "mode": plan.mode }),
        changes,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure

    use super::*;
    use mf_document::ObjectId;

    fn cube(size: f64) -> Mesh {
        let h = size / 2.0;
        Mesh::new(
            vec![
                [-h, -h, -h],
                [h, -h, -h],
                [h, h, -h],
                [-h, h, -h],
                [-h, -h, h],
                [h, -h, h],
                [h, h, h],
                [-h, h, h],
            ],
            vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [1, 2, 6],
                [1, 6, 5],
                [2, 3, 7],
                [2, 7, 6],
                [3, 0, 4],
                [3, 4, 7],
            ],
        )
        .unwrap()
    }

    fn translate(x: f64, y: f64, z: f64) -> [f64; 12] {
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, x, y, z]
    }

    fn input<'a>(
        name: &'a str,
        mesh: &'a Mesh,
        transform: [f64; 12],
        slot: &'a str,
    ) -> OperandInput<'a> {
        OperandInput {
            slot,
            object: ObjectId(1),
            name,
            mesh,
            transform,
            target: Target::WholeObject,
        }
    }

    fn params(axis: &str, mode: &str, leaders: &str) -> Params {
        mf_spec::builtin::get("object.align")
            .unwrap()
            .validate(&std::collections::BTreeMap::from([
                ("axis".to_owned(), axis.to_owned()),
                ("mode".to_owned(), mode.to_owned()),
                ("leaders".to_owned(), leaders.to_owned()),
            ]))
            .unwrap()
    }

    #[test]
    fn min_centre_max_align_every_target_to_the_anchor() {
        // a: [-5, 5] on x; b (half-width 2): [48, 52] on x. With no
        // anchor, the overall bounds are always [-5, 52] (computed from
        // the original positions): min lands both boxes' min at -5, max
        // lands both boxes' max at 52, centre lands both boxes' centre
        // at the overall centre, 23.5.
        let a = cube(10.0);
        let b = cube(4.0);
        let ta = translate(0.0, 0.0, 0.0);
        let tb = translate(50.0, 0.0, 0.0);
        for (mode, expect_a_tx, expect_b_tx) in [
            ("min", 0.0, -3.0),
            ("centre", 23.5, 23.5),
            ("max", 47.0, 50.0),
        ] {
            let inputs = [input("a", &a, ta, "target"), input("b", &b, tb, "target")];
            let out = Align
                .execute_doc(
                    &inputs,
                    &params("x", mode, "[]"),
                    &DocContext::default(),
                    &RunContext::none(),
                )
                .unwrap();
            // No change for a target means its translation is already
            // what align wanted (`delta == 0.0`): its original x stands.
            let original = [ta[9], tb[9]];
            let tx_of = |operand: usize| {
                out.changes
                    .iter()
                    .find_map(|c| match c {
                        ObjectChange::Transform {
                            operand: o,
                            transform,
                        } if *o == operand => Some(transform[9]),
                        _ => None,
                    })
                    .unwrap_or(original[operand])
            };
            assert!(
                (tx_of(0) - expect_a_tx).abs() < 1e-9,
                "{mode}: a's new x translation should be {expect_a_tx}, got {}",
                tx_of(0)
            );
            assert!(
                (tx_of(1) - expect_b_tx).abs() < 1e-9,
                "{mode}: b's new x translation should be {expect_b_tx}, got {}",
                tx_of(1)
            );
        }
    }

    #[test]
    fn an_anchor_never_moves_and_the_others_align_to_it() {
        let a = cube(10.0);
        let b = cube(10.0);
        let anchor = cube(10.0);
        let ta = translate(-30.0, 0.0, 0.0);
        let tb = translate(30.0, 0.0, 0.0);
        let t_anchor = translate(5.0, 0.0, 0.0);
        let inputs = [
            input("a", &a, ta, "target"),
            input("b", &b, tb, "target"),
            input("anchor", &anchor, t_anchor, "anchor"),
        ];
        let out = Align
            .execute_doc(
                &inputs,
                &params("x", "centre", "[]"),
                &DocContext::default(),
                &RunContext::none(),
            )
            .unwrap();
        assert_eq!(out.changes.len(), 2, "{:?}", out.changes);
        for c in &out.changes {
            let ObjectChange::Transform { operand, transform } = c else {
                panic!("{c:?}")
            };
            assert!(*operand < 2);
            assert!(
                (transform[9] - 5.0).abs() < 1e-9,
                "operand {operand} should centre on the anchor's x=5: {transform:?}"
            );
        }
    }

    #[test]
    fn distribute_gaps_spaces_the_middle_objects_evenly() {
        let a = cube(10.0); // [-5, 5]
        let b = cube(10.0); // placed far right of its final spot
        let c = cube(10.0); // the right anchor
        let ta = translate(0.0, 0.0, 0.0); // [-5, 5]
        let tb = translate(17.0, 0.0, 0.0); // [12, 22], out of order on purpose
        let tc = translate(100.0, 0.0, 0.0); // [95, 105]
        let inputs = [
            input("a", &a, ta, "target"),
            input("b", &b, tb, "target"),
            input("c", &c, tc, "target"),
        ];
        let out = Align
            .execute_doc(
                &inputs,
                &params("x", "distribute_gaps", "[]"),
                &DocContext::default(),
                &RunContext::none(),
            )
            .unwrap();
        // a and c (the extremes by position) don't move; only b moves,
        // landing centred between a's max (5) and c's min (95): gap on
        // each side is (90 - 10) / 2 = 40, so b's box is [45, 55].
        assert_eq!(out.changes.len(), 1, "{:?}", out.changes);
        let ObjectChange::Transform { operand, transform } = &out.changes[0] else {
            panic!("{:?}", out.changes)
        };
        assert_eq!(*operand, 1);
        assert!(
            (transform[9] - 50.0).abs() < 1e-9,
            "b should centre at x=50: {transform:?}"
        );
    }

    #[test]
    fn distribute_centres_spaces_the_middle_centres_evenly() {
        let a = cube(4.0);
        let b = cube(4.0);
        let c = cube(4.0);
        let ta = translate(0.0, 0.0, 0.0);
        let tb = translate(5.0, 0.0, 0.0);
        let tc = translate(100.0, 0.0, 0.0);
        let inputs = [
            input("a", &a, ta, "target"),
            input("b", &b, tb, "target"),
            input("c", &c, tc, "target"),
        ];
        let out = Align
            .execute_doc(
                &inputs,
                &params("x", "distribute_centres", "[]"),
                &DocContext::default(),
                &RunContext::none(),
            )
            .unwrap();
        assert_eq!(out.changes.len(), 1, "{:?}", out.changes);
        let ObjectChange::Transform { operand, transform } = &out.changes[0] else {
            panic!("{:?}", out.changes)
        };
        assert_eq!(*operand, 1);
        assert!(
            (transform[9] - 50.0).abs() < 1e-9,
            "b's centre should land at the midpoint, x=50: {transform:?}"
        );
    }

    #[test]
    fn a_follower_copies_its_leaders_delta() {
        let a = cube(10.0);
        let hole = cube(2.0);
        let b = cube(10.0);
        let ta = translate(0.0, 0.0, 0.0);
        let t_hole = translate(0.0, 0.0, 3.0); // a group-mate of a, offset in z
        let tb = translate(50.0, 0.0, 0.0);
        let inputs = [
            input("a", &a, ta, "target"),
            input("hole", &hole, t_hole, "target"),
            input("b", &b, tb, "target"),
        ];
        let out = Align
            .execute_doc(
                &inputs,
                &params("x", "min", "[null, 0, null]"),
                &DocContext::default(),
                &RunContext::none(),
            )
            .unwrap();
        let delta_a = out
            .changes
            .iter()
            .find_map(|c| match c {
                ObjectChange::Transform {
                    operand: 0,
                    transform,
                } => Some(transform[9]),
                _ => None,
            })
            .unwrap_or(0.0);
        let delta_hole = out
            .changes
            .iter()
            .find_map(|c| match c {
                ObjectChange::Transform {
                    operand: 1,
                    transform,
                } => Some(transform[9] - t_hole[9]),
                _ => None,
            })
            .unwrap_or(0.0);
        assert!(
            (delta_hole - delta_a).abs() < 1e-9,
            "the follower must move by exactly the leader's own delta: leader {delta_a}, follower {delta_hole}"
        );
    }

    #[test]
    fn fewer_than_two_targets_is_refused_by_name() {
        let a = cube(10.0);
        let inputs = [input("a", &a, translate(0.0, 0.0, 0.0), "target")];
        match Align.execute_doc(
            &inputs,
            &params("x", "centre", "[]"),
            &DocContext::default(),
            &RunContext::none(),
        ) {
            Err(OpError::Refused { code, .. }) => assert_eq!(code, "too_few_objects"),
            other => panic!("{other:?}"),
        }
    }
}
