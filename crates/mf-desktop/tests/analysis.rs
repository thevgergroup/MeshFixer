#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mf_desktop::analysis::{AnalysisCache, LatestWorker};
use mf_fixtures::{cube, tetrahedron};

#[test]
fn latest_worker_bounds_work_and_always_runs_the_last_request() {
    let started = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let max_running = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = channel();
    let (s, r, m) = (started.clone(), running.clone(), max_running.clone());
    let worker = LatestWorker::new(
        move |n: u32| {
            s.fetch_add(1, Ordering::SeqCst);
            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            r.fetch_sub(1, Ordering::SeqCst);
            n
        },
        move |n| tx.send(n).unwrap(),
    );
    for n in 0..50 {
        worker.submit(n);
    }
    let mut results = Vec::new();
    while let Ok(n) = rx.recv_timeout(Duration::from_secs(5)) {
        results.push(n);
        if n == 49 {
            break;
        }
    }
    assert_eq!(results.last(), Some(&49), "the latest request must run");
    assert!(
        started.load(Ordering::SeqCst) <= 3,
        "superseded requests are dropped, ran {}",
        started.load(Ordering::SeqCst)
    );
    assert_eq!(
        max_running.load(Ordering::SeqCst),
        1,
        "one analysis at a time"
    );
}

#[test]
fn cache_only_answers_for_the_exact_revision() {
    let a = cube(1.0);
    let b = tetrahedron();
    let mut cache: AnalysisCache<&'static str> = AnalysisCache::new(2);
    cache.insert(a.clone(), "A");
    assert_eq!(cache.get(&a), Some(&"A"));
    // Same content, different revision object: not a hit (identity is by revision, not by value).
    assert_eq!(
        cache.get(&{
            let (p, t) = a.to_arrays();
            mf_geometry::Mesh::new(p, t).unwrap()
        }),
        None
    );
    assert_eq!(
        cache.get(&b),
        None,
        "a stale result for A never answers for B"
    );
    cache.insert(b.clone(), "B");
    cache.insert(cube(2.0), "C");
    assert_eq!(cache.get(&a), None, "evicted beyond capacity");
    assert_eq!(cache.get(&b), Some(&"B"));
    let _ = Mutex::new(());
}
