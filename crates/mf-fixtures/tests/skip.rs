#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::skip::{record_fixture_skip, record_gpu_skip};

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

#[test]
fn missing_fixture_skips_are_logged_too() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("skips.txt");
    let missing = dir.path().join("scan.obj");
    record_fixture_skip("a_test", &missing, false, Some(&log));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "a_test\n");
}

#[test]
#[should_panic(expected = "fixture required")]
fn required_fixtures_turn_a_skip_into_a_failure() {
    record_fixture_skip("a_test", std::path::Path::new("scan.obj"), true, None);
}

#[test]
fn concurrent_skips_are_logged_one_per_line() {
    // Tests run in parallel; each skip must land as one whole line, or the
    // CI summary miscounts (a Windows run logged 3 skips as 2 lines).
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("skips.txt");
    std::thread::scope(|s| {
        for t in 0..16 {
            let log = &log;
            s.spawn(move || {
                for i in 0..50 {
                    record_gpu_skip(&format!("test_{t}_{i}"), false, Some(log));
                }
            });
        }
    });
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 16 * 50);
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("test_") && l.matches('_').count() == 2)
    );
}
