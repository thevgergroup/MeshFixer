#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Job pool, cancellation and progress behaviour (MF-RT-002/003/004).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mf_runtime::{CancelToken, JobPool, Progress, RunContext};

#[test]
fn pool_size_is_clamped_and_reported() {
    // Default: all cores but one.
    let (pool, choice) = JobPool::new(None);
    let cores = choice.cores;
    assert_eq!(choice.threads, cores.saturating_sub(1).max(1));
    assert!(!choice.reason.is_empty());
    drop(pool);

    // Explicit 0 threads clamps to 1.
    let (pool, choice) = JobPool::new(Some(0));
    assert_eq!(choice.threads, 1);
    assert_eq!(choice.cores, cores);
    assert!(choice.reason.contains("0"));
    drop(pool);

    // More than cores is allowed, but called out in the reason.
    let requested = cores + 4;
    let (pool, choice) = JobPool::new(Some(requested));
    assert_eq!(choice.threads, requested);
    assert_eq!(choice.cores, cores);
    assert!(choice.reason.contains("more threads than cores"));
    drop(pool);

    // A plain in-range request is honoured as-is.
    let (pool, choice) = JobPool::new(Some(1));
    assert_eq!(choice.threads, 1);
    drop(pool);
}

#[test]
fn progress_is_monotonic_and_clamped() {
    let progress = Progress::new();

    progress.report("import", 0.2);
    assert_eq!(progress.snapshot(), ("import", 0.2));

    // Fraction is clamped to [0, 1].
    progress.report("import", 2.5);
    assert_eq!(progress.snapshot(), ("import", 1.0));

    // A lower fraction within the same stage never moves progress backwards.
    progress.report("import", 0.1);
    assert_eq!(progress.snapshot(), ("import", 1.0));

    // Negative values clamp to 0, but still can't move an in-progress stage
    // backwards.
    progress.report("import", -1.0);
    assert_eq!(progress.snapshot(), ("import", 1.0));

    // A new stage resets the monotonic floor.
    progress.report("topology", 0.0);
    assert_eq!(progress.snapshot(), ("topology", 0.0));
    progress.report("topology", 0.4);
    assert_eq!(progress.snapshot(), ("topology", 0.4));
    progress.report("topology", 0.3);
    assert_eq!(progress.snapshot(), ("topology", 0.4));
}

#[test]
fn checkpoint_fails_after_cancel() {
    let cancel = CancelToken::new();
    let ctx = RunContext {
        cancel: cancel.clone(),
        progress: Progress::new(),
    };

    assert!(ctx.checkpoint().is_ok());
    assert!(!cancel.is_cancelled());

    cancel.cancel();

    assert!(cancel.is_cancelled());
    let err = ctx
        .checkpoint()
        .expect_err("checkpoint should fail after cancel");
    assert_eq!(err.to_string(), "cancelled by the user");

    // A context that never cancels always checkpoints cleanly.
    let never = RunContext::none();
    assert!(never.checkpoint().is_ok());
}

#[test]
fn pool_runs_rayon_work_on_its_own_threads() {
    let (pool, _choice) = JobPool::new(Some(4));

    let names: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let names_for_closure = Arc::clone(&names);

    pool.install(move || {
        use rayon::prelude::*;
        (0..64).into_par_iter().for_each(|_| {
            if let Some(name) = std::thread::current().name() {
                names_for_closure.lock().unwrap().insert(name.to_string());
            }
        });
    });

    let seen = names.lock().unwrap();
    assert!(
        !seen.is_empty(),
        "expected work to run on named pool threads"
    );
    for name in seen.iter() {
        assert!(
            name.starts_with("mf-pool-"),
            "thread name {name:?} did not match mf-pool-N"
        );
    }
}
