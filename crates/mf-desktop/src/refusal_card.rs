//! The one refusal card (ADR-0010 part 2, consolidation plan C16).
//!
//! An operation's refusal reaches the panel of the tool that issued the
//! invocation (recorded on the invocation when it starts, never looked
//! up from the active tool's table), for previews and Apply alike, and is
//! shown here:
//!
//! - its text comes from the i18n catalogue (the spec's `refusals:`
//!   message id), filled from the refusal's typed arguments, with every
//!   length through [`mf_units::format_length`];
//! - the object it names is kept (`'<object>': <detail>`) even when the
//!   catalogue sentence has no `{object}` placeholder;
//! - its next action is the spec registry's typed [`RefusalNext`], shown
//!   as a button, never a per-tool table of codes.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use mf_operations::OpError;
use mf_operations::refusal::{RefusalArg, render_message};
use mf_spec::RefusalNext;

use crate::MeshFixerApp;
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

/// A refusal from an operation, rendered for a person: the op that
/// refused, its code, and the plain-language sentence [`render`] built.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub op_id: String,
    pub code: String,
    pub message: String,
}

impl Refusal {
    /// The refusal `op_id` returned as `e`, or `None` when `e` isn't one
    /// (a cancellation, or a spec/parameter error the panel didn't
    /// cause). A declared refusal (`OpError::Refused`) is rendered through
    /// the catalogue; an undeclared `OpError::Failed` keeps the op's own
    /// sentence, and still goes to the issuing panel rather than the
    /// generic error bar.
    pub fn from_op_error(op_id: &str, e: &OpError) -> Option<Self> {
        match e {
            OpError::Refused { code, args } => Some(Self {
                op_id: op_id.to_owned(),
                code: (*code).to_owned(),
                message: render(op_id, code, args),
            }),
            OpError::Failed { code, message } => Some(Self {
                op_id: op_id.to_owned(),
                code: (*code).to_owned(),
                message: message.clone(),
            }),
            OpError::Spec(_) | OpError::Cancelled => None,
        }
    }

    /// This refusal's next action, from the spec registry.
    pub fn next(&self) -> RefusalNext {
        next_of(&self.op_id, &self.code)
    }
}

/// One argument as text: a length through `format_length` (its own
/// unit, the display unit `format_length` picks), never a bare number.
fn arg_text(value: &RefusalArg) -> String {
    match value {
        RefusalArg::Length(l) => mf_units::format_length(l.value, l.unit),
        RefusalArg::Count(n) => crate::defects::thousands(*n),
        RefusalArg::Text(s) => s.clone(),
    }
}

/// The sentence for refusal `code` of `op_id` with `args`: the spec's
/// catalogue message filled from the typed arguments, prefixed with
/// `'<object>': ` when the refusal names an object the sentence itself
/// doesn't place. A code the spec doesn't declare (or a message id the
/// catalogue lacks: a test enforces it never happens for built-ins)
/// falls back to the registry's own plain rendering, which keeps the
/// same `'<object>': <detail>` shape.
pub fn render(op_id: &str, code: &str, args: &BTreeMap<&'static str, RefusalArg>) -> String {
    let Some(id) = mf_spec::builtin::get(op_id)
        .and_then(|spec| spec.refusal(code))
        .map(|r| r.message.as_str())
        .filter(|id| crate::i18n::has(id))
    else {
        return render_message(args);
    };
    let filled: Vec<(&str, String)> = args.iter().map(|(k, v)| (*k, arg_text(v))).collect();
    let body = crate::i18n::tr(id, &filled);
    let template = crate::i18n::english(id);
    match args.get("object") {
        Some(object) if !template.contains("{object}") => {
            format!("'{}': {body}", arg_text(object))
        }
        _ => body,
    }
}

/// The declared next action for `code` of `op_id` ([`RefusalNext::None`]
/// for an undeclared code or an unknown op).
pub fn next_of(op_id: &str, code: &str) -> RefusalNext {
    mf_spec::builtin::get(op_id)
        .and_then(|spec| spec.refusal(code))
        .map_or(RefusalNext::None, |r| r.next)
}

/// The card: a warning title, the sentence, the code, and the next
/// action as a button.
pub struct RefusalCard<'a> {
    refusal: &'a Refusal,
    title: Option<String>,
}

impl<'a> RefusalCard<'a> {
    pub fn new(refusal: &'a Refusal) -> Self {
        Self {
            refusal,
            title: None,
        }
    }

    /// A different title than "This can't be done yet" (Hollow's
    /// preview says the preview was refused, not the Apply).
    pub fn title(mut self, title: String) -> Self {
        self.title = Some(title);
        self
    }

    pub fn show(self, app: &mut MeshFixerApp, ui: &mut egui::Ui) {
        let r = self.refusal;
        let title = self.title.unwrap_or_else(|| t!("refusal.title"));
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::WARNING)
                        .font(icons::font(theme::ICON_SIZE))
                        .color(theme::TEXT),
                );
                ui.label(RichText::new(title).color(theme::TEXT_STRONG));
            });
            widgets::body(ui, &r.message);
            widgets::caption(ui, format!("({})", r.code));
            ui.add_space(theme::SPACE_XS);
            let next = r.next();
            if let Some((icon, label)) = button(next)
                && widgets::wide_button(ui, Kind::Secondary, Some(icon), &label).clicked()
            {
                app.refusal = None;
                follow(app, &r.op_id, next);
            }
        });
        ui.add_space(theme::SPACE_S);
    }
}

/// The button for `next`, if it has one.
fn button(next: RefusalNext) -> Option<(&'static str, String)> {
    match next {
        RefusalNext::Repair => Some((icons::REPAIR, t!("refusal.repair"))),
        RefusalNext::RunMakeSolid => Some((icons::MAKE_SOLID, t!("refusal.make_solid"))),
        RefusalNext::UseLargerCell => Some((icons::MAKE_SOLID, t!("refusal.use_larger_cell"))),
        RefusalNext::MovePlane => Some((icons::SPLIT, t!("refusal.move_plane"))),
        RefusalNext::Undo => Some((icons::UNDO, t!("refusal.undo"))),
        RefusalNext::None => None,
    }
}

/// Does what `next` says, for a refusal of `op_id`.
fn follow(app: &mut MeshFixerApp, op_id: &str, next: RefusalNext) {
    match next {
        RefusalNext::Repair => app.open_rail_tool(crate::tool_panels::REPAIR),
        RefusalNext::RunMakeSolid => app.open_rail_tool(crate::shape_tools::MAKE_SOLID),
        RefusalNext::UseLargerCell => match op_id {
            "mesh.hollow" => {
                // `None` is the automatic cell, which the op already
                // sizes from the wall and the model: only a typed cell
                // has a value to make larger (the field's own range
                // clamps it when drawn).
                if let Some(d) = app.hollow.detail.as_mut() {
                    d.value *= 2.0;
                }
            }
            _ => {
                if let Some(c) = app
                    .tool_state_mut::<crate::shape_tools::SolidState>()
                    .cell
                    .as_mut()
                {
                    c.value *= 2.0;
                }
                app.open_rail_tool(crate::shape_tools::MAKE_SOLID);
            }
        },
        RefusalNext::MovePlane => {
            app.open_rail_tool(crate::shape_tools::SPLIT);
            app.split.mode = mf_render::GizmoMode::Move;
        }
        RefusalNext::Undo => app.undo(),
        RefusalNext::None => {}
    }
}

/// Shows `app.refusal` in the panel being drawn when it names `op_id`
/// and has no recorded issuer: a refusal a panel built itself (a
/// preview's, Parts' unreadable fit settings, Connect's no-joint). A
/// refused command's card is the shell's: the tool dock draws it in its
/// issuer's panel ([`show_issued`]). Returns true if it was shown.
pub(crate) fn show_for(app: &mut MeshFixerApp, ui: &mut egui::Ui, op_id: &str) -> bool {
    let Some(r) = app.refusal.clone().filter(|r| r.op_id == op_id) else {
        return false;
    };
    if app.refusal_issuer().is_some() {
        return false;
    }
    RefusalCard::new(&r).show(app, ui);
    true
}

/// Shows `app.refusal` in the open tool's panel when that tool issued the
/// command it came back from, whatever op it was (Repair runs several,
/// Sculpt's strokes are commands too). Returns true if it was shown.
pub(crate) fn show_issued(app: &mut MeshFixerApp, ui: &mut egui::Ui) -> bool {
    if app.refusal_issuer() != Some(app.active_tool) {
        return false;
    }
    let Some(r) = app.refusal.clone() else {
        return false;
    };
    RefusalCard::new(&r).show(app, ui);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_refusal_keeps_its_object_and_its_next() {
        let args = BTreeMap::from([
            ("object", RefusalArg::Text("tangle".into())),
            ("detail", RefusalArg::Text("near the cutter".into())),
        ]);
        let text = render("mesh.combine", "operand_overlaps_itself", &args);
        assert_eq!(
            text,
            "'tangle': This part of the model overlaps itself near the tool (near the cutter)"
        );
        assert_eq!(
            next_of("mesh.combine", "operand_overlaps_itself"),
            RefusalNext::RunMakeSolid
        );
        assert_eq!(next_of("mesh.combine", "no_such_code"), RefusalNext::None);
    }

    #[test]
    fn a_length_argument_is_formatted_with_its_unit() {
        let args = BTreeMap::from([(
            "detail",
            RefusalArg::Length(mf_units::Length::new(2.0, mf_units::Unit::Millimeter)),
        )]);
        let text = render("mesh.hollow", "invalid_parameter", &args);
        assert_eq!(
            text,
            mf_units::format_length(2.0, mf_units::Unit::Millimeter)
        );
        assert!(text.contains("mm"), "{text}");
    }
}
