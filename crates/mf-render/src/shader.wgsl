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
    // pulse after framing, 0–1), y: x-ray opacity, z: a Hole's hidden-part
    // ghost opacity (0 while "Show holes through solids" is off).
    cue: vec4<f32>,
    // The camera eye, in the same world_origin-shifted frame as vertex
    // positions (xyz; w unused): the build plate fades by distance from here.
    eye_pos: vec4<f32>,
    // The build plate's grid colour (D9 `PLATE`).
    plate: vec4<f32>,
    // The background gradient: bottom, top.
    background: vec4<f32>,
    background_top: vec4<f32>,
    // x: preset (0 clay, 1 studio, 2 x-ray), y: 1 for the wireframe,
    // z: 1 while Split's plane previews.
    shading: vec4<f32>,
    // Split's plane in the shifted frame: normal (xyz), offset (w).
    split_plane: vec4<f32>,
    // The studio lights' directions (world, towards the light), worked
    // out once per frame on the CPU: key, fill, and the key's half vector.
    light_key: vec4<f32>,
    light_fill: vec4<f32>,
    light_half: vec4<f32>,
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
    // Barycentric coordinates (each face is three consecutive vertices):
    // the wireframe draws where one of them is near 0.
    @location(2) bary: vec3<f32>,
    // Shifted world position (Split's preview).
    @location(3) world: vec3<f32>,
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

// Soft studio lighting (UI plan §4, the Clay preset): a key light above
// and left of the camera, a fill from the right, a sky/ground hemisphere
// and a faint rim, all relative to the view so the model reads the same
// from any side. Two-sided: the normal is turned to face the camera.
fn studio(n_in: vec3<f32>) -> vec4<f32> {
    // Kept cheap (the lights are per frame, powers by squaring): a heavy
    // fragment stage makes Metal's pass timestamps unusable.
    let v = scene.eye_dir.xyz;
    var n = normalize(n_in);
    let nv = dot(n, v);
    n = select(-n, n, nv >= 0.0);
    // Wrapped diffuse: soft terminators, no hard black side.
    let k = max((dot(n, scene.light_key.xyz) + 0.25) * 0.8, 0.0);
    let f = max(dot(n, scene.light_fill.xyz), 0.0);
    let hemi = 0.5 + 0.5 * n.z;
    let e = 1.0 - abs(nv);
    let rim = e * e * e;
    var s = max(dot(n, scene.light_half.xyz), 0.0);
    s = s * s;
    s = s * s;
    s = s * s;
    s = s * s;
    s = s * s;
    let spec = s;
    // x: diffuse light, y: specular, z: rim.
    return vec4<f32>(0.14 + 0.70 * k + 0.12 * f + 0.12 * hemi, spec, rim, 0.0);
}

fn lit(rgb: vec3<f32>, n_in: vec3<f32>) -> vec3<f32> {
    let l = studio(n_in);
    if (scene.shading.x > 0.5) {
        // Studio: brighter, with a soft highlight.
        return rgb * (0.08 + 1.0 * l.x) + vec3<f32>(0.22 * l.y + 0.06 * l.z);
    }
    return rgb * l.x + vec3<f32>(0.05 * l.z);
}

// The wireframe: how much of a triangle edge this fragment is (0–1), a
// screen-constant ~1 px line.
fn wire(bary: vec3<f32>) -> f32 {
    let d = fwidth(bary);
    let a = smoothstep(vec3<f32>(0.0), d * 1.25, bary);
    return (1.0 - min(min(a.x, a.y), a.z)) * scene.shading.y;
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
    let world = draw.model * vec4<f32>(position, 1.0);
    out.clip = scene.view_proj * world;
    out.world = world.xyz;
    // Normals need the inverse-transpose of the linear part, not the model
    // matrix itself, to stay correct under non-uniform scale or shear
    // (MF-ISSUE-042).
    out.normal = (draw.normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    let face = first_face + vertex / 3u;
    let bits = (face_states[face / 4u] >> ((face % 4u) * 8u)) & 0xffu;
    out.state = bits & draw.states.x;
    let corner = vertex % 3u;
    out.bary = vec3<f32>(f32(corner == 0u), f32(corner == 1u), f32(corner == 2u));
    return out;
}

// A hovered face (unless selected): a light tint (its colour comes from
// `base_color`) kept shaded, so the model's own shape stays visible (UI
// plan §11: overlays never repaint large areas).
fn glow(rgb: vec3<f32>, n_in: vec3<f32>) -> vec3<f32> {
    return lit(rgb, n_in) * 1.08;
}

// A rim in `color` where the surface turns edge-on: with the outline,
// how a selected or hovered object reads without a solid fill.
fn rim(rgb: vec3<f32>, n_in: vec3<f32>, color: vec3<f32>, strength: f32) -> vec3<f32> {
    let v = normalize(scene.eye_dir.xyz);
    let r = pow(1.0 - abs(dot(normalize(n_in), v)), 2.0);
    return mix(rgb, color, clamp(r * strength, 0.0, 1.0));
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
    if (selected) {
        rgb = rim(rgb, in.normal, scene.tint_selected.rgb, 0.9);
    } else if (glowing) {
        rgb = rim(rgb, in.normal, scene.tint_hover.rgb, 0.7);
    }
    // Derivatives only when the wireframe is on (a uniform branch).
    if (scene.shading.y > 0.5) {
        rgb = mix(rgb, rgb * 0.45, wire(in.bary));
    }
    // Split's preview: the top part a little lighter, the bottom a little
    // darker, and the cut drawn where the plane crosses the surface.
    if (scene.shading.z > 0.5) {
        let s = dot(scene.split_plane.xyz, in.world) + scene.split_plane.w;
        rgb = rgb * select(0.72, 1.08, s > 0.0);
        let line = 1.0 - smoothstep(0.0, fwidth(s) * 1.5, abs(s));
        rgb = mix(rgb, vec3<f32>(1.0), line * 0.85);
    }
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), 1.0);
}

// The x-ray preset: every surface faintly, brighter where it turns edge-on
// (a fresnel), so what's inside or behind shows.
@fragment
fn fs_glass_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base = base_color(front, in.state);
    let v = normalize(scene.eye_dir.xyz);
    let n = normalize(in.normal);
    let edge_on = pow(1.0 - abs(dot(n, v)), 2.0);
    let w = wire(in.bary);
    var rgb = mix(base, base * 0.5, w);
    var a = clamp(0.10 + 0.45 * edge_on + 0.5 * w, 0.0, 1.0);
    // Split's section view: the part above the plane a faint ghost, the
    // part below shaded and nearly solid, and the cut drawn at the plane,
    // so the inside of the cut shows.
    if (scene.shading.z > 0.5) {
        let s = dot(scene.split_plane.xyz, in.world) + scene.split_plane.w;
        let line = 1.0 - smoothstep(0.0, fwidth(s) * 1.5, abs(s));
        if (s > 0.0) {
            a = 0.06 + 0.2 * edge_on;
        } else {
            rgb = lit(base, in.normal);
            a = 0.8;
        }
        rgb = mix(rgb, vec3<f32>(1.0), line);
        a = max(a, line);
    }
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), a);
}

// A Hole (its role in a combine): lit like a solid but about 35% opaque,
// with grey diagonal stripes in screen space (a pattern, not a colour:
// ~8 pt apart, `draw.params.w` pixels per point), so it reads as "cut
// away" in any palette.
@fragment
fn fs_hole_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base = base_color(front, in.state);
    var rgb = lit(base, in.normal);
    let selected = (in.state & SELECTED) != 0u;
    if (selected) {
        rgb = rim(rgb, in.normal, scene.tint_selected.rgb, 0.9);
    }
    let h = hatch(in.clip);
    rgb = mix(rgb, vec3<f32>(0.5), 0.75 * h);
    let a = mix(0.35, 0.6, h);
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), a);
}

// A see-through object (the Split workflow's transparent piece): lit like
// a solid but about 25% opaque, a little more where it turns edge-on so
// its outline reads, and no hatch (it isn't a hole), so the other piece's
// cut face and the connectors' counterparts show through it.
@fragment
fn fs_ghost_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base = base_color(front, in.state);
    let rgb = lit(base, in.normal);
    let v = normalize(scene.eye_dir.xyz);
    let edge_on = pow(1.0 - abs(dot(normalize(in.normal), v)), 2.0);
    let a = clamp(0.22 + 0.3 * edge_on, 0.0, 0.6);
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), a);
}

// How much of this fragment the hole's hatch stripes cover (0–1), from its
// clip-space position (screen-constant ~8 pt period, `draw.params.w`
// physical pixels per point).
fn hatch(clip: vec4<f32>) -> f32 {
    let period = 8.0 * max(draw.params.w, 0.1);
    let d = (clip.x + clip.y) / period;
    let f = abs(fract(d) - 0.5) * 2.0;
    let aa = fwidth(d) * 2.0;
    return 1.0 - smoothstep(0.35 - aa, 0.35 + aa, f);
}

// A Hole's hidden part — buried inside or behind a solid — the same
// hatched surface as `fs_hole_surface`, but fainter (`scene.cue.z`,
// about 20%) and drawn only where a nearer surface is already in the
// depth buffer (the pipeline's `Greater` compare): a ghost through solid
// geometry, Tinkercad-style (View ▸ Overlays ▸ "Show holes through
// solids"). `scene.cue.z` is 0 while the overlay is off, so the pass
// draws nothing visible rather than needing a separate toggle here.
@fragment
fn fs_hole_xray_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let base = base_color(front, in.state);
    var rgb = lit(base, in.normal);
    let h = hatch(in.clip);
    rgb = mix(rgb, vec3<f32>(0.5), 0.75 * h);
    let a = mix(scene.cue.z, scene.cue.z * 1.7, h);
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), a);
}

// --- The stage's background: a soft vertical gradient (UI plan §4). ---
struct BackgroundOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn vs_background(@builtin(vertex_index) i: u32) -> BackgroundOut {
    // One triangle covering the target.
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    var out: BackgroundOut;
    out.clip = vec4<f32>(x, y, 1.0, 1.0);
    out.t = y * 0.5 + 0.5;
    return out;
}

@fragment
fn fs_background(in: BackgroundOut) -> @location(0) vec4<f32> {
    let t = clamp(in.t, 0.0, 1.0);
    return vec4<f32>(mix(scene.background.rgb, scene.background_top.rgb, t), 1.0);
}

// One line segment (a..b) as a screen-space quad `draw.params.y` pixels
// wide: six vertices per instance.
struct LineOut {
    @builtin(position) clip: vec4<f32>,
    // -1..1 across the line: its outer edge is darkened (a halo), so a
    // line reads on light and dark surfaces alike (UI plan §11).
    @location(0) across: f32,
};

// The halo: the line's colour in its core, darkened at its edges.
fn halo(rgb: vec3<f32>, across: f32) -> vec3<f32> {
    return mix(rgb, rgb * 0.22, smoothstep(0.55, 0.95, abs(across)));
}

@vertex
fn vs_line(
    @builtin(vertex_index) corner: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
) -> LineOut {
    var out: LineOut;
    out.across = 0.0;
    var ca = scene.view_proj * draw.model * vec4<f32>(a, 1.0);
    var cb = scene.view_proj * draw.model * vec4<f32>(b, 1.0);
    // Keep both ends in front of the camera before dividing by w.
    let near = 1e-5;
    if (ca.w < near && cb.w < near) {
        out.clip = vec4<f32>(0.0, 0.0, 2.0, 1.0); // fully behind: clipped
        return out;
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
    out.clip = clip;
    out.across = side[corner];
    return out;
}

// Every boundary loop's outline (the hole colour).
@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    let rgb = halo(scene.boundary.rgb, in.across);
    return vec4<f32>(select(rgb, inactive(rgb), draw.params.x > 0.5), 1.0);
}

// Overlay lines: selected loops, cracks, tangled edges, hover.
@fragment
fn fs_overlay_line(in: LineOut) -> @location(0) vec4<f32> {
    return vec4<f32>(halo(draw.color.rgb, in.across), 1.0);
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
    // Fades over the camera's own framing distance (eye_pos's length:
    // how far the eye is from what it looks at): full strength near the
    // camera, gone by four times that, so the grid reads around and well
    // beyond the object.
    let scale = max(length(scene.eye_pos.xyz), 1e-3);
    // Fades faster than it used to (UI plan §11: the accoutrement steps
    // back): squared, gone by 2.5 framing distances.
    let lin = clamp(1.0 - dist / (scale * 2.5), 0.0, 1.0);
    let fade = lin * lin;
    // The line's colour: the plate's minor or major lines (renderer.rs
    // `draw_plate` sets it per draw).
    return vec4<f32>(draw.color.rgb, draw.color.a * fade);
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
