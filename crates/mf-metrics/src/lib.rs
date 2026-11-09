//! Memory and timing instrumentation (MF-RT-001, MF-RT-005, MF-ISSUE-013).
//!
//! [`CountingAlloc`] counts live and peak heap bytes when a binary installs
//! it as its `#[global_allocator]`. [`measure`] reports a phase's wall time,
//! its peak heap above the starting level, and the bytes it retained.
//! Phases are meant to run one after another: `measure` resets the peak.
//! [`FrameTimes`] records the desktop app's recent frames.

mod alloc;
mod frames;
mod process;

use std::time::Instant;

use serde::Serialize;

pub use process::process_rss_bytes;

pub use frames::{FRAME_HISTORY, FrameSample, FrameTimes, GpuSummary, GpuTime, Percentiles};

pub use alloc::{
    CountingAlloc, is_installed, live_bytes, peak_bytes, peak_bytes_since_start, reset_peak,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Phase {
    pub name: String,
    pub wall_s: f64,
    /// Peak heap above the live bytes at the start of the phase.
    pub peak_bytes: u64,
    /// Live bytes at the end minus at the start (0 if the phase freed memory).
    pub retained_bytes: u64,
}

pub fn measure<R>(name: &str, f: impl FnOnce() -> R) -> (R, Phase) {
    let start_live = live_bytes();
    reset_peak();
    let start = Instant::now();
    let result = f();
    let wall_s = start.elapsed().as_secs_f64();
    let phase = Phase {
        name: name.to_owned(),
        wall_s,
        peak_bytes: peak_bytes().saturating_sub(start_live),
        retained_bytes: live_bytes().saturating_sub(start_live),
    };
    (result, phase)
}
