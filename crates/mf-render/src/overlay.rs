//! The transform gizmo and the build plate (MF-OBJ-001, D8/D9): two
//! overlays drawn on top of the ordinary scene, each with its own pass.
//! The gizmo is screen-space quads with no depth test, like the existing
//! 3 pt overlay lines, drawn last so the handles are never hidden by the
//! model (D8). The plate is a world-space grid at z = 0, depth-tested
//! like the model's own geometry, that fades with distance from the
//! camera.
//!
//! `mf-render` has no theme of its own: [`GizmoPalette`] and the plate's
//! colour ([`crate::Palette::plate`]) are plain values the frontend
//! passes in (`mf-desktop`'s `theme.rs`, D9). This module only turns
//! those values and [`crate::HandleShape`]/[`Plate`] into vertex data; the
//! GPU pipelines live in `renderer.rs`.

use bytemuck::{Pod, Zeroable};
use glam::DVec3;

use crate::{Handle, HandleShape};

/// The gizmo's own colours (D8): the conventional axis colours, plus a
/// neutral for handles with no single axis (the view ring, the uniform
/// scale handle, the move centre). Nothing else on the gizmo has a
/// defect-table meaning, so this is a separate, small palette rather than
/// a part of [`crate::Palette`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GizmoPalette {
    pub axis_x: [f32; 4],
    pub axis_y: [f32; 4],
    pub axis_z: [f32; 4],
    pub neutral: [f32; 4],
}

impl GizmoPalette {
    /// A headless palette for offscreen renders and tests, matching
    /// [`crate::Palette::HEADLESS`]'s spirit: plain values, no theme.
    pub const HEADLESS: GizmoPalette = GizmoPalette {
        axis_x: [0.90, 0.28, 0.30, 1.0],
        axis_y: [0.27, 0.65, 0.35, 1.0],
        axis_z: [0.24, 0.55, 0.94, 1.0],
        neutral: [1.0, 1.0, 1.0, 1.0],
    };

    /// This palette's colour for `handle` (D8: axis handles take their
    /// world axis's colour; plane and ring handles take the colour of the
    /// axis they're named after — `Plane(i)`/`Ring(i)`'s index is that
    /// axis, per [`crate::Handle`]; everything else is neutral).
    pub fn color_for(&self, handle: Handle) -> [f32; 4] {
        match handle {
            Handle::Axis(0) | Handle::ScaleAxis(0) | Handle::Plane(0) | Handle::Ring(0) => {
                self.axis_x
            }
            Handle::Axis(1) | Handle::ScaleAxis(1) | Handle::Plane(1) | Handle::Ring(1) => {
                self.axis_y
            }
            Handle::Axis(2) | Handle::ScaleAxis(2) | Handle::Plane(2) | Handle::Ring(2) => {
                self.axis_z
            }
            _ => self.neutral,
        }
    }
}

/// The build plate (D9): a grid at world z = 0. The default is 256 x 256
/// mm with a 10 mm grid, matching a common consumer FDM bed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plate {
    pub size_mm: [f64; 2],
    pub grid_mm: f64,
}

impl Default for Plate {
    fn default() -> Plate {
        Plate {
            size_mm: [256.0, 256.0],
            grid_mm: 10.0,
        }
    }
}

/// One screen-space gizmo line vertex: a pixel position (y down, the same
/// unit [`crate::screen_position`] returns) and this handle's colour,
/// brightened and widened by the caller for the hovered handle (the
/// emphasis is baked into `width_px` and `color` per vertex so the shader
/// stays a plain screen-space quad, like the existing overlay lines).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GizmoVertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
    pub width_px: f32,
}

/// How many segments a ring/circle handle is tessellated into for drawing.
const RING_SEGMENTS: usize = 48;

/// The base gizmo line width, in points (like [`crate::Palette::overlay_width`]
/// for the ordinary overlay lines); the hovered handle draws at twice this.
pub const GIZMO_LINE_WIDTH_PT: f32 = 2.0;

/// Screen-space line-segment vertices (pairs, one segment per two
/// vertices) for every shape in `shapes`, coloured by `palette` per
/// [`GizmoPalette::color_for`]. `highlight`'s shape, if present, is drawn
/// 40% lighter and twice as wide (the same rule as the selection pulse,
/// D8/visual-language.md).
pub fn gizmo_vertices(
    shapes: &[HandleShape],
    highlight: Option<Handle>,
    palette: &GizmoPalette,
    line_width_pt: f32,
) -> Vec<GizmoVertex> {
    let mut out = Vec::new();
    for shape in shapes {
        let emphasized = highlight == Some(shape.handle);
        let color = lighten(palette.color_for(shape.handle), emphasized);
        let width = if emphasized {
            line_width_pt * 2.0
        } else {
            line_width_pt
        };
        match shape.radius {
            Some(r) => push_ring(&mut out, shape.a, r, color, width),
            None => push_segment(&mut out, shape.a, shape.b, color, width),
        }
    }
    out
}

/// 40% lighter (same hue), as the selection pulse (D8/visual-language.md).
fn lighten(c: [f32; 4], emphasize: bool) -> [f32; 4] {
    if !emphasize {
        return c;
    }
    let [r, g, b, a] = c;
    let up = |v: f32| v + (1.0 - v) * 0.4;
    [up(r), up(g), up(b), a]
}

fn push_segment(
    out: &mut Vec<GizmoVertex>,
    a: (f64, f64),
    b: (f64, f64),
    color: [f32; 4],
    width_px: f32,
) {
    out.push(GizmoVertex {
        position: [a.0 as f32, a.1 as f32],
        color,
        width_px,
    });
    out.push(GizmoVertex {
        position: [b.0 as f32, b.1 as f32],
        color,
        width_px,
    });
}

fn push_ring(
    out: &mut Vec<GizmoVertex>,
    centre: (f64, f64),
    radius: f64,
    color: [f32; 4],
    width_px: f32,
) {
    for i in 0..RING_SEGMENTS {
        let a0 = std::f64::consts::TAU * i as f64 / RING_SEGMENTS as f64;
        let a1 = std::f64::consts::TAU * (i + 1) as f64 / RING_SEGMENTS as f64;
        let p0 = (centre.0 + radius * a0.cos(), centre.1 + radius * a0.sin());
        let p1 = (centre.0 + radius * a1.cos(), centre.1 + radius * a1.sin());
        push_segment(out, p0, p1, color, width_px);
    }
}

/// One world-space plate grid-line vertex.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct PlateVertex {
    pub position: [f32; 3],
}

/// World-space line-segment vertices (pairs) for `plate`'s grid: lines
/// parallel to X and to Y, `grid_mm` apart, at z = 0. The grid is snapped
/// to multiples of `grid_mm` (not just centred on the plate's own
/// bounds), so a line always falls exactly on the world origin — where an
/// object placed at the origin (the common case) meets the plate.
/// Degenerate input (a non-positive size or grid) draws nothing rather
/// than looping forever or dividing by zero.
pub fn plate_vertices(plate: &Plate) -> Vec<PlateVertex> {
    let [sx, sy] = plate.size_mm;
    let grid = plate.grid_mm;
    if sx <= 0.0 || sy <= 0.0 || grid <= 0.0 {
        return Vec::new();
    }
    let hx = sx * 0.5;
    let hy = sy * 0.5;
    let mut out = Vec::new();
    let mut push = |a: DVec3, b: DVec3| {
        out.push(PlateVertex {
            position: a.as_vec3().to_array(),
        });
        out.push(PlateVertex {
            position: b.as_vec3().to_array(),
        });
    };
    let max_i = (hx / grid).floor() as i64;
    for i in -max_i..=max_i {
        let x = i as f64 * grid;
        push(DVec3::new(x, -hy, 0.0), DVec3::new(x, hy, 0.0));
    }
    let max_j = (hy / grid).floor() as i64;
    for j in -max_j..=max_j {
        let y = j as f64 * grid;
        push(DVec3::new(-hx, y, 0.0), DVec3::new(hx, y, 0.0));
    }
    out
}
