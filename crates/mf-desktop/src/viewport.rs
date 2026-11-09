//! 3D viewport. The scene renders into an `mf-render` [`RenderTarget`] that
//! egui shows as an image, so the renderer is independent of egui's own
//! render pass formats. It only re-renders when something changed.

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use mf_command::ObjectId;
use mf_render::{
    GpuFaces, GpuLines, GpuMesh, Layer, LayerGeometry, LineVertex, MeshRenderer, MeshVertices,
    OrbitCamera, Palette, RenderTarget, SurfaceVertex, model_matrix,
};

use crate::theme;

pub const MSAA_SAMPLES: u32 = 4;

#[derive(Clone, PartialEq)]
struct FrameKey {
    camera: OrbitCamera,
    show_boundaries: bool,
    size: (u32, u32),
    /// Which object is active (desaturation depends on this).
    active: Option<ObjectId>,
    /// Identity and colour of each overlay layer, in draw order.
    layers: Vec<(ObjectId, usize, [u8; 4], u32)>,
}

/// One overlay to draw on an object (built by a worker; see app.rs).
pub struct ViewLayer {
    pub object: ObjectId,
    pub geometry: ViewGeometry,
    pub color: egui::Color32,
    /// Face layers: how much of `color` replaces the surface colour.
    pub strength: f32,
}

pub enum ViewGeometry {
    Lines(Arc<Vec<LineVertex>>),
    Faces(Arc<Vec<SurfaceVertex>>),
}

impl ViewGeometry {
    fn identity(&self) -> usize {
        match self {
            ViewGeometry::Lines(v) => Arc::as_ptr(v) as usize,
            ViewGeometry::Faces(v) => Arc::as_ptr(v) as usize,
        }
    }
}

/// A layer's GPU buffers, kept while the same vertex set is drawn.
enum GpuLayer {
    Lines(GpuLines),
    Faces(GpuFaces),
}

/// One object to draw this frame, at its own transform (MF-ISSUE-042).
pub struct ObjectView {
    pub id: ObjectId,
    pub vertices: Arc<MeshVertices>,
    pub transform: [f64; 12],
    pub active: bool,
}

/// How to draw the view this frame.
pub struct ViewOptions<'a> {
    pub show_boundaries: bool,
    /// Overlays, bottom to top.
    pub layers: Vec<ViewLayer>,
    /// The geometry shown is from the previous revision while the current one is analysed.
    pub updating: bool,
    /// Text shown when there's nothing to draw.
    pub placeholder: &'a str,
}

/// A primary click in the viewport (not a drag), in points relative to the
/// viewport's top-left, with the viewport size in points.
#[derive(Debug, Clone, Copy)]
pub struct Click {
    pub px: (f64, f64),
    pub size: (f64, f64),
    pub shift: bool,
}

/// What a plain primary-button drag does in the 3D view (the navigation
/// bar's mode switch). Option/Alt + drag always zooms, Shift + drag pans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavMode {
    Orbit,
    Pan,
    Zoom,
}

/// A camera move asked for from the navigation bar that needs the app
/// (it knows the selection and every object's bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    FrameSelection,
    ResetView,
}

/// What happened in the viewport this frame.
#[derive(Debug, Default)]
pub struct ViewOutput {
    pub click: Option<Click>,
    pub action: Option<NavAction>,
}

/// One zoom step of the zoom buttons and keys.
pub const ZOOM_STEP: f64 = 1.25;
/// Holding a zoom button repeats after this long (seconds)…
const REPEAT_DELAY: f64 = 0.4;
/// …every this long.
const REPEAT_EVERY: f64 = 0.12;

pub struct Viewport {
    /// What a plain primary drag does.
    pub nav_mode: NavMode,
    /// A held zoom button: (zoom in?, when it was pressed, repeats applied).
    hold: Option<(bool, f64, u32)>,
    render_state: RenderState,
    renderer: MeshRenderer,
    target: Option<RenderTarget>,
    texture: Option<egui::TextureId>,
    /// GPU meshes uploaded for the objects drawn last frame, by object id.
    uploaded: Vec<(ObjectId, Arc<MeshVertices>, GpuMesh)>,
    /// Overlay buffers by vertex-set identity (the `Arc` keeps it alive).
    layers: Vec<(usize, GpuLayer)>,
    last_frame: Option<FrameKey>,
    /// Where the 3D view was last drawn (for picking and screen projections).
    pub last_rect: Option<egui::Rect>,
    /// Frames actually rendered (for tests and the debug readout).
    pub frames_rendered: u64,
}

impl Viewport {
    pub fn new(render_state: RenderState, palette: Palette) -> Viewport {
        let renderer = MeshRenderer::new(
            &render_state.device,
            RenderTarget::format(MSAA_SAMPLES),
            palette,
        );
        Viewport {
            nav_mode: NavMode::Orbit,
            hold: None,
            render_state,
            renderer,
            target: None,
            texture: None,
            uploaded: Vec::new(),
            layers: Vec::new(),
            last_frame: None,
            last_rect: None,
            frames_rendered: 0,
        }
    }

    /// Draw the viewport into the remaining space and handle camera input.
    /// Every object of `objects` is drawn at its own transform
    /// (MF-ISSUE-042): the active one full colour, the rest dimmed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        camera: &mut Option<OrbitCamera>,
        objects: &[ObjectView],
        options: ViewOptions<'_>,
    ) -> ViewOutput {
        let ViewOptions {
            show_boundaries,
            layers,
            updating,
            placeholder,
        } = options;
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let (Some(cam), false) = (camera.as_mut(), objects.is_empty()) else {
            // Nothing ready to draw yet (no camera, or no object's vertices
            // are in hand): release GPU buffers for a vanished document
            // (MF-ISSUE-009).
            self.uploaded.clear();
            self.layers.clear();
            self.last_frame = None;
            self.last_rect = None;
            ui.painter()
                .rect_filled(rect, 0.0, theme::VIEWPORT_BACKGROUND);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                placeholder,
                egui::FontId::proportional(16.0),
                theme::TEXT_MUTED,
            );
            return ViewOutput::default();
        };
        self.last_rect = Some(rect);
        let click = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .map(|p| Click {
                px: ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64),
                size: (rect.width() as f64, rect.height() as f64),
                shift: ui.input(|i| i.modifiers.shift),
            });
        handle_input(ui, &response, rect, cam, self.nav_mode);

        // Clamp to the device limit; egui scales the image up if needed.
        let ppp = ui.ctx().pixels_per_point();
        self.renderer.set_pixels_per_point(ppp);
        let max = RenderTarget::max_dimension(&self.render_state.device) as f32;
        let size = (
            (rect.width() * ppp).round().clamp(1.0, max) as u32,
            (rect.height() * ppp).round().clamp(1.0, max) as u32,
        );
        let texture = self.render(*cam, objects, show_boundaries, &layers, size);
        ui.painter().image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        if updating {
            ui.painter().text(
                rect.left_top() + egui::vec2(8.0, 8.0),
                egui::Align2::LEFT_TOP,
                "Updating view…",
                egui::FontId::proportional(14.0),
                theme::TEXT_MUTED,
            );
        }
        // The controls hint, on a backdrop so it reads over the model. Kept
        // short so it clears the navigation bar at the bottom-right; Help >
        // Controls lists everything.
        let hint = ui.painter().layout_no_wrap(
            "Drag: orbit · Shift-drag: pan · Alt-drag: zoom · F: frame · Help > Controls"
                .to_owned(),
            egui::FontId::proportional(12.0),
            theme::TEXT_MUTED,
        );
        let at = rect.left_bottom() + egui::vec2(8.0, -8.0 - hint.size().y);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(at, hint.size()).expand(4.0),
            4.0,
            theme::VIEWPORT_BACKGROUND.gamma_multiply(0.85),
        );
        ui.painter().galley(at, hint, theme::TEXT_MUTED);
        let action = self.navigation_bar(ui, rect, cam);
        ViewOutput { click, action }
    }

    /// The on-screen navigation bar at the view's bottom-right, for people
    /// without a mouse wheel: drag mode (Orbit / Pan / Zoom), zoom out / in
    /// (hold to repeat), frame selection and reset view. Camera maths only.
    fn navigation_bar(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        cam: &mut OrbitCamera,
    ) -> Option<NavAction> {
        let mut action = None;
        let mut zoom_steps = 0i32;
        let mut held = None;
        egui::Area::new(egui::Id::unique("navigation bar"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(rect.right_bottom() - egui::vec2(10.0, 10.0))
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::VIEWPORT_BACKGROUND.gamma_multiply(0.92))
                    .stroke(egui::Stroke::new(
                        1.0,
                        theme::TEXT_MUTED.gamma_multiply(0.4),
                    ))
                    .corner_radius(6.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (mode, label, tip) in [
                                (NavMode::Orbit, "Orbit", "Drag orbits"),
                                (NavMode::Pan, "Pan", "Drag pans (or Shift + drag)"),
                                (
                                    NavMode::Zoom,
                                    "Zoom",
                                    "Drag up/down zooms (or Option/Alt + drag)",
                                ),
                            ] {
                                if ui
                                    .selectable_label(self.nav_mode == mode, label)
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    self.nav_mode = mode;
                                }
                            }
                            ui.separator();
                            for (zoom_in, text, label, tip) in [
                                (false, "−", "Zoom out", "Zoom out (−); hold to repeat"),
                                (true, "+", "Zoom in", "Zoom in (+ or =); hold to repeat"),
                            ] {
                                let r = labelled(ui.button(text), label).on_hover_text(tip);
                                if r.clicked() {
                                    zoom_steps += if zoom_in { 1 } else { -1 };
                                }
                                if r.is_pointer_button_down_on() {
                                    held = Some(zoom_in);
                                }
                            }
                            ui.separator();
                            if labelled(ui.button("Frame"), "Frame selection")
                                .on_hover_text("Frame selection (F)")
                                .clicked()
                            {
                                action = Some(NavAction::FrameSelection);
                            }
                            if labelled(ui.button("Reset"), "Reset view")
                                .on_hover_text("Reset view (Home, or Cmd/Ctrl+0)")
                                .clicked()
                            {
                                action = Some(NavAction::ResetView);
                            }
                        });
                    });
            });
        // Holding a zoom button repeats the step.
        let now = ui.input(|i| i.time);
        match (held, self.hold) {
            (Some(zoom_in), Some((was_in, start, done))) if zoom_in == was_in => {
                let due = if now - start < REPEAT_DELAY {
                    0
                } else {
                    ((now - start - REPEAT_DELAY) / REPEAT_EVERY) as u32 + 1
                };
                let sign = if zoom_in { 1 } else { -1 };
                zoom_steps += sign * (due.saturating_sub(done)) as i32;
                self.hold = Some((zoom_in, start, due.max(done)));
                ui.ctx().request_repaint();
            }
            (Some(zoom_in), _) => {
                self.hold = Some((zoom_in, now, 0));
                ui.ctx().request_repaint();
            }
            (None, _) => self.hold = None,
        }
        if zoom_steps != 0 {
            cam.zoom(ZOOM_STEP.powi(-zoom_steps));
        }
        action
    }

    fn render(
        &mut self,
        camera: OrbitCamera,
        objects: &[ObjectView],
        show_boundaries: bool,
        layers: &[ViewLayer],
        size: (u32, u32),
    ) -> egui::TextureId {
        let device = &self.render_state.device;
        let mut dirty = false;

        let resize = self
            .target
            .as_ref()
            .is_none_or(|t| (t.width, t.height) != size);
        if resize {
            let target = RenderTarget::new(device, size.0, size.1, MSAA_SAMPLES)
                .expect("viewport size is clamped to the device limit");
            let mut egui_renderer = self.render_state.renderer.write();
            let id = match self.texture {
                Some(id) => {
                    egui_renderer.update_egui_texture_from_wgpu_texture(
                        device,
                        target.view(),
                        wgpu::FilterMode::Linear,
                        id,
                    );
                    id
                }
                None => egui_renderer.register_native_texture(
                    device,
                    target.view(),
                    wgpu::FilterMode::Linear,
                ),
            };
            self.texture = Some(id);
            self.target = Some(target);
            dirty = true;
        }

        // Upload only objects whose vertex set changed; drop ones no longer
        // in the document. Vertex data was built on a worker thread.
        let mut uploaded = Vec::with_capacity(objects.len());
        for o in objects {
            let existing = self
                .uploaded
                .iter()
                .position(|(id, v, _)| *id == o.id && Arc::ptr_eq(v, &o.vertices));
            let entry = match existing {
                Some(i) => self.uploaded.swap_remove(i),
                None => {
                    dirty = true;
                    (
                        o.id,
                        o.vertices.clone(),
                        GpuMesh::from_vertices(device, &o.vertices),
                    )
                }
            };
            uploaded.push(entry);
        }
        // Anything left in `self.uploaded` belonged to an object no longer
        // drawn this frame (removed, or no longer the active/ready one).
        dirty |= !self.uploaded.is_empty();
        self.uploaded = uploaded;

        // Overlays upload separately from the meshes (MF-RENDER-003), and
        // only when their vertex set changes. Layers on an object that isn't
        // drawn this frame are skipped.
        let drawn: Vec<&ViewLayer> = layers
            .iter()
            .filter(|l| objects.iter().any(|o| o.id == l.object))
            .collect();
        let mut kept = Vec::with_capacity(drawn.len());
        for l in &drawn {
            let id = l.geometry.identity();
            let entry = match self.layers.iter().position(|(i, _)| *i == id) {
                Some(i) => self.layers.swap_remove(i),
                None => (
                    id,
                    match &l.geometry {
                        ViewGeometry::Lines(v) => GpuLayer::Lines(GpuLines::upload(device, v)),
                        ViewGeometry::Faces(v) => GpuLayer::Faces(GpuFaces::upload(device, v)),
                    },
                ),
            };
            kept.push(entry);
        }
        self.layers = kept;
        let key = FrameKey {
            camera,
            show_boundaries,
            size,
            // Content changes (a new vertex set, an object added or
            // removed) already set `dirty` above; this only needs to catch
            // a change of *which* object is active (dimming changes).
            active: objects.iter().find(|o| o.active).map(|o| o.id),
            layers: drawn
                .iter()
                .map(|l| {
                    (
                        l.object,
                        l.geometry.identity(),
                        l.color.to_array(),
                        l.strength.to_bits(),
                    )
                })
                .collect(),
        };
        dirty |= self.last_frame.as_ref() != Some(&key);

        if dirty && let Some(target) = &self.target {
            let items: Vec<mf_render::DrawItem<'_>> = objects
                .iter()
                .zip(&self.uploaded)
                .map(|(o, (_, _, gpu))| mf_render::DrawItem {
                    mesh: gpu,
                    model: model_matrix(&o.transform),
                    inactive: !o.active,
                })
                .collect();
            let gpu_layers: Vec<Layer<'_>> = drawn
                .iter()
                .zip(&self.layers)
                .filter_map(|(l, (_, gpu))| {
                    let o = objects.iter().find(|o| o.id == l.object)?;
                    Some(Layer {
                        geometry: match gpu {
                            GpuLayer::Lines(g) => LayerGeometry::Lines(g),
                            GpuLayer::Faces(g) => LayerGeometry::Faces(g),
                        },
                        model: model_matrix(&o.transform),
                        mesh_origin: o.vertices.origin,
                        color: theme::rgba(l.color),
                        strength: l.strength,
                    })
                })
                .collect();
            target.draw(
                device,
                &self.render_state.queue,
                &mut self.renderer,
                Some(&camera),
                &items,
                show_boundaries,
                &gpu_layers,
            );
            self.last_frame = Some(key);
            self.frames_rendered += 1;
        }
        self.texture
            .expect("the texture is registered whenever a render target exists")
    }
}

/// Mouse: left-drag orbits, right- or middle-drag pans, the wheel zooms
/// to the cursor. Trackpad: two-finger drag orbits, Shift + two-finger drag
/// pans, pinch zooms (to the cursor). egui reports both wheels and
/// two-finger drags as scroll events; the unit tells them apart (points:
/// trackpad, lines or pages: wheel). Momentum after the fingers lift is
/// ignored so the view stops when you stop.
fn handle_input(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    cam: &mut OrbitCamera,
    mode: NavMode,
) {
    let h = rect.height().max(1.0) as f64;
    let size = (rect.width() as f64, rect.height() as f64);
    let delta = response.drag_delta();
    let (alt, shift, origin) =
        ui.input(|i| (i.modifiers.alt, i.modifiers.shift, i.pointer.press_origin()));
    let primary = response.dragged_by(egui::PointerButton::Primary);
    let drag = if response.dragged_by(egui::PointerButton::Secondary)
        || response.dragged_by(egui::PointerButton::Middle)
    {
        Some(NavMode::Pan)
    } else if primary && alt {
        // One-button devices: Option/Alt + drag zooms, Shift + drag pans.
        Some(NavMode::Zoom)
    } else if primary && shift {
        Some(NavMode::Pan)
    } else if primary {
        Some(mode)
    } else {
        None
    };
    match drag {
        Some(NavMode::Pan) => cam.pan(delta.x as f64 / h, delta.y as f64 / h),
        Some(NavMode::Orbit) => cam.orbit(delta.x as f64 * 0.01, delta.y as f64 * 0.01),
        Some(NavMode::Zoom) if delta.y != 0.0 => {
            // Up zooms in, toward where the drag started.
            let at = origin.map_or((size.0 * 0.5, size.1 * 0.5), |p| {
                ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64)
            });
            cam.zoom_at((delta.y as f64 * 0.01).exp(), size, at);
        }
        Some(NavMode::Zoom) | None => {}
    }
    if !response.hovered() {
        return;
    }
    let (events, zoom, pointer) = ui.input(|i| {
        let wheels: Vec<_> = i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    source,
                    modifiers,
                    ..
                } => Some((*unit, *delta, *source, *modifiers)),
                _ => None,
            })
            .collect();
        (wheels, i.zoom_delta(), i.pointer.hover_pos())
    });
    let cursor = pointer.map_or((size.0 * 0.5, size.1 * 0.5), |p| {
        ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64)
    });
    // Pinch (and Ctrl/Cmd + wheel, which egui turns into zoom).
    if zoom != 1.0 {
        cam.zoom_at(1.0 / zoom as f64, size, cursor);
    }
    for (unit, delta, source, modifiers) in events {
        if modifiers.ctrl || modifiers.command || source == egui::MouseWheelSource::Momentum {
            continue;
        }
        match unit {
            egui::MouseWheelUnit::Point if modifiers.shift => {
                // Fingers move the model like a drag does.
                cam.pan(delta.x as f64 / h, delta.y as f64 / h);
            }
            egui::MouseWheelUnit::Point => {
                cam.orbit(delta.x as f64 * 0.01, delta.y as f64 * 0.01);
            }
            egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => {
                let lines = if unit == egui::MouseWheelUnit::Page {
                    delta.y as f64 * 10.0
                } else {
                    delta.y as f64
                };
                cam.zoom_at((-lines * 0.1).exp(), size, cursor);
            }
        }
    }
}

/// Give a symbol button an accessible name (and the name tests find it by).
fn labelled(response: egui::Response, label: &str) -> egui::Response {
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    response
}
