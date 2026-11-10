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
    tint_loose_piece: vec4<f32>,
    tint_non_manifold_adj: vec4<f32>,
    tint_recent_repair: vec4<f32>,
    tint_selected: vec4<f32>,
    tint_hover: vec4<f32>,
    tint_hole_neighbourhood: vec4<f32>,
    // The selection cue (renderer.rs `SelectionCue`): x: emphasis (the
    // pulse after framing, 0–1), y: x-ray opacity.
    cue: vec4<f32>,
    // The camera eye, in the same world_origin-shifted frame as vertex
    // positions (xyz; w unused): the build plate fades by distance from here.
    eye_pos: vec4<f32>,
    // The build plate's grid colour (D9 `PLATE`).
    plate: vec4<f32>,
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
    // x: 1 for an inactive object.
    // y: line width in pixels.
    // z: how much further than a mesh's own outlines a layer's lines are
    // pulled towards the camera (0 for the outlines themselves).
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
const LOOSE_PIECE: u32 = 2u;
const HOVER: u32 = 4u;
const NON_MANIFOLD_ADJ: u32 = 8u;
const RECENT_REPAIR: u32 = 16u;
const HOLE_NEIGHBOURHOOD: u32 = 32u;

struct SurfaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    // The face's shown state flags.
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

// The face's colour before lighting: front or back, then one tint. One
// colour, one meaning: a selected face shows only the selection tint (even
// when hovered, or a loose piece), a hovered one only the hover tint;
// otherwise the defect and repair tints apply, bottom to top.
fn base_color(front: bool, state: u32) -> vec3<f32> {
    // Back faces (seen through holes, or flipped) are tinted so they stand out.
    var rgb = select(scene.back_face.rgb, scene.surface.rgb, front);
    if ((state & SELECTED) != 0u) {
        // The pulse brightens it: the selection tint at up to full strength.
        let t = scene.tint_selected;
        let strength = t.a + (1.0 - t.a) * scene.cue.x;
        return tint(rgb, state, SELECTED, vec4<f32>(t.rgb, strength));
    }
    if ((state & HOVER) != 0u) {
        return tint(rgb, state, HOVER, scene.tint_hover);
    }
    rgb = tint(rgb, state, HOLE_NEIGHBOURHOOD, scene.tint_hole_neighbourhood);
    rgb = tint(rgb, state, LOOSE_PIECE, scene.tint_loose_piece);
    rgb = tint(rgb, state, NON_MANIFOLD_ADJ, scene.tint_non_manifold_adj);
    rgb = tint(rgb, state, RECENT_REPAIR, scene.tint_recent_repair);
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

// A hovered face (unless selected) glows: strongly tinted and barely
// shaded, so it reads as near-white against the shaded model at any angle.
fn glow(rgb: vec3<f32>, n_in: vec3<f32>) -> vec3<f32> {
    let light = abs(dot(normalize(n_in), normalize(scene.eye_dir.xyz)));
    return rgb * (0.85 + 0.15 * light);
}

@fragment
fn fs_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base = base_color(front, in.state);
    let selected = (in.state & SELECTED) != 0u;
    let glowing = (in.state & HOVER) != 0u && !selected;
    var rgb = select(lit(base, in.normal), glow(base, in.normal), glowing);
    // While the selection pulses, it's lit evenly too (brighter).
    if (selected) {
        rgb = mix(rgb, glow(base, in.normal), scene.cue.x);
    }
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), 1.0);
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
    // Pull lines slightly towards the camera so they draw over their own
    // faces; overlay layers a little further, so they win over the
    // (narrower, so differently sloped) outlines they're drawn on.
    clip.z = clip.z - (0.0005 + draw.params.z) * clip.w;
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

// The selection where nearer surfaces hide it (the x-ray pass: drawn only
// behind the depth buffer, blended, no depth write): the selection colour
// at the x-ray opacity, a little stronger while it pulses. Everything else
// is discarded.
@fragment
fn fs_xray_surface(in: SurfaceOut) -> @location(0) vec4<f32> {
    if ((in.state & SELECTED) == 0u) {
        discard;
    }
    let a = min(scene.cue.y * (1.0 + 0.5 * scene.cue.x), 1.0);
    return vec4<f32>(scene.tint_selected.rgb, a);
}

// A selection outline where nearer surfaces hide it.
@fragment
fn fs_xray_line() -> @location(0) vec4<f32> {
    return vec4<f32>(draw.color.rgb, min(scene.cue.y * (1.0 + 0.5 * scene.cue.x), 1.0));
}

// --- The build plate (overlay.rs `plate_vertices`): a world-space grid at
// z = 0, drawn before the model, depth-tested like its own geometry, and
// fading with distance from the camera so it reads as a floor, not a
// flat decal. ---

struct PlateOut {
    @builtin(position) clip: vec4<f32>,
    // World-space (shifted) position, for the distance fade.
    @location(0) world: vec3<f32>,
};

// Reuses `vs_line`'s screen-space quad widening (the plate's lines are the
// same kind of 3 pt-style overlay line, just world-space and depth-tested),
// but also carries the unprojected world position through for the fade.
@vertex
fn vs_plate(
    @builtin(vertex_index) corner: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
) -> PlateOut {
    var ca = scene.view_proj * draw.model * vec4<f32>(a, 1.0);
    var cb = scene.view_proj * draw.model * vec4<f32>(b, 1.0);
    let wa = (draw.model * vec4<f32>(a, 1.0)).xyz;
    let wb = (draw.model * vec4<f32>(b, 1.0)).xyz;
    let near = 1e-5;
    if (ca.w < near && cb.w < near) {
        var out: PlateOut;
        out.clip = vec4<f32>(0.0, 0.0, 2.0, 1.0);
        out.world = wa;
        return out;
    }
    var wt = 0.0;
    if (ca.w < near) {
        wt = (near - ca.w) / (cb.w - ca.w);
        ca = mix(ca, cb, wt);
    }
    if (cb.w < near) {
        wt = (near - cb.w) / (ca.w - cb.w);
        cb = mix(cb, ca, wt);
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
    var t = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    var side = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    var clip = mix(ca, cb, t[corner]);
    clip = vec4<f32>(clip.xy + normal * side[corner] * draw.params.y / px * clip.w, clip.zw);
    // Pushed slightly *away* from the camera (fix round 1, review point
    // 4): an object resting on the plate is exactly coplanar with it at
    // z = 0, which without a bias is a depth tie the depth test can break
    // either way (z-fighting). The plate loses every such tie, so a
    // resting object's own opaque surface always wins, not just when its
    // triangles happen to rasterise first.
    clip.z = clip.z + 0.0005 * clip.w;
    var out: PlateOut;
    out.clip = clip;
    out.world = mix(wa, wb, t[corner]);
    return out;
}

// The grid colour, faded to transparent with distance from the camera (an
// infinite-floor convention) so the plate recedes rather than clipping
// hard at its edge.
@fragment
fn fs_plate(in: PlateOut) -> @location(0) vec4<f32> {
    let dist = distance(in.world, scene.eye_pos.xyz);
    // Fades over the camera's own framing distance (eye_pos's own length
    // from the origin the plate sits at): full strength near the camera,
    // gone by twice that.
    let scale = max(length(scene.eye_pos.xyz), 1e-3);
    let fade = clamp(1.0 - dist / (scale * 2.0), 0.0, 1.0);
    return vec4<f32>(scene.plate.rgb, scene.plate.a * fade);
}

// --- The transform gizmo (overlay.rs): screen-space geometry in pixel
// coordinates (the same unit `screen_position` returns), drawn last with
// no depth test so a handle is never hidden by the model (D8). Lines
// (shafts, a ring's front-facing polyline) go through `vs_gizmo_line`,
// the same instanced-quad-per-segment trick as the model's own overlay
// lines; filled shapes (arrowheads, scale cubes, plane squares, point
// discs) go through `vs_gizmo_fill`, a plain triangle list. Every vertex
// already carries its own (possibly brightened) colour, computed on the
// CPU per handle (overlay.rs `gizmo_line_vertices`/`gizmo_fill_vertices`). ---

fn pixel_to_ndc(p: vec2<f32>) -> vec2<f32> {
    let px = scene.viewport.xy;
    // Pixel space (y down) to NDC (y up): the inverse of `screen_position`.
    return vec2<f32>(p.x / px.x * 2.0 - 1.0, 1.0 - p.y / px.y * 2.0);
}

struct GizmoOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_gizmo_line(
    @builtin(vertex_index) corner: u32,
    @location(0) a_position: vec2<f32>,
    @location(1) a_color: vec4<f32>,
    @location(2) a_width_px: f32,
    @location(3) b_position: vec2<f32>,
    @location(4) b_color: vec4<f32>,
    @location(5) b_width_px: f32,
) -> GizmoOut {
    let px = scene.viewport.xy;
    let d = b_position - a_position;
    let len = length(d);
    var normal = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) {
        normal = vec2<f32>(-d.y, d.x) / len;
    }
    var t = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    var side = array<f32, 6>(-1.0, -1.0, 1.0, -1.0, 1.0, 1.0);
    let width = mix(a_width_px, b_width_px, t[corner]);
    let p = mix(a_position, b_position, t[corner]) + normal * side[corner] * width * 0.5;
    var out: GizmoOut;
    // Depth is 1.0 (the far plane), not the near plane: irrelevant under
    // the real `NoTest` pipeline (`CompareFunction::Always`, no depth
    // write), but it is what makes "no depth test" an actual, checkable
    // claim — at the far plane, the gizmo would lose a real depth test
    // against literally anything else in the scene, so if it still draws
    // over geometry, only `NoTest` (not a bug that happened to write a
    // nearer depth) can explain it.
    out.clip = vec4<f32>(pixel_to_ndc(p), 1.0, 1.0);
    out.color = mix(a_color, b_color, t[corner]);
    return out;
}

@fragment
fn fs_gizmo_line(in: GizmoOut) -> @location(0) vec4<f32> {
    return in.color;
}

@vertex
fn vs_gizmo_fill(
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
) -> GizmoOut {
    var out: GizmoOut;
    // See `vs_gizmo_line`'s comment: the far plane, not the near plane.
    out.clip = vec4<f32>(pixel_to_ndc(position), 1.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_gizmo_fill(in: GizmoOut) -> @location(0) vec4<f32> {
    return in.color;
}
