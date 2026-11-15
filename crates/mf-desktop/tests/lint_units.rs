#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The desktop conversion-scan lint (ADR-0006 rule 6, units-audit.md
//! §6.6 item 2, consolidation plan C13).
//!
//! Tool code must not convert lengths itself (the one conversion is
//! `Session::prepare`, reached through `crate::intent::Intent`), must not
//! use a fixed snap or fixed display decimals, and must not give a
//! length widget a literal float range in document units. This scans
//! every tool file for:
//!
//! - `mm_per(`, `split_flow::mm(` (and its imported alias `mm(app)`),
//!   `scale_linear(`: a panel-side unit conversion;
//! - `Snap::default()`: a fixed 1-unit snap;
//! - `.max_decimals(`: fixed display precision instead of `nice_step`;
//! - `DragValue::new` / `Slider::new` with a float-literal range.
//!
//! [`ALLOWED`] lists the sites still waiting for their panel migration,
//! each naming the task that removes it. It may only shrink: an entry
//! whose pattern no longer appears in its file fails the lint too, so
//! the list is cleaned up in the same commit that migrates the site.

use std::path::{Path, PathBuf};

/// `(file, pattern, task that removes it)`. Must reach empty (C17).
const ALLOWED: &[(&str, &str, &str)] = &[
    // C17: panel migration B (Sculpt, Split, shape tools, Transform,
    // Hollow).
    ("hollow_tool.rs", "mm_per(", "C17"),
    ("hollow_tool.rs", "literal-range", "C17"),
    ("hollow_tool.rs", ".max_decimals(", "C17"),
    ("sculpt_tool.rs", "literal-range", "C17"),
    ("sculpt_tool.rs", ".max_decimals(", "C17"),
    ("shape_panel.rs", ".max_decimals(", "C17"),
    ("shape_tools.rs", "Snap::default()", "C17"),
    ("shape_tools.rs", "literal-range", "C17"),
    ("shape_tools.rs", ".max_decimals(", "C17"),
    ("split_flow.rs", "mm_per(", "C17"),
    ("split_flow.rs", "mm(app)", "C17"),
    ("split_flow.rs", "Snap::default()", "C17"),
    ("split_flow.rs", "literal-range", "C17"),
    ("split_flow.rs", ".max_decimals(", "C17"),
    ("transform_tool.rs", "mm_per(", "C17"),
    // The definition only; its one caller is app.rs's unit rescale.
    ("transform_tool.rs", "scale_linear(", "C17"),
];

/// The plain-text patterns, matched outside `//` comments.
const PATTERNS: &[&str] = &[
    "mm_per(",
    "split_flow::mm(",
    "mm(app)",
    "scale_linear(",
    "Snap::default()",
    ".max_decimals(",
];

/// The pseudo-pattern name for a float-literal widget range.
const LITERAL_RANGE: &str = "literal-range";

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every tool file: `*_tool.rs`, plus the other panels that build tool
/// invocations or length widgets.
fn tool_files() -> Vec<PathBuf> {
    let extra = [
        "split_flow.rs",
        "shape_tools.rs",
        "shape_panel.rs",
        "tool_panels.rs",
        "import_dialog.rs",
    ];
    let mut files: Vec<PathBuf> = std::fs::read_dir(src_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.ends_with("_tool.rs") || extra.contains(&name)
        })
        .collect();
    files.sort();
    files
}

/// `text` with every `//` comment removed (line by line), so a comment
/// explaining a banned pattern doesn't trip the lint.
fn code_only(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The 1-based line numbers where `pattern` occurs in `code`.
fn lines_with(code: &str, pattern: &str) -> Vec<usize> {
    code.lines()
        .enumerate()
        .filter(|(_, l)| l.contains(pattern))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Whether `s` starts with a float literal (`0.5`, `200.0`, `1e-3`,
/// `-2.5_f64`), returning its length.
fn float_literal_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i += 1;
    }
    let start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i == start {
        return None;
    }
    let mut float = false;
    if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        float = true;
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        float = true;
        i += 1;
        if i < b.len() && b[i] == b'-' {
            i += 1;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if s[i..].starts_with("_f64") || s[i..].starts_with("f64") {
        float = true;
        i += if s[i..].starts_with('_') { 4 } else { 3 };
    }
    float.then_some(i)
}

/// Whether a range bound expression is a literal length: a float
/// literal (`200.0`), or one converted by a unit factor (`0.4 / mm`,
/// `10.0 * k`). A bound computed from the model (`wall / 16.0`,
/// `diag`) is not.
fn is_literal_bound(expr: &str) -> bool {
    let mut e: String = expr.chars().filter(|c| !c.is_whitespace()).collect();
    while e.starts_with('(') && e.ends_with(')') {
        e = e[1..e.len() - 1].to_owned();
    }
    let Some(n) = float_literal_len(&e) else {
        return false;
    };
    let rest = &e[n..];
    if rest.is_empty() {
        return true;
    }
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (rest.starts_with('/') || rest.starts_with('*')) && ident(&rest[1..])
}

/// Whether `window` (a widget constructor and its builder chain) holds
/// a range with a literal bound: `0.1..=200.0`, `x..=5.0`,
/// `(0.4 / mm)..=(10.0 / mm)`.
fn has_literal_range(window: &str) -> bool {
    let b = window.as_bytes();
    for (i, _) in window.match_indices("..") {
        if i > 0 && b[i - 1] == b'.' {
            continue;
        }
        // The lower bound: back to the enclosing `(` or `,`.
        let mut depth = 0i32;
        let mut start = 0;
        for (k, c) in window[..i].char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' if depth == 0 => {
                    start = k + 1;
                    break;
                }
                '(' => depth -= 1,
                ',' if depth == 0 => {
                    start = k + 1;
                    break;
                }
                _ => {}
            }
        }
        // The upper bound: on to the enclosing `)` or `,`.
        let mut j = i + 2;
        if j < b.len() && b[j] == b'=' {
            j += 1;
        }
        let mut depth = 0i32;
        let mut end = window.len();
        for (k, c) in window[j..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = j + k;
                    break;
                }
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    end = j + k;
                    break;
                }
                _ => {}
            }
        }
        if is_literal_bound(&window[start..i]) || is_literal_bound(&window[j..end]) {
            return true;
        }
    }
    false
}

/// The 1-based lines of each `DragValue::new` / `Slider::new` whose
/// builder chain (up to the end of the statement) has a float-literal
/// range.
fn literal_range_lines(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for ctor in ["DragValue::new(", "Slider::new("] {
        for (at, _) in code.match_indices(ctor) {
            let rest = &code[at..];
            let end = rest.find(';').unwrap_or(rest.len()).min(600);
            if has_literal_range(&rest[..end]) {
                out.push(code[..at].lines().count().max(1));
            }
        }
    }
    out.sort_unstable();
    out
}

#[test]
fn literal_range_detector() {
    assert!(has_literal_range("Slider::new(&mut v, 0.1..=200.0)"));
    assert!(has_literal_range(
        "DragValue::new(&mut v).range(lo..=5.0).speed(0.1)"
    ));
    assert!(has_literal_range("DragValue::new(&mut v).range(0.0..=hi)"));
    assert!(!has_literal_range("Slider::new(&mut n, 1..=64)"));
    assert!(!has_literal_range("Slider::new(&mut v, lo..=hi)"));
    assert!(!has_literal_range("DragValue::new(&mut v).speed(0.1)"));
    assert!(has_literal_range(
        "Slider::new(&mut wall, (0.4 / mm)..=(10.0 / mm))"
    ));
    assert!(!has_literal_range(
        "Slider::new(&mut cell, (wall / 16.0)..=(wall / 2.0))"
    ));
}

#[test]
fn tool_files_do_not_convert_units_or_hard_code_scale() {
    let mut violations = Vec::new();
    let mut seen: Vec<(String, &str)> = Vec::new();
    for path in tool_files() {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let code = code_only(&std::fs::read_to_string(&path).unwrap());
        let mut found: Vec<(&str, Vec<usize>)> = PATTERNS
            .iter()
            .map(|&p| (p, lines_with(&code, p)))
            .collect();
        found.push((LITERAL_RANGE, literal_range_lines(&code)));
        for (pattern, lines) in found {
            if lines.is_empty() {
                continue;
            }
            seen.push((name.clone(), pattern));
            if !ALLOWED.iter().any(|&(f, p, _)| f == name && p == pattern) {
                violations.push(format!("{name}: `{pattern}` at lines {lines:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "unit/scale rules broken in tool code (ADR-0006: convert only in \
         `Session::prepare` via `Intent`, use `length_field`, `nice_step`, \
         `snap_step`):\n{}",
        violations.join("\n")
    );
    let stale: Vec<String> = ALLOWED
        .iter()
        .filter(|&&(f, p, _)| !seen.iter().any(|(n, s)| n == f && *s == p))
        .map(|&(f, p, task)| format!("{f}: `{p}` ({task})"))
        .collect();
    assert!(
        stale.is_empty(),
        "these allow-list entries no longer match anything; remove them so \
         the list only shrinks:\n{}",
        stale.join("\n")
    );
}

#[test]
fn every_allow_list_entry_names_its_task() {
    for &(file, pattern, task) in ALLOWED {
        assert!(
            task == "C15" || task == "C17",
            "{file} `{pattern}`: allow-list entries must name the task that removes them, got {task}"
        );
    }
}
