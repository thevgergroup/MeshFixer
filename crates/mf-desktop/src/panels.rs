use eframe::egui;
use mf_spec::{ParamKind, ParamValue};

use crate::app::MeshFixerApp;

pub fn menu_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    egui::Panel::top("menu").show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let idle = app.busy.is_none();
                if ui.add_enabled(idle, egui::Button::new("Open…")).clicked() {
                    app.open_dialog(&ctx);
                    ui.close();
                }
                let can_export = idle && app.active_mesh().is_some();
                if ui
                    .add_enabled(can_export, egui::Button::new("Export…"))
                    .clicked()
                {
                    app.export_dialog(&ctx);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(
                        app.session.can_undo(),
                        egui::Button::new("Undo").shortcut_text("Cmd+Z"),
                    )
                    .clicked()
                {
                    app.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        app.session.can_redo(),
                        egui::Button::new("Redo").shortcut_text("Shift+Cmd+Z"),
                    )
                    .clicked()
                {
                    app.redo();
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut app.show_boundaries, "Show boundary loops");
                if ui.button("Fit to view (F)").clicked() {
                    app.fit_camera();
                    ui.close();
                }
            });
        });
    });
}

pub fn error_banner(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let Some(message) = app.error.clone() else {
        return;
    };
    egui::Panel::top("error")
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(120, 30, 30))
                .inner_margin(6.0),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::WHITE, format!("Error: {message}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Dismiss").clicked() {
                        app.error = None;
                    }
                });
            });
        });
}

pub fn status_bar(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("status").show(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some(busy) = &app.busy {
                ui.spinner();
                ui.label(busy);
            } else if app.is_analyzing() {
                ui.spinner();
                ui.label("Analyzing…");
            } else if let Some(status) = &app.status {
                ui.label(status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "view frames rendered: {}",
                    app.viewport.frames_rendered
                ));
            });
        });
    });
}

pub fn side_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    egui::Panel::right("side")
        .default_size(340.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                objects(app, ui);
                ui.separator();
                diagnostics(app, ui);
                ui.separator();
                operations(app, ui, &ctx);
                ui.separator();
                history(app, ui);
            });
        });
}

fn objects(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.heading("Objects");
    let objects: Vec<_> = app
        .session
        .document()
        .objects()
        .iter()
        .map(|o| (o.id, o.name.clone(), o.mesh.triangle_count()))
        .collect();
    if objects.is_empty() {
        ui.weak("No objects");
    }
    for (id, name, tris) in objects {
        if ui
            .selectable_label(
                app.active == Some(id),
                format!("{name}  ({tris} triangles)"),
            )
            .clicked()
        {
            app.active = Some(id);
            app.camera = None;
        }
    }
}

fn diagnostics(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.heading("Diagnostics");
    let Some(analysis) = &app.analysis else {
        ui.weak(if app.is_analyzing() {
            "Analyzing…"
        } else {
            "—"
        });
        return;
    };
    let d = &analysis.diagnostics;
    let flag = |ok: bool| if ok { "yes" } else { "no" };
    egui::Grid::new("diag")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(k);
                ui.label(v);
                ui.end_row();
            };
            row("Triangles", d.triangle_count.to_string());
            row("Vertices", d.vertex_count.to_string());
            row("Components", d.components.to_string());
            row("Closed", flag(d.is_closed).into());
            row("Manifold", flag(d.is_manifold).into());
            row("Boundary loops", d.boundary_loops.len().to_string());
            row("Boundary edges", d.boundary_edges.to_string());
            row(
                "Untraced boundary edges",
                d.untraced_boundary_edges.to_string(),
            );
            row("Non-manifold edges", d.non_manifold_edges.to_string());
            row("Non-manifold vertices", d.non_manifold_vertices.to_string());
            row("Degenerate triangles", d.degenerate_triangles.to_string());
            row("Duplicate vertices", d.duplicate_vertices_exact.to_string());
            row(
                "Inconsistent edges",
                d.inconsistent_orientation_edges.to_string(),
            );
            row("Surface area", format!("{:.4}", d.surface_area));
            row(
                "Volume",
                d.signed_volume
                    .map_or("— (not closed)".into(), |v| format!("{v:.4}")),
            );
            if let Some(bb) = d.bounding_box {
                row(
                    "Size",
                    format!(
                        "{:.3} × {:.3} × {:.3}",
                        bb.max[0] - bb.min[0],
                        bb.max[1] - bb.min[1],
                        bb.max[2] - bb.min[2]
                    ),
                );
            }
        });
    if !d.boundary_loops.is_empty() {
        ui.collapsing(
            format!("Boundary loops ({})", d.boundary_loops.len()),
            |ui| {
                for l in d.boundary_loops.iter().take(200) {
                    ui.label(format!(
                        "#{}: {} edges, perimeter {:.3}",
                        l.id, l.edge_count, l.perimeter
                    ));
                }
                if d.boundary_loops.len() > 200 {
                    ui.weak(format!("…and {} more", d.boundary_loops.len() - 200));
                }
            },
        );
    }
}

/// Operation panel generated entirely from the operation spec (MF-SPEC-003).
fn operations(app: &mut MeshFixerApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.heading("Operations");
    let specs: Vec<_> = mf_spec::builtin::specs()
        .iter()
        .filter(|s| !s.is_read_only())
        .collect();
    egui::ComboBox::from_id_salt("op")
        .selected_text(app.selected_op.clone())
        .show_ui(ui, |ui| {
            for s in &specs {
                ui.selectable_value(&mut app.selected_op, s.id.clone(), &s.id);
            }
        });
    let Some(spec) = mf_spec::builtin::get(&app.selected_op) else {
        return;
    };
    ui.label(&spec.summary);
    let op_id = spec.id.clone();
    let values = app.params_for(&op_id);
    egui::Grid::new("params").num_columns(2).show(ui, |ui| {
        for p in &spec.parameters {
            ui.label(&p.name).on_hover_text(&p.description);
            let Some(value) = values.get_mut(&p.name) else {
                continue;
            };
            match (&p.kind, value) {
                (ParamKind::Integer { min, max, .. }, ParamValue::Int(v)) => {
                    let range = min.unwrap_or(i64::MIN)..=max.unwrap_or(i64::MAX);
                    ui.add(egui::DragValue::new(v).range(range))
                        .on_hover_text(&p.description);
                }
                (ParamKind::Float { min, max, .. }, ParamValue::Float(v)) => {
                    let range = min.unwrap_or(f64::MIN)..=max.unwrap_or(f64::MAX);
                    let suffix = p
                        .unit
                        .as_deref()
                        .map(|u| format!(" {u}"))
                        .unwrap_or_default();
                    ui.add(
                        egui::DragValue::new(v)
                            .range(range)
                            .speed(0.01)
                            .suffix(suffix),
                    )
                    .on_hover_text(&p.description);
                }
                (ParamKind::Bool { .. }, ParamValue::Bool(v)) => {
                    ui.checkbox(v, "").on_hover_text(&p.description);
                }
                _ => {
                    ui.label("type mismatch");
                }
            }
            ui.end_row();
        }
    });
    ui.horizontal(|ui| {
        let can_apply = app.busy.is_none() && app.active.is_some();
        if ui
            .add_enabled(can_apply, egui::Button::new("Apply"))
            .clicked()
        {
            app.apply(ctx, op_id.clone());
        }
        if ui.button("Defaults").clicked() {
            app.op_params.remove(&op_id);
        }
    });
}

fn history(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    ui.heading("History");
    ui.horizontal(|ui| {
        let idle = app.busy.is_none();
        if ui
            .add_enabled(idle && app.session.can_undo(), egui::Button::new("Undo"))
            .clicked()
        {
            app.undo();
        }
        if ui
            .add_enabled(idle && app.session.can_redo(), egui::Button::new("Redo"))
            .clicked()
        {
            app.redo();
        }
    });
    let cursor = app.session.history_cursor();
    for (i, label) in app.session.history_labels().iter().enumerate() {
        let text = egui::RichText::new(label);
        let text = if i == cursor {
            text.strong()
        } else if i > cursor {
            text.weak().italics()
        } else {
            text
        };
        ui.label(text);
    }
}
