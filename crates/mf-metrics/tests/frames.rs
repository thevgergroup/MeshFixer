#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Frame timing (MF-RT-001, MF-RT-005): the last 1000 frames, CPU, GPU and
//! frame-to-frame interval recorded separately.

use std::time::{Duration, Instant};

use mf_metrics::{FRAME_HISTORY, FrameTimes, GpuSummary, GpuTime};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Frames `1..=n` ms of CPU, started 16 ms apart.
fn filled(n: u64, gpu: impl Fn(u64) -> GpuTime) -> FrameTimes {
    let mut t = FrameTimes::new();
    let start = Instant::now();
    for i in 1..=n {
        let f = t.begin(start + ms(16 * i));
        t.end(f, ms(i), gpu(i));
    }
    t
}

#[test]
fn percentiles_use_nearest_rank() {
    let t = filled(100, |_| GpuTime::NotRendered);
    let cpu = t.cpu().unwrap();
    assert_eq!(cpu.p50(), ms(50));
    assert_eq!(cpu.p95(), ms(95));
    assert_eq!(cpu.p99(), ms(99));
    assert_eq!(cpu.max(), ms(100));
    assert_eq!(cpu.count(), 100);
}

#[test]
fn only_the_last_1000_frames_are_kept() {
    let t = filled(1500, |_| GpuTime::NotRendered);
    assert_eq!(FRAME_HISTORY, 1000);
    assert_eq!(t.len(), 1000);
    let cpu = t.cpu().unwrap();
    // Frames 501..=1500 remain.
    assert_eq!(cpu.max(), ms(1500));
    assert_eq!(cpu.p50(), ms(1000));
}

#[test]
fn interval_is_the_time_between_frame_starts() {
    let t = filled(10, |_| GpuTime::NotRendered);
    let interval = t.interval().unwrap();
    // The first frame has no predecessor, so 9 intervals of 16 ms.
    assert_eq!(interval.count(), 9);
    assert_eq!(interval.p99(), ms(16));
}

#[test]
fn unsupported_gpu_timing_is_reported_as_such_never_as_zero() {
    let t = filled(10, |_| GpuTime::Unsupported);
    assert!(matches!(t.gpu(), GpuSummary::Unsupported));
}

#[test]
fn gpu_times_arrive_later_and_skip_frames_without_a_3d_render() {
    let mut t = filled(10, |i| {
        if i % 2 == 0 {
            GpuTime::Pending
        } else {
            GpuTime::NotRendered
        }
    });
    assert!(matches!(t.gpu(), GpuSummary::NoSamples));
    // Frame indices start at 0: frames 1, 3, 5, 7, 9 had a 3D render.
    for (index, gpu_ms) in [(1, 4), (3, 2), (5, 8), (7, 6)] {
        t.set_gpu(index, ms(gpu_ms));
    }
    // A late result for a frame that rendered nothing is ignored.
    t.set_gpu(0, ms(500));
    let GpuSummary::Measured(gpu) = t.gpu() else {
        panic!("expected measured GPU times");
    };
    assert_eq!(gpu.count(), 4);
    assert_eq!(gpu.max(), ms(8));
    assert_eq!(gpu.p50(), ms(4));
}

#[test]
fn empty_history_has_no_percentiles() {
    let t = FrameTimes::new();
    assert!(t.cpu().is_none());
    assert!(t.interval().is_none());
    assert!(matches!(t.gpu(), GpuSummary::NoSamples));
}
