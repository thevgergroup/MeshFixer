//! Units for a unitless file (MF-ISSUE-045, owner 2026-11-14 "smarter
//! import units"). STL and OBJ don't say what their numbers measure: a
//! scan in metres would otherwise open a thousand times too small.
//! Opening one asks, showing the model's size in each candidate unit
//! against the active Machine profile's print volume (or a 250 mm
//! default plate with none), preselecting whichever unit "fits the
//! plate" best.

use std::path::Path;

use eframe::egui::{self, RichText};
use mf_io::ImportedModel;
use mf_io::threemf::Unit;
use mf_parts::MachineProfile;
use serde::{Deserialize, Serialize};

use crate::MeshFixerApp;
use crate::defects::{measure, unit_symbol};
use crate::theme;

/// The units offered, in the order shown (consolidation plan C15: every
/// [`Unit`], not the import dialog's original 4).
const OFFERED: [Unit; 6] = Unit::ALL;

/// The plate used when no Machine profile is active: the one print
/// volume the app knows without a user choice, the built-in Uncalibrated
/// profile's own (consolidation plan C18, "There is ONE print volume,
/// from the machine profile"). This used to be a second, independent
/// `250 × 250 × 250 mm` literal, inconsistent with both
/// [`mf_parts::MachineProfile::uncalibrated`]'s volume and the render
/// plate's own default.
fn default_plate_mm() -> [f64; 3] {
    mf_parts::MachineProfile::uncalibrated().print_volume_mm
}

/// Below this, on its largest side, a unit's reading is unprintable.
const TOO_SMALL_MM: f64 = 5.0;
/// Below this (and at or above [`TOO_SMALL_MM`]), a unit's reading is
/// printable but tiny.
const VERY_SMALL_MM: f64 = 15.0;
/// When nothing fits, the largest "too big" unit is still offered
/// outright up to this multiple of the plate's own size; beyond it,
/// millimetres is the fallback (so a value of, say, 1e30 doesn't get
/// preselected just because it's "the biggest that's over").
const MAX_OVERSIZE_MULTIPLE: f64 = 10.0;

/// Millimetres in one `unit`: [`mf_units::Length`]'s own conversion,
/// the one table (ADR-0006 rule 1; this used to duplicate it locally).
pub fn millimetres(unit: Unit) -> f64 {
    mf_units::Length::new(1.0, unit).mm()
}

/// How a unit's reading compares to the plate: shown next to every
/// option other than the preselected one, which gets its own "Fits
/// your plate" wording instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Under [`TOO_SMALL_MM`] on the largest side: can't be printed.
    TooSmall,
    /// [`TOO_SMALL_MM`]..[`VERY_SMALL_MM`]: printable, but tiny.
    VerySmall,
    /// Fits the plate on every axis and is at least [`TOO_SMALL_MM`].
    Fits,
    /// Doesn't fit the plate on some axis.
    TooBig,
}

impl Fit {
    /// Plain status words (brief: "too small to print", "very small",
    /// "fits", "too big for your plate").
    pub fn words(self) -> &'static str {
        match self {
            Fit::TooSmall => "too small to print",
            Fit::VerySmall => "very small",
            Fit::Fits => "fits",
            Fit::TooBig => "too big for your plate",
        }
    }
}

/// How `extent_mm` (a model's size in mm) compares to `volume_mm` (the
/// plate): fits only if every axis is within the plate, classifying by
/// the largest side otherwise.
fn classify(extent_mm: [f64; 3], volume_mm: [f64; 3]) -> Fit {
    let largest = extent_mm.iter().copied().fold(0.0, f64::max);
    let fits_volume = extent_mm.iter().zip(volume_mm).all(|(&e, v)| e <= v);
    if !fits_volume {
        Fit::TooBig
    } else if largest < TOO_SMALL_MM {
        Fit::TooSmall
    } else if largest < VERY_SMALL_MM {
        Fit::VerySmall
    } else {
        Fit::Fits
    }
}

/// The plate a model is checked against: the active Machine profile's
/// print volume and a name to show, or [`default_plate_mm`] unnamed when
/// there's no profile.
#[derive(Debug, Clone)]
pub struct Plate {
    pub name: Option<String>,
    pub volume_mm: [f64; 3],
}

impl Plate {
    pub fn from_profile(profile: Option<&MachineProfile>) -> Plate {
        match profile {
            Some(p) => Plate {
                name: Some(p.name.clone()),
                volume_mm: p.print_volume_mm,
            },
            None => Plate {
                name: None,
                volume_mm: default_plate_mm(),
            },
        }
    }

    /// "Fits your Prusa MK4 plate" / "Fits a 220 × 220 × 250 mm plate"
    /// with none — the actual numbers, read off `self`, rather than a
    /// second copy of [`default_plate_mm`]'s own values that could drift
    /// from them.
    fn fits_label(&self) -> String {
        match &self.name {
            Some(name) => format!("Fits your {name} plate"),
            None => {
                let [x, y, z] = self.volume_mm;
                format!("Fits a {x:.0} × {y:.0} × {z:.0} mm plate")
            }
        }
    }
}

/// The file name the import-unit memory is saved under, in
/// [`crate::MeshFixerApp::settings_dir`] (the same directory Parts and
/// Connectors keep their own settings in).
const MEMORY_FILE: &str = "import-units.json";

/// Why a unit was preselected instead of [`choose_unit`]'s own pick
/// (import-units-brief.md, "Remember choices": "Say why").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remembered {
    /// The same folder's last import used this unit.
    Folder,
    /// The same exporter (an STL header/OBJ comment signature, e.g.
    /// "Blender") last imported to this unit.
    Exporter,
}

impl Remembered {
    /// The reason shown in the dialog (brief: "Same as your last
    /// import from this folder").
    pub fn reason(self) -> &'static str {
        match self {
            Remembered::Folder => "Same as your last import from this folder",
            Remembered::Exporter => "Same as your last import from this exporter",
        }
    }
}

/// The last unit chosen per folder and per exporter signature
/// (import-units-brief.md, "Remember choices"), saved as
/// [`MEMORY_FILE`] next to the Machine profile and Fit class settings.
/// A folder match wins over an exporter match when both are known (it's
/// the more specific signal: the same project, not just the same CAD
/// tool); `recall` tries the folder first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportMemory {
    #[serde(default)]
    by_folder: std::collections::BTreeMap<String, Unit>,
    #[serde(default)]
    by_exporter: std::collections::BTreeMap<String, Unit>,
}

impl ImportMemory {
    /// Reads [`MEMORY_FILE`] from `dir`; an absent file is an empty,
    /// unremarkable memory (nothing imported here yet), never an error.
    /// A present-but-malformed file is an error (D4: never silently
    /// replaced by "no memory").
    pub fn load(dir: &Path) -> Result<ImportMemory, String> {
        let path = dir.join(MEMORY_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ImportMemory::default());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Writes this memory to [`MEMORY_FILE`] in `dir`, creating `dir` if
    /// it doesn't exist yet.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = dir.join(MEMORY_FILE);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    /// The remembered unit for this import, and why, trying the folder
    /// before the exporter signature.
    pub fn recall(
        &self,
        folder: Option<&str>,
        exporter: Option<&str>,
    ) -> Option<(Unit, Remembered)> {
        if let Some(u) = folder.and_then(|f| self.by_folder.get(f)) {
            return Some((*u, Remembered::Folder));
        }
        exporter
            .and_then(|e| self.by_exporter.get(e))
            .map(|&u| (u, Remembered::Exporter))
    }

    /// Records `unit` as the choice for this folder and/or exporter
    /// signature, so the next import from either preselects it.
    pub fn remember(&mut self, folder: Option<&str>, exporter: Option<&str>, unit: Unit) {
        if let Some(f) = folder {
            self.by_folder.insert(f.to_owned(), unit);
        }
        if let Some(e) = exporter {
            self.by_exporter.insert(e.to_owned(), unit);
        }
    }
}

/// `path`'s parent directory as a string key for [`ImportMemory`]
/// (`None` for a path with no parent, e.g. a bare file name).
pub fn folder_key(path: &std::path::Path) -> Option<String> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
}

/// The import-unit memory from `app`'s settings directory (empty, with
/// no error, when there's no settings directory or no file yet: the
/// same "nothing saved there yet" treatment as Machine profiles and Fit
/// classes).
pub(crate) fn import_memory(app: &MeshFixerApp) -> (ImportMemory, Option<String>) {
    let Some(dir) = app.settings_dir() else {
        return (ImportMemory::default(), None);
    };
    match ImportMemory::load(&dir) {
        Ok(m) => (m, None),
        Err(e) => (ImportMemory::default(), Some(e)),
    }
}

/// The unit to preselect, and whether nothing fit (brief: "Nothing
/// fits" needs its own "Scale to fit plate" option).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub unit: Unit,
    pub needs_scale: bool,
}

/// Picks the default unit for a model whose size (in the file's own
/// numbers) is `extent`, against `plate`: among units that fit the
/// plate and are at least [`TOO_SMALL_MM`] on their largest side, the
/// one giving the largest size; if none fits, the largest unit that's
/// too big but under [`MAX_OVERSIZE_MULTIPLE`]× the plate, else
/// millimetres, with `needs_scale` set.
pub fn choose_unit(extent: [f64; 3], plate: &Plate) -> Choice {
    let mm_of = |u: Unit| extent.map(|v| v * millimetres(u));
    let largest_mm = |u: Unit| mm_of(u).into_iter().fold(0.0, f64::max);

    let fitting = OFFERED
        .into_iter()
        .filter(|&u| classify(mm_of(u), plate.volume_mm) == Fit::Fits)
        .max_by(|&a, &b| largest_mm(a).total_cmp(&largest_mm(b)));
    if let Some(unit) = fitting {
        return Choice {
            unit,
            needs_scale: false,
        };
    }

    // Nothing fits: the largest unit that's too big but still
    // plausible (under MAX_OVERSIZE_MULTIPLE times the plate's own
    // longest side), else millimetres.
    let plate_longest = plate.volume_mm.iter().copied().fold(0.0, f64::max);
    let plausible = OFFERED
        .into_iter()
        .filter(|&u| {
            let l = largest_mm(u);
            l > plate_longest && l < plate_longest * MAX_OVERSIZE_MULTIPLE
        })
        .max_by(|&a, &b| largest_mm(a).total_cmp(&largest_mm(b)));
    Choice {
        unit: plausible.unwrap_or(Unit::Millimeter),
        needs_scale: true,
    }
}

/// A unitless file read and waiting for its unit.
#[derive(Debug)]
pub struct UnitPrompt {
    /// The file's name, for the dialog's title.
    pub name: String,
    /// The model's size along each axis, in the file's own numbers.
    pub extent: [f64; 3],
    /// The unit picked so far (preselected by [`choose_unit`], or by
    /// [`ImportMemory::recall`] when it has one for this folder or
    /// exporter); the unit imported at either way, "Scale to fit plate"
    /// scales afterwards rather than changing this.
    pub choice: Unit,
    /// Set when the preselected unit still doesn't fit the plate: the
    /// dialog offers "Scale to fit plate" (brief, "Nothing fits"),
    /// preselected in that case. Never set together with
    /// [`UnitPrompt::remembered`] (a remembered choice always fits, or
    /// at least the person already said it was right last time).
    pub needs_scale: bool,
    /// The dialog's own radio selection: `true` while "Scale to fit
    /// plate" is chosen instead of one of the plain unit rows. Only
    /// meaningful when [`UnitPrompt::needs_scale`] is set (the row
    /// isn't shown otherwise).
    pub scale_to_fit: bool,
    /// The plate the model was checked against.
    pub plate: Plate,
    /// A hint read from the file itself (an STL header or OBJ comment
    /// naming its exporter), shown but never acted on automatically.
    pub exporter_hint: Option<String>,
    /// Why [`UnitPrompt::choice`] was preselected from
    /// [`ImportMemory`] instead of [`choose_unit`]'s own "fits the
    /// plate" pick (import-units-brief.md, "Remember choices": "Say
    /// why"); `None` when nothing was remembered and the size rule
    /// picked it.
    pub remembered: Option<Remembered>,
    /// This import's folder, for [`ImportMemory::remember`] once it's
    /// confirmed.
    pub(crate) folder: Option<String>,
    /// This import's exporter signature (e.g. "Blender"), for
    /// [`ImportMemory::remember`] once it's confirmed.
    pub(crate) exporter_signature: Option<&'static str>,
    pub(crate) model: ImportedModel,
}

impl UnitPrompt {
    pub fn new(
        name: String,
        extent: [f64; 3],
        model: ImportedModel,
        plate: Plate,
        exporter_hint: Option<String>,
        memory: &ImportMemory,
    ) -> UnitPrompt {
        let folder = folder_key(&model.source.path);
        let exporter_signature = exporter_hint.as_deref().and_then(exporter_signature);
        let remembered = memory.recall(folder.as_deref(), exporter_signature.map(|(n, _)| n));
        let (choice, needs_scale, remembered) = match remembered {
            Some((unit, reason)) => (unit, false, Some(reason)),
            None => {
                let Choice { unit, needs_scale } = choose_unit(extent, &plate);
                (unit, needs_scale, None)
            }
        };
        UnitPrompt {
            name,
            extent,
            choice,
            needs_scale,
            scale_to_fit: needs_scale,
            plate,
            exporter_hint: exporter_hint.map(|h| exporter_hint_line(&h)),
            remembered,
            folder,
            exporter_signature: exporter_signature.map(|(n, _)| n),
            model,
        }
    }

    /// The model, with the chosen unit.
    pub(crate) fn into_model(self) -> ImportedModel {
        let mut model = self.model;
        model.unit = Some(self.choice);
        model
    }

    /// The model's size in mm at `unit`.
    fn extent_mm(&self, unit: Unit) -> [f64; 3] {
        self.extent.map(|v| v * millimetres(unit))
    }

    /// The uniform scale factor "Scale to fit plate" applies after
    /// import at [`UnitPrompt::choice`], so the model ends up at 90% of
    /// the plate on every axis (the brief's "scale the model uniformly
    /// to 90% of the volume"): the smallest per-axis ratio, so no axis
    /// overflows even if the model's proportions don't match the
    /// plate's.
    pub(crate) fn scale_to_fit_factor(&self) -> f64 {
        let model_mm = self.extent_mm(self.choice);
        model_mm
            .iter()
            .zip(self.plate.volume_mm)
            .map(|(&m, v)| if m > 0.0 { 0.9 * v / m } else { f64::INFINITY })
            .fold(f64::INFINITY, f64::min)
    }
}

/// Known exporter signatures found in an STL header or OBJ comment,
/// and the plain-language hint shown for each (brief: "SolidWorks",
/// "Fusion", "Onshape" mean mm; "Blender" means m"): the exporter's
/// name as shown, and the unit words. `None` if the raw text names no
/// exporter this is sure about; the raw text is still shown on its own.
fn exporter_signature(raw: &str) -> Option<(&'static str, &'static str)> {
    let lower = raw.to_ascii_lowercase();
    let mm_exporters = [
        ("solidworks", "SolidWorks"),
        ("fusion", "Fusion"),
        ("onshape", "Onshape"),
        ("freecad", "FreeCAD"),
        ("sketchup", "SketchUp"),
    ];
    let m_exporters = [("blender", "Blender")];
    for (needle, name) in m_exporters {
        if lower.contains(needle) {
            return Some((name, "usually metres"));
        }
    }
    for (needle, name) in mm_exporters {
        if lower.contains(needle) {
            return Some((name, "usually millimetres"));
        }
    }
    None
}

/// The hint shown in the dialog for a file's own header/comment text:
/// "Exported from Blender — usually metres" when the text names a
/// known exporter, else the raw text verbatim (e.g. an OBJ "# unit mm"
/// comment, shown as-is).
pub fn exporter_hint_line(raw: &str) -> String {
    match exporter_signature(raw) {
        Some((name, hint)) => format!("Exported from {name} — {hint}"),
        None => raw.to_owned(),
    }
}

/// The unit's plain name ("Millimetres", "Metres", ...).
pub(crate) fn unit_name(unit: Unit) -> &'static str {
    match unit {
        Unit::Micron => "Microns",
        Unit::Millimeter => "Millimetres",
        Unit::Centimeter => "Centimetres",
        Unit::Meter => "Metres",
        Unit::Inch => "Inches",
        Unit::Foot => "Feet",
    }
}

/// The size of a model whose bounds measure `extent`, plainly: "Metres ·
/// 0.500 × 0.400 × 0.300 m (500 × 400 × 300 mm)".
fn dims_row(unit: Unit, extent: [f64; 3]) -> String {
    let dims = |scale: f64| {
        extent
            .iter()
            .map(|&v| measure(v * scale))
            .collect::<Vec<_>>()
            .join(" × ")
    };
    let own = format!("{} · {} {}", unit_name(unit), dims(1.0), unit_symbol(unit));
    if unit == Unit::Millimeter {
        own
    } else {
        format!("{own} ({} mm)", dims(millimetres(unit)))
    }
}

/// One option's full row label: the size, and the preselected unit's
/// reason (a remembered choice's "Same as your last import..." when
/// there is one, else "Fits your plate") or the plain status of the
/// rest.
fn option_row(prompt: &UnitPrompt, unit: Unit) -> String {
    let dims = dims_row(unit, prompt.extent);
    if unit == prompt.choice && !prompt.needs_scale {
        let dims_mm = prompt
            .extent_mm(unit)
            .iter()
            .map(|&v| measure(v))
            .collect::<Vec<_>>()
            .join(" × ");
        let reason = match prompt.remembered {
            Some(r) => r.reason().to_owned(),
            None => prompt.plate.fits_label(),
        };
        format!("{dims} — {reason} ({dims_mm} mm)")
    } else {
        let fit = classify(prompt.extent_mm(unit), prompt.plate.volume_mm);
        format!("{dims} — {}", fit.words())
    }
}

/// The "Scale to fit plate" row's label, shown only when nothing fits:
/// the unit it imports at, and the plate it scales down to.
fn scale_to_fit_row(prompt: &UnitPrompt) -> String {
    let plate_words = prompt.plate.name.as_deref().map_or_else(
        || "a 250 mm plate".to_string(),
        |n| format!("your {n} plate"),
    );
    format!(
        "Scale to fit plate — import at {} and scale down to 90% of {plate_words}",
        unit_name(prompt.choice).to_lowercase()
    )
}

/// The dialog, centred over the window. Returns the answer once given:
/// `(true, scale)` to import at [`UnitPrompt::choice`] (`scale`: also
/// apply [`UnitPrompt::scale_to_fit_factor`], as its own undoable
/// step, the caller's job), or `(false, _)` for Cancel or Esc.
pub fn show(prompt: &mut UnitPrompt, ctx: &egui::Context) -> Option<(bool, bool)> {
    let mut answer = None;
    // Modal: nothing behind it answers until the unit is chosen.
    egui::Modal::new(egui::Id::unique("unit prompt")).show(ctx, |ui| {
        ui.heading(format!("Units for {}", prompt.name));
        ui.label("This file doesn't say what unit its numbers are in.");
        if let Some(hint) = &prompt.exporter_hint {
            ui.label(RichText::new(hint).color(theme::TEXT_MUTED));
        }
        ui.label(
            RichText::new("Pick the one that gives the size you expect:").color(theme::TEXT_MUTED),
        );
        ui.add_space(6.0);
        for unit in OFFERED {
            let row = option_row(prompt, unit);
            if ui
                .radio(prompt.choice == unit && !prompt.scale_to_fit, row)
                .clicked()
            {
                // A choice typed by hand no longer carries the
                // remembered reason, even if it happens to match the
                // unit that was preselected (re-picking it by hand).
                prompt.choice = unit;
                prompt.scale_to_fit = false;
                prompt.remembered = None;
            }
        }
        if prompt.needs_scale {
            ui.add_space(4.0);
            let row = scale_to_fit_row(prompt);
            if ui.radio(prompt.scale_to_fit, row).clicked() {
                prompt.scale_to_fit = true;
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Import").clicked() {
                answer = Some((true, prompt.scale_to_fit));
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some((false, false));
            }
        });
    });
    // Esc cancels; a click outside the dialog doesn't (the choice matters).
    if answer.is_none()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        answer = Some((false, false));
    }
    answer
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    fn plate(volume_mm: [f64; 3]) -> Plate {
        Plate {
            name: None,
            volume_mm,
        }
    }

    #[test]
    fn the_bunny_defaults_to_metres() {
        // The brief's test: "the bunny (0.156 units) defaults to m,
        // which gives 156 mm, labelled fits."
        let extent = [0.156, 0.156, 0.156];
        let choice = choose_unit(extent, &plate(default_plate_mm()));
        assert_eq!(choice.unit, Unit::Meter);
        assert!(!choice.needs_scale);
        assert_eq!(
            classify(extent.map(|v| v * 1000.0), default_plate_mm()),
            Fit::Fits
        );
    }

    #[test]
    fn a_100_unit_cube_defaults_to_millimetres() {
        let extent = [100.0, 100.0, 100.0];
        let choice = choose_unit(extent, &plate(default_plate_mm()));
        assert_eq!(choice.unit, Unit::Millimeter);
        assert!(!choice.needs_scale);
    }

    #[test]
    fn a_3000_unit_part_defaults_to_scale_to_fit() {
        // Every candidate unit overflows the default plate (3000 mm,
        // 30 m, 300 cm, 76200 in): nothing fits.
        let extent = [3000.0, 3000.0, 3000.0];
        let choice = choose_unit(extent, &plate(default_plate_mm()));
        assert!(choice.needs_scale, "{choice:?}");

        // Scaling to fit brings a cube model to 90% of the plate's own
        // smallest axis (its tightest constraint): the other axes land
        // short of their own 90%, since one uniform factor applies to
        // every axis of a cube model.
        let model = mf_io::ImportedModel {
            objects: Vec::new(),
            unit: None,
            source: mf_io::SourceInfo {
                path: "p.stl".into(),
                content_hash: mf_geometry::ContentHash([0; 32]),
                format: mf_io::Format::StlBinary,
                precision: mf_io::SourcePrecision::F32,
                exporter_hint: None,
            },
            warnings: Vec::new(),
            metadata: Default::default(),
        };
        let prompt = UnitPrompt::new(
            "p".into(),
            extent,
            model,
            plate(default_plate_mm()),
            None,
            &ImportMemory::default(),
        );
        let factor = prompt.scale_to_fit_factor();
        let scaled_mm = prompt.extent_mm(prompt.choice).map(|v| v * factor);
        let smallest = default_plate_mm().into_iter().fold(f64::INFINITY, f64::min);
        for s in scaled_mm {
            assert!((s - 0.9 * smallest).abs() < 1e-9, "{scaled_mm:?}");
        }
    }

    #[test]
    fn largest_fitting_unit_wins_among_several_that_fit() {
        // At 0.02 units: mm -> 0.02mm (too small), cm -> 0.2mm (too
        // small), m -> 20mm (fits), inch -> 0.508mm (too small). Only
        // metres fits, so it wins outright.
        let choice = choose_unit([0.02, 0.02, 0.02], &plate(default_plate_mm()));
        assert_eq!(choice.unit, Unit::Meter);

        // At 5.0 units: mm -> 5mm (fits, borderline), cm -> 50mm
        // (fits), m -> 5000mm (too big), inch -> 127mm (fits). The
        // largest that fits is inches.
        let choice = choose_unit([5.0, 5.0, 5.0], &plate(default_plate_mm()));
        assert_eq!(choice.unit, Unit::Inch);
    }

    #[test]
    fn status_words_cover_every_bracket() {
        let v = default_plate_mm();
        assert_eq!(classify([1.0, 1.0, 1.0], v), Fit::TooSmall);
        assert_eq!(classify([10.0, 10.0, 10.0], v), Fit::VerySmall);
        assert_eq!(classify([50.0, 50.0, 50.0], v), Fit::Fits);
        assert_eq!(classify([3000.0, 10.0, 10.0], v), Fit::TooBig);
    }

    #[test]
    fn size_rows_read_in_each_unit() {
        let e = [0.5, 0.4, 0.3];
        assert_eq!(
            dims_row(Unit::Meter, e),
            "Metres · 0.500 × 0.400 × 0.300 m (500 × 400 × 300 mm)"
        );
        assert_eq!(
            dims_row(Unit::Millimeter, e),
            "Millimetres · 0.500 × 0.400 × 0.300 mm"
        );
        assert_eq!(
            dims_row(Unit::Inch, e),
            "Inches · 0.500 × 0.400 × 0.300 in (12.7 × 10.2 × 7.62 mm)"
        );
    }

    #[test]
    fn the_blender_stl_header_hint_is_shown() {
        // The brief's test: "The Blender STL header hint is shown."
        assert_eq!(
            exporter_hint_line("Exported from Blender-3.6.0"),
            "Exported from Blender — usually metres"
        );
        assert_eq!(
            exporter_hint_line("SolidWorks STL"),
            "Exported from SolidWorks — usually millimetres"
        );
        assert_eq!(exporter_hint_line("# unit mm"), "# unit mm");
    }

    #[test]
    fn a_named_profile_s_plate_is_shown_by_name() {
        let p = Plate {
            name: Some("Prusa MK4".into()),
            volume_mm: [250.0, 210.0, 220.0],
        };
        assert_eq!(p.fits_label(), "Fits your Prusa MK4 plate");
        let none = plate(default_plate_mm());
        let [x, y, z] = default_plate_mm();
        assert_eq!(
            none.fits_label(),
            format!("Fits a {x:.0} × {y:.0} × {z:.0} mm plate")
        );
    }

    fn model_at(path: &str, exporter_hint: Option<String>) -> ImportedModel {
        ImportedModel {
            objects: Vec::new(),
            unit: None,
            source: mf_io::SourceInfo {
                path: path.into(),
                content_hash: mf_geometry::ContentHash([0; 32]),
                format: mf_io::Format::StlBinary,
                precision: mf_io::SourcePrecision::F32,
                exporter_hint,
            },
            warnings: Vec::new(),
            metadata: Default::default(),
        }
    }

    /// Import-units-brief.md's test: "A second import from the same
    /// folder pre-selects the same unit, with that reason." Even though
    /// the model's own size would default to millimetres (a 100-unit
    /// cube), a remembered inches choice for the same folder wins, and
    /// says why.
    #[test]
    fn a_second_import_from_the_same_folder_is_remembered() {
        let extent = [100.0, 100.0, 100.0];
        let plate = plate(default_plate_mm());

        let first = UnitPrompt::new(
            "a.stl".into(),
            extent,
            model_at("/scans/batch/a.stl", None),
            plate.clone(),
            None,
            &ImportMemory::default(),
        );
        assert_eq!(first.choice, Unit::Millimeter, "the size rule, unaided");
        assert_eq!(first.remembered, None);

        let mut memory = ImportMemory::default();
        memory.remember(
            first.folder.as_deref(),
            first.exporter_signature,
            Unit::Inch,
        );

        let second = UnitPrompt::new(
            "b.stl".into(),
            extent,
            model_at("/scans/batch/b.stl", None),
            plate,
            None,
            &memory,
        );
        assert_eq!(second.choice, Unit::Inch, "remembered for this folder");
        assert_eq!(second.remembered, Some(Remembered::Folder));
        assert!(!second.needs_scale);
        let row = option_row(&second, Unit::Inch);
        assert!(
            row.contains("Same as your last import from this folder"),
            "{row}"
        );
    }

    /// The same memory, by exporter signature instead of folder (a
    /// different folder, same CAD tool).
    #[test]
    fn a_second_import_from_the_same_exporter_is_remembered() {
        let extent = [100.0, 100.0, 100.0];
        let plate = plate(default_plate_mm());
        let hint = Some("Exported from Blender-3.6.0".to_owned());

        let first = UnitPrompt::new(
            "a.stl".into(),
            extent,
            model_at("/one/a.stl", hint.clone()),
            plate.clone(),
            hint.clone(),
            &ImportMemory::default(),
        );
        let mut memory = ImportMemory::default();
        memory.remember(None, first.exporter_signature, Unit::Centimeter);

        // A different folder, same exporter: the folder lookup misses,
        // the exporter lookup hits.
        let second = UnitPrompt::new(
            "b.stl".into(),
            extent,
            model_at("/two/b.stl", hint.clone()),
            plate,
            hint,
            &memory,
        );
        assert_eq!(second.choice, Unit::Centimeter);
        assert_eq!(second.remembered, Some(Remembered::Exporter));
    }

    #[test]
    fn every_unit_all_is_offered() {
        assert_eq!(OFFERED, Unit::ALL);
    }

    #[test]
    fn import_memory_round_trips_through_the_settings_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut memory = ImportMemory::load(dir.path()).unwrap();
        assert_eq!(memory, ImportMemory::default(), "nothing saved there yet");

        memory.remember(Some("/scans/batch"), Some("Blender"), Unit::Meter);
        memory.save(dir.path()).unwrap();

        let reloaded = ImportMemory::load(dir.path()).unwrap();
        assert_eq!(
            reloaded.recall(Some("/scans/batch"), None),
            Some((Unit::Meter, Remembered::Folder))
        );
        assert_eq!(
            reloaded.recall(None, Some("Blender")),
            Some((Unit::Meter, Remembered::Exporter))
        );
        assert_eq!(reloaded.recall(Some("/elsewhere"), None), None);
    }
}
