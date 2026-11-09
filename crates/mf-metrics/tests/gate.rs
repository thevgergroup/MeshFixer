#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The frame-gate policy (MF-RT-001, M4 Task 10 fix round 1, P1): the
//! 50 ms budget never widens. A run whose over-budget frames all waited
//! on page-ins (the system swapping) is INCONCLUSIVE and is run exactly
//! once more; an over-budget frame without page-ins fails at once, and a
//! retry that breaks the budget again fails.

use std::time::Duration;

use mf_metrics::{GateVerdict, GatedFrame, frame_gate, judge_frames};

const BUDGET: Duration = Duration::from_millis(50);
const GB: u64 = 1 << 30;

fn frame(ms: u64, page_ins: Option<u64>) -> GatedFrame {
    GatedFrame {
        time: Duration::from_millis(ms),
        page_ins,
        at: format!("frame at {ms} ms"),
    }
}

#[test]
fn frames_within_budget_pass() {
    let frames = [frame(10, Some(0)), frame(50, Some(3))];
    assert_eq!(judge_frames(&frames, BUDGET, None), GateVerdict::Pass);
}

#[test]
fn an_over_budget_frame_without_page_ins_fails() {
    let frames = [frame(10, Some(0)), frame(52, Some(0))];
    let GateVerdict::Fail(why) = judge_frames(&frames, BUDGET, Some((28 * GB, 29 * GB))) else {
        panic!("should fail");
    };
    assert!(
        why.contains("52.0 ms") && why.contains("no page-ins"),
        "{why}"
    );
}

#[test]
fn unknown_page_ins_are_never_an_excuse() {
    let frames = [frame(60, None)];
    assert!(matches!(
        judge_frames(&frames, BUDGET, None),
        GateVerdict::Fail(_)
    ));
}

#[test]
fn over_budget_frames_that_all_paged_in_are_inconclusive() {
    let frames = [frame(51, Some(4)), frame(58, Some(12)), frame(20, Some(0))];
    let GateVerdict::Inconclusive(msg) = judge_frames(&frames, BUDGET, Some((28 * GB, 29 * GB)))
    else {
        panic!("should be inconclusive");
    };
    assert_eq!(
        msg,
        "INCONCLUSIVE: 58.0 ms frame with 12 page-ins (system swapping: swap 28.0/29.0 GB)"
    );
    // One over-budget frame without page-ins is enough to fail.
    let mixed = [frame(51, Some(4)), frame(53, Some(0))];
    assert!(matches!(
        judge_frames(&mixed, BUDGET, None),
        GateVerdict::Fail(_)
    ));
}

#[test]
fn a_passing_gate_runs_once() {
    let mut runs = 0;
    let out = frame_gate(
        "g",
        BUDGET,
        || None,
        |_| {
            runs += 1;
            vec![frame(20, Some(0))]
        },
    );
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(runs, 1);
}

#[test]
fn a_failing_gate_is_never_retried() {
    let mut runs = 0;
    let out = frame_gate(
        "g",
        BUDGET,
        || None,
        |_| {
            runs += 1;
            vec![frame(70, Some(0))]
        },
    );
    assert!(out.is_err());
    assert_eq!(runs, 1);
}

#[test]
fn an_inconclusive_gate_is_retried_exactly_once_and_both_runs_are_reported() {
    // Retry passes.
    let mut runs = Vec::new();
    let out = frame_gate(
        "import",
        BUDGET,
        || Some((28 * GB, 29 * GB)),
        |attempt| {
            runs.push(attempt);
            if attempt == 1 {
                vec![frame(55, Some(9))]
            } else {
                vec![frame(30, Some(0))]
            }
        },
    );
    assert_eq!(runs, [1, 2]);
    let log = out.unwrap();
    assert!(log.contains("run 1") && log.contains("run 2"), "{log}");
    assert!(
        log.contains("INCONCLUSIVE: 55.0 ms frame with 9 page-ins"),
        "{log}"
    );

    // Retry breaks the budget again (even with page-ins): fails, no third run.
    let mut runs = 0;
    let out = frame_gate(
        "import",
        BUDGET,
        || None,
        |_| {
            runs += 1;
            vec![frame(55, Some(9))]
        },
    );
    assert_eq!(runs, 2);
    let log = out.unwrap_err();
    assert!(log.contains("run 1") && log.contains("run 2"), "{log}");
}
