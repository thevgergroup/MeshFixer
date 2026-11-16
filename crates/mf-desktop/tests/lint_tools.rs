#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The tool-module lint (ADR-0007, consolidation plan C21).
//!
//! `app.rs` is the shell's host: it keeps no per-tool fields, hand-called
//! lists or tool-specific code. A tool reaches it only through the shell
//! (`shell::Tool` and its `ToolSession`), so `app.rs` may not name a tool
//! module (`<module>::` anywhere in it, a `use` included).
//!
//! The tool modules are every module that registers a tool on the rail,
//! every `*_tool.rs`, and the per-tool modules around them (`split_flow`:
//! Split and Connect's workflow; `shape_panel`: the generator section).
//! [`ALLOWED`] lists the ones still named, each with the task that
//! removes it; it may only shrink (an entry no longer needed fails too).
//! `tool_panels` is the shell's own chrome (the rail, the tool dock, the
//! inspector card) besides the built-in tools' panels: `app.rs` may name
//! only its chrome ([`CHROME`]).

use std::path::{Path, PathBuf};

/// Per-tool modules that don't register a tool themselves.
const PER_TOOL: &[&str] = &["split_flow", "shape_panel"];

/// `(module, task that removes it from app.rs)`.
const ALLOWED: &[(&str, &str)] = &[
    (
        "shape_tools",
        "C23: Split's session on the plan (the plane, pegs and section view)",
    ),
    (
        "split_flow",
        "C23: Split and Connect sessions on the plan (the workflow's state)",
    ),
    (
        "hollow_tool",
        "C24: Hollow's session (its preview slot and state)",
    ),
    (
        "sculpt_tool",
        "C24: Sculpt's session (its stroke, baseline and settings)",
    ),
    (
        "hinge_tool",
        "C26: generator facts (the hinge's state and swing)",
    ),
    (
        "shape_panel",
        "C26: generator facts (the shape draft and reconcile after a move)",
    ),
];

/// What `app.rs` may name of `tool_panels` once only the chrome is left.
const CHROME: &[&str] = &[
    "rail",
    "tool_dock",
    "inspector_card",
    "first_use_hint",
    "NARROW_WINDOW",
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The tool modules, by module name.
fn tool_modules() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(src_dir()).unwrap() {
        let path = entry.unwrap().path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".rs"))
        else {
            continue;
        };
        if name == "shell" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if name.ends_with("_tool") || PER_TOOL.contains(&name) || text.contains("register_tool(") {
            out.push(name.to_owned());
        }
    }
    out.sort();
    out
}

/// The names `app.rs` uses from module `m` (`m::Name`), each once.
fn uses_of(app: &str, m: &str) -> Vec<String> {
    let needle = format!("{m}::");
    let mut names: Vec<String> = Vec::new();
    let mut rest = app;
    while let Some(i) = rest.find(&needle) {
        let before = rest[..i].chars().next_back();
        let after = &rest[i + needle.len()..];
        rest = after;
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue; // a longer name ending in `m`
        }
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '{')
            .collect();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[test]
fn app_rs_names_no_tool_module() {
    let app = std::fs::read_to_string(src_dir().join("app.rs")).unwrap();
    let modules = tool_modules();
    assert!(
        modules.iter().any(|m| m == "align_tool") && modules.iter().any(|m| m == "tool_panels"),
        "the tool modules are found: {modules:?}"
    );
    let mut bad = Vec::new();
    for m in &modules {
        let used = uses_of(&app, m);
        let allowed = ALLOWED.iter().any(|(a, _)| a == m);
        if m == "tool_panels" && !allowed {
            let outside: Vec<&String> = used
                .iter()
                .filter(|n| !CHROME.contains(&n.as_str()))
                .collect();
            if !outside.is_empty() {
                bad.push(format!(
                    "app.rs names tool_panels::{outside:?} (only its chrome)"
                ));
            }
            continue;
        }
        if !used.is_empty() && !allowed {
            bad.push(format!(
                "app.rs names {m}::{used:?}: route it through the shell (ToolSession)"
            ));
        }
        if used.is_empty() && allowed {
            bad.push(format!(
                "{m} is allow-listed but app.rs no longer names it: drop its entry"
            ));
        }
    }
    for (a, task) in ALLOWED {
        assert!(!task.is_empty(), "{a}: every entry names its task");
        if !modules.iter().any(|m| m == a) {
            bad.push(format!("{a} is allow-listed but isn't a tool module"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
