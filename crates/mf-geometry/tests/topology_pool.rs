#![allow(clippy::unwrap_used)] // tests may panic on failure

//! A mesh's topology asked for by several callers at once (M4 Task 8a, fix
//! round 1, C1). No caller ever waits for another caller's computation:
//! inside a thread pool, the computing caller may itself be waiting (in a
//! rayon join) for the very worker that would be waiting for it, and the
//! pool deadlocks ("Analyzing…" forever). Each caller computes, or takes
//! what's already published; the first result published is the one kept.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use mf_runtime::RunContext;

/// 2M triangles: computing its topology takes long enough to overlap.
fn big() -> mf_geometry::Mesh {
    mf_fixtures::grid(1000)
}

/// While one thread computes the topology, a second caller doesn't wait
/// for it: it computes its own (its progress moves through the stage)
/// and finishes, even if the first is held up indefinitely. With a lock
/// held while computing, the second caller only ever reports the cached
/// result, after the first one finished.
#[test]
fn a_second_caller_never_waits_for_another_threads_computation() {
    let mesh = big();
    let first = RunContext::none();
    let (tx, rx) = channel();
    {
        let (m, ctx, tx) = (mesh.clone(), first.clone(), tx.clone());
        std::thread::spawn(move || {
            let r = m.topology_with(&ctx).map(|t| t.components.len());
            tx.send(("first", r.ok(), Instant::now())).unwrap();
        });
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(first.progress.snapshot(), ("topology", f) if f >= 0.1) {
        assert!(Instant::now() < deadline, "the first caller never started");
        std::thread::yield_now();
    }
    let second = RunContext::none();
    let (_, updates) = second.progress.channel();
    {
        let (m, tx) = (mesh.clone(), tx.clone());
        std::thread::spawn(move || {
            let r = m.topology_with(&second).map(|t| t.components.len());
            tx.send(("second", r.ok(), Instant::now())).unwrap();
        });
    }
    // The second caller works on its own: it reports progress within the
    // stage (not just "done"), within a few seconds, while the first is
    // still busy or not.
    let mut computing = false;
    while Instant::now() < deadline && !computing {
        if let Some((stage, f)) = updates.try_recv() {
            computing = stage == "topology" && f > 0.0 && f < 1.0;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(
        computing,
        "the second caller waited for the first one's computation instead of computing"
    );
    for _ in 0..2 {
        let (who, r, _) = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(r, Some(1), "{who}");
    }
    assert!(mesh.has_cached_topology());
}

/// Many requests on one pool while one computes (the overlay-vs-analysis
/// shape): every one finishes, all with the same published topology.
#[test]
fn topology_requests_on_one_pool_all_finish() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .unwrap();
    let mesh = big();
    let deadline = Instant::now() + Duration::from_secs(120);
    for round in 0..4 {
        mesh.release_topology();
        let (tx, rx) = channel();
        let first = RunContext::none();
        {
            let (m, tx, ctx) = (mesh.clone(), tx.clone(), first.clone());
            pool.spawn(move || {
                tx.send(m.topology_with(&ctx).unwrap()).unwrap();
            });
        }
        while !matches!(first.progress.snapshot(), ("topology", f) if f >= 0.1) {
            assert!(Instant::now() < deadline, "round {round}: never started");
            std::thread::yield_now();
        }
        let jobs = 16;
        for _ in 1..jobs {
            let (m, tx) = (mesh.clone(), tx.clone());
            pool.spawn(move || {
                tx.send(m.topology()).unwrap();
            });
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut results = Vec::new();
        for done in 0..jobs {
            let left = deadline.saturating_duration_since(Instant::now());
            results.push(rx.recv_timeout(left).unwrap_or_else(|_| {
                panic!("round {round}: {done} of {jobs} requests finished: the pool deadlocked")
            }));
        }
        assert!(mesh.has_cached_topology(), "round {round}: published");
        let published = mesh.topology();
        // Everyone after the publication got the published one; every
        // result is the same topology.
        assert!(results.iter().all(|t| **t == *published));
    }
}
