struct Scene {
    view_proj: mat4x4<f32>,
    // Direction from the scene towards the camera, in world space.
    eye_dir: vec4<f32>,
};

struct Draw {
    // Object space (relative to the mesh's local origin) to the camera's frame.
    model: mat4x4<f32>,
    // Colour multiplier; inactive objects are dimmed. Alpha is unused.
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> scene: Scene;
@group(1) @binding(0) var<uniform> draw: Draw;

struct SurfaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_surface(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> SurfaceOut {
    var out: SurfaceOut;
    out.clip = scene.view_proj * draw.model * vec4<f32>(position, 1.0);
    // The model matrix may mirror (negative determinant); normals only
    // rotate/scale, so the 3x3 part is enough (MF-ISSUE-042).
    out.normal = (draw.model * vec4<f32>(normal, 0.0)).xyz;
    return out;
}

@fragment
fn fs_surface(in: SurfaceOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let light = abs(dot(n, normalize(scene.eye_dir.xyz)));
    // Back faces (seen through holes, or flipped) are tinted so they stand out.
    let base = select(vec3<f32>(0.62, 0.30, 0.30), vec3<f32>(0.70, 0.72, 0.76), front);
    let shade = 0.25 + 0.75 * light;
    return vec4<f32>(base * shade * draw.tint.rgb, 1.0);
}

@vertex
fn vs_line(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    var clip = scene.view_proj * draw.model * vec4<f32>(position, 1.0);
    // Pull lines slightly towards the camera so they draw over their own faces.
    clip.z = clip.z - 0.0005 * clip.w;
    return clip;
}

@fragment
fn fs_line() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.55, 0.05, 1.0);
}

// Selected boundary loops.
@fragment
fn fs_highlight() -> @location(0) vec4<f32> {
    return vec4<f32>(0.1, 0.9, 1.0, 1.0);
}
