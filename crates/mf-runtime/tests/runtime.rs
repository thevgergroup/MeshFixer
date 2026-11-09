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
fn progress_snapshot_never_tears_under_concurrent_reports() {
    // Two stages with disjoint fraction ranges, so any snapshot that pairs
    // the "wrong" stage with the "wrong" fraction range proves stage and
    // fraction were updated/read as two separable pieces of state rather
    // than one atomic unit (a torn read, or a stale same-stage write that
    // lands after a concurrent stage transition).
    //
    // "low": fraction always in [0.0, 0.1]. "high": fraction always in
    // [0.5, 1.0]. A correct snapshot is always ("low", f <= 0.1) or
    // ("high", f >= 0.5) — never ("low", f > 0.1) or ("high", f < 0.5).
    const ITERS: usize = 20_000;

    for attempt in 0..20 {
        let progress = Progress::new();
        let reporter_progress = progress.clone();
        let snapshot_progress = progress.clone();

        let reporter = std::thread::spawn(move || {
            for i in 0..ITERS {
                if i % 2 == 0 {
                    reporter_progress.report("low", (i % 100) as f64 / 1000.0);
                } else {
                    reporter_progress.report("high", 0.5 + (i % 100) as f64 / 1000.0);
                }
            }
        });

        let reader = std::thread::spawn(move || {
            let mut torn = Vec::new();
            for _ in 0..ITERS {
                let (stage, fraction) = snapshot_progress.snapshot();
                let bad = match stage {
                    "low" => fraction > 0.1,
                    "high" => fraction < 0.5,
                    _ => false,
                };
                if bad {
                    torn.push((stage, fraction));
                }
            }
            torn
        });

        reporter.join().expect("reporter thread panicked");
        let torn = reader.join().expect("reader thread panicked");

        assert!(
            torn.is_empty(),
            "attempt {attempt}: observed torn (stage, fraction) pairs: {torn:?}"
        );
    }
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

/// Several writers report into one `Progress` at once (as rayon workers do
/// inside a parallel operation), first all in the same stage, then racing
/// across stage transitions. Every snapshot must pair a stage with one of
/// its own fractions, and within a stage (while no other stage is being
/// reported) a snapshot never goes backwards.
#[test]
fn progress_is_consistent_with_many_concurrent_reporters() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    const WRITERS: usize = 8;
    const ITERS: usize = 20_000;
    const STAGES: [&str; 4] = ["s0", "s1", "s2", "s3"];
    // Stage k only ever reports fractions in [k/4, k/4 + 0.2]: a snapshot
    // outside its stage's range pairs a stage with another one's fraction.
    let range = |stage: &str| -> Option<(f64, f64)> {
        let k = STAGES.iter().position(|&s| s == stage)? as f64;
        Some((k * 0.25, k * 0.25 + 0.2))
    };
    // A cheap deterministic per-thread sequence (fractions arrive out of
    // order across threads, and also within one thread).
    let fraction = |stage: usize, thread: usize, i: usize| -> f64 {
        let jitter = ((i * 7919 + thread * 104_729) % 1000) as f64 / 1000.0;
        stage as f64 * 0.25 + 0.2 * jitter
    };

    // Part 1: same-stage concurrency, stages in order with a barrier
    // between them. A channel reader must see the same: updates in the
    // order they were applied.
    let progress = Progress::new();
    let (progress, channel) = progress.channel();
    let barrier = Arc::new(Barrier::new(WRITERS));
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let progress = progress.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut problems = Vec::new();
            let mut last: Option<(&'static str, f64)> = None;
            while !done.load(Ordering::Acquire) {
                let (stage, f) = progress.snapshot();
                if stage.is_empty() {
                    continue;
                }
                match range(stage) {
                    Some((lo, hi)) if f >= lo && f <= hi => {}
                    _ => problems.push(format!("torn: {stage} with {f}")),
                }
                if let Some((ls, lf)) = last {
                    let (li, si) = (
                        STAGES.iter().position(|&s| s == ls),
                        STAGES.iter().position(|&s| s == stage),
                    );
                    if si < li {
                        problems.push(format!("stage went back: {ls} -> {stage}"));
                    }
                    if ls == stage && f < lf {
                        problems.push(format!("{stage} regressed: {lf} -> {f}"));
                    }
                }
                last = Some((stage, f));
            }
            problems
        })
    };
    let writers: Vec<_> = (0..WRITERS)
        .map(|t| {
            let progress = progress.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                for (k, &stage) in STAGES.iter().enumerate() {
                    barrier.wait();
                    for i in 0..ITERS {
                        progress.report(stage, fraction(k, t, i));
                    }
                    barrier.wait();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    done.store(true, Ordering::Release);
    let problems = reader.join().unwrap();
    assert!(
        problems.is_empty(),
        "{:?}",
        &problems[..problems.len().min(10)]
    );
    // The highest value any writer reported in the last stage won.
    let max_last = (0..WRITERS)
        .flat_map(|t| (0..ITERS).map(move |i| fraction(3, t, i)))
        .fold(0.0f64, f64::max);
    let (stage, f) = progress.snapshot();
    assert_eq!(stage, "s3");
    assert!((f - max_last).abs() < 1e-6, "{f} vs {max_last}");
    let updates: Vec<_> = std::iter::from_fn(|| channel.try_recv()).collect();
    assert_eq!(updates.len(), WRITERS * ITERS * STAGES.len());
    for w in updates.windows(2) {
        let (ls, lf) = w[0];
        let (s, f) = w[1];
        let (li, si) = (
            STAGES.iter().position(|&x| x == ls).unwrap(),
            STAGES.iter().position(|&x| x == s).unwrap(),
        );
        assert!(si >= li, "channel: stage went back: {w:?}");
        assert!(ls != s || f >= lf, "channel: {s} regressed: {w:?}");
        let (lo, hi) = range(s).unwrap();
        assert!(f >= lo && f <= hi, "channel: torn: {w:?}");
    }
    assert_eq!(updates.last().copied(), Some(progress.snapshot()));

    // Part 2: writers race across stage transitions (each thread walks the
    // stages at its own pace, so stale writers of an earlier stage overlap
    // fresh writers of a later one). Pairing must still never tear.
    let progress = Progress::new();
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let progress = progress.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut torn = Vec::new();
            while !done.load(Ordering::Acquire) {
                let (stage, f) = progress.snapshot();
                if stage.is_empty() {
                    continue;
                }
                match range(stage) {
                    Some((lo, hi)) if f >= lo && f <= hi => {}
                    _ => torn.push((stage, f)),
                }
            }
            torn
        })
    };
    let writers: Vec<_> = (0..WRITERS)
        .map(|t| {
            let progress = progress.clone();
            std::thread::spawn(move || {
                for i in 0..ITERS * 2 {
                    // Thread t moves to the next stage every (t + 1) * 1000
                    // reports, so stages overlap across threads.
                    let k = (i / ((t + 1) * 1000)) % STAGES.len();
                    progress.report(STAGES[k], fraction(k, t, i));
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    done.store(true, Ordering::Release);
    let torn = reader.join().unwrap();
    assert!(torn.is_empty(), "{:?}", &torn[..torn.len().min(10)]);
    let (stage, f) = progress.snapshot();
    let (lo, hi) = range(stage).unwrap();
    assert!(f >= lo && f <= hi, "final {stage} with {f}");
}

#[test]
fn channel_sends_the_fraction_that_was_committed() {
    let (progress, reader) = Progress::new().channel();
    progress.report("a", 0.5);
    progress.report("a", 0.25); // held at 0.5
    progress.report("b", 0.1); // a new stage resets
    let updates: Vec<_> = std::iter::from_fn(|| reader.try_recv()).collect();
    assert_eq!(updates, [("a", 0.5), ("a", 0.5), ("b", 0.1)]);
}

#[test]
fn instrumented_context_records_the_largest_checkpoint_gap() {
    let (ctx, probe) = RunContext::instrumented();
    ctx.checkpoint().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    ctx.checkpoint().unwrap();
    ctx.checkpoint().unwrap();
    let worst = probe.worst().unwrap();
    assert!(
        worst.gap >= std::time::Duration::from_millis(30),
        "{worst:?}"
    );
    assert!(
        worst.gap < std::time::Duration::from_millis(500),
        "{worst:?}"
    );
    assert_eq!(probe.max_gap(), worst.gap);
    assert_eq!(probe.checkpoints(), 3);
    // Both ends of the gap name this file.
    assert!(worst.to.file().ends_with("runtime.rs"), "{worst:?}");
    // Clones share the probe; an uninstrumented context records nothing.
    let clone = ctx.clone();
    clone.checkpoint().unwrap();
    assert_eq!(probe.checkpoints(), 4);
}

#[test]
fn cancellable_sort_matches_std_sort() {
    use mf_runtime::par_sort_unstable_by_key;
    let ctx = RunContext::none();
    for n in [0usize, 1, 2, 7, 1000, 40_000, 300_000, 1_000_003] {
        // Many duplicates, a sorted run and a reversed run.
        let mut v: Vec<(u32, u32)> = (0..n as u32)
            .map(|i| match i % 3 {
                0 => (i.wrapping_mul(2_654_435_761) % 1000, i % 7),
                1 => (i, 0),
                _ => (n as u32 - i, 1),
            })
            .collect();
        let mut expected = v.clone();
        expected.sort_unstable();
        par_sort_unstable_by_key(&mut v, &ctx, |x| *x).unwrap();
        assert_eq!(v, expected, "n = {n}");
    }
    // All equal.
    let mut v = vec![5u8; 200_000];
    par_sort_unstable_by_key(&mut v, &ctx, |x| *x).unwrap();
    assert!(v.iter().all(|&x| x == 5));
}

#[test]
fn cancellable_sort_stops_when_cancelled() {
    use mf_runtime::par_sort_unstable_by_key;
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    let mut v: Vec<u64> = (0..2_000_000u64).rev().collect();
    assert!(par_sort_unstable_by_key(&mut v, &ctx, |x| *x).is_err());
    // Small inputs are cancellable too (one checkpoint up front).
    let mut small = vec![3, 2, 1];
    assert!(par_sort_unstable_by_key(&mut small, &ctx, |x| *x).is_err());
}

#[test]
fn ticker_checkpoints_every_32k_ticks() {
    let ctx = RunContext::none();
    let mut ticker = ctx.ticker();
    for _ in 0..100_000 {
        ticker.tick().unwrap();
    }
    ctx.cancel.cancel();
    let mut ticker = ctx.ticker();
    let mut ok = 0;
    while ticker.tick().is_ok() {
        ok += 1;
    }
    assert!(ok < 32_768, "{ok}");
}
