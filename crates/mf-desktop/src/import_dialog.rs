//! Units for a unitless file (MF-ISSUE-045). STL and OBJ don't say what
//! their numbers measure: a scan in metres would otherwise open a
//! thousand times too small. Opening one asks, showing the model's size
//! read in each unit, with the likely one preselected.

use eframe::egui::{self, RichText};
use mf_io::ImportedModel;
use mf_io::threemf::Unit;

use crate::defects::{measure, unit_symbol};
use crate::theme;

/// The units offered, in the order they're tried when preselecting.
const OFFERED: [Unit; 4] = [Unit::Millimeter, Unit::Meter, Unit::Centimeter, Unit::Inch];

/// A unitless file read and waiting for its unit.
#[derive(Debug)]
pub struct UnitPrompt {
    /// The file's name, for the dialog's title.
    pub name: String,
    /// The model's size along each axis, in the file's own numbers.
    pub extent: [f64; 3],
    /// The unit picked so far (preselected by [`suggested_unit`]).
    pub choice: Unit,
    pub(crate) model: ImportedModel,
}

/// Millimetres in one `unit`.
pub fn millimetres(unit: Unit) -> f64 {
    match unit {
        Unit::Micron => 0.001,
        Unit::Millimeter => 1.0,
        Unit::Centimeter => 10.0,
        Unit::Meter => 1000.0,
        Unit::Inch => 25.4,
        Unit::Foot => 304.8,
    }
}

/// The unit that makes a model whose largest dimension is `largest` (in
/// the file's numbers) between 10 mm and 2 m, trying millimetres, metres,
/// centimetres and inches in that order; millimetres if none does.
pub fn suggested_unit(largest: f64) -> Unit {
    OFFERED
        .into_iter()
        .find(|&u| (10.0..=2000.0).contains(&(largest * millimetres(u))))
        .unwrap_or(Unit::Millimeter)
}

/// The size of a model whose bounds measure `extent`, plainly: "Metres ·
/// 0.500 × 0.400 × 0.300 m (500 × 400 × 300 mm)".
pub fn size_row(unit: Unit, extent: [f64; 3]) -> String {
    let dims = |scale: f64| {
        extent
            .iter()
            .map(|&v| measure(v * scale))
            .collect::<Vec<_>>()
            .join(" × ")
    };
    let name = match unit {
        Unit::Micron => "Microns",
        Unit::Millimeter => "Millimetres",
        Unit::Centimeter => "Centimetres",
        Unit::Meter => "Metres",
        Unit::Inch => "Inches",
        Unit::Foot => "Feet",
    };
    let own = format!("{name} · {} {}", dims(1.0), unit_symbol(unit));
    if unit == Unit::Millimeter {
        own
    } else {
        format!("{own} ({} mm)", dims(millimetres(unit)))
    }
}

impl UnitPrompt {
    pub fn new(name: String, extent: [f64; 3], model: ImportedModel) -> UnitPrompt {
        let largest = extent.iter().copied().fold(0.0, f64::max);
        UnitPrompt {
            name,
            extent,
            choice: suggested_unit(largest),
            model,
        }
    }

    /// The model, with the chosen unit.
    pub(crate) fn into_model(self) -> ImportedModel {
        let mut model = self.model;
        model.unit = self.choice;
        model
    }
}

/// The dialog, centred over the window. Returns the answer once given:
/// `true` to import at the chosen unit, `false` for Cancel or Esc.
pub fn show(prompt: &mut UnitPrompt, ctx: &egui::Context) -> Option<bool> {
    let mut answer = None;
    // Modal: nothing behind it answers until the unit is chosen.
    egui::Modal::new(egui::Id::unique("unit prompt")).show(ctx, |ui| {
        ui.heading(format!("Units for {}", prompt.name));
        ui.label("This file doesn't say what unit its numbers are in.");
        ui.label(
            RichText::new("Pick the one that gives the size you expect:").color(theme::TEXT_MUTED),
        );
        ui.add_space(6.0);
        for unit in OFFERED {
            ui.radio_value(&mut prompt.choice, unit, size_row(unit, prompt.extent));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Import").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
        });
    });
    // Esc cancels; a click outside the dialog doesn't (the choice matters).
    if answer.is_none()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        answer = Some(false);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suggested_unit_puts_the_model_between_10_mm_and_2_m() {
        assert_eq!(suggested_unit(0.495), Unit::Meter, "a scan in metres");
        assert_eq!(suggested_unit(120.0), Unit::Millimeter);
        assert_eq!(suggested_unit(1.5), Unit::Meter);
        assert_eq!(suggested_unit(5.0), Unit::Centimeter);
        assert_eq!(suggested_unit(1e-5), Unit::Millimeter, "nothing fits");
        assert_eq!(suggested_unit(5000.0), Unit::Millimeter, "nothing fits");
    }

    #[test]
    fn size_rows_read_in_each_unit() {
        let e = [0.5, 0.4, 0.3];
        assert_eq!(
            size_row(Unit::Meter, e),
            "Metres · 0.500 × 0.400 × 0.300 m (500 × 400 × 300 mm)"
        );
        assert_eq!(
            size_row(Unit::Millimeter, e),
            "Millimetres · 0.500 × 0.400 × 0.300 mm"
        );
        assert_eq!(
            size_row(Unit::Inch, e),
            "Inches · 0.500 × 0.400 × 0.300 in (12.7 × 10.2 × 7.62 mm)"
        );
    }
}
