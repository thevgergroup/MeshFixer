#![allow(clippy::unwrap_used)] // tests may panic on failure

//! One print volume (consolidation plan C18, units-audit.md §1.3's "the
//! codebase has three different default plate sizes"): the Machine
//! profile's own volume
//! ([`mf_parts::MachineProfile::uncalibrated`]'s `print_volume_mm`) is
//! the only place a plate-size default is written down. The import
//! dialog's "no profile" fallback and the render plate's placeholder
//! must read it (or, for the render plate — a leaf crate with no mf-*
//! dependencies of its own — be driven by it through
//! [`mf_render::Plate::from_print_volume_mm`]), rather than each
//! carrying their own independent literal that could drift from it.
//!
//! `one_default_print_volume` is a source scan: it fails the moment a
//! second plate-size literal (an `[f64; 2]` or `[f64; 3]` array that
//! looks like a millimetre plate size) appears outside
//! `mf-parts/src/machine.rs`, with one reviewed exception
//! ([`ALLOWED`]): `mf-render`'s own `Plate::default`, a pre-existing,
//! heavily tested first-frame placeholder (used only before any
//! camera/viewport or Machine profile is known) that this leaf crate
//! can't derive from the profile directly (no mf-* dependency of its
//! own) and that a caller holding the real profile overrides every
//! frame through [`mf_render::Plate::from_print_volume_mm`] instead of
//! ever reading as a plate size.

use std::path::{Path, PathBuf};

/// `(file, literal name)` exempted from "one default print volume",
/// each with a reason above. The list only shrinks.
const ALLOWED: &[(&str, &str)] = &[("mf-render/src/overlay.rs", "size_mm")];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates")
}

/// `text` with every `//` comment removed (line by line), so a comment
/// mentioning a plate size in passing doesn't trip the scan.
fn code_only(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every line naming a plate/volume/plate-size array literal in `code`:
/// `(name, values)`. Matches both a typed binding
/// (`const NAME: [f64; 3] = [220.0, 220.0, 250.0]`) and a struct field
/// initializer with no type repeated on the line
/// (`size_mm: [256.0, 256.0]`); a deliberately narrow scan (a name
/// containing `mm`/`plate`/`volume`, 2 or 3 values, each at least 1.0 —
/// a real plate is never sub-millimetre) rather than every `[f64; N]`
/// literal in the crate, since those are legion and almost none of them
/// are plate sizes.
fn plate_literals(code: &str) -> Vec<(String, Vec<f64>)> {
    let mut out = Vec::new();
    for line in code.lines() {
        let lower = line.to_ascii_lowercase();
        let looks_plate_named =
            lower.contains("mm") || lower.contains("plate") || lower.contains("volume");
        if !looks_plate_named || !line.contains('[') || !line.contains(']') {
            continue;
        }
        let Some(name_end) = line.find(':') else {
            continue;
        };
        let name = line[..name_end]
            .trim()
            .trim_start_matches("const")
            .trim_start_matches("let")
            .trim_start_matches("pub")
            .trim()
            .to_owned();
        // The *value*'s `[...]`: after `=` for a typed binding (skipping
        // the type's own `[f64; N]`), or straight after the field name's
        // `:` for a struct literal field with no type on the line.
        let after = match line.find('=') {
            Some(eq) => &line[eq..],
            None => &line[name_end..],
        };
        let Some(open) = after.find('[') else {
            continue;
        };
        let Some(close) = after[open..].find(']') else {
            continue;
        };
        let inside = &after[open + 1..open + close];
        let values: Vec<f64> = inside
            .split(',')
            .filter_map(|v| v.trim().parse::<f64>().ok())
            .collect();
        if values.len() >= 2 && values.iter().all(|v| *v >= 1.0) {
            out.push((name, values));
        }
    }
    out
}

/// Every plate-size-shaped literal array in every `.rs` file under
/// `crates/*/src`, keyed by the file's path relative to `crates/`
/// (production code only: a file's `#[cfg(test)]` tail is excluded, as
/// the units lints already do).
fn found() -> Vec<(String, String, Vec<f64>)> {
    let crates = crates_dir();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap() {
        let krate = entry.unwrap().path();
        let src = krate.join("src");
        if !src.is_dir() {
            continue;
        }
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&f).unwrap();
        let production: String = text
            .lines()
            .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
            .collect::<Vec<_>>()
            .join("\n");
        for (name, values) in plate_literals(&code_only(&production)) {
            out.push((rel.clone(), name, values));
        }
    }
    out
}

#[test]
fn plate_literal_scanner_finds_plate_shaped_arrays_only() {
    let code = "const DEFAULT_VOLUME_MM: [f64; 3] = [220.0, 220.0, 250.0];\n\
                size_mm: [256.0, 256.0],\n\
                let x: [f64; 2] = [0.1, 0.2];\n\
                const OTHER: [usize; 3] = [1, 2, 3];";
    let found = plate_literals(code);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, "DEFAULT_VOLUME_MM");
    assert_eq!(found[0].1, vec![220.0, 220.0, 250.0]);
    assert_eq!(found[1].0, "size_mm");
    assert_eq!(found[1].1, vec![256.0, 256.0]);
}

/// Consolidation plan C18: there is one print volume, from the Machine
/// profile. `mf-parts/src/machine.rs` is where it's written down
/// (`MachineProfile::uncalibrated`'s `print_volume_mm`); nowhere else in
/// the app may carry its own independent plate-size literal — the import
/// dialog's "no profile" fallback and the render plate's placeholder
/// must derive their numbers from it instead (even if, for the render
/// plate, that means a caller converts the profile's volume into a
/// [`mf_render::Plate`] rather than the crate holding a second literal).
#[test]
fn one_default_print_volume() {
    let found = found();
    let sources: Vec<&(String, String, Vec<f64>)> = found
        .iter()
        .filter(|(file, _, _)| file == "mf-parts/src/machine.rs")
        .collect();
    assert_eq!(
        sources.len(),
        1,
        "mf-parts/src/machine.rs should hold exactly one plate-size literal \
         (DEFAULT_VOLUME_MM), found {sources:?}"
    );
    let elsewhere: Vec<&(String, String, Vec<f64>)> = found
        .iter()
        .filter(|(file, name, _)| {
            file != "mf-parts/src/machine.rs" && !ALLOWED.contains(&(file.as_str(), name.as_str()))
        })
        .collect();
    assert!(
        elsewhere.is_empty(),
        "a second plate-size literal exists outside mf-parts/src/machine.rs: {elsewhere:?} \
         — derive it from mf_parts::MachineProfile::uncalibrated().print_volume_mm instead, or \
         add a reviewed ALLOWED entry with a reason"
    );
    // The allow-list only shrinks: every entry must still match a real
    // site, or it's stale.
    for (file, name) in ALLOWED {
        assert!(
            found.iter().any(|(f, n, _)| f == file && n == name),
            "ALLOWED lists {file} {name}, but no such literal was found; remove the stale entry"
        );
    }
}

/// The import dialog's "no Machine profile active" fallback is the
/// built-in Uncalibrated profile's own volume, not a second number.
#[test]
fn import_dialog_default_plate_is_the_uncalibrated_profile_s_volume() {
    let plate = mf_desktop::import_dialog::Plate::from_profile(None);
    assert_eq!(
        plate.volume_mm,
        mf_parts::MachineProfile::uncalibrated().print_volume_mm
    );
}

/// The render plate, built from a profile's volume (the shape
/// `mf-desktop`'s `app.rs::plate()` uses, consolidation plan C27),
/// carries that profile's own X/Y, not `mf_render::Plate::default`'s
/// independent 256 mm placeholder.
#[test]
fn render_plate_from_profile_carries_the_profile_s_volume() {
    let profile = mf_parts::MachineProfile::uncalibrated();
    let [x, y, _z] = profile.print_volume_mm;
    let plate = mf_render::Plate::from_print_volume_mm([x, y], 10.0, 50.0);
    assert_eq!(plate.size_mm, [x, y]);
    assert_ne!(
        plate.size_mm,
        mf_render::Plate::default().size_mm,
        "the Uncalibrated profile's volume (220 x 220) differs from the render \
         crate's own first-frame placeholder (256 x 256); from_print_volume_mm should carry \
         the profile's own numbers, not fall back to the placeholder"
    );
}
