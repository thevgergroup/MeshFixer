//! 3D viewport. The scene renders into an `mf-render` [`RenderTarget`] that
//! egui shows as an image, so the renderer is independent of egui's own
//! render pass formats. It only re-renders when something changed.

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use mf_command::ObjectId;
use mf_render::{
    GpuLines, GpuMesh, LineVertex, MeshRenderer, MeshVertices, OrbitCamera, RenderTarget,
    model_matrix,
};

pub const MSAA_SAMPLES: u32 = 4;

/// Colour multiplier for an object that isn't the active one (MF-ISSUE-042).
const INACTIVE_TINT: f32 = 0.45;

#[derive(Clone, Copy, PartialEq)]
struct FrameKey {
    camera: OrbitCamera,
    show_boundaries: bool,
    size: (u32, u32),
    /// Which object is active (dimming depends on this).
    active: Option<ObjectId>,
    /// Identity of the highlight line set (0 = none).
    highlight: usize,
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
    pub highlight: Option<&'a Arc<Vec<LineVertex>>>,
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

pub struct Viewport {
    render_state: RenderState,
    renderer: MeshRenderer,
    target: Option<RenderTarget>,
    texture: Option<egui::TextureId>,
    /// GPU meshes uploaded for the objects drawn last frame, by object id.
    uploaded: Vec<(ObjectId, Arc<MeshVertices>, GpuMesh)>,
    highlight: Option<(Arc<Vec<LineVertex>>, GpuLines)>,
    last_frame: Option<FrameKey>,
    /// Where the 3D view was last drawn (for picking and screen projections).
    pub last_rect: Option<egui::Rect>,
    /// Frames actually rendered (for tests and the debug readout).
    pub frames_rendered: u64,
}

impl Viewport {
    pub fn new(render_state: RenderState) -> Viewport {
        let renderer = MeshRenderer::new(&render_state.device, RenderTarget::format(MSAA_SAMPLES));
        Viewport {
            render_state,
            renderer,
            target: None,
            texture: None,
            uploaded: Vec::new(),
            highlight: None,
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
    ) -> Option<Click> {
        let ViewOptions {
            show_boundaries,
            highlight,
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
            self.highlight = None;
            self.last_frame = None;
            self.last_rect = None;
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(38, 41, 46));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                placeholder,
                egui::FontId::proportional(16.0),
                egui::Color32::GRAY,
            );
            return None;
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
        handle_input(ui, &response, rect, cam);

        // Clamp to the device limit; egui scales the image up if needed.
        let ppp = ui.ctx().pixels_per_point();
        let max = RenderTarget::max_dimension(&self.render_state.device) as f32;
        let size = (
            (rect.width() * ppp).round().clamp(1.0, max) as u32,
            (rect.height() * ppp).round().clamp(1.0, max) as u32,
        );
        let texture = self.render(*cam, objects, show_boundaries, highlight, size);
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
                egui::Color32::from_rgb(255, 200, 80),
            );
        }
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag: orbit · shift/right-drag: pan · scroll: zoom · F: fit",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(140),
        );
        click
    }

    fn render(
        &mut self,
        camera: OrbitCamera,
        objects: &[ObjectView],
        show_boundaries: bool,
        highlight: Option<&Arc<Vec<LineVertex>>>,
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

        // Highlight lines upload separately from the mesh (MF-RENDER-003).
        match highlight {
            None => self.highlight = None,
            Some(lines) => {
                if !matches!(&self.highlight, Some((h, _)) if Arc::ptr_eq(h, lines)) {
                    self.highlight = Some((lines.clone(), GpuLines::upload(device, lines)));
                }
            }
        }
        let key = FrameKey {
            camera,
            show_boundaries,
            size,
            // Content changes (a new vertex set, an object added or
            // removed) already set `dirty` above; this only needs to catch
            // a change of *which* object is active (dimming changes).
            active: objects.iter().find(|o| o.active).map(|o| o.id),
            highlight: self
                .highlight
                .as_ref()
                .map_or(0, |(h, _)| Arc::as_ptr(h) as usize),
        };
        dirty |= self.last_frame != Some(key);

        if dirty && let Some(target) = &self.target {
            let items: Vec<mf_render::DrawItem<'_>> = objects
                .iter()
                .zip(&self.uploaded)
                .map(|(o, (_, _, gpu))| mf_render::DrawItem {
                    mesh: gpu,
                    model: model_matrix(&o.transform),
                    tint: if o.active { 1.0 } else { INACTIVE_TINT },
                })
                .collect();
            // The highlight only exists while there's a selection on the
            // active object, so there's always one to find here.
            let active_highlight = self.highlight.as_ref().and_then(|(_, lines)| {
                let active = objects.iter().find(|o| o.active)?;
                Some((
                    lines,
                    model_matrix(&active.transform),
                    active.vertices.origin,
                ))
            });
            target.draw(
                device,
                &self.render_state.queue,
                &mut self.renderer,
                Some(&camera),
                &items,
                show_boundaries,
                active_highlight,
            );
            self.last_frame = Some(key);
            self.frames_rendered += 1;
        }
        self.texture
            .expect("the texture is registered whenever a render target exists")
    }
}

fn handle_input(ui: &egui::Ui, response: &egui::Response, rect: egui::Rect, cam: &mut OrbitCamera) {
    let delta = response.drag_delta();
    let pan = response.dragged_by(egui::PointerButton::Secondary)
        || response.dragged_by(egui::PointerButton::Middle)
        || (response.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.modifiers.shift));
    if pan {
        let h = rect.height().max(1.0) as f64;
        cam.pan(delta.x as f64 / h, delta.y as f64 / h);
    } else if response.dragged_by(egui::PointerButton::Primary) {
        cam.orbit(delta.x as f64 * 0.01, delta.y as f64 * 0.01);
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            cam.zoom((-scroll as f64 * 0.002).exp());
        }
    }
}
