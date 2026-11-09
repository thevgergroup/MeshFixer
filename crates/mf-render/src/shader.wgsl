struct Scene {
    view_proj: mat4x4<f32>,
    // Direction from the scene towards the camera, in world space.
    eye_dir: vec4<f32>,
    // The palette (renderer.rs `Palette`), passed in by the frontend.
    surface: vec4<f32>,
    back_face: vec4<f32>,
    boundary: vec4<f32>,
    // x: saturation an inactive object keeps, y: its brightness.
    inactive: vec4<f32>,
    // Render target size in pixels (xy).
    viewport: vec4<f32>,
    // Face-state tints (renderer.rs `FaceTints`): rgb, a = strength.
    tint_small_component: vec4<f32>,
    tint_non_manifold_adj: vec4<f32>,
    tint_recent_repair: vec4<f32>,
    tint_selected: vec4<f32>,
    tint_hover: vec4<f32>,
};

struct Draw {
    // Object space (relative to the mesh's local origin) to the camera's frame.
    model: mat4x4<f32>,
    // Inverse-transpose of model's 3x3 linear part, for normals (correct
    // under non-uniform scale or shear, not just rotation). Translation
    // column is unused padding.
    normal_matrix: mat4x4<f32>,
    // A layer's colour.
    color: vec4<f32>,
    // x: 1 for an inactive object, or a face layer's strength.
    // y: line width in pixels.
    params: vec4<f32>,
    // x: the face-state flags shown (the overlays switched on).
    states: vec4<u32>,
};

@group(0) @binding(0) var<uniform> scene: Scene;
@group(1) @binding(0) var<uniform> draw: Draw;
// One byte of flags per drawn face, four to a word, every face chunk's
// words in turn (gpu_mesh.rs `FaceStates`).
@group(2) @binding(0) var<storage, read> face_states: array<u32>;

// gpu_mesh.rs `FaceStates` flags.
const SELECTED: u32 = 1u;
const SMALL_COMPONENT: u32 = 2u;
const HOVER: u32 = 4u;
const NON_MANIFOLD_ADJ: u32 = 8u;
const RECENT_REPAIR: u32 = 16u;

struct SurfaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    // The face's shown state flags (0 for overlay faces).
    @location(1) @interpolate(flat) state: u32,
};

// Inactive objects are desaturated and dimmed (MF-ISSUE-042, theme.rs).
fn inactive(rgb: vec3<f32>) -> vec3<f32> {
    let grey = vec3<f32>(dot(rgb, vec3<f32>(0.299, 0.587, 0.114)));
    return mix(grey, rgb, scene.inactive.x) * scene.inactive.y;
}

fn tint(rgb: vec3<f32>, state: u32, flag: u32, t: vec4<f32>) -> vec3<f32> {
    return select(rgb, mix(rgb, t.rgb, t.a), (state & flag) != 0u);
}

// The face's colour before lighting: front or back, then each tint its
// state flags call for, bottom to top. A selected or hovered face shows
// only that (one colour, one meaning: a selected loose piece reads as
// selected, not as a mix of cyan and amber).
fn base_color(front: bool, state: u32) -> vec3<f32> {
    // Back faces (seen through holes, or flipped) are tinted so they stand out.
    var rgb = select(scene.back_face.rgb, scene.surface.rgb, front);
    if ((state & (SELECTED | HOVER)) == 0u) {
        rgb = tint(rgb, state, SMALL_COMPONENT, scene.tint_small_component);
        rgb = tint(rgb, state, NON_MANIFOLD_ADJ, scene.tint_non_manifold_adj);
        rgb = tint(rgb, state, RECENT_REPAIR, scene.tint_recent_repair);
    }
    rgb = tint(rgb, state, SELECTED, scene.tint_selected);
    rgb = tint(rgb, state, HOVER, scene.tint_hover);
    return rgb;
}

fn lit(rgb: vec3<f32>, n_in: vec3<f32>) -> vec3<f32> {
    let light = abs(dot(normalize(n_in), normalize(scene.eye_dir.xyz)));
    return rgb * (0.25 + 0.75 * light);
}

// `first_face` (the instance index) is where the buffer's first face's
// state is; each face is three vertices.
@vertex
fn vs_surface(
    @builtin(vertex_index) vertex: u32,
    @builtin(instance_index) first_face: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
) -> SurfaceOut {
    var out: SurfaceOut;
    out.clip = scene.view_proj * draw.model * vec4<f32>(position, 1.0);
    // Normals need the inverse-transpose of the linear part, not the model
    // matrix itself, to stay correct under non-uniform scale or shear
    // (MF-ISSUE-042).
    out.normal = (draw.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    let face = first_face + vertex / 3u;
    let bits = (face_states[face / 4u] >> ((face % 4u) * 8u)) & 0xffu;
    out.state = bits & draw.states.x;
    return out;
}

@fragment
fn fs_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let rgb = lit(base_color(front, in.state), in.normal);
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), 1.0);
}

// Face overlays: the same faces again, pulled slightly towards the camera
// so they win the depth test against the object's own surface.
@vertex
fn vs_overlay_surface(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> SurfaceOut {
    var out: SurfaceOut;
    out.clip = scene.view_proj * draw.model * vec4<f32>(position, 1.0);
    out.clip.z = out.clip.z - 0.0002 * out.clip.w;
    out.normal = (draw.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    out.state = 0u;
    return out;
}

@fragment
fn fs_overlay_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let light = abs(dot(n, normalize(scene.eye_dir.xyz)));
    let base = select(scene.back_face.rgb, scene.surface.rgb, front);
    let tinted = mix(base, draw.color.rgb, draw.params.x);
    return vec4<f32>(tinted * (0.25 + 0.75 * light), 1.0);
}

// One line segment (a..b) as a screen-space quad `draw.params.y` pixels
// wide: six vertices per instance.
@vertex
fn vs_line(
    @builtin(vertex_index) corner: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
) -> @builtin(position) vec4<f32> {
    var ca = scene.view_proj * draw.model * vec4<f32>(a, 1.0);
    var cb = scene.view_proj * draw.model * vec4<f32>(b, 1.0);
    // Keep both ends in front of the camera before dividing by w.
    let near = 1e-5;
    if (ca.w < near && cb.w < near) {
        return vec4<f32>(0.0, 0.0, 2.0, 1.0); // fully behind: clipped
    }
    if (ca.w < near) {
        ca = mix(ca, cb, (near - ca.w) / (cb.w - ca.w));
    }
    if (cb.w < near) {
        cb = mix(cb, ca, (near - cb.w) / (ca.w - cb.w));
    }
    let px = scene.viewport.xy;
    let sa = ca.xy / ca.w * px;
    let sb = cb.xy / cb.w * px;
    let d = sb - sa;
    let len = length(d);
    var normal = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) {
        normal = vec2<f32>(-d.y, d.x) / len;
    }
    // Corners: (t along the segment, side across it).
    var t = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    var side = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    var clip = mix(ca, cb, t[corner]);
    // NDC spans 2 units per viewport, so half the width in pixels is
    // width / viewport in NDC.
    clip = vec4<f32>(clip.xy + normal * side[corner] * draw.params.y / px * clip.w, clip.zw);
    // Pull lines slightly towards the camera so they draw over their own faces.
    clip.z = clip.z - 0.0005 * clip.w;
    return clip;
}

// Every boundary loop's outline (the hole colour).
@fragment
fn fs_line() -> @location(0) vec4<f32> {
    let rgb = scene.boundary.rgb;
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), 1.0);
}

// Overlay lines: selected loops, cracks, tangled edges, hover.
@fragment
fn fs_overlay_line() -> @location(0) vec4<f32> {
    return vec4<f32>(draw.color.rgb, 1.0);
}
