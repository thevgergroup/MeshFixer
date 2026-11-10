//! The widget kit: the visual system's pieces (docs/design/visual-language.md,
//! "Visual system"), built from `theme.rs`'s tokens. Panels compose these
//! rather than styling egui widgets one by one, so a section label, a
//! primary button or a badge looks the same everywhere.
//!
//! Every interactive piece reports an accessible label of plain words
//! (never an icon's codepoint): the GUI tests, and screen readers, find
//! widgets by it.

use eframe::egui::{self, RichText};

use crate::{icons, theme};

/// A section label: small, upper-case, muted and letter-spaced, with the
/// grid's spacing above it.
pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(theme::SPACE_S);
    ui.label(
        RichText::new(text.to_uppercase())
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_MUTED)
            .extra_letter_spacing(0.8),
    );
}

/// A caption: small, muted, wrapped.
pub fn caption(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(text.into())
                .size(theme::TYPE_CAPTION)
                .color(theme::TEXT_MUTED),
        )
        .wrap(),
    )
}

/// Body text, wrapped.
pub fn body(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(egui::Label::new(RichText::new(text.into()).color(theme::TEXT)).wrap())
}

/// A title: the type scale's largest size, bright.
pub fn title(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(text.into())
                .size(theme::TYPE_TITLE)
                .color(theme::TEXT_STRONG),
        )
        .truncate(),
    )
}

/// A raised card: the object summary, a tool's group of controls.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::CARD)
        .corner_radius(theme::RADIUS)
        .inner_margin(theme::SPACE_M)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Which of the three button styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Accent-filled: the panel's one main action.
    Primary,
    /// A subtle fill: every other action.
    Secondary,
    /// Text only (a fill under the pointer): minor actions, links.
    Tertiary,
}

/// A button with an icon before its text, at least `width` wide (0: as
/// wide as its content), in one of the three styles, painted by hand so
/// the three read the same everywhere: content centred on a filled
/// (primary, secondary) button, left-aligned on a tertiary one. Its
/// accessible label is `text`. Text too long for the button is cut short
/// ("…"); the tooltip can carry it whole.
pub fn button(
    ui: &mut egui::Ui,
    kind: Kind,
    icon: Option<&str>,
    text: &str,
    width: f32,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let pad = match kind {
        Kind::Tertiary => theme::SPACE_S,
        _ => theme::SPACE_M,
    };
    let icon_w = if icon.is_some() {
        theme::ICON_SIZE + 6.0
    } else {
        0.0
    };
    let natural = ui
        .painter()
        .layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(theme::TYPE_BODY),
            theme::TEXT,
        )
        .size()
        .x;
    let max_w = ui.available_width().max(width);
    let w = (natural + icon_w + 2.0 * pad).max(width).min(max_w);
    let (rect, r) =
        ui.allocate_exact_size(egui::vec2(w, theme::CONTROL_HEIGHT), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = r.hovered() && enabled;
        let pressed = r.is_pointer_button_down_on() && enabled;
        let (fill, fg) = match kind {
            Kind::Primary if !enabled => (theme::CONTROL, theme::TEXT_FAINT),
            Kind::Primary if pressed => (theme::ACCENT.gamma_multiply(0.85), theme::ON_ACCENT),
            Kind::Primary if hovered => (theme::ACCENT_HOVER, theme::ON_ACCENT),
            Kind::Primary => (theme::ACCENT, theme::ON_ACCENT),
            Kind::Secondary if !enabled => (theme::CONTROL.gamma_multiply(0.6), theme::TEXT_FAINT),
            Kind::Secondary if pressed => (theme::CONTROL_ACTIVE, theme::TEXT_STRONG),
            Kind::Secondary if hovered => (theme::CONTROL_HOVER, theme::TEXT_STRONG),
            Kind::Secondary => (theme::CONTROL, theme::TEXT),
            Kind::Tertiary if !enabled => (egui::Color32::TRANSPARENT, theme::TEXT_FAINT),
            Kind::Tertiary if pressed => (theme::CONTROL_ACTIVE, theme::TEXT_STRONG),
            Kind::Tertiary if hovered => (theme::CONTROL_HOVER.gamma_multiply(0.7), theme::TEXT),
            Kind::Tertiary => (egui::Color32::TRANSPARENT, theme::TEXT_MUTED),
        };
        let p = ui.painter();
        p.rect_filled(rect, theme::RADIUS, fill);
        if r.has_focus() {
            p.rect_stroke(
                rect,
                theme::RADIUS,
                egui::Stroke::new(1.5, theme::ACCENT_HOVER),
                egui::StrokeKind::Inside,
            );
        }
        let room = (rect.width() - 2.0 * pad - icon_w).max(8.0);
        let galley = egui::WidgetText::from(RichText::new(text).size(theme::TYPE_BODY).color(fg))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                room,
                egui::TextStyle::Button,
            );
        let content = icon_w + galley.size().x;
        let left = match kind {
            Kind::Tertiary => rect.left() + pad,
            _ => rect.center().x - content / 2.0,
        };
        let mid = rect.center().y;
        if let Some(icon) = icon {
            p.text(
                egui::pos2(left + theme::ICON_SIZE / 2.0, mid),
                egui::Align2::CENTER_CENTER,
                icon,
                icons::font(theme::ICON_SIZE),
                fg,
            );
        }
        p.galley(
            egui::pos2(left + icon_w, mid - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    if r.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let label = text.to_owned();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
    r
}

/// A tab: an icon and a title, the active one bright with an accent bar
/// under it. Its accessible label is `title` (role tab, selected when
/// active).
pub fn tab(ui: &mut egui::Ui, icon: &str, title: &str, on: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        title.to_owned(),
        egui::FontId::proportional(theme::TYPE_BODY),
        theme::TEXT,
    );
    let w = galley.size().x + theme::ICON_SIZE + 6.0 + 2.0 * theme::SPACE_S;
    let (rect, r) =
        ui.allocate_exact_size(egui::vec2(w, theme::CONTROL_HEIGHT), egui::Sense::click());
    let fg = if on {
        theme::TEXT_STRONG
    } else if r.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_MUTED
    };
    let p = ui.painter();
    if r.hovered() && !on {
        p.rect_filled(
            rect,
            theme::RADIUS,
            theme::CONTROL_HOVER.gamma_multiply(0.5),
        );
    }
    let mid = rect.center().y;
    p.text(
        egui::pos2(rect.left() + theme::SPACE_S + theme::ICON_SIZE / 2.0, mid),
        egui::Align2::CENTER_CENTER,
        icon,
        icons::font(theme::ICON_SIZE - 1.0),
        if on { theme::ACCENT_HOVER } else { fg },
    );
    p.text(
        egui::pos2(rect.left() + theme::SPACE_S + theme::ICON_SIZE + 6.0, mid),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(theme::TYPE_BODY),
        fg,
    );
    if on {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 4.0, rect.bottom()),
        );
        p.rect_filled(bar, 1.0, theme::ACCENT);
    }
    let label = title.to_owned();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, &label));
    r
}

/// A full-width button (see [`button`]).
pub fn wide_button(
    ui: &mut egui::Ui,
    kind: Kind,
    icon: Option<&str>,
    text: &str,
) -> egui::Response {
    let w = ui.available_width();
    button(ui, kind, icon, text, w)
}

/// A square icon button `size` points across: transparent at rest, a
/// subtle fill under the pointer, the accent when `selected` (a mode that
/// is on). `label` is its accessible name; the caller adds the tooltip.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    selected: Option<bool>,
    size: f32,
) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let enabled = r.enabled();
    let on = selected == Some(true);
    if ui.is_rect_visible(rect) {
        let fill = if on {
            theme::ACCENT
        } else if r.is_pointer_button_down_on() {
            theme::CONTROL_ACTIVE
        } else if r.hovered() && enabled {
            theme::CONTROL_HOVER
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, theme::RADIUS, fill);
        let color = if !enabled {
            theme::TEXT_FAINT
        } else if on {
            theme::ON_ACCENT
        } else if r.hovered() {
            theme::TEXT_STRONG
        } else {
            theme::TEXT
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            icons::font(theme::ICON_SIZE + 1.0),
            color,
        );
    }
    let label = label.to_owned();
    r.widget_info(|| match selected {
        Some(s) => egui::WidgetInfo::selected(egui::Role::Button, enabled, s, &label),
        None => egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label),
    });
    r
}

/// A segmented control: one track, a segment per option, the current one
/// accent-filled. Each segment is a button labelled with its option's
/// accessible name (and reports whether it is the current one). Returns
/// the option clicked.
pub fn segmented(
    ui: &mut egui::Ui,
    options: &[(&str, &str, bool)],
    current: usize,
) -> (Option<usize>, Vec<egui::Response>) {
    let width = ui.available_width();
    let (track, _) = ui.allocate_exact_size(
        egui::vec2(width, theme::CONTROL_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(track, theme::RADIUS, theme::FIELD);
    let n = options.len().max(1) as f32;
    let seg = (track.width() - 4.0) / n;
    let mut clicked = None;
    let mut responses = Vec::new();
    for (i, &(text, label, enabled)) in options.iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            track.min + egui::vec2(2.0 + i as f32 * seg, 2.0),
            egui::vec2(seg, track.height() - 4.0),
        );
        let id = ui.scope_id().with(("segment", label));
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let r = ui.interact(rect, id, sense);
        let on = i == current && enabled;
        let fill = if on {
            theme::ACCENT
        } else if r.hovered() && enabled {
            theme::CONTROL_HOVER
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, theme::RADIUS - 2.0, fill);
        let color = if !enabled {
            theme::TEXT_FAINT
        } else if on {
            theme::ON_ACCENT
        } else {
            theme::TEXT
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(theme::TYPE_BODY),
            color,
        );
        let label = label.to_owned();
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, &label));
        if r.clicked() {
            clicked = Some(i);
        }
        responses.push(r);
    }
    (clicked, responses)
}

/// A count as a subtle badge at `right_center` (painted: the row it sits
/// in carries the count in its accessible label).
pub fn paint_badge(ui: &egui::Ui, right_center: egui::Pos2, text: &str, color: egui::Color32) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(theme::TYPE_CAPTION),
        color,
    );
    let pad = egui::vec2(6.0, 1.0);
    let size = galley.size() + 2.0 * pad;
    let rect = egui::Rect::from_min_size(right_center - egui::vec2(size.x, size.y / 2.0), size);
    ui.painter()
        .rect_filled(rect, size.y / 2.0, theme::CONTROL.gamma_multiply(0.8));
    ui.painter().galley(rect.min + pad, galley, color);
}

/// A status chip: an icon in `color` and `text`, on a raised pill. Its
/// accessible label is `text`.
pub fn chip(ui: &mut egui::Ui, icon: &str, color: egui::Color32, text: &str) -> egui::Response {
    let width = ui.available_width();
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::proportional(theme::TYPE_BODY),
        theme::TEXT_STRONG,
        width - 36.0,
    );
    let height = (galley.size().y + 10.0).max(theme::CONTROL_HEIGHT);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, theme::RADIUS, theme::CONTROL.gamma_multiply(0.7));
    ui.painter().text(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        icons::font(theme::ICON_SIZE),
        color,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT_STRONG,
    );
    let label = text.to_owned();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    r
}

/// What a disclosure row (a tree category, a Repair section) shows.
pub struct Disclosure<'a> {
    /// Its accessible label (what tests find it by): "Holes (3)".
    pub label: &'a str,
    pub icon: &'a str,
    pub icon_color: egui::Color32,
    /// Its visible name: "Holes".
    pub name: &'a str,
    pub name_color: egui::Color32,
    /// The count, as a badge at the right (`None`: no badge).
    pub count: Option<String>,
    pub default_open: bool,
    /// Opens (or closes) it this frame.
    pub force_open: Option<bool>,
    /// Its tooltip.
    pub tip: &'a str,
}

/// A row that folds its body away: a caret, an icon in its colour, its
/// name and a count badge, as one full-width click target. Its state is
/// egui's collapsing state, keyed by `id_salt`.
pub fn disclosure<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    d: Disclosure<'_>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::Response {
    let id = ui.make_persistent_id(id_salt);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        d.default_open,
    );
    let width = ui.available_width();
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, theme::ROW_HEIGHT), egui::Sense::hover());
    let mut r = ui.interact(rect, id, egui::Sense::click());
    if let Some(open) = d.force_open {
        if open != state.is_open() {
            state.set_open(open);
            r.mark_changed();
        }
    } else if r.clicked() {
        state.toggle(ui);
        r.mark_changed();
    }
    let label = d.label.to_owned();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::DisclosureTriangle, true, &label));
    let open = state.is_open();
    ui.ctx()
        .accesskit_node_builder(r.id, |node| node.set_expanded(open));
    if ui.is_rect_visible(rect) {
        if r.hovered() {
            ui.painter()
                .rect_filled(rect, theme::RADIUS, theme::CONTROL.gamma_multiply(0.6));
        }
        let mid = rect.center().y;
        ui.painter().text(
            egui::pos2(rect.left() + 8.0, mid),
            egui::Align2::CENTER_CENTER,
            if open {
                icons::CARET_DOWN
            } else {
                icons::CARET_RIGHT
            },
            icons::font(11.0),
            theme::TEXT_MUTED,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 26.0, mid),
            egui::Align2::CENTER_CENTER,
            d.icon,
            icons::font(theme::ICON_SIZE - 1.0),
            d.icon_color,
        );
        let badge_room = if d.count.is_some() { 40.0 } else { 0.0 };
        let galley = ui.painter().layout_no_wrap(
            d.name.to_owned(),
            egui::FontId::proportional(theme::TYPE_BODY),
            d.name_color,
        );
        let clip = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 38.0, rect.top()),
            egui::pos2(rect.right() - badge_room, rect.bottom()),
        );
        ui.painter().with_clip_rect(clip).galley(
            egui::pos2(rect.left() + 38.0, mid - galley.size().y / 2.0),
            galley,
            d.name_color,
        );
        if let Some(count) = &d.count {
            paint_badge(
                ui,
                egui::pos2(rect.right() - 4.0, mid),
                count,
                theme::TEXT_MUTED,
            );
        }
    }
    let r = r.on_hover_text(d.tip);
    state.show_body_indented(&r, ui, body);
    r
}
