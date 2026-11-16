//! ADR-0010's refusal registry (consolidation plan C14): every refusal
//! code an operation's own source constructs with a literal string
//! (`refuse(`, `refuse_plain(`, `refuse_object(`, `refuse_at(`,
//! `refuse_naming(`, or `crate::refusal::refuse(` directly) must be
//! declared — a message id and a `next` — in a spec that op is known to
//! run under (`every_refusal_code_is_declared`). `mf-spec`'s own
//! `every_declared_refusal_has_a_message_and_next` checks the other
//! half: that every declared code is well formed.
//!
//! A lint checks the other direction: no `OpError::Failed` is
//! constructed outside its reviewed allow-list
//! (`docs/failed-refusals.allow`), so a future refusal can't quietly
//! skip the registry the way every one of these once did.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Literal string codes passed as the first argument to `name(` in
/// `text` (comments and `#[cfg(test)]` tails excluded; a dynamic code,
/// such as `e.code()` or a `code` variable, is never caught here —
/// those come from a shared, reviewed error enum instead, listed in
/// `dynamic_codes_for`).
fn literal_codes(text: &str, fn_name: &str) -> BTreeSet<String> {
    let body = text.split("\n#[cfg(test)]").next().unwrap_or(text); // allow-swallow: no test module means the whole file is production code
    let needle = format!("{fn_name}(");
    let mut out = BTreeSet::new();
    let mut from = 0;
    while let Some(i) = body[from..].find(&needle) {
        let start = from + i + needle.len();
        let after = body[start..].trim_start();
        if let Some(rest) = after.strip_prefix('"')
            && let Some(end) = rest.find('"')
        {
            out.insert(rest[..end].to_owned());
        }
        from = start;
    }
    out
}

/// Every refusal code `path` constructs, as a literal string, through
/// any of the registry helper's call sites. `extra_fns` names this
/// file's own local helper(s) that forward into the registry helper
/// (`hollow.rs` and `make_solid.rs` keep their pre-registry name,
/// `failed`, for a smaller diff — unlike `align.rs`'s, `shapes.rs`'s
/// and `connector_ops.rs`'s own `failed`, which still build a genuine
/// `OpError::Failed` and are never passed here).
fn codes_in_file(path: &Path, extra_fns: &[&str]) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = BTreeSet::new();
    for f in [
        "refuse",
        "refuse_plain",
        "refuse_object",
        "refuse_at",
        "refuse_naming",
        "refusal::refuse",
        "refusal::refuse_plain",
    ]
    .into_iter()
    .chain(extra_fns.iter().copied())
    {
        out.extend(literal_codes(&text, f));
    }
    out
}

/// A declared code's spec id, or `None` if `code` isn't a choice
/// keyword or parameter name this scan's naive string search also
/// matches by accident (it never does, in practice, since every
/// `refuse`-family call's first argument is quoted and distinct from
/// `ParamValue` text).
fn declared_in(spec: &mf_spec::OperationSpec, code: &str) -> bool {
    spec.refusal(code).is_some()
}

/// (file relative to crates/, codes it literally constructs, the spec
/// ids that should declare them). A code this file constructs that
/// isn't declared in at least one of these specs fails the test.
fn migrated_files() -> Vec<(
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)> {
    vec![
        ("mf-operations/src/boolean.rs", &["mesh.boolean"], &[]),
        // mesh.blend has no spec of its own: it runs only inside
        // mesh.boolean's union, so its codes are declared there.
        ("mf-operations/src/blend.rs", &["mesh.boolean"], &[]),
        // check_moving_gap (moving_parts_fused) lives here but is only
        // ever called from object.attach's body; mesh.combine's own
        // codes are the boolean ones, named by object.
        (
            "mf-operations/src/combine.rs",
            &["mesh.combine", "object.attach", "connector.join"],
            &[],
        ),
        ("mf-operations/src/align.rs", &["object.align"], &[]),
        (
            "mf-operations/src/shapes.rs",
            &[
                "object.add_shape",
                "object.set_shape_params",
                "object.attach",
                "object.move_group",
            ],
            &[],
        ),
        ("mf-operations/src/hollow.rs", &["mesh.hollow"], &["failed"]),
        ("mf-operations/src/split/mod.rs", &["mesh.split_plane"], &[]),
        (
            "mf-operations/src/make_solid.rs",
            &["mesh.make_solid"],
            &["failed"],
        ),
        (
            "mf-parts/src/connectors/leaf_hinge.rs",
            &["object.add_shape", "object.set_shape_params", "hinge.leaf"],
            &[],
        ),
    ]
}

/// A code a migrated file's scan finds that is never actually a
/// declared refusal — a malformed invocation or a cancellation
/// relabelled through an error enum that has no `Cancelled` variant of
/// its own (`mf_parts::ConnectorError`) — so it's exempt from
/// `every_refusal_code_is_declared`'s "declared somewhere" check. Each
/// entry needs a reason; this is the registry test's own allow-list,
/// distinct from `docs/failed-refusals.allow` (which is about
/// `OpError::Failed` sites, not codes a `refuse`-family call already
/// reaches).
fn exempt(rel: &str, code: &str) -> bool {
    matches!(
        (rel, code),
        // leaf_hinge's own params are never malformed from the UI (the
        // panel only ever sends validated floats/ints), and "cancelled"
        // is RunContext's cancellation, not a refusal a person acts on.
        ("mf-parts/src/connectors/leaf_hinge.rs", "cancelled")
            | ("mf-parts/src/connectors/leaf_hinge.rs", "invalid_parameter")
    )
}

/// Codes that are genuine internal errors (ADR-0010: a malformed
/// invocation, a transform that can't be inverted, an id a replay log
/// carries that this build doesn't register) rather than a declared
/// refusal — never run through the registry helper, so this scan never
/// sees them as a `refuse`-family call. Listed here only to document
/// why they're absent from every migrated file's declared set, for a
/// reviewer cross-checking this test against `docs/failed-refusals.allow`.
#[allow(dead_code)]
const INTERNAL_NOT_REFUSALS: &[&str] = &[
    "missing_operand",
    "missing_target",
    "invalid_parameter",
    "singular_transform",
    "unknown_connector",
    "invalid_connector_params",
    "invalid_mesh",
    "invalid_plane",
    "region_not_supported",
    "invalid_leader",
    "invalid_leaders",
    "leaders_mismatch",
    "unknown_mode",
    "invalid_shapes",
    "invalid_generators",
    "invalid_generated",
    "invalid_members",
    "invalid_transforms",
    "generators_mismatch",
    "members_mismatch",
    "transforms_mismatch",
    "nothing_to_add",
];

fn hinge_leaf_spec() -> mf_spec::OperationSpec {
    let g = mf_parts::generators()
        .into_iter()
        .find(|g| g.id() == "hinge.leaf")
        .expect("hinge.leaf is a built-in generator");
    g.spec().clone()
}

fn spec_of(id: &str) -> mf_spec::OperationSpec {
    if id == "hinge.leaf" {
        return hinge_leaf_spec();
    }
    mf_spec::builtin::get(id)
        .unwrap_or_else(|| panic!("{id} isn't a built-in op"))
        .clone()
}

#[test]
fn every_refusal_code_is_declared() {
    let crates = root().join("crates");
    let mut problems = Vec::new();
    for (rel, spec_ids, extra_fns) in migrated_files() {
        let path = crates.join(rel);
        let codes = codes_in_file(&path, extra_fns);
        let specs: Vec<mf_spec::OperationSpec> = spec_ids.iter().map(|id| spec_of(id)).collect();
        for code in &codes {
            if exempt(rel, code) {
                continue;
            }
            let ok = specs.iter().any(|s| declared_in(s, code));
            if !ok {
                problems.push(format!(
                    "{rel}: refuses '{code}', not declared in any of {spec_ids:?}"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every file this test scans actually has at least one declared
/// refusal: a canary against the scan itself going quietly blind (a
/// renamed helper, a changed call-site shape) rather than the codes
/// genuinely vanishing.
#[test]
fn the_scan_finds_codes_in_every_migrated_file() {
    let crates = root().join("crates");
    for (rel, _, extra_fns) in migrated_files() {
        // shapes.rs relays every refusal dynamically (a generator's own
        // `code`, from `mf_parts::ConnectorError::Failed`), so this
        // literal-string scan never finds one here by design; its
        // generators' own literal codes (leaf_hinge.rs's) are checked
        // against the same specs directly.
        if rel == "mf-operations/src/shapes.rs" {
            continue;
        }
        let path = crates.join(rel);
        let codes = codes_in_file(&path, extra_fns);
        assert!(
            !codes.is_empty(),
            "{rel}: the scan found no refuse(...) codes at all"
        );
    }
}

// ---------------------------------------------------------------------
// The lint: no `OpError::Failed`/`ConnectorError::Failed` is
// constructed outside `docs/failed-refusals.allow` (ADR-0010).
// ---------------------------------------------------------------------

const LINT_CRATES: [&str; 2] = ["mf-operations", "mf-parts"];

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

/// True if `pos` (the start of the matched `OpError::Failed`/
/// `ConnectorError::Failed` text) is immediately preceded, up to
/// whitespace and a leading `let ` and type-path qualifiers, only by
/// `let `: a destructuring pattern (`let OpError::Failed { code, .. }
/// = e;` or `let mf_parts::ConnectorError::Failed { code, message } =
/// e;`), never a construction. A `let` that binds something else first
/// (`let target = ...Failed { ... }`) is a construction nested inside
/// an expression, not this.
fn is_let_pattern(text: &str, pos: usize) -> bool {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let before = text[line_start..pos].trim_start();
    let Some(rest) = before.strip_prefix("let ") else {
        return false;
    };
    // Only path segments (word characters, `::`, whitespace) between
    // `let ` and the matched type name: a genuine `let <path> = `
    // destructure, not `let name = <expr containing Failed>`.
    rest.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c.is_whitespace())
}

/// True if what follows the struct literal's matching closing brace is
/// a match arm's own guard or body (`Err(...Failed { .. }) =>` or
/// `... if ...`), never a construction.
fn is_match_arm(text: &str, open_brace_end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 1i32;
    let mut i = open_brace_end;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    let after = text[i..].trim_start();
    after.starts_with("=>") || after.starts_with("if ")
}

/// Every `OpError::Failed {`/`ConnectorError::Failed {` construction
/// (never a `let`-destructure or a match arm's own pattern) in `text`'s
/// production code (a `#[cfg(test)]` tail excluded).
fn failed_construction_sites(text: &str) -> usize {
    let body = text.split("\n#[cfg(test)]").next().unwrap_or(text); // allow-swallow: no test module means the whole file is production code
    let needle_a = "OpError::Failed";
    let needle_b = "ConnectorError::Failed";
    let mut count = 0;
    for needle in [needle_a, needle_b] {
        let mut from = 0;
        while let Some(i) = body[from..].find(needle) {
            let at = from + i;
            from = at + needle.len();
            let after_name = body[from..].trim_start();
            if !after_name.starts_with('{') {
                continue; // allow-swallow: a mention with no struct literal (a doc comment's own prose) isn't a site
            }
            let brace_at = from + (body[from..].len() - after_name.len());
            let brace_end = brace_at + after_name[..1].len();
            if is_let_pattern(body, at) {
                continue;
            }
            if is_match_arm(body, brace_end) {
                continue;
            }
            count += 1;
        }
    }
    count
}

/// `(file relative to crates/, count)` → reviewed entries, from the
/// allow-list.
fn allowed_failed_sites() -> std::collections::BTreeMap<String, (usize, usize)> {
    let path = root().join("docs/failed-refusals.allow");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (the reviewed allow-list)", path.display()));
    let mut out = std::collections::BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(file), Some(count)) = (words.next(), words.next()) else {
            panic!(
                "docs/failed-refusals.allow:{}: `<file> <count> <reason>`",
                n + 1
            );
        };
        let count: usize = count.parse().unwrap_or_else(|_| {
            panic!(
                "docs/failed-refusals.allow:{}: {count} isn't a count",
                n + 1
            )
        });
        assert!(
            words.next().is_some(),
            "docs/failed-refusals.allow:{}: {file} {count} has no reason",
            n + 1
        );
        out.insert(file.to_owned(), (count, n + 1));
    }
    out
}

#[test]
fn failed_sites_are_on_the_reviewed_allow_list() {
    let crates = root().join("crates");
    let allowed = allowed_failed_sites();
    let mut found: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for c in LINT_CRATES {
        let mut files = Vec::new();
        rust_files(&crates.join(c).join("src"), &mut files);
        for f in files {
            let rel = f
                .strip_prefix(&crates)
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&f).expect("readable source");
            let n = failed_construction_sites(&text);
            if n > 0 {
                found.insert(rel, n);
            }
        }
    }
    let mut problems = Vec::new();
    for (file, n) in &found {
        let expected = allowed.get(file).map(|&(c, _)| c).unwrap_or(0); // allow-swallow: absent means none reviewed
        if *n != expected {
            problems.push(format!(
                "{file}: {n} OpError::Failed construction site(s), {expected} reviewed on \
                 docs/failed-refusals.allow: review the new one(s) through the registry \
                 helper, or add/update the allow-list entry with a reason"
            ));
        }
    }
    for (file, &(count, line)) in &allowed {
        let have = found.get(file).copied().unwrap_or(0);
        if have != count {
            problems.push(format!(
                "docs/failed-refusals.allow:{line}: lists {count} for {file}, the code has \
                 {have}: the list only shrinks, so update or remove the stale entry"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
