//! The frame-gate policy (MF-RT-001; M4 Task 10 fix round 1, P1).
//!
//! The budget never widens. A frame over budget that waited on page-ins
//! (the process's memory being read back from swap: the machine, not the
//! app) makes a run INCONCLUSIVE; the gate is then run exactly once more,
//! and fails if that run breaks the budget too. An over-budget frame
//! without page-ins (or whose page-ins are unknown) fails at once: no
//! blind retry, and never a pass on a report alone.

use std::fmt::Write as _;
use std::time::Duration;

use crate::duration_ms;

/// One gated frame: how long it took, the page-ins the process had while
/// it ran (`None`: unknown on this platform), and what it was doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedFrame {
    pub time: Duration,
    pub page_ins: Option<u64>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    Pass,
    /// The reason, naming the frame.
    Fail(String),
    /// `INCONCLUSIVE: <ms> frame with N page-ins (system swapping: swap X/Y GB)`.
    Inconclusive(String),
}

fn swap_text(swap: Option<(u64, u64)>) -> String {
    match swap {
        Some((used, total)) => {
            let gb = |b: u64| b as f64 / f64::from(1u32 << 30);
            format!("swap {:.1}/{:.1} GB", gb(used), gb(total))
        }
        None => "swap unknown".into(),
    }
}

/// Judge one run's frames against `budget`. `swap` is (used, total) bytes,
/// for the message.
pub fn judge_frames(
    frames: &[GatedFrame],
    budget: Duration,
    swap: Option<(u64, u64)>,
) -> GateVerdict {
    let over: Vec<&GatedFrame> = frames.iter().filter(|f| f.time > budget).collect();
    if over.is_empty() {
        return GateVerdict::Pass;
    }
    if let Some(f) = over
        .iter()
        .filter(|f| f.page_ins.is_none_or(|p| p == 0))
        .max_by_key(|f| f.time)
    {
        let why = match f.page_ins {
            None => "page-ins unknown",
            Some(_) => "no page-ins",
        };
        return GateVerdict::Fail(format!(
            "{:.1} ms frame ({why}; budget {} ms) at {}",
            duration_ms(f.time),
            budget.as_millis(),
            f.at
        ));
    }
    let worst = over
        .iter()
        .max_by_key(|f| f.time)
        .expect("over is not empty");
    GateVerdict::Inconclusive(format!(
        "INCONCLUSIVE: {:.1} ms frame with {} page-ins (system swapping: {})",
        duration_ms(worst.time),
        worst
            .page_ins
            .expect("every over-budget frame here has page-ins (checked above)"),
        swap_text(swap)
    ))
}

/// Run a frame gate: `run(attempt)` produces one run's frames (attempt 1,
/// and 2 only after an inconclusive first run). `Ok` or `Err` carries the
/// log of every run, to print either way.
pub fn frame_gate(
    what: &str,
    budget: Duration,
    swap: impl Fn() -> Option<(u64, u64)>,
    mut run: impl FnMut(u32) -> Vec<GatedFrame>,
) -> Result<String, String> {
    let mut log = String::new();
    for attempt in 1..=2 {
        let frames = run(attempt);
        let worst = frames.iter().max_by_key(|f| f.time);
        // allow-swallow: writing to a String can't fail
        let _ = writeln!(
            log,
            "{what}, run {attempt}: {} frames, worst {}",
            frames.len(),
            worst.map_or("none".into(), |f| format!(
                "{:.1} ms ({} page-ins) at {}",
                duration_ms(f.time),
                f.page_ins.map_or("unknown".into(), |p| p.to_string()),
                f.at
            ))
        );
        match judge_frames(&frames, budget, swap()) {
            GateVerdict::Pass => return Ok(log),
            GateVerdict::Fail(why) => {
                // allow-swallow: writing to a String can't fail
                let _ = writeln!(log, "{what}, run {attempt}: FAILED: {why}");
                return Err(log);
            }
            GateVerdict::Inconclusive(msg) => {
                // allow-swallow: writing to a String can't fail
                let _ = writeln!(log, "{what}, run {attempt}: {msg}");
                if attempt == 2 {
                    // allow-swallow: writing to a String can't fail
                    let _ = writeln!(
                        log,
                        "{what}: FAILED: the retry broke the {} ms budget too",
                        budget.as_millis()
                    );
                    return Err(log);
                }
            }
        }
    }
    unreachable!("the second run always returns")
}
