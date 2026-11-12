use bytemuck::{Pod, Zeroable};
use glam::{DMat3, DMat4, DVec3, Mat4, Vec4};
use wgpu::util::DeviceExt;

use crate::camera::OrbitCamera;
use crate::gpu_mesh::{
    FaceStates, GpuLines, GpuMesh, LineVertex, SurfaceVertex, face_state_layout,
};
use crate::overlay::GIZMO_LINE_WIDTH_PT;

/// The colours the 3D view draws with. `mf-render` has no theme of its own
/// in the app: the desktop passes its colour tokens in (theme.rs), so one
/// colour means one thing everywhere. Components are linear 0–1 RGBA as
/// written to the (non-sRGB) render target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The stage's background at the bottom of the view…
    pub background: [f32; 4],
    /// …and at the top: a soft vertical gradient between the two (the
    /// same colour twice for a flat background).
    pub background_top: [f32; 4],
    /// Front faces of the model.
    pub surface: [f32; 4],
    /// Back faces (seen through holes, or flipped).
    pub back_face: [f32; 4],
    /// Every traced boundary loop's outline (holes).
    pub boundary: [f32; 4],
    /// The build plate's grid (D9 `PLATE`): a neutral, so it never
    /// competes with a defect or selection colour.
    pub plate: [f32; 4],
    /// The plate's major lines (every [`crate::Plate::major_mm`]): the same
    /// neutral, a step brighter.
    pub plate_major: [f32; 4],
    /// Saturation kept by an inactive object (0 = grey, 1 = unchanged).
    pub inactive_saturation: f32,
    /// Brightness multiplier of an inactive object.
    pub inactive_brightness: f32,
    /// Width of boundary outlines, in points.
    pub boundary_width: f32,
    /// Width of overlay lines (selection, cracks, tangled edges, hover), in points.
    pub overlay_width: f32,
    /// What each face-state flag tints a face with ([`FaceStates`]).
    pub tints: FaceTints,
    /// Opacity (0–1) of the selection drawn through nearer surfaces (the
    /// x-ray pass, in the selection's own colour; see [`SelectionCue`]).
    pub xray_opacity: f32,
    /// Opacity (0–1) of a Hole's hidden part — buried inside or behind a
    /// solid — drawn as a ghost (Tinkercad-style "show holes through
    /// solids"; about 20%, well under the ~35% of its visible part).
    pub hole_xray_opacity: f32,
}

/// How the model's surface is shaded (UI plan §4 "Shaders").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadingPreset {
    /// One matte neutral under soft studio light: best for reading shape
    /// and defects. The default.
    #[default]
    Clay,
    /// Brighter, with a soft highlight: for presenting a part.
    Studio,
    /// See-through: every surface at once, edges brighter, for what's
    /// inside or behind.
    XRay,
}

/// The surface's shading this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shading {
    pub preset: ShadingPreset,
    /// Draw every triangle's edges over the surface.
    pub wireframe: bool,
}

/// How the selection is drawn this frame, besides its face tint and
/// outline layers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SelectionCue {
    /// 0–1: how much brighter (and, for outlines, wider) the selection is
    /// drawn: the pulse after a camera move frames it.
    pub emphasis: f32,
    /// Draw selected faces of every active item where nearer surfaces hide
    /// them, at [`Palette::xray_opacity`] (an x-ray pass: no depth write,
    /// only where something is in front). Outline layers have their own
    /// [`Layer::xray`].
    pub xray_faces: bool,
}

/// The tint of each face-state flag: RGB, and in alpha how much of it
/// replaces the surface colour (0–1). A selected face shows only the
/// selection tint (even when hovered), a hovered one only the hover tint;
/// otherwise later tints in this list draw over earlier ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceTints {
    pub loose_piece: [f32; 4],
    /// Faces around a hole (the hole overview): drawn under every other tint.
    pub hole_neighbourhood: [f32; 4],
    pub non_manifold_adj: [f32; 4],
    pub recent_repair: [f32; 4],
    pub selected: [f32; 4],
    pub hover: [f32; 4],
}

impl Palette {
    /// Headless renders (the CLI's `render`, offscreen tests) have no app
    /// theme; these are the renderer's own neutral colours for them.
    pub const HEADLESS: Palette = Palette {
        background: [38.0 / 255.0, 41.0 / 255.0, 46.0 / 255.0, 1.0],
        background_top: [38.0 / 255.0, 41.0 / 255.0, 46.0 / 255.0, 1.0],
        surface: [0.70, 0.72, 0.76, 1.0],
        back_face: [0.62, 0.30, 0.30, 1.0],
        boundary: [1.0, 0.55, 0.05, 1.0],
        plate: [100.0 / 255.0, 100.0 / 255.0, 100.0 / 255.0, 1.0],
        plate_major: [140.0 / 255.0, 140.0 / 255.0, 140.0 / 255.0, 1.0],
        inactive_saturation: 0.0,
        inactive_brightness: 0.55,
        boundary_width: 1.0,
        overlay_width: 1.0,
        tints: FaceTints {
            loose_piece: [1.0, 0.78, 0.16, 0.6],
            hole_neighbourhood: [1.0, 0.5, 0.13, 0.25],
            non_manifold_adj: [0.92, 0.2, 0.2, 0.35],
            recent_repair: [0.31, 0.8, 0.47, 0.45],
            selected: [0.16, 0.82, 1.0, 0.6],
            hover: [0.91, 0.93, 0.96, 0.85],
        },
        xray_opacity: 0.55,
        hole_xray_opacity: 0.2,
    };
}

/// How much further overlay lines are pulled towards the camera than a
/// mesh's own outlines (clip-space depth, times w).
const OVERLAY_PULL: f32 = 0.0002;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniforms {
    view_proj: [[f32; 4]; 4],
    eye_dir: [f32; 4],
    surface: [f32; 4],
    back_face: [f32; 4],
    boundary: [f32; 4],
    /// x: inactive saturation, y: inactive brightness.
    inactive: [f32; 4],
    /// Render target size in pixels (x, y), for screen-space line widths.
    viewport: [f32; 4],
    /// [`FaceTints`], in its order.
    tints: [[f32; 4]; 6],
    /// x: the selection's emphasis ([`SelectionCue`]), y: x-ray opacity,
    /// z: a Hole's hidden-part ghost opacity (0 while the "Show holes
    /// through solids" overlay is off, so the pass draws nothing visible).
    cue: [f32; 4],
    /// The camera's eye, in the same shifted (`world_origin`-relative)
    /// frame as vertex positions: the build plate fades by distance from
    /// here (xyz; w unused padding).
    eye_pos: [f32; 4],
    /// The build plate's grid colour (D9 `PLATE`).
    plate: [f32; 4],
    /// The background gradient: bottom, top.
    background: [f32; 4],
    background_top: [f32; 4],
    /// x: the shading preset (0 clay, 1 studio, 2 x-ray), y: 1 for the
    /// wireframe.
    shading: [f32; 4],
    /// Split's preview plane in the shifted frame: normal (xyz) and
    /// offset (w); `shading.z` is 1 while it shows.
    split_plane: [f32; 4],
    /// The studio lights (towards the light, world): key, fill, and the
    /// key's half vector with the view.
    light_key: [f32; 4],
    light_fill: [f32; 4],
    light_half: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawUniforms {
    model: [[f32; 4]; 4],
    /// Inverse-transpose of `model`'s linear (3x3) part, for normals: the
    /// model matrix itself is only correct for normals under a rotation or
    /// uniform scale, not a non-uniform scale or shear (both legal in a
    /// 3MF transform). The translation column is unused padding.
    normal_matrix: [[f32; 4]; 4],
    /// A layer's colour (unused for an object's own surface and outlines).
    color: [f32; 4],
    /// x: 1 for an inactive object (desaturated). y: line width in pixels.
    /// z: unused for an object (a layer's depth pull). w: physical pixels
    /// per point (a Hole's hatch spacing).
    params: [f32; 4],
    /// x: the face-state flags shown ([`DrawItem::shown_states`]).
    states: [u32; 4],
}

/// The render target the pipelines are built for. It must match the render
/// pass they're used in.
#[derive(Debug, Clone, Copy)]
pub struct RenderTargetFormat {
    pub color: wgpu::TextureFormat,
    pub depth: wgpu::TextureFormat,
    pub samples: u32,
}

/// One mesh to draw this frame, placed by its object-to-world transform
/// (already in column-vector form; see [`crate::model_matrix`]).
pub struct DrawItem<'a> {
    pub mesh: &'a GpuMesh,
    pub model: DMat4,
    /// An inactive object is drawn desaturated (see [`Palette`]).
    pub inactive: bool,
    /// Which face-state flags ([`FaceStates`]) tint it: the overlays that
    /// are switched on. [`FaceStates::ALL`] shows every one.
    pub shown_states: u32,
    /// A Hole (its role in a combine): drawn about 35% opaque with a grey
    /// diagonal hatch (a pattern, not a colour), after every solid so
    /// they show through it.
    pub hole: bool,
    /// Drawn see-through (about 25% opaque, lit, no hatch) after every
    /// solid, so what's behind or inside it shows: the Split workflow's
    /// transparent piece, with the other piece's cut face solid behind it.
    pub ghost: bool,
    /// View ▸ Colour objects (UI plan §11): this object's own shade from
    /// the palette, replacing [`Palette::surface`] for its front faces.
    /// `None` draws the one neutral surface colour (Clay, or a Hole,
    /// which always keeps its hatch regardless of this field). The
    /// frontend leaves this `None` where the mesh carries its own 3MF
    /// material or colour, so that always overrides the palette.
    pub object_color: Option<[f32; 4]>,
}

/// One line overlay on an object (selected loops, cracks, tangled edges,
/// hover), drawn after every object's own surface, in the order given (a
/// later layer draws over an earlier one). Face tints aren't layers: they
/// are the mesh's face states ([`FaceStates`]).
pub struct Layer<'a> {
    pub lines: &'a GpuLines,
    /// The object's object-to-world transform.
    pub model: DMat4,
    /// The local origin the layer's vertices are relative to.
    pub mesh_origin: DVec3,
    pub color: [f32; 4],
    /// Also drawn, at [`Palette::xray_opacity`], where nearer surfaces hide
    /// it (the selection's outline: one colour, one meaning).
    pub xray: bool,
    /// 0–1: drawn brighter and wider (the selection's pulse).
    pub emphasis: f32,
}

/// A per-draw uniform buffer and its bind group, reused across frames.
struct DrawSlot {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub struct MeshRenderer {
    surface_pipeline_ccw: wgpu::RenderPipeline,
    surface_pipeline_cw: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    overlay_line_pipeline: wgpu::RenderPipeline,
    /// The selection drawn through nearer surfaces (see [`SelectionCue`]).
    xray_surface_pipeline_ccw: wgpu::RenderPipeline,
    xray_surface_pipeline_cw: wgpu::RenderPipeline,
    xray_line_pipeline: wgpu::RenderPipeline,
    /// Where a Hole crosses a Solid (`INTERSECTION`): no depth test at
    /// all, over everything, like the gizmo.
    intersection_line_pipeline: wgpu::RenderPipeline,
    /// The build plate (D9): blended, depth-tested like an opaque pass.
    plate_pipeline: wgpu::RenderPipeline,
    /// The transform gizmo (D8): screen-space, no depth test.
    gizmo_line_pipeline: wgpu::RenderPipeline,
    gizmo_fill_pipeline: wgpu::RenderPipeline,
    palette: Palette,
    cue: SelectionCue,
    /// Physical pixels per point (line widths are in points).
    pixels_per_point: f32,
    draw_bgl: wgpu::BindGroupLayout,
    scene_uniforms: wgpu::Buffer,
    scene_bind_group: wgpu::BindGroup,
    /// Grown on demand to cover the most objects drawn in one frame.
    draw_slots: Vec<DrawSlot>,
    /// The plate's own draw uniforms (model = identity, its line width):
    /// one reusable slot, since only one plate is ever drawn.
    /// The plate's minor and major lines' uniforms (two draws, one pass).
    plate_slots: [DrawSlot; 2],
    glass_pipeline_ccw: wgpu::RenderPipeline,
    glass_pipeline_cw: wgpu::RenderPipeline,
    /// A Hole object: translucent with a grey diagonal hatch.
    hole_pipeline_ccw: wgpu::RenderPipeline,
    hole_pipeline_cw: wgpu::RenderPipeline,
    /// A Hole's hidden part — buried inside or behind a solid — drawn as a
    /// fainter ghost (`DepthMode::HoleXRay`: depth test `Greater`, no
    /// depth write), the same hatched surface as `hole_pipeline_*` at
    /// [`Palette::hole_xray_opacity`] (Tinkercad-style "show holes through
    /// solids").
    hole_xray_pipeline_ccw: wgpu::RenderPipeline,
    hole_xray_pipeline_cw: wgpu::RenderPipeline,
    /// A see-through object ([`DrawItem::ghost`]).
    ghost_pipeline_ccw: wgpu::RenderPipeline,
    ghost_pipeline_cw: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    shading: Shading,
    split_preview: Option<(DVec3, DVec3)>,
    /// View ▸ Overlays ▸ "Show holes through solids" (on by default).
    show_holes_through_solids: bool,
}

impl MeshRenderer {
    pub fn new(
        device: &wgpu::Device,
        target: RenderTargetFormat,
        palette: Palette,
    ) -> MeshRenderer {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let scene_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mf scene uniforms"),
            size: std::mem::size_of::<SceneUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mf scene uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mf scene uniforms"),
            layout: &scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_uniforms.as_entire_binding(),
            }],
        });
        let draw_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mf draw uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&draw_bgl)],
            immediate_size: 0,
        });
        // An object's own surface also reads its face states (group 2).
        let states_bgl = face_state_layout(device);
        let surface_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf surface pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&draw_bgl), Some(&states_bgl)],
            immediate_size: 0,
        });

        // Opaque passes write and test depth as usual; the x-ray passes
        // draw only behind what's already there (`Greater`), blended,
        // writing no depth and keeping the target opaque; the plate blends
        // (it fades with distance) but tests and writes depth like an
        // opaque pass, so the model drawn afterwards still hides it; the
        // gizmo draws last with no depth test at all (D8).
        #[derive(Clone, Copy, PartialEq)]
        enum DepthMode {
            Opaque,
            XRay,
            Blended,
            /// No depth test at all (the gizmo, D8): the pipeline still
            /// declares a depth-stencil state matching the pass's
            /// attachment (required for pipeline/pass compatibility), but
            /// always passes and never writes.
            NoTest,
            /// The x-ray preset's surface: blended, every surface drawn
            /// (no depth test), writing no depth.
            Glass,
            /// A Hole's surface: blended over what's already drawn,
            /// depth-tested so a nearer solid hides it, writing no depth.
            Translucent,
            /// A Hole's hidden part: blended, drawn only where something
            /// is already nearer (`Greater`, like the selection's x-ray
            /// pass), writing no depth — the ghost that shows a buried or
            /// backside hole through a solid.
            HoleXRay,
            /// A see-through object: as `Translucent`, pushed slightly
            /// back so where it lies on a solid's face (a cut's two caps)
            /// the solid face wins cleanly instead of shimmering.
            Ghost,
        }
        let blend_state = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline_with = |label: &str,
                             layout: &wgpu::PipelineLayout,
                             vs: &str,
                             fs: &str,
                             buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                             front_face: wgpu::FrontFace,
                             depth_mode: DepthMode| {
            let depth_stencil = Some(wgpu::DepthStencilState {
                format: target.depth,
                depth_write_enabled: Some(
                    depth_mode == DepthMode::Opaque || depth_mode == DepthMode::Blended,
                ),
                depth_compare: Some(match depth_mode {
                    DepthMode::XRay | DepthMode::HoleXRay => wgpu::CompareFunction::Greater,
                    DepthMode::NoTest | DepthMode::Glass => wgpu::CompareFunction::Always,
                    DepthMode::Opaque
                    | DepthMode::Blended
                    | DepthMode::Translucent
                    | DepthMode::Ghost => wgpu::CompareFunction::LessEqual,
                }),
                stencil: Default::default(),
                bias: if depth_mode == DepthMode::Ghost {
                    // Depth runs 0 near to 1 far: a positive bias pushes
                    // it away from the eye.
                    wgpu::DepthBiasState {
                        constant: 4,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    }
                } else {
                    Default::default()
                },
            });
            let blend = matches!(
                depth_mode,
                DepthMode::XRay
                    | DepthMode::Blended
                    | DepthMode::Glass
                    | DepthMode::Translucent
                    | DepthMode::HoleXRay
                    | DepthMode::Ghost
            )
            .then_some(blend_state);
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState {
                    count: target.samples.max(1),
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target.color,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        vs: &str,
                        fs: &str,
                        buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                        front_face: wgpu::FrontFace| {
            pipeline_with(
                label,
                layout,
                vs,
                fs,
                buffers,
                front_face,
                DepthMode::Opaque,
            )
        };
        let surface_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SurfaceVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        })];
        // Two variants, differing only in which winding counts as the front
        // face: a draw whose model matrix has a negative determinant
        // (mirrored) uses `Cw` so `front_facing` (and its shading) stays
        // correct even though the triangles are now wound the other way.
        let surface_pipeline_ccw = pipeline(
            "mf surface ccw",
            &surface_layout,
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::FrontFace::Ccw,
        );
        let surface_pipeline_cw = pipeline(
            "mf surface cw",
            &surface_layout,
            "vs_surface",
            "fs_surface",
            &surface_buffers,
            wgpu::FrontFace::Cw,
        );
        // Lines are drawn as screen-space quads so they can be wider than a
        // pixel: each segment (a pair of consecutive line-list vertices) is
        // one instance of six vertices.
        let line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: 2 * std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
        })];
        let line_pipeline = pipeline(
            "mf boundary lines",
            &layout,
            "vs_line",
            "fs_line",
            &line_buffers,
            wgpu::FrontFace::Ccw,
        );
        let overlay_line_pipeline = pipeline(
            "mf overlay lines",
            &layout,
            "vs_line",
            "fs_overlay_line",
            &line_buffers,
            wgpu::FrontFace::Ccw,
        );
        let xray_surface = |label: &str, front_face| {
            pipeline_with(
                label,
                &surface_layout,
                "vs_surface",
                "fs_xray_surface",
                &surface_buffers,
                front_face,
                DepthMode::XRay,
            )
        };
        let glass = |label: &str, front_face| {
            pipeline_with(
                label,
                &surface_layout,
                "vs_surface",
                "fs_glass_surface",
                &surface_buffers,
                front_face,
                DepthMode::Glass,
            )
        };
        let glass_pipeline_ccw = glass("mf x-ray preset ccw", wgpu::FrontFace::Ccw);
        let glass_pipeline_cw = glass("mf x-ray preset cw", wgpu::FrontFace::Cw);
        let hole = |label: &str, front_face| {
            pipeline_with(
                label,
                &surface_layout,
                "vs_surface",
                "fs_hole_surface",
                &surface_buffers,
                front_face,
                DepthMode::Translucent,
            )
        };
        let hole_pipeline_ccw = hole("mf hole ccw", wgpu::FrontFace::Ccw);
        let hole_pipeline_cw = hole("mf hole cw", wgpu::FrontFace::Cw);
        let hole_xray = |label: &str, front_face| {
            pipeline_with(
                label,
                &surface_layout,
                "vs_surface",
                "fs_hole_xray_surface",
                &surface_buffers,
                front_face,
                DepthMode::HoleXRay,
            )
        };
        let hole_xray_pipeline_ccw = hole_xray("mf hole x-ray ccw", wgpu::FrontFace::Ccw);
        let hole_xray_pipeline_cw = hole_xray("mf hole x-ray cw", wgpu::FrontFace::Cw);
        let ghost = |label: &str, front_face| {
            pipeline_with(
                label,
                &surface_layout,
                "vs_surface",
                "fs_ghost_surface",
                &surface_buffers,
                front_face,
                DepthMode::Ghost,
            )
        };
        let ghost_pipeline_ccw = ghost("mf ghost ccw", wgpu::FrontFace::Ccw);
        let ghost_pipeline_cw = ghost("mf ghost cw", wgpu::FrontFace::Cw);
        // The stage's background gradient: one full-screen triangle, drawn
        // first, testing and writing no depth.
        let background_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf background pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl)],
            immediate_size: 0,
        });
        let background_pipeline = pipeline_with(
            "mf background",
            &background_layout,
            "vs_background",
            "fs_background",
            &[],
            wgpu::FrontFace::Ccw,
            DepthMode::NoTest,
        );
        let xray_surface_pipeline_ccw = xray_surface("mf x-ray surface ccw", wgpu::FrontFace::Ccw);
        let xray_surface_pipeline_cw = xray_surface("mf x-ray surface cw", wgpu::FrontFace::Cw);
        let xray_line_pipeline = pipeline_with(
            "mf x-ray lines",
            &layout,
            "vs_line",
            "fs_xray_line",
            &line_buffers,
            wgpu::FrontFace::Ccw,
            DepthMode::XRay,
        );
        // Where a Hole's surface crosses a Solid's (the exact curve from
        // `mesh.intersections`): drawn over everything, no depth test at
        // all (like the gizmo), with the same halo as every other overlay
        // line (`fs_overlay_line`), in `draw.color` (the app's own
        // `INTERSECTION` token).
        let intersection_line_pipeline = pipeline_with(
            "mf intersection lines",
            &layout,
            "vs_line",
            "fs_overlay_line",
            &line_buffers,
            wgpu::FrontFace::Ccw,
            DepthMode::NoTest,
        );
        // The build plate: world-space lines like `line_buffers`, but
        // blended (it fades with distance) while still depth-testing and
        // -writing like an opaque pass, so the model hides it (D9).
        let plate_pipeline = pipeline_with(
            "mf plate",
            &layout,
            "vs_plate",
            "fs_plate",
            &line_buffers,
            wgpu::FrontFace::Ccw,
            DepthMode::Blended,
        );
        // The gizmo: screen-space pixel geometry, like the overlay lines,
        // but with per-vertex colour (and, for the line half, width)
        // baked on the CPU (see overlay.rs) and no depth test at all, and
        // no scene-colour/face-state bind groups beyond the scene
        // uniforms (for the viewport size). Two pipelines: `gizmo_line_*`
        // for shafts/ring polylines (instanced quads, like the model's
        // own overlay lines), `gizmo_fill_*` for the filled shapes
        // (arrowheads, scale cubes, plane squares, point discs: a plain
        // triangle list).
        let gizmo_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mf gizmo pipeline layout"),
            bind_group_layouts: &[Some(&scene_bgl)],
            immediate_size: 0,
        });
        let gizmo_line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: 2 * std::mem::size_of::<crate::GizmoLineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x4, 2 => Float32,
                3 => Float32x2, 4 => Float32x4, 5 => Float32,
            ],
        })];
        let gizmo_line_pipeline = pipeline_with(
            "mf gizmo line",
            &gizmo_layout,
            "vs_gizmo_line",
            "fs_gizmo_line",
            &gizmo_line_buffers,
            wgpu::FrontFace::Ccw,
            DepthMode::NoTest,
        );
        let gizmo_fill_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<crate::GizmoFillVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
        })];
        let gizmo_fill_pipeline = pipeline_with(
            "mf gizmo fill",
            &gizmo_layout,
            "vs_gizmo_fill",
            "fs_gizmo_fill",
            &gizmo_fill_buffers,
            wgpu::FrontFace::Ccw,
            DepthMode::NoTest,
        );
        let plate_slot = || {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mf plate draw uniforms"),
                size: std::mem::size_of::<DrawUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mf plate draw uniforms"),
                layout: &draw_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            DrawSlot { buffer, bind_group }
        };
        let plate_slots = [plate_slot(), plate_slot()];
        MeshRenderer {
            glass_pipeline_ccw,
            glass_pipeline_cw,
            hole_pipeline_ccw,
            hole_pipeline_cw,
            hole_xray_pipeline_ccw,
            hole_xray_pipeline_cw,
            ghost_pipeline_ccw,
            ghost_pipeline_cw,
            background_pipeline,
            shading: Shading::default(),
            split_preview: None,
            show_holes_through_solids: true,
            xray_surface_pipeline_ccw,
            xray_surface_pipeline_cw,
            xray_line_pipeline,
            intersection_line_pipeline,
            plate_pipeline,
            gizmo_line_pipeline,
            gizmo_fill_pipeline,
            cue: SelectionCue::default(),
            surface_pipeline_ccw,
            surface_pipeline_cw,
            line_pipeline,
            overlay_line_pipeline,
            palette,
            pixels_per_point: 1.0,
            draw_bgl,
            scene_uniforms,
            scene_bind_group,
            draw_slots: Vec::new(),
            plate_slots,
        }
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Split's plane (point, normal; world) from the next
    /// [`Self::prepare`] on: the surface on either side is shaded apart and
    /// the cut drawn where the plane crosses it (`None`: no preview).
    pub fn set_split_preview(&mut self, plane: Option<(DVec3, DVec3)>) {
        self.split_preview = plane;
    }

    /// View ▸ Overlays ▸ "Show holes through solids" (on by default): draw
    /// a Hole's hidden part — buried inside or behind a solid — as a
    /// fainter ghost, from the next [`Self::prepare`] on.
    pub fn set_show_holes_through_solids(&mut self, show: bool) {
        self.show_holes_through_solids = show;
    }

    /// How the surface is shaded from the next [`Self::prepare`] on.
    pub fn set_shading(&mut self, shading: Shading) {
        self.shading = shading;
    }

    pub fn shading(&self) -> Shading {
        self.shading
    }

    /// Fill the target with the stage's background gradient. Call first in
    /// a pass, after [`Self::prepare`] (a flat palette background makes it
    /// the clear colour again).
    pub fn draw_background(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.palette.background == self.palette.background_top {
            return;
        }
        pass.set_bind_group(0, &self.scene_bind_group, &[]);
        pass.set_pipeline(&self.background_pipeline);
        pass.draw(0..3, 0..1);
    }

    /// How the selection is drawn from the next [`Self::prepare`] on.
    pub fn set_selection_cue(&mut self, cue: SelectionCue) {
        self.cue = cue;
    }

    pub fn selection_cue(&self) -> SelectionCue {
        self.cue
    }

    /// Physical pixels per point of the target, so line widths (in points)
    /// look the same on every display.
    pub fn set_pixels_per_point(&mut self, ppp: f32) {
        self.pixels_per_point = ppp.max(0.1);
    }

    /// Write the scene (view-projection, eye direction) uniforms. Call once
    /// per frame before the render pass begins. `world_origin` is the
    /// shared reference point every mesh's local origin is shifted from
    /// (the camera target's frame), kept close to the drawn geometry so the
    /// `f32` GPU matrix holds enough precision (MF-GEO-001).
    pub fn prepare(&self, queue: &wgpu::Queue, camera: &OrbitCamera, size: (u32, u32)) {
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        // The camera's target is `world_origin` (see `paint`'s doc): the
        // eye, shifted into that same frame, is exactly `eye() - target`.
        let eye_shifted = (camera.eye() - camera.target).as_vec3();
        let eye_dir = eye_shifted.normalize_or_zero();
        let p = &self.palette;
        // The studio lights, relative to the view (UI plan §4): the key
        // above and left of the camera, the fill from the right.
        let v = eye_dir;
        let right = glam::Vec3::Z
            .cross(v)
            .try_normalize()
            .unwrap_or(glam::Vec3::X);
        let cam_up = v.cross(right).normalize_or_zero();
        let key = (v + 1.1 * cam_up - 0.6 * right).normalize_or_zero();
        let fill = (v - 0.25 * cam_up + 0.8 * right).normalize_or_zero();
        let half = (key + v).normalize_or_zero();
        let u = SceneUniforms {
            view_proj: camera.view_proj(aspect, camera.target).to_cols_array_2d(),
            eye_dir: Vec4::from((eye_dir, 0.0)).to_array(),
            surface: p.surface,
            back_face: p.back_face,
            boundary: p.boundary,
            inactive: [p.inactive_saturation, p.inactive_brightness, 0.0, 0.0],
            viewport: [size.0 as f32, size.1 as f32, 0.0, 0.0],
            tints: [
                p.tints.loose_piece,
                p.tints.non_manifold_adj,
                p.tints.recent_repair,
                p.tints.selected,
                p.tints.hover,
                p.tints.hole_neighbourhood,
            ],
            cue: [
                self.cue.emphasis.clamp(0.0, 1.0),
                p.xray_opacity,
                if self.show_holes_through_solids {
                    p.hole_xray_opacity
                } else {
                    0.0
                },
                0.0,
            ],
            eye_pos: Vec4::from((eye_shifted, 0.0)).to_array(),
            plate: p.plate,
            background: p.background,
            background_top: p.background_top,
            shading: [
                match self.shading.preset {
                    ShadingPreset::Clay => 0.0,
                    ShadingPreset::Studio => 1.0,
                    ShadingPreset::XRay => 2.0,
                },
                f32::from(u8::from(self.shading.wireframe)),
                f32::from(u8::from(self.split_preview.is_some())),
                0.0,
            ],
            light_key: Vec4::from((key, 0.0)).to_array(),
            light_fill: Vec4::from((fill, 0.0)).to_array(),
            light_half: Vec4::from((half, 0.0)).to_array(),
            split_plane: match self.split_preview {
                Some((p, n)) => {
                    let n = n.normalize_or_zero();
                    let shifted = p - camera.target;
                    [n.x as f32, n.y as f32, n.z as f32, -n.dot(shifted) as f32]
                }
                None => [0.0; 4],
            },
        };
        queue.write_buffer(&self.scene_uniforms, 0, bytemuck::bytes_of(&u));
    }

    /// Grow the draw-slot pool to at least `n` entries.
    fn ensure_slots(&mut self, device: &wgpu::Device, n: usize) {
        while self.draw_slots.len() < n {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mf draw uniforms"),
                size: std::mem::size_of::<DrawUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mf draw uniforms"),
                layout: &self.draw_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            self.draw_slots.push(DrawSlot { buffer, bind_group });
        }
    }

    /// Draw every item of `items` at its transform (MF-ISSUE-042), each
    /// shifted into the frame [`Self::prepare`] was called with
    /// (`camera.target`), with its boundary outlines if `show_boundaries`,
    /// then every overlay of `layers` (see [`Layer`]).
    ///
    /// Call after [`Self::prepare`] and before the render pass ends. Must
    /// be called on the same device the renderer and `items` were built
    /// with.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        world_origin: DVec3,
        items: &[DrawItem<'_>],
        show_boundaries: bool,
        layers: &[Layer<'_>],
        intersections: &[Layer<'_>],
    ) {
        self.ensure_slots(device, items.len() + layers.len() + intersections.len());

        for (slot, item) in self.draw_slots.iter().zip(items) {
            let model = shifted_model(item.model, item.mesh.origin, world_origin);
            let u = DrawUniforms {
                model: model.to_cols_array_2d(),
                normal_matrix: normal_matrix(item.model).to_cols_array_2d(),
                color: item.object_color.unwrap_or([0.0; 4]),
                params: [
                    f32::from(u8::from(item.inactive)),
                    self.palette.boundary_width * self.pixels_per_point,
                    // z: 1 while this object has its own palette colour
                    // (View ▸ Colour objects): `fs_surface` mixes it in
                    // for `draw.color` instead of the palette's one
                    // neutral surface colour.
                    f32::from(u8::from(item.object_color.is_some())),
                    self.pixels_per_point,
                ],
                states: [item.shown_states & u32::from(FaceStates::ALL), 0, 0, 0],
            };
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
        }
        let layer_slots = &self.draw_slots[items.len()..items.len() + layers.len()];
        let write_layer = |slot: &DrawSlot, layer: &Layer<'_>| {
            let shifted = shifted_model(layer.model, layer.mesh_origin, world_origin);
            // The pulse: up to 40% lighter (the same hue) and twice as wide.
            let e = layer.emphasis.clamp(0.0, 1.0);
            let [r, g, b, a] = layer.color;
            let lighter = |c: f32| c + (1.0 - c) * 0.4 * e;
            let u = DrawUniforms {
                model: shifted.to_cols_array_2d(),
                normal_matrix: normal_matrix(layer.model).to_cols_array_2d(),
                color: [lighter(r), lighter(g), lighter(b), a],
                params: [
                    0.0,
                    self.palette.overlay_width * self.pixels_per_point * (1.0 + e),
                    // Over the mesh's own outlines: a selected or hovered
                    // hole draws its colour, not the hole's (fix round 1,
                    // I1). Layers among themselves tie (same width, same
                    // origin) and the later one wins.
                    OVERLAY_PULL,
                    0.0,
                ],
                states: [0; 4],
            };
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
        };
        for (slot, layer) in layer_slots.iter().zip(layers) {
            write_layer(slot, layer);
        }
        let intersection_slots = &self.draw_slots[items.len() + layers.len()..];
        for (slot, layer) in intersection_slots.iter().zip(intersections) {
            write_layer(slot, layer);
        }

        pass.set_bind_group(0, &self.scene_bind_group, &[]);
        let glass = self.shading.preset == ShadingPreset::XRay;
        // Solids first, then Holes over them (blended, so what's behind or
        // inside a hole shows through), then see-through objects.
        let drawn = self
            .draw_slots
            .iter()
            .zip(items)
            .filter(|(_, i)| !i.hole && !i.ghost)
            .chain(
                self.draw_slots
                    .iter()
                    .zip(items)
                    .filter(|(_, i)| i.hole && !i.ghost),
            )
            .chain(self.draw_slots.iter().zip(items).filter(|(_, i)| i.ghost));
        for (slot, item) in drawn {
            let mirrored = item.model.determinant() < 0.0;
            let pipeline = match (item.hole, glass, mirrored) {
                _ if item.ghost && mirrored => &self.ghost_pipeline_cw,
                _ if item.ghost => &self.ghost_pipeline_ccw,
                (true, _, true) => &self.hole_pipeline_cw,
                (true, _, false) => &self.hole_pipeline_ccw,
                (false, true, true) => &self.glass_pipeline_cw,
                (false, true, false) => &self.glass_pipeline_ccw,
                (false, false, true) => &self.surface_pipeline_cw,
                (false, false, false) => &self.surface_pipeline_ccw,
            };
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(pipeline);
            let (states, buffers) = item.mesh.surface_draws();
            pass.set_bind_group(2, states, &[]);
            for (buf, count, first_face) in buffers {
                pass.set_vertex_buffer(0, buf.slice(..));
                // The instance index is where the buffer's first face's
                // state is in the mesh's state buffer.
                pass.draw(0..count, first_face..first_face + 1);
            }
            if show_boundaries {
                pass.set_pipeline(&self.line_pipeline);
                for (buf, count) in &item.mesh.lines {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..6, 0..*count / 2);
                }
            }
        }
        // A Hole's hidden part — buried inside or behind a solid — as a
        // fainter ghost, once every surface is in the depth buffer (so
        // `Greater` finds what's in front of it). Drawn for every Hole
        // whether or not it's inactive: the ghost reads the same as the
        // visible hatch, which isn't desaturated either.
        if self.show_holes_through_solids {
            for (slot, item) in self.draw_slots.iter().zip(items).filter(|(_, i)| i.hole) {
                pass.set_bind_group(1, &slot.bind_group, &[]);
                pass.set_pipeline(if item.model.determinant() < 0.0 {
                    &self.hole_xray_pipeline_cw
                } else {
                    &self.hole_xray_pipeline_ccw
                });
                let (states, buffers) = item.mesh.surface_draws();
                pass.set_bind_group(2, states, &[]);
                for (buf, count, first_face) in buffers {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..count, first_face..first_face + 1);
                }
            }
        }
        // The selected faces where nearer surfaces hide them, once every
        // surface is in the depth buffer.
        if self.cue.xray_faces {
            for (slot, item) in self.draw_slots.iter().zip(items) {
                if item.inactive || item.shown_states & u32::from(FaceStates::SELECTED) == 0 {
                    continue;
                }
                pass.set_bind_group(1, &slot.bind_group, &[]);
                pass.set_pipeline(if item.model.determinant() < 0.0 {
                    &self.xray_surface_pipeline_cw
                } else {
                    &self.xray_surface_pipeline_ccw
                });
                let (states, buffers) = item.mesh.surface_draws();
                pass.set_bind_group(2, states, &[]);
                for (buf, count, first_face) in buffers {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..count, first_face..first_face + 1);
                }
            }
        }
        for (slot, layer) in layer_slots.iter().zip(layers) {
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.overlay_line_pipeline);
            for (buf, count) in &layer.lines.chunks {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*count / 2);
            }
        }
        for (slot, layer) in layer_slots.iter().zip(layers).filter(|(_, l)| l.xray) {
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.xray_line_pipeline);
            for (buf, count) in &layer.lines.chunks {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*count / 2);
            }
        }
        // Where a Hole crosses a Solid: on top of everything, no depth
        // test at all (so the exact curve reads even buried inside the
        // solid, like the hidden-hole ghost but a crisp line, not a
        // translucent fill).
        for (slot, layer) in intersection_slots.iter().zip(intersections) {
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.intersection_line_pipeline);
            for (buf, count) in &layer.lines.chunks {
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*count / 2);
            }
        }
    }

    /// Draw the build plate's grid (D9): world-space, depth-tested like
    /// the model's own geometry (so an object resting on it hides the
    /// lines underneath) and fading with distance from the camera. Its
    /// colour is [`Palette::plate`] (the renderer has no colours of its
    /// own). Call after [`Self::prepare`]; draw it before the model's
    /// surfaces (`paint`) so a resting object's opaque pixels overdraw it.
    /// `world_origin` is the frame `prepare` set up (`camera.target`, as
    /// `paint` takes it): the grid is shifted into it, so it lies at world
    /// z = 0 wherever the camera looks.
    pub fn draw_plate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        plate: &crate::Plate,
        world_origin: DVec3,
    ) {
        // Minor lines, then the major ones over them.
        let major = crate::Plate {
            grid_mm: plate.major_mm,
            ..*plate
        };
        let layers = [
            (plate, self.palette.plate),
            (&major, self.palette.plate_major),
        ];
        for (slot, (grid, color)) in layers.into_iter().enumerate() {
            let vertices = crate::plate_vertices(grid);
            if vertices.is_empty() {
                continue;
            }
            let u = DrawUniforms {
                model: shifted_model(DMat4::IDENTITY, DVec3::ZERO, world_origin).to_cols_array_2d(),
                normal_matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                color,
                // Hairlines: the plate steps back (UI plan §11).
                params: [0.0, self.pixels_per_point, 0.0, 0.0],
                states: [0; 4],
            };
            let slot = &self.plate_slots[slot];
            queue.write_buffer(&slot.buffer, 0, bytemuck::bytes_of(&u));
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mf plate vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            pass.set_bind_group(0, &self.scene_bind_group, &[]);
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_pipeline(&self.plate_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..6, 0..(vertices.len() / 2) as u32);
        }
    }

    /// Draw the transform gizmo's handles (D8): screen-space quads with no
    /// depth test, after every other layer, so a handle is never hidden by
    /// the model. `highlight`'s shape (the hovered handle) draws 40%
    /// lighter and twice as wide, the same rule as the selection pulse.
    /// Colours come from `palette`; the renderer has no colours of its own.
    /// Call after [`Self::prepare`] and last, once the model and every
    /// overlay layer are drawn.
    pub fn draw_gizmo(
        &mut self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass<'_>,
        shapes: &[crate::HandleShape],
        highlight: Option<crate::Handle>,
        palette: &crate::GizmoPalette,
    ) {
        pass.set_bind_group(0, &self.scene_bind_group, &[]);
        // Filled shapes first (arrowheads, scale cubes, plane squares,
        // point discs), then lines (shafts, a ring's front-facing
        // polyline) over them, so an axis's shaft reaches visibly into
        // its own arrowhead/cube rather than stopping short under it.
        let fill = crate::gizmo_fill_vertices(shapes, highlight, palette);
        if !fill.is_empty() {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mf gizmo fill vertices"),
                contents: bytemuck::cast_slice(&fill),
                usage: wgpu::BufferUsages::VERTEX,
            });
            pass.set_pipeline(&self.gizmo_fill_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..fill.len() as u32, 0..1);
        }
        let width = GIZMO_LINE_WIDTH_PT * self.pixels_per_point;
        let lines = crate::gizmo_line_vertices(shapes, highlight, palette, width);
        if !lines.is_empty() {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mf gizmo line vertices"),
                contents: bytemuck::cast_slice(&lines),
                usage: wgpu::BufferUsages::VERTEX,
            });
            pass.set_pipeline(&self.gizmo_line_pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..6, 0..(lines.len() / 2) as u32);
        }
    }
}

/// `model` maps object space (as stored in 3MF) to world space. GPU vertex
/// data is stored relative to the mesh's own local origin (still in object
/// space), and the camera's view-projection is relative to `world_origin`.
/// This is the single matrix the shader needs to go straight from a local
/// vertex to the camera's frame: `shifted = T(-world_origin) * model *
/// T(mesh_origin)`.
fn shifted_model(model: DMat4, mesh_origin: DVec3, world_origin: DVec3) -> glam::Mat4 {
    (DMat4::from_translation(-world_origin) * model * DMat4::from_translation(mesh_origin))
        .as_mat4()
}

/// Inverse-transpose of `model`'s linear (3x3) part, computed in `f64`
/// before the cast to the GPU's `f32`. A rotation or uniform scale needs no
/// correction (the model matrix itself is already its own inverse-transpose
/// up to a scalar), but a non-uniform scale or shear — both legal in a 3MF
/// transform — tilts normals unless they go through this instead. The
/// translation never affects a direction, so only the 3x3 part matters;
/// the result is padded into a `Mat4` to keep the uniform layout simple.
fn normal_matrix(model: DMat4) -> Mat4 {
    let linear = DMat3::from_mat4(model);
    // A singular linear part (e.g. a transform that collapses a whole axis)
    // has no meaningful normal transform; falling back to the identity
    // keeps shading stable rather than propagating NaN/Inf into the frame.
    let it = if linear.determinant().abs() > 1e-18 {
        linear.inverse().transpose()
    } else {
        DMat3::IDENTITY
    };
    Mat4::from_mat3(it.as_mat3())
}
