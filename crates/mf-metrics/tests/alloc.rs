#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_metrics::{CountingAlloc, is_installed, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// The counters are process-wide: one test at a time allocates (the RSS
/// test's 64 MB would otherwise land in the other's measured window).
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[test]
fn measure_reports_peak_and_retained() {
    let _serial = serial();
    let (kept, phase) = measure("test", || {
        let big = vec![1u8; 10 << 20];
        std::hint::black_box(&big);
        drop(big);
        vec![2u8; 1 << 20]
    });
    assert!(is_installed());
    assert_eq!(phase.name, "test");
    // Within 64 KB, like `retained` below: the test harness's own threads
    // allocate and free concurrently, and the counters are process-wide.
    assert!(
        phase.peak_bytes >= (10 << 20) - (64 << 10),
        "peak {}",
        phase.peak_bytes
    );
    let retained = phase.retained_bytes as i64;
    assert!(
        (retained - (1 << 20)).abs() < 64 << 10,
        "retained {retained}"
    );
    assert!(phase.wall_s >= 0.0);
    drop(kept);
}

#[test]
fn process_rss_comes_from_the_os() {
    let _serial = serial();
    let before = mf_metrics::process_rss_bytes().unwrap();
    assert!(
        before >= 1 << 20,
        "a running test binary has a few MB resident: {before}"
    );
    // Written page by page, so resident (not just reserved).
    let big = vec![1u8; 64 << 20];
    std::hint::black_box(&big);
    let after = mf_metrics::process_rss_bytes().unwrap();
    assert!(
        after >= before + (16 << 20),
        "touching 64 MB raised RSS from {} to {} MB",
        before >> 20,
        after >> 20
    );
    drop(big);
}
