//! `boolean`: a boolean on the target's local window (D5, D10, D15), with
//! independent validation (D4).
//!
//! Stages (progress words, each followed by a cancel checkpoint):
//! 1. **finding the overlap**: the tool's bounds, grown by a margin, are the
//!    window; the target is clipped to it and closed ([`clip_to_box`]);
//! 2. **cutting**: whether the window and the tool meet at all (exact);
//! 3. **combining**: the kernel, its degenerate post-pass, and
//!    `validate_boolean` on its result (only *new* self-intersections
//!    count, geometry-granular);
//! 4. **joining back**: the window's caps are dropped and the result is
//!    welded back into the target by exact position along the seam, with
//!    [`MeshEdit`] on the target, so faces outside the window are kept
//!    (bitwise, same ids) and every created face names its source;
//! 5. **checking the result**: `mf_geometry::validate_solid_with` around
//!    every created face (openings the target already had, away from the
//!    window, may stay open).
//!
//! A cancelled run returns [`BooleanError::Cancelled`] and produces
//! nothing.

use std::collections::HashMap;

use mf_geometry::{Aabb, Bvh, FaceId, Mesh, MeshEdit, Provenance, SolidDefect, Sources, VertexId};
use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;

use crate::clip::{ClipError, ClipOptions, Point, WindowFace, clip_to_box};
use crate::kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};
use crate::remove_created_degenerates;
use crate::section::SectionError;
use crate::validate::{SolidError, has_open_edge, operands_meet, validate_boolean};
use crate::weld::key;

/// The stage words, in order.
pub const STAGES: [&str; 5] = [
    "finding the overlap",
    "cutting",
    "combining",
    "joining back",
    "checking the result",
];

/// A boolean's result.
#[derive(Debug, Clone)]
pub struct BooleanOutput {
    /// The target's new geometry (in the target's frame).
    pub mesh: Mesh,
    /// How it differs from the target: kept faces keep their ids; created
    /// faces name target faces (`Sources::faces`) or tool faces
    /// (`Sources::operands`, operand 1).
    pub provenance: Provenance,
    /// Triangles in the window operand the kernel saw.
    pub window_faces: usize,
}

/// Why a boolean produced no result. Each has a stable code
/// ([`BooleanError::code`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BooleanError {
    #[error("cancelled by the user")]
    Cancelled,
    /// Subtract or intersect on a target with openings (D15).
    #[error("the target has {holes} openings")]
    TargetOpen { holes: usize },
    #[error("the tool is not closed")]
    ToolOpen,
    /// A temporary closure of an opening survived a join (D15).
    #[error("the tool doesn't cover the opening it was joined over")]
    OpeningNotCovered,
    #[error("an opening crosses the window and is too large to close for the join")]
    OpeningTooLarge,
    #[error("the objects don't touch")]
    NoOverlap,
    /// The window operand (or the tool) intersects itself, which the kernel
    /// can't take.
    #[error("the {} overlaps itself near the tool ({faces} faces)", if *tool { "tool" } else { "target" })]
    OperandOverlapsItself { tool: bool, faces: usize },
    #[error("the section where the window cuts the target folds onto itself")]
    SectionSelfIntersects,
    /// No window within the D5 margin avoids touching the target exactly
    /// (ADR-0011): the window refuses rather than grow to the whole model.
    #[error("the target's surface near the tool is too thin to cut cleanly")]
    WindowDegenerate,
    #[error("the result is not a closed, manifold surface: {0}")]
    ResultNotManifold(String),
    #[error("the result intersects itself: {0}")]
    ResultSelfIntersects(String),
    #[error("the result is empty")]
    ResultEmpty,
    #[error("the boolean kernel failed: {0}")]
    KernelInternal(String),
}

impl BooleanError {
    /// A stable, machine-readable code (the copy table's).
    pub fn code(&self) -> &'static str {
        match self {
            BooleanError::Cancelled => "cancelled",
            BooleanError::TargetOpen { .. } => "operand_not_closed",
            BooleanError::ToolOpen => "tool_not_closed",
            BooleanError::OpeningNotCovered => "opening_not_covered",
            BooleanError::OpeningTooLarge => "opening_too_large_for_join",
            BooleanError::NoOverlap => "no_overlap",
            BooleanError::OperandOverlapsItself { .. } => "operand_overlaps_itself",
            BooleanError::SectionSelfIntersects => "section_self_intersects",
            BooleanError::WindowDegenerate => "window_degenerate",
            BooleanError::ResultNotManifold(_) => "result_not_manifold",
            BooleanError::ResultSelfIntersects(_) => "result_self_intersects",
            BooleanError::ResultEmpty => "result_empty",
            BooleanError::KernelInternal(_) => "kernel_internal",
        }
    }
}

impl From<Cancelled> for BooleanError {
    fn from(_: Cancelled) -> Self {
        BooleanError::Cancelled
    }
}

pub use mf_geometry::affine::{apply, compose, determinant, inverse};

/// The tool brought into the target's frame (D10): the tool's transform
/// followed by the inverse of the target's. A mirroring transform (negative
/// determinant) has the tool's winding flipped, so its normals stay
/// outward. Face ids are kept. `None` if the target's transform can't be
/// inverted. Intersection detection uses the same function, so detection
/// and the boolean agree.
pub fn operand_in_frame(tool: &Mesh, tool_t: &[f64; 12], target_t: &[f64; 12]) -> Option<Mesh> {
    let into = compose(tool_t, &inverse(target_t)?);
    let mirrored = determinant(&into) < 0.0;
    let mut edit = tool.edit();
    let moves: Vec<(VertexId, [f64; 3])> =
        tool.vertices().map(|(v, p)| (v, apply(&into, p))).collect();
    for (v, p) in moves {
        edit.move_vertex(v, p).ok()?; // allow-swallow: every vertex was just listed as live
    }
    if mirrored {
        let faces: Vec<(FaceId, [u32; 3])> = tool.faces().collect();
        for (f, t) in faces {
            edit.set_face(f, [t[0], t[2], t[1]]).ok()?; // allow-swallow: every face was just listed as live
        }
    }
    edit.finish().ok().map(|(m, _)| m) // allow-swallow: positions are finite images of finite ones
}

/// The tool as a kernel operand: compact arrays, each triangle labelled
/// `(1, its face id)`.
fn tool_operand(tool: &Mesh) -> KernelMesh {
    let mut slot = vec![u32::MAX; tool.vertex_slots() as usize];
    let mut positions = Vec::with_capacity(tool.vertex_count());
    for (v, p) in tool.vertices() {
        slot[v.0 as usize] = positions.len() as u32;
        positions.push(p);
    }
    let mut triangles = Vec::with_capacity(tool.face_count());
    let mut face_ids = Vec::with_capacity(tool.face_count());
    for (f, t) in tool.faces() {
        triangles.push(t.map(|v| slot[v as usize]));
        face_ids.push((1u8, f.0));
    }
    KernelMesh {
        positions,
        triangles,
        face_ids,
    }
}

fn stage(ctx: &RunContext, i: usize) -> Result<(), Cancelled> {
    ctx.progress.report(STAGES[i], 0.0);
    ctx.checkpoint()
}

/// Computes `target op tool` on the target's local window (see the module
/// docs). `tool` is already in the target's frame ([`operand_in_frame`]).
pub fn boolean(
    target: &Mesh,
    tool: &Mesh,
    op: BoolOp,
    kernel: &dyn Kernel,
    ctx: &RunContext,
) -> Result<BooleanOutput, BooleanError> {
    stage(ctx, 0)?;
    let b = tool_operand(tool);
    if b.triangles.is_empty() || has_open_edge(&b.triangles) {
        return Err(BooleanError::ToolOpen);
    }
    if op != BoolOp::Union {
        let topo = target.topology_with(ctx)?;
        if topo.boundary_edges > 0 {
            return Err(BooleanError::TargetOpen {
                holes: topo.boundary_loops.len().max(1),
            });
        }
    }
    let Some(tb) = tool.bounding_box_with(ctx)? else {
        return Err(BooleanError::ToolOpen);
    };
    let reach = tb
        .min
        .iter()
        .chain(&tb.max)
        .fold(0.0f64, |m, x| m.max(x.abs()));
    let margin = tb.diagonal() * 0.05 + reach * 1e-9;
    let window = Aabb {
        min: tb.min.map(|x| x - margin),
        max: tb.max.map(|x| x + margin),
    };
    // D15: a window may grow to 4× the tool's bounds for an opening.
    let c = tb.center();
    let limit = Aabb {
        min: [0, 1, 2].map(|a| c[a] - 2.0 * (tb.max[a] - tb.min[a]) - 2.0 * margin),
        max: [0, 1, 2].map(|a| c[a] + 2.0 * (tb.max[a] - tb.min[a]) + 2.0 * margin),
    };
    let clipped = clip_to_box(
        target,
        window,
        ClipOptions {
            close_openings: op == BoolOp::Union,
            limit,
            margin,
        },
        ctx,
    )
    .map_err(|e| match e {
        ClipError::Cancelled => BooleanError::Cancelled,
        ClipError::OpeningTooLarge => BooleanError::OpeningTooLarge,
        ClipError::Open => BooleanError::TargetOpen { holes: 1 },
        // Sections crossing each other: the target overlaps itself where
        // the window cuts it (the same thing the kernel's operand check
        // finds inside the window).
        ClipError::Section(SectionError::SelfIntersects) => BooleanError::OperandOverlapsItself {
            tool: false,
            faces: 0,
        },
        ClipError::Section(SectionError::Degenerate) | ClipError::Degenerate => {
            BooleanError::SectionSelfIntersects
        }
        ClipError::WindowDegenerate => BooleanError::WindowDegenerate,
    })?;
    let a = &clipped.inside;

    stage(ctx, 1)?;
    let target_part = KernelMesh {
        positions: a.positions.clone(),
        triangles: a
            .triangles
            .iter()
            .zip(&clipped.inside_faces)
            .filter(|(_, s)| matches!(s, WindowFace::Target(_)))
            .map(|(t, _)| *t)
            .collect(),
        face_ids: Vec::new(),
    };
    if !operands_meet(&target_part, &b, ctx).map_err(solid_error)? {
        return Err(BooleanError::NoOverlap);
    }
    drop(target_part);

    stage(ctx, 2)?;
    let mut r = kernel.boolean(a, &b, op, ctx).map_err(|e| match e {
        KernelError::Cancelled => BooleanError::Cancelled,
        KernelError::NotClosed { operand: 1 } => BooleanError::ToolOpen,
        KernelError::NotClosed { .. } => BooleanError::KernelInternal(format!(
            "{}: the window wasn't closed (a defect in the window, not the model)",
            kernel.id()
        )),
        KernelError::SelfIntersecting { operand, faces } => BooleanError::OperandOverlapsItself {
            tool: operand == 1,
            faces,
        },
        KernelError::Internal(m) => BooleanError::KernelInternal(format!("{}: {m}", kernel.id())),
    })?;
    ctx.checkpoint()?;
    remove_created_degenerates(&mut r, [a, &b], ctx).map_err(solid_error)?;
    validate_boolean(&r, [a, &b], ctx).map_err(solid_error)?;
    let source = |label: (u8, u32)| -> Option<WindowFace> {
        match label.0 {
            0 => clipped.inside_faces.get(label.1 as usize).copied(),
            _ => None,
        }
    };
    if r.origin
        .iter()
        .any(|&o| source(o) == Some(WindowFace::Fill))
    {
        return Err(BooleanError::OpeningNotCovered);
    }
    if op == BoolOp::Intersect && r.origin.iter().any(|&o| source(o) == Some(WindowFace::Cap)) {
        return Err(BooleanError::KernelInternal(format!(
            "{}: part of the intersection lies on the window's edge",
            kernel.id()
        )));
    }

    stage(ctx, 3)?;
    let (mesh, provenance) = splice(target, &clipped, &r, op, ctx)?;

    stage(ctx, 4)?;
    check(target, &mesh, &provenance, ctx)?;
    ctx.progress.report(STAGES[4], 1.0);
    Ok(BooleanOutput {
        mesh,
        provenance,
        window_faces: a.triangles.len(),
    })
}

fn solid_error(e: SolidError) -> BooleanError {
    match e {
        SolidError::Cancelled => BooleanError::Cancelled,
        SolidError::Empty => BooleanError::ResultEmpty,
        SolidError::SelfIntersecting { .. } => BooleanError::ResultSelfIntersects(e.to_string()),
        SolidError::NotClosed { .. }
        | SolidError::NotManifold { .. }
        | SolidError::InconsistentOrientation { .. }
        | SolidError::DegenerateFace { .. } => BooleanError::ResultNotManifold(e.to_string()),
        SolidError::IndexOutOfRange { .. } => BooleanError::KernelInternal(e.to_string()),
    }
}

fn geometry(e: mf_geometry::GeometryError) -> BooleanError {
    BooleanError::KernelInternal(format!("joining the result back: {e}"))
}

/// Joins the kernel's result back into the target (stage 4).
fn splice(
    target: &Mesh,
    clipped: &crate::clip::Clipped,
    r: &KernelResult,
    op: BoolOp,
    ctx: &RunContext,
) -> Result<(Mesh, Provenance), BooleanError> {
    let mut edit: MeshEdit = target.edit();
    let mut new_vertex: HashMap<usize, VertexId> = HashMap::new();
    let mut point = |p: Point, edit: &mut MeshEdit| -> u32 {
        match p {
            Point::Original(v) => v,
            Point::New(i) => {
                new_vertex
                    .entry(i)
                    .or_insert_with(|| edit.add_vertex(clipped.new_points[i]))
                    .0
            }
        }
    };
    let mut ticker = ctx.ticker();
    if op == BoolOp::Intersect {
        // The result is the overlap alone: nothing outside the window stays.
        let faces: Vec<FaceId> = target.faces().map(|(f, _)| f).collect();
        for f in faces {
            ticker.tick()?;
            edit.remove_face(f).map_err(geometry)?;
        }
        let vertices: Vec<VertexId> = target.vertices().map(|(v, _)| v).collect();
        for v in vertices {
            ticker.tick()?;
            edit.remove_vertex(v).map_err(geometry)?;
        }
    } else {
        for &f in &clipped.cut {
            edit.remove_face(f).map_err(geometry)?;
        }
        for &v in &clipped.inside_vertices {
            edit.remove_vertex(VertexId(v)).map_err(geometry)?;
        }
        for &(t, f) in &clipped.outside_pieces {
            ticker.tick()?;
            let tri = t.map(|p| point(p, &mut edit));
            edit.add_face(
                tri,
                Sources {
                    faces: vec![f],
                    ..Sources::default()
                },
            )
            .map_err(geometry)?;
        }
    }
    // The result's vertices: seam positions are the seam's points.
    let seam: HashMap<[u64; 3], usize> = if op == BoolOp::Intersect {
        HashMap::new()
    } else {
        clipped
            .seam
            .iter()
            .map(|&i| (key(clipped.new_points[i]), i))
            .collect()
    };
    let mut vertex: Vec<Option<u32>> = vec![None; r.positions.len()];
    for (t, &label) in r.triangles.iter().zip(&r.origin) {
        ticker.tick()?;
        let sources = match label {
            (0, i) => match clipped.inside_faces.get(i as usize) {
                Some(WindowFace::Target(f)) => Sources {
                    faces: vec![*f],
                    ..Sources::default()
                },
                Some(WindowFace::Cap) => continue,
                _ => {
                    return Err(BooleanError::KernelInternal(format!(
                        "result face names window face {i}, which can't be in a result"
                    )));
                }
            },
            (_, f) => Sources {
                operands: vec![(1, FaceId(f))],
                ..Sources::default()
            },
        };
        let mut tri = [0u32; 3];
        for (k, &v) in t.iter().enumerate() {
            let id = match vertex[v as usize] {
                Some(id) => id,
                None => {
                    let p = r.positions[v as usize];
                    let id = match seam.get(&key(p)) {
                        Some(&i) => point(Point::New(i), &mut edit),
                        None => edit.add_vertex(p).0,
                    };
                    vertex[v as usize] = Some(id);
                    id
                }
            };
            tri[k] = id;
        }
        edit.add_face(tri, sources).map_err(geometry)?;
    }
    edit.finish().map_err(geometry)
}

/// Stage 5: `validate_solid_with` around the created faces.
fn check(
    target: &Mesh,
    mesh: &Mesh,
    provenance: &Provenance,
    ctx: &RunContext,
) -> Result<(), BooleanError> {
    let created: Vec<FaceId> = provenance.created_faces.iter().map(|(f, _)| *f).collect();
    if mesh.face_count() == 0 {
        return Err(BooleanError::ResultEmpty);
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &f in &created {
        let Some(t) = mesh.face(f) else { continue };
        for v in t {
            let p = mesh.corner_position(v);
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
    }
    // Every face that could touch a created face.
    let slots = mesh.face_slots();
    let chunks: Vec<u32> = (0..slots.div_ceil(1 << 15)).collect();
    let near: Vec<FaceId> = chunks
        .par_iter()
        .map(|&c| -> Result<Vec<FaceId>, Cancelled> {
            ctx.checkpoint()?;
            Ok(((c << 15)..((c + 1) << 15).min(slots))
                .filter_map(|f| {
                    let t = mesh.face(FaceId(f))?;
                    let p = t.map(|v| mesh.corner_position(v));
                    let touches = (0..3).all(|a| {
                        p.iter().map(|q| q[a]).fold(f64::INFINITY, f64::min) <= hi[a]
                            && p.iter().map(|q| q[a]).fold(f64::NEG_INFINITY, f64::max) >= lo[a]
                    });
                    touches.then_some(FaceId(f))
                })
                .collect())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let bvh = Bvh::build_subset(mesh, &near, ctx)?;
    let original = target.vertex_slots();
    let allow_open = move |u: u32, v: u32| u < original && v < original;
    match mf_geometry::validate_solid_with(mesh, &created, &bvh, &allow_open, ctx)? {
        Ok(()) => Ok(()),
        Err(d @ SolidDefect::SelfIntersects { .. }) => {
            Err(BooleanError::ResultSelfIntersects(d.to_string()))
        }
        Err(SolidDefect::NonPositiveVolume) => Err(BooleanError::ResultEmpty),
        Err(d) => Err(BooleanError::ResultNotManifold(d.to_string())),
    }
}
