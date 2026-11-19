#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The thread-spawn lint (ADR-0009, architecture-review.md §3,
//! consolidation plan C19).
//!
//! Background work in mf-desktop goes through the one job system: the
//! serial command queue, `jobs::PreviewSlot`s and `Jobs::once` on the
//! background lane, and the pick/hover lane (`worker::Workers`). Those
//! are panic-safe, cancellable and counted by `settle()`; a raw thread is
//! none of these. So outside the lane modules (`jobs.rs`, `worker.rs`)
//! and startup (`main.rs`), mf-desktop's `src` must not contain:
//!
//! - `thread::spawn(`: a thread nobody can cancel, whose panic is lost,
//!   invisible to `settle()`;
//! - `RunContext::none()`: a context nothing can cancel.
//!
//! [`ALLOWED`] counts the `RunContext::none()` sites still waiting for
//! their move off the UI thread, per file, each naming the task that
//! removes them. It may only shrink: a file with fewer sites than listed
//! fails too, so the count is lowered in the same commit that moves one.
//! No `thread::spawn(` is allowed anywhere outside the lane modules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The modules that own the lanes, and startup.
const LANE_MODULES: &[&str] = &["jobs.rs", "worker.rs", "main.rs"];

/// `(file, RunContext::none() sites, task that removes them)`: all are
/// synchronous calls on the UI thread today (a generator build, a fit
/// check, a sculpt stroke's local work, a region or cap search), which
/// can't be cancelled because nothing else is running them.
const ALLOWED: &[(&str, usize, &str)] = &[
    (
        "connect_tool.rs",
        3,
        "C23: Connect previews from a PreviewSlot",
    ),
    (
        "connector_tool.rs",
        4,
        "C26: generator facts; the fit check on the preview lane",
    ),
    (
        "hollow_tool.rs",
        2,
        "C24: Hollow's region and cap picks on the pick service",
    ),
    (
        "parts_tool.rs",
        2,
        "C26: generator builds on the preview lane",
    ),
    (
        "sculpt_tool.rs",
        3,
        "C24: Sculpt's picks on the pick service",
    ),
    ("shape_panel.rs", 1, "C26: generator facts (attach sides)"),
    (
        "split_flow.rs",
        2,
        "C23: Split and Connect sessions on the plan",
    ),
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src`, by its path relative to `src`.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `pattern`'s occurrences in `text` outside `//` comments.
fn count(text: &str, pattern: &str) -> usize {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .map(|code| code.matches(pattern).count())
        .sum()
}

#[test]
fn no_thread_spawn_or_uncancellable_context_outside_the_lanes() {
    let mut files = Vec::new();
    sources(&src_dir(), &mut files);
    let mut spawns = Vec::new();
    let mut contexts: BTreeMap<String, usize> = BTreeMap::new();
    for path in &files {
        let name = path
            .strip_prefix(src_dir())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if LANE_MODULES.contains(&name.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        if count(&text, "thread::spawn(") > 0 {
            spawns.push(name.clone());
        }
        let n = count(&text, "RunContext::none()");
        if n > 0 {
            contexts.insert(name, n);
        }
    }
    assert!(
        spawns.is_empty(),
        "a raw thread outside the lane modules (use `jobs::Jobs::once` or a \
         `jobs::PreviewSlot`): {spawns:?}"
    );
    let allowed: BTreeMap<String, usize> = ALLOWED
        .iter()
        .map(|(f, n, _)| ((*f).to_owned(), *n))
        .collect();
    let mut wrong = Vec::new();
    for (file, &n) in &contexts {
        match allowed.get(file) {
            None => wrong.push(format!("{file}: {n} new RunContext::none() site(s)")),
            Some(&a) if n > a => wrong.push(format!("{file}: {n} sites, {a} allowed")),
            Some(&a) if n < a => wrong.push(format!(
                "{file}: {n} sites, {a} listed: lower its ALLOWED count"
            )),
            Some(_) => {}
        }
    }
    for file in allowed.keys() {
        if !contexts.contains_key(file) {
            wrong.push(format!("{file}: no sites left: remove it from ALLOWED"));
        }
    }
    assert!(
        wrong.is_empty(),
        "RunContext::none() outside the lane modules (use `jobs::Jobs::once`, a \
         `jobs::PreviewSlot` or the job's own context): {wrong:#?}"
    );
}

/// The lint sees what it should: a seeded spawn and context count, a
/// commented one doesn't.
#[test]
fn the_lint_counts_code_not_comments() {
    assert_eq!(
        count("std::thread::spawn(move || {});", "thread::spawn("),
        1
    );
    assert_eq!(count("// std::thread::spawn(", "thread::spawn("), 0);
    assert_eq!(
        count(
            "let a = RunContext::none(); // RunContext::none()",
            "RunContext::none()"
        ),
        1
    );
}
