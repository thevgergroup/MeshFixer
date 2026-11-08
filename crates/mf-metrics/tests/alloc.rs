#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_metrics::{CountingAlloc, is_installed, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn measure_reports_peak_and_retained() {
    let (kept, phase) = measure("test", || {
        let big = vec![1u8; 10 << 20];
        std::hint::black_box(&big);
        drop(big);
        vec![2u8; 1 << 20]
    });
    assert!(is_installed());
    assert_eq!(phase.name, "test");
    assert!(phase.peak_bytes >= 10 << 20, "peak {}", phase.peak_bytes);
    let retained = phase.retained_bytes as i64;
    assert!(
        (retained - (1 << 20)).abs() < 64 << 10,
        "retained {retained}"
    );
    assert!(phase.wall_s >= 0.0);
    drop(kept);
}
