//! 3D viewport. The scene renders into an `mf-render` [`RenderTarget`] that
//! egui shows as an image, so the renderer is independent of egui's own
//! render pass formats. It only re-renders when something changed.

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use mf_command::ObjectId;
use mf_metrics::GpuTime;
use mf_render::{
    GpuLines, GpuTimer, Layer, MeshRenderer, OrbitCamera, Palette, RenderTarget, SelectionCue,
    model_matrix,
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
    /// generation (a selection change redraws), the states shown and
    /// whether it's dimmed.
    objects: Vec<(ObjectId, usize, u64, u32, bool, bool, bool)>,
    /// Identity, colour, x-ray and emphasis (bits) of each overlay layer,
    /// in draw order.
    layers: Vec<(ObjectId, usize, [u8; 4], bool, u32)>,
    /// The selection cue: emphasis (bits) and x-ray.
    cue: (u32, bool),
    /// Each object's transform (bits): a gizmo drag redraws.
    transforms: Vec<[u64; 12]>,
    /// The gizmo's handles (target pixels) and the highlighted one.
    gizmo: Option<(Vec<mf_render::HandleShape>, Option<mf_render::Handle>)>,
    plate: Option<mf_render::Plate>,
    /// "Show holes through solids" (View ▸ Overlays): a toggle redraws.
    show_holes_through_solids: bool,
    /// Identity of each intersection curve drawn (a new curve redraws).
    intersections: Vec<usize>,
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
    /// Also drawn faintly where nearer surfaces hide it (the selection).
    pub xray: bool,
    /// 0–1: drawn brighter and wider (the selection's pulse).
    pub emphasis: f32,
}

/// Where a Hole's surface crosses a Solid's (`mesh.intersections`): the
/// exact curve, already in world space (built by a worker), drawn in the
/// `INTERSECTION` token over everything with no depth test.
pub struct IntersectionView {
    pub lines: Arc<GpuLines>,
}

/// One object to draw this frame, at its own transform (MF-ISSUE-042).
pub struct ObjectView {
    pub id: ObjectId,
    /// Built on a worker, GPU buffers included (MF-RT-001).
    pub render: Arc<RenderMesh>,
    pub transform: [f64; 12],
    pub active: bool,
    /// Its row is hovered in the tree: drawn undimmed even if it isn't
    /// active, so its hover glow shows.
    pub hovered: bool,
    /// Which face-state flags tint it (`mf_render::FaceStates`).
    pub shown_states: u32,
    /// One of several objects selected together (Shift-click): drawn
    /// undimmed like the active one.
    pub picked: bool,
    /// Its role is Hole: drawn translucent with a grey hatch.
    pub hole: bool,
    /// Drawn see-through: the Split workflow's transparent piece.
    pub ghost: bool,
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
    /// How the selection is drawn besides its tint and outline.
    pub cue: SelectionCue,
    /// The hole overview's markers (window points), drawn over the view.
    pub markers: Option<Arc<egui::Mesh>>,
    /// The transform gizmo to draw over everything, and its highlighted
    /// handle (the one hovered or dragged).
    pub gizmo: Option<(mf_render::Gizmo, Option<mf_render::Handle>)>,
    /// The build plate to draw under the model.
    pub plate: Option<mf_render::Plate>,
    /// The open interactive tool's line at the view's top-left.
    pub tool_hint: Option<&'a str>,
    /// How the surface is shaded (the View menu's presets and wireframe).
    pub shading: mf_render::Shading,
    /// Split's plane (point, normal; world): the two sides are shaded
    /// apart and the cut line drawn on the model.
    pub split_preview: Option<(glam::DVec3, glam::DVec3)>,
    /// View ▸ Overlays ▸ "Show holes through solids" (on by default): a
    /// Hole's hidden part — buried inside or behind a solid — draws as a
    /// fainter ghost.
    pub show_holes_through_solids: bool,
    /// Every overlapping Solid/Hole pair's exact crossing curve.
    pub intersections: Vec<IntersectionView>,
}

/// What [`Viewport::input`] found, for [`Viewport::paint`].
pub struct ViewInput {
    rect: egui::Rect,
    /// Something can be drawn (a camera, and objects to draw).
    drawn: bool,
    /// The input part of this frame's output.
    pub out: ViewOutput,
}

/// The pointer, for the open interactive tool (points from the view's
/// top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolPointer {
    /// A drag the tool claimed started where the button went down, and
    /// the pointer is now at `at`.
    Start { from: (f64, f64), at: (f64, f64) },
    /// The tool's drag continues.
    Move((f64, f64)),
    /// The tool's drag ended (the button was released).
    End((f64, f64)),
    /// A plain click (no drag) where the tool claims the press: the
    /// tool's, not a pick (a sculpt dab, a handle clicked).
    Click((f64, f64)),
}

/// A box dragged in Select mode (or after B): `rect` in points from the
/// view's top-left, of a view of `size` points; `add` with Shift.
#[derive(Debug, Clone, Copy)]
pub struct BoxSelect {
    pub rect: egui::Rect,
    pub size: (f64, f64),
    pub add: bool,
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
    /// A drag draws a box that selects the visible holes in it.
    Select,
}

/// A camera move asked for from the navigation bar that needs the app
/// (it knows the selection and every object's bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    FrameSelection,
    ResetView,
    /// The Overlays popover (what each colour means).
    Overlays,
    /// The wireframe on or off.
    Wireframe,
}

/// What happened in the viewport this frame.
#[derive(Debug, Default)]
pub struct ViewOutput {
    pub click: Option<Click>,
    pub action: Option<NavAction>,
    /// Where the pointer rests over the 3D view (points from its top-left,
    /// and the view's size), with no button held: what hover picks under.
    pub pointer: Option<((f64, f64), (f64, f64))>,
    /// A box selection finished this frame.
    pub box_select: Option<BoxSelect>,
    /// The open interactive tool's drag, this frame.
    pub tool: Option<ToolPointer>,
    /// Where the pointer is over the view (points from its top-left),
    /// held button or not: what an interactive tool highlights under.
    pub tool_hover: Option<(f64, f64)>,
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
    /// Where the controls hint was last drawn (`None`: not drawn, the view
    /// is too small for it beside the navigation bar).
    pub last_hint_rect: Option<egui::Rect>,
    /// Frames actually rendered (for tests and the debug readout).
    pub frames_rendered: u64,
    /// B: the next drag box-selects, whatever the mode.
    pub box_once: bool,
    /// Where the box being dragged started (window points).
    box_from: Option<egui::Pos2>,
    /// The primary drag under way belongs to the open interactive tool.
    tool_drag: bool,
    /// Where the navigation bar was last drawn (it is dim until hovered).
    nav_rect: Option<egui::Rect>,
    /// The wireframe is on (the navigation bar's toggle shows it).
    wireframe_on: bool,
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
            last_hint_rect: None,
            frames_rendered: 0,
            box_once: false,
            box_from: None,
            tool_drag: false,
            nav_rect: None,
            wireframe_on: false,
        }
    }

    /// Take the viewport's space and handle its input this frame: the
    /// camera moves, and clicks, boxes and the open interactive tool's
    /// pointer are reported (`drawable`: there is something to draw). Then
    /// [`Self::paint`] draws it, so a tool's drag moves what is drawn in
    /// the same frame.
    pub fn input(
        &mut self,
        ui: &mut egui::Ui,
        camera: &mut Option<OrbitCamera>,
        drawable: bool,
        tool_grabs: Option<&dyn Fn((f64, f64)) -> bool>,
    ) -> ViewInput {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let (Some(cam), true) = (camera.as_mut(), drawable) else {
            // Nothing ready to draw yet (no camera, or no object's vertices
            // are in hand): release GPU buffers for a vanished document
            // (MF-ISSUE-009).
            self.last_frame = None;
            self.last_rect = None;
            self.tool_drag = false;
            return ViewInput {
                rect,
                drawn: false,
                out: ViewOutput::default(),
            };
        };
        self.last_rect = Some(rect);
        let local = |p: egui::Pos2| ((p.x - rect.left()) as f64, (p.y - rect.top()) as f64);
        // An open interactive tool gets the primary drag first (I8), where
        // it claims the press; Alt and Shift camera drags always win.
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let mut tool = None;
        if response.drag_started_by(egui::PointerButton::Primary)
            && !alt
            && !shift
            && let (Some(grabs), Some(origin)) =
                (tool_grabs, ui.input(|i| i.pointer.press_origin()))
            && grabs(local(origin))
        {
            self.tool_drag = true;
            let at = response.interact_pointer_pos().unwrap_or(origin);
            tool = Some(ToolPointer::Start {
                from: local(origin),
                at: local(at),
            });
        } else if self.tool_drag {
            let at = response
                .interact_pointer_pos()
                .or_else(|| ui.input(|i| i.pointer.latest_pos()));
            if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                self.tool_drag = false;
                tool = at.map(|p| ToolPointer::End(local(p)));
            } else if let Some(p) = at {
                tool = Some(ToolPointer::Move(local(p)));
            }
        }
        let tool_hover = response
            .hover_pos()
            .or_else(|| {
                self.tool_drag
                    .then(|| ui.input(|i| i.pointer.latest_pos()))
                    .flatten()
            })
            .map(local);
        let claimed = |p: egui::Pos2| tool_grabs.is_some_and(|grabs| grabs(local(p)));
        let clicked = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten();
        // A click the tool claims is the tool's, not a pick.
        if let Some(p) = clicked.filter(|p| claimed(*p))
            && tool.is_none()
        {
            tool = Some(ToolPointer::Click(local(p)));
        }
        let click = clicked.filter(|p| !claimed(*p)).map(|p| Click {
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
        let selecting = (self.box_once || self.nav_mode == NavMode::Select)
            && !self.tool_drag
            && tool.is_none();
        let mut box_select = None;
        if selecting && response.drag_started_by(egui::PointerButton::Primary) {
            self.box_from = ui.input(|i| i.pointer.press_origin());
        }
        let to = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.latest_pos()));
        if let (true, Some(from), Some(to)) = (selecting, self.box_from, to) {
            let r = egui::Rect::from_two_pos(from, to);
            if response.drag_stopped() {
                box_select = Some(BoxSelect {
                    rect: r.translate(-rect.left_top().to_vec2()),
                    size: (rect.width() as f64, rect.height() as f64),
                    add: ui.input(|i| i.modifiers.shift),
                });
                self.box_from = None;
                self.box_once = false;
            } else {
                ui.painter()
                    .rect_filled(r, 0.0, theme::SELECTED.gamma_multiply(0.12));
                ui.painter().rect_stroke(
                    r,
                    0.0,
                    egui::Stroke::new(1.0, theme::SELECTED),
                    egui::StrokeKind::Inside,
                );
            }
        }
        // While the tool drags, the camera holds still: no orbit, pan,
        // wheel or pinch (the drag maps the pointer through this camera).
        let owned =
            self.tool_drag || matches!(tool, Some(ToolPointer::Start { .. } | ToolPointer::End(_)));
        if !owned {
            handle_input(ui, &response, rect, cam, self.nav_mode, selecting);
        }
        ViewInput {
            rect,
            drawn: true,
            out: ViewOutput {
                click,
                action: None,
                pointer,
                box_select,
                tool,
                tool_hover,
            },
        }
    }

    /// Draw the view [`Self::input`] laid out: every object of `objects`
    /// at its own transform (MF-ISSUE-042), the active one full colour and
    /// the rest dimmed, then the overlays, the gizmo and the navigation
    /// bar. Returns what happened this frame.
    pub fn paint(
        &mut self,
        ui: &mut egui::Ui,
        input: ViewInput,
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
            cue,
            markers,
            gizmo,
            plate,
            tool_hint,
            shading,
            split_preview,
            show_holes_through_solids,
            intersections,
        } = options;
        let rect = input.rect;
        let (Some(cam), true) = (camera.as_mut(), input.drawn && !objects.is_empty()) else {
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
        let ViewOutput {
            click,
            pointer,
            box_select,
            tool,
            tool_hover,
            ..
        } = input.out;

        self.renderer
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        let size = self.target_size(ui, rect);
        self.renderer.set_selection_cue(cue);
        self.renderer.set_shading(shading);
        self.renderer.set_split_preview(split_preview);
        self.renderer
            .set_show_holes_through_solids(show_holes_through_solids);
        self.wireframe_on = shading.wireframe;
        let ppp = f64::from(ui.ctx().pixels_per_point());
        let view = (rect.width() as f64, rect.height() as f64);
        // The gizmo is laid out in points (its size is in points); the
        // renderer draws in target pixels.
        let gizmo = gizmo.map(|(g, highlight)| {
            let shapes: Vec<mf_render::HandleShape> = g
                .shapes(cam, view, highlight)
                .into_iter()
                .map(|s| scaled_shape(s, ppp))
                .collect();
            (shapes, highlight)
        });
        let texture = self.render(
            *cam,
            objects,
            show_boundaries,
            &layers,
            size,
            frame,
            gizmo.as_ref().map(|(s, h)| (s.as_slice(), *h)),
            plate,
            show_holes_through_solids,
            &intersections,
        );
        ui.painter().image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        let mut top = rect.left_top() + egui::vec2(8.0, 8.0);
        if updating {
            let text = updating_text(ui);
            let h = text.size().y;
            ui.painter().galley(top, text, theme::TEXT_MUTED);
            top.y += h + 4.0;
        }
        // The open interactive tool says so, and how to leave it (I8).
        if let Some(hint) = tool_hint {
            let text = ui.painter().layout_no_wrap(
                hint.to_owned(),
                egui::FontId::proportional(13.0),
                theme::TEXT,
            );
            let area = egui::Rect::from_min_size(top, text.size()).expand(4.0);
            ui.painter()
                .rect_filled(area, 4.0, theme::VIEWPORT_BACKGROUND.gamma_multiply(0.85));
            ui.painter().galley(top, text, theme::TEXT);
        }
        // The controls hint, on a backdrop so it reads over the model. Kept
        // short so it clears the navigation bar at the bottom-right; Help >
        // Controls lists everything.
        if let Some(m) = markers {
            ui.painter().add(egui::Shape::Mesh(m));
        }
        let (action, bar) = self.navigation_bar(ui, rect, cam);
        // The hint sits at the bottom-left; where the navigation bar would
        // cover it (a narrow view), it goes above the bar, and if it's
        // wider than the view it isn't drawn (Help > Controls has it all).
        let hint = controls_hint(ui);
        let mut at = rect.left_bottom() + egui::vec2(8.0, -8.0 - hint.size().y);
        let mut area = egui::Rect::from_min_size(at, hint.size()).expand(4.0);
        if area.intersects(bar) {
            at.y = bar.top() - 8.0 - hint.size().y;
            area = egui::Rect::from_min_size(at, hint.size()).expand(4.0);
        }
        self.last_hint_rect = None;
        if rect.contains_rect(area) && !area.intersects(bar) {
            ui.painter()
                .rect_filled(area, 4.0, theme::VIEWPORT_BACKGROUND.gamma_multiply(0.85));
            ui.painter().galley(at, hint, theme::TEXT_MUTED);
            self.last_hint_rect = Some(area);
        }
        ViewOutput {
            click,
            action,
            pointer,
            box_select,
            tool,
            tool_hover,
        }
    }

    /// The navigation bar at the view's bottom centre (ui-plan.md §3), for
    /// people without a mouse wheel: drag mode (Orbit / Pan / Zoom / Box
    /// select), zoom out / in (hold to repeat), frame selection, reset
    /// view and the Overlays popover, as icons with accessible names and
    /// tooltips. Dim until the pointer is over it. Camera maths only.
    /// Returns the action and where the bar is.
    fn navigation_bar(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        cam: &mut OrbitCamera,
    ) -> (Option<NavAction>, egui::Rect) {
        use crate::{icons, t, widgets};
        let mut action = None;
        let mut zoom_steps = 0i32;
        let mut held = None;
        let hovered = self.nav_rect.is_some_and(|r| {
            ui.ctx()
                .pointer_hover_pos()
                .is_some_and(|p| r.expand(6.0).contains(p))
        });
        let bar = egui::Area::new(egui::Id::unique("navigation bar"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(rect.center_bottom() - egui::vec2(0.0, theme::SPACE_M))
            .constrain_to(rect)
            .show(ui.ctx(), |ui| {
                if !hovered {
                    ui.multiply_opacity(0.6);
                }
                egui::Frame::new()
                    .fill(theme::CARD.gamma_multiply(0.92))
                    .stroke(egui::Stroke::new(1.0, theme::SEPARATOR))
                    .corner_radius(theme::RADIUS + 2.0)
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            const SIZE: f32 = 28.0;
                            for (mode, icon, label, tip) in [
                                (
                                    NavMode::Orbit,
                                    icons::ORBIT,
                                    t!("nav.orbit"),
                                    t!("nav.orbit.tip"),
                                ),
                                (NavMode::Pan, icons::PAN, t!("nav.pan"), t!("nav.pan.tip")),
                                (
                                    NavMode::Zoom,
                                    icons::ZOOM,
                                    t!("nav.zoom"),
                                    t!("nav.zoom.tip"),
                                ),
                                (
                                    NavMode::Select,
                                    icons::BOX_SELECT,
                                    t!("nav.box"),
                                    t!("nav.box.tip"),
                                ),
                            ] {
                                let on = self.nav_mode == mode;
                                if widgets::icon_button(ui, icon, &label, Some(on), SIZE)
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    self.nav_mode = mode;
                                }
                            }
                            divider(ui);
                            for (zoom_in, icon, label, tip) in [
                                (
                                    false,
                                    icons::ZOOM_OUT,
                                    t!("nav.zoom_out"),
                                    t!("nav.zoom_out.tip"),
                                ),
                                (
                                    true,
                                    icons::ZOOM_IN,
                                    t!("nav.zoom_in"),
                                    t!("nav.zoom_in.tip"),
                                ),
                            ] {
                                let r = widgets::icon_button(ui, icon, &label, None, SIZE)
                                    .on_hover_text(tip);
                                if r.clicked() {
                                    zoom_steps += if zoom_in { 1 } else { -1 };
                                }
                                if r.is_pointer_button_down_on() {
                                    held = Some(zoom_in);
                                }
                            }
                            divider(ui);
                            if widgets::icon_button(ui, icons::FRAME, &t!("cmd.frame"), None, SIZE)
                                .on_hover_text(t!("nav.frame.tip"))
                                .clicked()
                            {
                                action = Some(NavAction::FrameSelection);
                            }
                            if widgets::icon_button(
                                ui,
                                icons::HOME,
                                &t!("cmd.reset_view"),
                                None,
                                SIZE,
                            )
                            .on_hover_text(t!("nav.reset.tip"))
                            .clicked()
                            {
                                action = Some(NavAction::ResetView);
                            }
                            divider(ui);
                            if widgets::icon_button(
                                ui,
                                icons::WIREFRAME,
                                &t!("nav.wireframe"),
                                Some(self.wireframe_on),
                                SIZE,
                            )
                            .on_hover_text(t!("nav.wireframe.tip"))
                            .clicked()
                            {
                                action = Some(NavAction::Wireframe);
                            }
                            if widgets::icon_button(
                                ui,
                                icons::OVERLAYS,
                                &t!("nav.overlays"),
                                None,
                                SIZE,
                            )
                            .on_hover_text(t!("nav.overlays.tip"))
                            .clicked()
                            {
                                action = Some(NavAction::Overlays);
                            }
                        });
                    });
            })
            .response
            .rect;
        self.nav_rect = Some(bar);
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
        (action, bar)
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        camera: OrbitCamera,
        objects: &[ObjectView],
        show_boundaries: bool,
        layers: &[ViewLayer],
        size: (u32, u32),
        frame: u64,
        gizmo: Option<(&[mf_render::HandleShape], Option<mf_render::Handle>)>,
        plate: Option<mf_render::Plate>,
        show_holes_through_solids: bool,
        intersections: &[IntersectionView],
    ) -> egui::TextureId {
        let mut dirty = self.ensure_target(size);
        let cue = self.renderer.selection_cue();
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
                        !(o.active || o.hovered || o.picked),
                        o.hole,
                        o.ghost,
                    )
                })
                .collect(),
            layers: drawn
                .iter()
                .map(|l| {
                    (
                        l.object,
                        Arc::as_ptr(&l.lines) as usize,
                        l.color.to_array(),
                        l.xray,
                        l.emphasis.to_bits(),
                    )
                })
                .collect(),
            cue: (cue.emphasis.to_bits(), cue.xray_faces),
            transforms: objects
                .iter()
                .map(|o| o.transform.map(f64::to_bits))
                .collect(),
            gizmo: gizmo.map(|(shapes, h)| (shapes.to_vec(), h)),
            plate,
            show_holes_through_solids,
            intersections: intersections
                .iter()
                .map(|i| Arc::as_ptr(&i.lines) as usize)
                .collect(),
        };
        dirty |= self.last_frame.as_ref() != Some(&key);

        if dirty && let Some(target) = &self.target {
            let items: Vec<mf_render::DrawItem<'_>> = objects
                .iter()
                .map(|o| mf_render::DrawItem {
                    hole: o.hole,
                    ghost: o.ghost,
                    mesh: &o.render.gpu,
                    model: model_matrix(&o.transform),
                    inactive: !(o.active || o.hovered || o.picked),
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
                        xray: l.xray,
                        emphasis: l.emphasis,
                    })
                })
                .collect();
            // Already in world space (built on a worker): an identity
            // model and origin, like the build plate.
            let gpu_intersections: Vec<Layer<'_>> = intersections
                .iter()
                .map(|i| Layer {
                    lines: &i.lines,
                    model: glam::DMat4::IDENTITY,
                    mesh_origin: glam::DVec3::ZERO,
                    color: theme::rgba(theme::INTERSECTION),
                    xray: false,
                    emphasis: 0.0,
                })
                .collect();
            let palette = theme::gizmo_palette();
            let extras = mf_render::SceneExtras {
                plate: plate.as_ref(),
                gizmo: gizmo.map(|(shapes, highlight)| mf_render::GizmoDraw {
                    shapes,
                    highlight,
                    palette: &palette,
                }),
            };
            let timed = target.draw_with(
                device,
                &self.render_state.queue,
                &mut self.renderer,
                Some(&camera),
                &items,
                show_boundaries,
                &gpu_layers,
                &gpu_intersections,
                &extras,
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
    /// Bytes of the view's render target textures (0 before the first
    /// frame).
    pub fn texture_bytes(&self) -> u64 {
        self.target.as_ref().map_or(0, RenderTarget::texture_bytes)
    }

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
    // A box selection owns the primary drag.
    selecting: bool,
) {
    let h = rect.height().max(1.0) as f64;
    let size = (rect.width() as f64, rect.height() as f64);
    let delta = response.drag_delta();
    let (alt, shift, origin) =
        ui.input(|i| (i.modifiers.alt, i.modifiers.shift, i.pointer.press_origin()));
    // A box selection owns the primary drag.
    let primary = response.dragged_by(egui::PointerButton::Primary) && !selecting;
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
        Some(NavMode::Zoom | NavMode::Select) | None => {}
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
        hole: false,
        ghost: false,
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
        // The x-ray pipelines too.
        xray: true,
        emphasis: 0.0,
    }];
    renderer.set_selection_cue(SelectionCue {
        emphasis: 0.0,
        xray_faces: true,
    });
    target.draw(
        device,
        queue,
        renderer,
        Some(&camera),
        &items,
        true,
        &layers,
    );
    renderer.set_selection_cue(SelectionCue::default());
}

/// A thin vertical rule between groups of the navigation bar.
fn divider(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(9.0, 20.0), egui::Sense::hover());
    ui.painter().line_segment(
        [r.center_top(), r.center_bottom()],
        egui::Stroke::new(1.0, theme::SEPARATOR),
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

/// `shape` laid out in points, in target pixels (`ppp` pixels per point).
fn scaled_shape(shape: mf_render::HandleShape, ppp: f64) -> mf_render::HandleShape {
    use mf_render::HandleGeometry as G;
    let p = |(x, y): (f64, f64)| (x * ppp, y * ppp);
    let geometry = match shape.geometry {
        G::Arrow {
            shaft_start,
            shaft_end,
            tip,
            head,
        } => G::Arrow {
            shaft_start: p(shaft_start),
            shaft_end: p(shaft_end),
            tip: p(tip),
            head: head.into_iter().map(p).collect(),
        },
        G::Ring { points, front } => G::Ring {
            points: points.into_iter().map(p).collect(),
            front,
        },
        G::RingGrip { arc, heads } => G::RingGrip {
            arc: arc.into_iter().map(p).collect(),
            heads: heads.map(|h| h.map(p)),
        },
        G::Circle { center, radius_px } => G::Circle {
            center: p(center),
            radius_px: radius_px * ppp,
        },
        G::Plane { corners } => G::Plane {
            corners: corners.map(p),
        },
        G::Point { center, radius_px } => G::Point {
            center: p(center),
            radius_px: radius_px * ppp,
        },
    };
    mf_render::HandleShape {
        handle: shape.handle,
        geometry,
    }
}
