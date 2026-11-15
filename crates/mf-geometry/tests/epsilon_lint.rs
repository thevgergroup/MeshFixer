//! ADR-0011's epsilon lint: no tolerance literal (`1e-6`, `2.5e-9`, …)
//! in the production code of mf-operations, mf-parts or mf-desktop unless
//! it is on the one reviewed allow-list, `docs/tolerances.allow`, with a
//! reason. "On plane", "same point" and "inside" are decided by
//! `mf_geometry::exact`, not by a distance against an epsilon.
//!
//! The list counts sites per file and literal, so moving a line is free,
//! adding a site fails until it is reviewed onto the list, and removing
//! one fails until its entry is removed too (the list only shrinks).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CRATES: [&str; 3] = ["mf-operations", "mf-parts", "mf-desktop"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Float literals with a negative exponent in `code` (comments removed).
fn literals(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let starts = b[i].is_ascii_digit()
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.'));
        if !starts {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_' || b[i] == b'.') {
            i += 1;
        }
        if i + 1 < b.len()
            && (b[i] == b'e' || b[i] == b'E')
            && b[i + 1] == b'-'
            && b.get(i + 2).is_some_and(u8::is_ascii_digit)
        {
            i += 2;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(code[s..i].to_string());
        }
    }
    out
}

/// `(file relative to crates/, literal)` → sites, in production code
/// (a file's `#[cfg(test)]` tail and comments excluded).
fn found() -> BTreeMap<(String, String), Vec<usize>> {
    let crates = root().join("crates");
    let mut out: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for c in CRATES {
        let mut files = Vec::new();
        rust_files(&crates.join(c).join("src"), &mut files);
        for f in files {
            let rel = f
                .strip_prefix(&crates)
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&f).expect("readable source");
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                let code = line.split("//").next().unwrap_or(line); // allow-swallow: split always yields a first piece
                for lit in literals(code) {
                    out.entry((rel.clone(), lit)).or_default().push(n + 1);
                }
            }
        }
    }
    out
}

/// `(file, literal)` → reviewed sites, from the allow-list.
fn allowed() -> BTreeMap<(String, String), usize> {
    let path = root().join("docs/tolerances.allow");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (the reviewed allow-list)", path.display()));
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(file), Some(lit)) = (words.next(), words.next()) else {
            panic!(
                "docs/tolerances.allow:{}: `<file> <literal> <reason>`",
                n + 1
            );
        };
        assert!(
            words.next().is_some(),
            "docs/tolerances.allow:{}: {file} {lit} has no reason",
            n + 1
        );
        *out.entry((file.to_string(), lit.to_string())).or_insert(0) += 1;
    }
    out
}

#[test]
fn literal_scanner_finds_negative_exponents_only() {
    assert_eq!(literals("a.max(1e-6) + 2.5e-9 * x"), ["1e-6", "2.5e-9"]);
    assert_eq!(
        literals("x1e-6 + 1e6 + 1.0e+3 + v2.e-3"),
        Vec::<String>::new()
    );
    assert_eq!(literals("1_000.0E-12"), ["1_000.0E-12"]);
}

#[test]
fn tolerance_literals_are_on_the_reviewed_allow_list() {
    let found = found();
    let allowed = allowed();
    let mut problems = Vec::new();
    for ((file, lit), lines) in &found {
        let ok = allowed
            .get(&(file.clone(), lit.clone()))
            .copied()
            .unwrap_or(0); // allow-swallow: absent means none reviewed
        if lines.len() > ok {
            problems.push(format!(
                "{file}:{lines:?}: {lit} appears {} times, {ok} reviewed: decide it with \
                 mf_geometry::exact, name it, or add it to docs/tolerances.allow with a reason",
                lines.len()
            ));
        }
    }
    for ((file, lit), &n) in &allowed {
        let have = found.get(&(file.clone(), lit.clone())).map_or(0, Vec::len);
        if have < n {
            problems.push(format!(
                "docs/tolerances.allow lists {n} × {lit} in {file}, the code has {have}: \
                 remove the stale entries (the list only shrinks)"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
