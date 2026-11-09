//! 3D viewport. The scene renders into an `mf-render` [`RenderTarget`] that
//! egui shows as an image, so the renderer is independent of egui's own
//! render pass formats. It only re-renders when something changed.

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use mf_command::ObjectId;
use mf_metrics::GpuTime;
use mf_render::{
    GpuLines, GpuTimer, Layer, MeshRenderer, OrbitCamera, Palette, RenderTarget, model_matrix,
};

use crate::theme;
use crate::worker::RenderMesh;

pub const MSAA_SAMPLES: u32 = 4;

#[derive(Clone, PartialEq)]
struct FrameKey {
    camera: OrbitCamera,
    show_boundaries: bool,
    size: (u32, u32),
    /// Which object is active (desaturation depends on this).
    active: Option<ObjectId>,
    /// Each object drawn: the identity of its render mesh, its face-state
    /// generation (a selection change redraws) and the states shown.
    objects: Vec<(ObjectId, usize, u64, u32)>,
    /// Identity and colour of each overlay layer, in draw order.
    layers: Vec<(ObjectId, usize, [u8; 4])>,
}

/// One line overlay to draw on an object (built by a worker; see app.rs).
/// Face tints aren't layers: they're the render mesh's face states.
pub struct ViewLayer {
    pub object: ObjectId,
    /// The local origin the layer's vertices are relative to (an overlay's
    /// own; it needn't be the object's render mesh's).
    pub origin: glam::DVec3,
    /// Built on a worker (the view never uploads).
    pub lines: Arc<GpuLines>,
    pub color: egui::Color32,
}

/// One object to draw this frame, at its own transform (MF-ISSUE-042).
pub struct ObjectView {
    pub id: ObjectId,
    /// Built on a worker, GPU buffers included (MF-RT-001).
    pub render: Arc<RenderMesh>,
    pub transform: [f64; 12],
    pub active: bool,
    /// Which face-state flags tint it (`mf_render::FaceStates`).
    pub shown_states: u32,
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
    /// The app's frame index (`FrameTimes`), to tag the GPU time with.
    pub frame: u64,
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
    /// Where the pointer rests over the 3D view (points from its top-left,
    /// and the view's size), with no button held: what hover picks under.
    pub pointer: Option<((f64, f64), (f64, f64))>,
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
    last_frame: Option<FrameKey>,
    /// Where the 3D view was last drawn (for picking and screen projections).
    pub last_rect: Option<egui::Rect>,
    /// Frames actually rendered (for tests and the debug readout).
    pub frames_rendered: u64,
    /// Times the 3D view's pass on the GPU; `None` when the device can't.
    timer: Option<GpuTimer>,
    /// The last frame that redrew the 3D view, and whether it was timed.
    redrawn: Option<(u64, bool)>,
}

impl Viewport {
    pub fn new(render_state: RenderState, palette: Palette) -> Viewport {
        let mut renderer = MeshRenderer::new(
            &render_state.device,
            RenderTarget::format(MSAA_SAMPLES),
            palette,
        );
        warm_up(&render_state.device, &render_state.queue, &mut renderer);
        let timer = GpuTimer::new(&render_state.device, &render_state.queue);
        Viewport {
            timer,
            redrawn: None,
            nav_mode: NavMode::Orbit,
            hold: None,
            render_state,
            renderer,
            target: None,
            texture: None,
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
            frame,
        } = options;
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let (Some(cam), false) = (camera.as_mut(), objects.is_empty()) else {
            // Nothing ready to draw yet (no camera, or no object's vertices
            // are in hand): release GPU buffers for a vanished document
            // (MF-ISSUE-009).
            self.last_frame = None;
            self.last_rect = None;
            let size = self.target_size(ui, rect);
            self.ensure_target(size);
            // Laid out, not painted, so the first frame that draws a model
            // doesn't also rasterize this text's glyphs (MF-RT-001).
            let _warm = (controls_hint(ui), updating_text(ui));
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
        let pointer = response
            .hover_pos()
            .filter(|_| !ui.input(|i| i.pointer.any_down()))
            .map(|p| {
                (
                    ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64),
                    (rect.width() as f64, rect.height() as f64),
                )
            });
        handle_input(ui, &response, rect, cam, self.nav_mode);

        self.renderer
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        let size = self.target_size(ui, rect);
        let texture = self.render(*cam, objects, show_boundaries, &layers, size, frame);
        ui.painter().image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        if updating {
            ui.painter().galley(
                rect.left_top() + egui::vec2(8.0, 8.0),
                updating_text(ui),
                theme::TEXT_MUTED,
            );
        }
        // The controls hint, on a backdrop so it reads over the model. Kept
        // short so it clears the navigation bar at the bottom-right; Help >
        // Controls lists everything.
        let hint = controls_hint(ui);
        let at = rect.left_bottom() + egui::vec2(8.0, -8.0 - hint.size().y);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(at, hint.size()).expand(4.0),
            4.0,
            theme::VIEWPORT_BACKGROUND.gamma_multiply(0.85),
        );
        ui.painter().galley(at, hint, theme::TEXT_MUTED);
        let action = self.navigation_bar(ui, rect, cam);
        ViewOutput {
            click,
            action,
            pointer,
        }
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
        frame: u64,
    ) -> egui::TextureId {
        let mut dirty = self.ensure_target(size);
        let device = &self.render_state.device;

        // Every buffer was built on a worker (MF-RT-001): nothing is
        // created or uploaded here. Layers on an object that isn't drawn
        // this frame are skipped.
        let drawn: Vec<&ViewLayer> = layers
            .iter()
            .filter(|l| objects.iter().any(|o| o.id == l.object))
            .collect();
        let key = FrameKey {
            camera,
            show_boundaries,
            size,
            // Which object is active changes the dimming.
            active: objects.iter().find(|o| o.active).map(|o| o.id),
            objects: objects
                .iter()
                .map(|o| {
                    (
                        o.id,
                        Arc::as_ptr(&o.render) as usize,
                        o.render.gpu.state_generation(),
                        o.shown_states,
                    )
                })
                .collect(),
            layers: drawn
                .iter()
                .map(|l| (l.object, Arc::as_ptr(&l.lines) as usize, l.color.to_array()))
                .collect(),
        };
        dirty |= self.last_frame.as_ref() != Some(&key);

        if dirty && let Some(target) = &self.target {
            let items: Vec<mf_render::DrawItem<'_>> = objects
                .iter()
                .map(|o| mf_render::DrawItem {
                    mesh: &o.render.gpu,
                    model: model_matrix(&o.transform),
                    inactive: !o.active,
                    shown_states: o.shown_states,
                })
                .collect();
            let gpu_layers: Vec<Layer<'_>> = drawn
                .iter()
                .filter_map(|l| {
                    let o = objects.iter().find(|o| o.id == l.object)?;
                    Some(Layer {
                        lines: &l.lines,
                        model: model_matrix(&o.transform),
                        mesh_origin: l.origin,
                        color: theme::rgba(l.color),
                    })
                })
                .collect();
            let timed = target.draw_timed(
                device,
                &self.render_state.queue,
                &mut self.renderer,
                Some(&camera),
                &items,
                show_boundaries,
                &gpu_layers,
                self.timer.as_mut().map(|t| (t, frame)),
            );
            self.redrawn = Some((frame, timed));
            self.last_frame = Some(key);
            self.frames_rendered += 1;
        }
        self.texture
            .expect("the texture is registered whenever a render target exists")
    }
}

impl Viewport {
    /// The device and queue the view draws with.
    pub fn device(&self) -> (&wgpu::Device, &wgpu::Queue) {
        (&self.render_state.device, &self.render_state.queue)
    }

    /// The render target for `size` (pixels), made or remade if needed;
    /// returns whether it was. Also made while there's nothing to draw, so
    /// the first frame that draws a model doesn't pay for it (MF-RT-001).
    fn ensure_target(&mut self, size: (u32, u32)) -> bool {
        if self
            .target
            .as_ref()
            .is_some_and(|t| (t.width, t.height) == size)
        {
            return false;
        }
        let device = &self.render_state.device;
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
        true
    }

    /// The target size (pixels) for a view of `rect`, clamped to the
    /// device limit; egui scales the image up if needed.
    fn target_size(&self, ui: &egui::Ui, rect: egui::Rect) -> (u32, u32) {
        let ppp = ui.ctx().pixels_per_point();
        let max = RenderTarget::max_dimension(&self.render_state.device) as f32;
        (
            (rect.width() * ppp).round().clamp(1.0, max) as u32,
            (rect.height() * ppp).round().clamp(1.0, max) as u32,
        )
    }

    /// Earlier frames read back since the last call, with their GPU time
    /// or `None` if it was unusable. Never waits for the GPU.
    pub fn collect_gpu_times(&mut self) -> Vec<(u64, Option<std::time::Duration>)> {
        match self.timer.as_mut() {
            Some(t) => t.collect(&self.render_state.device),
            None => Vec::new(),
        }
    }

    /// GPU samples that couldn't be used (see [`GpuTimer::failed`]);
    /// `None` without GPU timestamps.
    pub fn gpu_samples_failed(&self) -> Option<u64> {
        self.timer.as_ref().map(|t| t.failed)
    }

    /// What frame `frame` did on the GPU: drew the 3D view (its time
    /// follows via [`Viewport::collect_gpu_times`]) or not. A device
    /// without GPU timestamps says so.
    pub fn frame_gpu_time(&self, frame: u64) -> GpuTime {
        if self.timer.is_none() {
            return GpuTime::Unsupported;
        }
        match self.redrawn {
            Some((f, true)) if f == frame => GpuTime::Pending,
            // Redrawn, but every read-back was busy: no time to be had.
            Some((f, false)) if f == frame => GpuTime::Unusable,
            _ => GpuTime::NotRendered,
        }
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

/// Draws a single triangle once, offscreen, through every pipeline the view
/// uses (surface with face states, outlines, line overlays, dimmed objects), so the
/// first frame that draws a real model doesn't also wait for the driver to
/// get them ready: ~25 ms of GPU wait on Metal, measured (MF-RT-001). It
/// isn't waited for here either.
fn warm_up(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut MeshRenderer) {
    let mesh = mf_geometry::Mesh::new(
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 2]],
    )
    .expect("a triangle is a valid mesh");
    let vertices = mf_render::MeshVertices::build(&mesh);
    let gpu = mf_render::GpuMesh::from_vertices(device, &vertices);
    let lines = GpuLines::upload(device, &vertices.lines);
    let target = RenderTarget::new(device, 8, 8, MSAA_SAMPLES).expect("an 8×8 target fits");
    let camera = OrbitCamera::fit(&mesh.bounding_box().expect("a triangle has bounds"));
    let model = glam::DMat4::IDENTITY;
    let items = [false, true].map(|inactive| mf_render::DrawItem {
        mesh: &gpu,
        model,
        inactive,
        shown_states: u32::from(mf_render::FaceStates::ALL),
    });
    let layers = [Layer {
        lines: &lines,
        model,
        mesh_origin: vertices.origin,
        color: [1.0; 4],
    }];
    target.draw(
        device,
        queue,
        renderer,
        Some(&camera),
        &items,
        true,
        &layers,
    );
}

/// The controls hint at the view's bottom-left.
fn controls_hint(ui: &egui::Ui) -> Arc<egui::Galley> {
    ui.painter().layout_no_wrap(
        "Drag: orbit · Shift-drag: pan · Alt-drag: zoom · F: frame · Help > Controls".to_owned(),
        egui::FontId::proportional(12.0),
        theme::TEXT_MUTED,
    )
}

/// Shown while the view still draws an object's previous revision.
fn updating_text(ui: &egui::Ui) -> Arc<egui::Galley> {
    ui.painter().layout_no_wrap(
        "Updating view…".to_owned(),
        egui::FontId::proportional(14.0),
        theme::TEXT_MUTED,
    )
}

/// Give a symbol button an accessible name (and the name tests find it by).
fn labelled(response: egui::Response, label: &str) -> egui::Response {
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    response
}
