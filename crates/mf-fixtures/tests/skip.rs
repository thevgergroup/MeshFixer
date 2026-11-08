#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::skip::record_gpu_skip;

#[test]
fn skips_are_logged_for_the_ci_summary() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("skips.txt");
    record_gpu_skip("a_test", false, Some(&log));
    record_gpu_skip("b_test", false, Some(&log));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "a_test\nb_test\n");
}

#[test]
#[should_panic(expected = "GPU required")]
fn required_gpu_turns_a_skip_into_a_failure() {
    record_gpu_skip("a_test", true, None);
}
