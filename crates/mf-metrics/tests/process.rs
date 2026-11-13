#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The OS's figures for this process (I1, P1): resident, footprint on
//! macOS, page-ins where the OS counts them.

#[test]
fn process_memory_reports_resident_footprint_and_page_ins() {
    let m = mf_metrics::process_memory().unwrap();
    assert!(m.resident_bytes > 0, "{m:?}");
    if cfg!(target_os = "macos") {
        let footprint = m.phys_footprint_bytes.unwrap();
        assert!(
            footprint > 0 && m.peak_phys_footprint_bytes.unwrap() >= footprint,
            "{m:?}"
        );
    }
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        // A counter: never goes down.
        let before = m.page_ins.unwrap();
        assert!(mf_metrics::page_ins().unwrap() >= before);
    }
}
