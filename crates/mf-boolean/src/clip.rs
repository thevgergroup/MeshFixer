//! The local window of D5: the target clipped to an axis-aligned box and
//! closed, so the kernel sees only what is near the tool.
//!
//! - **Which faces.** Faces whose bounds touch the box are clipped; the
//!   rest are kept as they are (bitwise, same ids). The box's planes are
//!   first moved outward (to the next `f64`) until no vertex of those faces
//!   lies exactly on one, so every original vertex is strictly inside or
//!   strictly outside.
//! - **Clipping.** Each face meeting the box is cut by the box's six planes
//!   (Sutherland–Hodgman), keeping the part inside, `Q`. A cut on an
//!   original edge is computed from that edge's two endpoints (ordered by
//!   position, not by id), with the plane's coordinate set exactly, so the
//!   two faces on an edge (and unwelded duplicates of it) get the same
//!   point. The part outside, `F \ Q`, is triangulated (D3) with only the
//!   points where the face's edges cross the box's surface, so a face that
//!   was kept never gets a T-junction. Classification against an
//!   axis-aligned plane is exact (a coordinate compared with a constant).
//! - **Closing it.** The inside part's open edges on the box's surface (the
//!   seam) are closed by caps: the box's faces are triangulated with the
//!   seam as constraints, and the triangles on the solid's side of the seam
//!   are the caps (found by walking the box's surface from the seam,
//!   flipping side at each seam edge). Openings of the target inside the
//!   box are closed by temporary flat fills (D15, joins only); an opening
//!   that crosses the box grows it to contain the opening's loop, up to the
//!   caller's limit. Caps and fills are labelled, so they are removed (or
//!   must have been removed by the kernel) after the boolean.
//! - **Weld.** Vertices at exactly the same position are one vertex in the
//!   window (the Task 8 amendment), so unwelded duplicates in a scan are
//!   neither openings nor self-intersections.

use std::collections::{HashMap, HashSet};

use mf_geometry::{Aabb, FaceId, Mesh};
use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use crate::kernel::KernelMesh;
use crate::section::{SectionError, triangulate_section, winding_faces};
use crate::validate::{drop_axis, o2_coords};
use crate::weld::key;

/// Where a window triangle came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFace {
    /// A face of the target, or a piece of one.
    Target(FaceId),
    /// A cap on the box's surface (temporary).
    Cap,
    /// A temporary fill of an opening inside the window (D15).
    Fill,
}

/// Why a window couldn't be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipError {
    #[error("cancelled by the user")]
    Cancelled,
    /// An opening crosses the window and its loop doesn't fit within the
    /// caller's limit (D15).
    #[error("an opening crosses the window and is larger than the limit")]
    OpeningTooLarge,
    /// An opening inside the window, while openings may not be closed
    /// (`close_openings` false).
    #[error("the target has an opening inside the window")]
    Open,
    /// The surface folds onto itself where the box cuts it, or an opening
    /// is too irregular to close flat.
    #[error("the section where the window cuts the target {0}")]
    Section(SectionError),
    /// No box near the requested one avoided touching the target's
    /// geometry exactly along an edge or at a corner.
    #[error("the window touches the target only along an edge or at a point")]
    Degenerate,
}

impl From<Cancelled> for ClipError {
    fn from(_: Cancelled) -> Self {
        ClipError::Cancelled
    }
}

impl From<SectionError> for ClipError {
    fn from(e: SectionError) -> Self {
        ClipError::Section(e)
    }
}

/// A point of the window: an original vertex, or a new point on the box's
/// surface (a cut, or a corner of the box).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Point {
    Original(u32),
    New(usize),
}

/// The target clipped to the window and closed (see the module docs).
#[derive(Debug, Clone)]
pub struct Clipped {
    /// The box actually used (moved off the target's vertices, grown for
    /// openings).
    pub window: Aabb,
    /// The closed window operand, welded exactly, faces labelled
    /// `(0, index)`.
    pub inside: KernelMesh,
    /// What each of `inside`'s triangles came from.
    pub inside_faces: Vec<WindowFace>,
    /// The new points (cuts and box corners), by `Point::New` index.
    pub new_points: Vec<[f64; 3]>,
    /// Positions of `inside`'s seam vertices (where it meets the outside),
    /// as `Point::New` indices.
    pub seam: Vec<usize>,
    /// Target faces that meet the window: replaced by their outside pieces
    /// and the kernel's result.
    pub cut: Vec<FaceId>,
    /// Original vertices strictly inside the window (only cut faces use
    /// them).
    pub inside_vertices: Vec<u32>,
    /// Triangles of the cut faces' parts outside the window, and the face
    /// each is part of.
    pub outside_pieces: Vec<([Point; 3], FaceId)>,
}

/// How [`clip_to_box`] treats the target's own openings.
#[derive(Debug, Clone, Copy)]
pub struct ClipOptions {
    /// Close openings inside the window with temporary fills (D15 joins);
    /// otherwise an opening there is [`ClipError::Open`].
    pub close_openings: bool,
    /// The window may grow to contain an opening's loop, up to this box.
    pub limit: Aabb,
}

#[derive(Clone, Copy)]
struct Planes {
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Planes {
    fn value(&self, k: usize) -> f64 {
        if k.is_multiple_of(2) {
            self.lo[k / 2]
        } else {
            self.hi[k / 2]
        }
    }

    /// +1 inside plane `k`'s half-space, 0 on it, -1 outside.
    fn side(&self, p: [f64; 3], k: usize) -> i8 {
        let (x, c) = (p[k / 2], self.value(k));
        let d = if k.is_multiple_of(2) {
            x.partial_cmp(&c)
        } else {
            c.partial_cmp(&x)
        };
        match d {
            Some(std::cmp::Ordering::Greater) => 1,
            Some(std::cmp::Ordering::Equal) => 0,
            _ => -1,
        }
    }

    fn strictly_inside(&self, p: [f64; 3]) -> bool {
        (0..6).all(|k| self.side(p, k) > 0)
    }

    fn on(&self, p: [f64; 3], k: usize) -> bool {
        p[k / 2] == self.value(k)
    }

    fn overlaps(&self, lo: [f64; 3], hi: [f64; 3]) -> bool {
        (0..3).all(|a| lo[a] <= self.hi[a] && self.lo[a] <= hi[a])
    }

    fn within(&self, limit: &Aabb) -> bool {
        (0..3).all(|a| self.lo[a] >= limit.min[a] && self.hi[a] <= limit.max[a])
    }
}

/// What a polygon edge lies on: an original edge (its endpoints), or a
/// box plane.
#[derive(Clone, Copy, Debug)]
enum Along {
    Edge(u32, u32),
    Plane,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Edge(u32, u32, u8),
    Corner(u8),
}

/// The window's points while clipping.
struct Points<'a> {
    mesh: &'a Mesh,
    new: Vec<[f64; 3]>,
    by_key: HashMap<Key, usize>,
    by_pos: HashMap<[u64; 3], usize>,
}

impl Points<'_> {
    fn pos(&self, p: Point) -> [f64; 3] {
        match p {
            Point::Original(v) => self.mesh.corner_position(v),
            Point::New(i) => self.new[i],
        }
    }

    /// A new point at `p` (one per position: the weld).
    fn add(&mut self, p: [f64; 3]) -> usize {
        if let Some(&i) = self.by_pos.get(&key(p)) {
            return i;
        }
        let i = self.new.len();
        self.new.push(p);
        self.by_pos.insert(key(p), i);
        i
    }

    fn keyed(&mut self, k: Key, p: impl FnOnce() -> [f64; 3]) -> usize {
        if let Some(&i) = self.by_key.get(&k) {
            return i;
        }
        let i = self.add(p());
        self.by_key.insert(k, i);
        i
    }
}

/// Where segment `p0 → p1` crosses plane `k` (the endpoints ordered by
/// position first, so the result doesn't depend on direction), with the
/// plane's coordinate exact.
fn cross(planes: &Planes, a: [f64; 3], b: [f64; 3], k: usize) -> [f64; 3] {
    let (p0, p1) = if lex_less(a, b) { (a, b) } else { (b, a) };
    let (ax, c) = (k / 2, planes.value(k));
    let t = (c - p0[ax]) / (p1[ax] - p0[ax]);
    let mut q = [0, 1, 2].map(|i| p0[i] + t * (p1[i] - p0[i]));
    q[ax] = c;
    q
}

fn lex_less(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter()
        .zip(&b)
        .map(|(x, y)| x.total_cmp(y))
        .find(|o| o.is_ne())
        == Some(std::cmp::Ordering::Less)
}

/// Clips `mesh` to `window` and closes the inside part (see the module
/// docs).
pub fn clip_to_box(
    mesh: &Mesh,
    window: Aabb,
    options: ClipOptions,
    ctx: &RunContext,
) -> Result<Clipped, ClipError> {
    let mut planes = Planes {
        lo: window.min,
        hi: window.max,
    };
    let mut nudges = 0;
    loop {
        ctx.checkpoint()?;
        let faces = select(mesh, &planes, ctx)?;
        if let Some(moved) = off_vertices(mesh, &faces, &planes) {
            planes = moved;
            continue;
        }
        match clip_once(mesh, &faces, &planes, options, ctx) {
            Ok(Attempt::Done(c)) => return Ok(*c),
            Ok(Attempt::Grow(to)) => {
                if !to.within(&options.limit) {
                    return Err(ClipError::OpeningTooLarge);
                }
                planes = to;
            }
            Err(ClipError::Degenerate) if nudges < 8 => {
                // Touching (almost) exactly along an edge or at a point:
                // move the planes out a little, by different amounts per
                // plane (a symmetric move keeps a symmetric model's
                // coincidence), and try again.
                nudges += 1;
                let size = (0..3)
                    .map(|a| planes.hi[a] - planes.lo[a])
                    .fold(0.0, f64::max);
                for (a, f) in [1.0, 1.7, 2.3].into_iter().enumerate() {
                    let step = size * 1e-6 * f * nudges as f64;
                    planes.lo[a] -= step * 0.737;
                    planes.hi[a] += step;
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Live faces whose bounds touch the box, in id order.
fn select(mesh: &Mesh, planes: &Planes, ctx: &RunContext) -> Result<Vec<FaceId>, ClipError> {
    let slots = mesh.face_slots();
    let chunks: Vec<u32> = (0..slots.div_ceil(1 << 15)).collect();
    let found: Vec<Vec<FaceId>> = chunks
        .par_iter()
        .map(|&c| -> Result<Vec<FaceId>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for f in (c << 15)..((c + 1) << 15).min(slots) {
                let Some(t) = mesh.face(FaceId(f)) else {
                    continue;
                };
                let p = t.map(|v| mesh.corner_position(v));
                let lo = [0, 1, 2].map(|a| p[0][a].min(p[1][a]).min(p[2][a]));
                let hi = [0, 1, 2].map(|a| p[0][a].max(p[1][a]).max(p[2][a]));
                if planes.overlaps(lo, hi) {
                    out.push(FaceId(f));
                }
            }
            Ok(out)
        })
        .collect::<Result<_, _>>()?;
    Ok(found.into_iter().flatten().collect())
}

/// The box moved outward until no vertex of `faces` lies on a plane, or
/// `None` if none does.
fn off_vertices(mesh: &Mesh, faces: &[FaceId], planes: &Planes) -> Option<Planes> {
    let mut moved = *planes;
    let mut any = false;
    for &f in faces {
        let Some(t) = mesh.face(f) else { continue };
        for v in t {
            let p = mesh.corner_position(v);
            for (a, &x) in p.iter().enumerate() {
                if x == moved.lo[a] {
                    moved.lo[a] = moved.lo[a].next_down();
                    any = true;
                }
                if x == moved.hi[a] {
                    moved.hi[a] = moved.hi[a].next_up();
                    any = true;
                }
            }
        }
    }
    any.then_some(moved)
}

enum Attempt {
    Done(Box<Clipped>),
    Grow(Planes),
}

/// One face's part inside the box (Sutherland–Hodgman over the six
/// planes), as points and what each edge (to the next point) lies on.
fn inside_polygon(points: &mut Points, planes: &Planes, t: [u32; 3]) -> Vec<(Point, Along)> {
    let mut poly: Vec<(Point, Along)> = (0..3)
        .map(|i| (Point::Original(t[i]), Along::Edge(t[i], t[(i + 1) % 3])))
        .collect();
    for k in 0..6 {
        if poly.is_empty() {
            break;
        }
        let side: Vec<i8> = poly
            .iter()
            .map(|&(p, _)| planes.side(points.pos(p), k))
            .collect();
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let j = (i + 1) % poly.len();
            let ((cur, along), (nxt, _)) = (poly[i], poly[j]);
            let (sc, sn) = (side[i], side[j]);
            let crossing = (sc > 0 && sn < 0) || (sc < 0 && sn > 0);
            let at = |points: &mut Points| -> Point {
                match along {
                    Along::Edge(u, v) => {
                        let (pu, pv) = (
                            points.mesh.corner_position(u),
                            points.mesh.corner_position(v),
                        );
                        Point::New(points.keyed(Key::Edge(u.min(v), u.max(v), k as u8), || {
                            cross(planes, pu, pv, k)
                        }))
                    }
                    Along::Plane => {
                        let (pc, pn) = (points.pos(cur), points.pos(nxt));
                        Point::New(points.add(cross(planes, pc, pn, k)))
                    }
                }
            };
            if sc >= 0 {
                if sn >= 0 {
                    out.push((cur, along));
                } else if sc > 0 {
                    out.push((cur, along));
                    let x = at(points);
                    out.push((x, Along::Plane));
                } else {
                    out.push((cur, Along::Plane));
                }
            } else if crossing {
                let x = at(points);
                out.push((x, along));
            }
        }
        // Consecutive repeats (a point on the plane emitted twice).
        out.dedup_by(|a, b| a.0 == b.0);
        while out.len() > 1 && out.first().map(|p| p.0) == out.last().map(|p| p.0) {
            out.pop();
        }
        poly = out;
    }
    poly
}

/// The 2D coordinates of `p` for a face whose projection drops axis `k`,
/// mirrored if `flip` (so the face runs counter-clockwise).
fn flat(p: [f64; 3], k: usize, flip: bool) -> [f64; 2] {
    let (x, y) = match k {
        0 => (p[1], p[2]),
        1 => (p[2], p[0]),
        _ => (p[0], p[1]),
    };
    if flip { [y, x] } else { [x, y] }
}

fn clip_once(
    mesh: &Mesh,
    faces: &[FaceId],
    planes: &Planes,
    options: ClipOptions,
    ctx: &RunContext,
) -> Result<Attempt, ClipError> {
    let mut points = Points {
        mesh,
        new: Vec::new(),
        by_key: HashMap::new(),
        by_pos: HashMap::new(),
    };
    let mut inside_tris: Vec<([Point; 3], FaceId)> = Vec::new();
    let mut outside_pieces = Vec::new();
    let mut cut = Vec::new();
    let mut inside_vertices = Vec::new();
    let mut ticker = ctx.ticker();
    for &f in faces {
        ticker.tick()?;
        let Some(t) = mesh.face(f) else { continue };
        let c = t.map(|v| mesh.corner_position(v));
        let inside = c.map(|p| planes.strictly_inside(p));
        if inside.iter().all(|&i| i) {
            inside_tris.push((t.map(Point::Original), f));
            cut.push(f);
            inside_vertices.extend(t);
            continue;
        }
        let q = inside_polygon(&mut points, planes, t);
        if q.is_empty() {
            continue; // bounds touch the box, the face doesn't
        }
        let Some(k) = drop_axis(c[0], c[1], c[2]) else {
            // A face with no area crossing the box's surface.
            return Err(ClipError::Section(SectionError::Degenerate));
        };
        if q.len() < 3 {
            return Err(ClipError::Degenerate);
        }
        cut.push(f);
        inside_vertices.extend(t.iter().zip(&inside).filter(|p| *p.1).map(|p| *p.0));
        let flip = o2_coords(
            flat(c[0], k, false),
            flat(c[1], k, false),
            flat(c[2], k, false),
        ) < 0.0;
        // Local points: the corners, then q's new points.
        let mut local: Vec<Point> = t.map(Point::Original).to_vec();
        for &(p, _) in &q {
            if !local.contains(&p) {
                local.push(p);
            }
        }
        let index = |p: Point, local: &[Point]| local.iter().position(|&x| x == p).unwrap_or(0); // allow-swallow: every point was just added
        let coords: Vec<[f64; 2]> = local
            .iter()
            .map(|&p| flat(points.pos(p), k, flip))
            .collect();
        let mut edges = Vec::new();
        // The face's outline, through q's points on each edge.
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            let (pu, pv) = (c[i], c[(i + 1) % 3]);
            let dir = [0, 1, 2].map(|a| pv[a] - pu[a]);
            let along = |p: [f64; 3]| (0..3).map(|a| (p[a] - pu[a]) * dir[a]).sum::<f64>();
            let mut on: Vec<(f64, Point)> = q
                .iter()
                .filter_map(|&(p, _)| match p {
                    Point::New(i)
                        if (0..6).any(|k| {
                            points.by_key.get(&Key::Edge(u.min(v), u.max(v), k as u8)) == Some(&i)
                        }) =>
                    {
                        Some((along(points.pos(p)), p))
                    }
                    _ => None,
                })
                .collect();
            on.sort_by(|a, b| a.0.total_cmp(&b.0));
            on.dedup_by(|a, b| a.1 == b.1);
            let mut chain = vec![Point::Original(u)];
            chain.extend(on.iter().map(|x| x.1));
            chain.push(Point::Original(v));
            for w in chain.windows(2) {
                edges.push((index(w[0], &local), index(w[1], &local)));
            }
        }
        for i in 0..q.len() {
            edges.push((index(q[i].0, &local), index(q[(i + 1) % q.len()].0, &local)));
        }
        for (tri, w) in winding_faces(&coords, &edges).map_err(|(e, _)| e)? {
            let tri = tri.map(|i| local[i]);
            match w {
                2 => inside_tris.push((tri, f)),
                1 => outside_pieces.push((tri, f)),
                0 => {}
                _ => return Err(ClipError::Section(SectionError::SelfIntersects)),
            }
        }
    }
    inside_vertices.sort_unstable();
    inside_vertices.dedup();
    ctx.checkpoint()?;

    // The window operand, welded: original vertices by position too.
    let mut weld: HashMap<[u64; 3], Point> = HashMap::new();
    let mut canon = |p: Point, points: &Points| -> Point {
        match p {
            Point::New(_) => p,
            Point::Original(_) => *weld.entry(key(points.pos(p))).or_insert(p),
        }
    };
    let tris: Vec<([Point; 3], FaceId)> = inside_tris
        .iter()
        .map(|&(t, f)| (t.map(|p| canon(p, &points)), f))
        .collect();
    // Open edges of the inside part.
    let mut directed: HashSet<(Point, Point)> = HashSet::new();
    for (t, _) in &tris {
        for i in 0..3 {
            directed.insert((t[i], t[(i + 1) % 3]));
        }
    }
    let mut open: Vec<(Point, Point)> = Vec::new();
    for (t, _) in &tris {
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            if !directed.contains(&(v, u)) {
                open.push((u, v));
            }
        }
    }
    let on_plane = |p: Point, points: &Points| -> Vec<usize> {
        match p {
            Point::Original(_) => Vec::new(),
            Point::New(_) => (0..6).filter(|&k| planes.on(points.pos(p), k)).collect(),
        }
    };
    let seam_plane = |(u, v): (Point, Point), points: &Points| -> Option<Vec<usize>> {
        let (a, b) = (on_plane(u, points), on_plane(v, points));
        let common: Vec<usize> = a.into_iter().filter(|k| b.contains(k)).collect();
        (!common.is_empty()).then_some(common)
    };
    let mut seam_edges: Vec<((Point, Point), usize)> = Vec::new();
    let mut holes: Vec<(Point, Point)> = Vec::new();
    for &e in &open {
        match seam_plane(e, &points) {
            Some(k) if k.len() == 1 => seam_edges.push((e, k[0])),
            // Along an edge of the box itself.
            Some(_) => return Err(ClipError::Degenerate),
            None => holes.push(e),
        }
    }

    // Openings inside the window (D15).
    let mut fills: Vec<[Point; 3]> = Vec::new();
    if !holes.is_empty() {
        if !options.close_openings {
            return Err(ClipError::Open);
        }
        let mut next: HashMap<Point, Vec<Point>> = HashMap::new();
        for &(u, v) in &open {
            next.entry(u).or_default().push(v);
        }
        let seam_set: HashSet<(Point, Point)> = seam_edges.iter().map(|x| x.0).collect();
        let mut done: HashSet<(Point, Point)> = HashSet::new();
        let mut crossing: Vec<u32> = Vec::new();
        let mut loops: Vec<Vec<Point>> = Vec::new();
        for &start in &holes {
            if done.contains(&start) {
                continue;
            }
            let mut walk = vec![start.0];
            let mut e = start;
            let mut touches_seam = false;
            loop {
                done.insert(e);
                if seam_set.contains(&e) {
                    touches_seam = true;
                }
                let outs = next.get(&e.1).map(Vec::as_slice).unwrap_or(&[]); // allow-swallow: an open edge always has an end; none means a broken loop, handled below
                if outs.len() != 1 {
                    // A vertex where openings meet: not one loop.
                    return Err(ClipError::Section(SectionError::SelfIntersects));
                }
                e = (e.1, outs[0]);
                if e == start {
                    break;
                }
                walk.push(e.0);
                if walk.len() > open.len() {
                    return Err(ClipError::Section(SectionError::SelfIntersects));
                }
            }
            if touches_seam {
                crossing.extend(walk.iter().filter_map(|p| match p {
                    Point::Original(v) => Some(*v),
                    Point::New(_) => None,
                }));
            } else {
                loops.push(walk);
            }
        }
        if !crossing.is_empty() {
            return Ok(Attempt::Grow(grown_for(mesh, planes, &crossing, ctx)?));
        }
        for l in loops {
            // The fill runs the loop backwards (it closes the opening).
            let ring: Vec<Point> = l.into_iter().rev().collect();
            let pos: Vec<[f64; 3]> = ring.iter().map(|&p| points.pos(p)).collect();
            let flat2 = project_loop(&pos);
            for t in triangulate_section(&[flat2])? {
                fills.push(t.map(|i| ring[i as usize]));
            }
        }
    }

    // A new point (almost) on top of another point of the window: a mesh
    // edge passing within rounding of the box's edge, or a vertex within
    // rounding of a plane. The kernel would merge them and the seam
    // wouldn't match: move the box instead.
    if crowded(&tris, &points, planes) {
        return Err(ClipError::Degenerate);
    }

    // Caps on the box's surface.
    let caps = box_caps(&mut points, planes, &seam_edges)?;
    ctx.checkpoint()?;

    // Assemble the operand: inside triangles, caps, fills.
    let mut index: HashMap<Point, u32> = HashMap::new();
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let mut inside_faces = Vec::new();
    let mut emit = |t: [Point; 3], src: WindowFace, points: &Points| {
        let tri = t.map(|p| {
            *index.entry(p).or_insert_with(|| {
                positions.push(points.pos(p));
                (positions.len() - 1) as u32
            })
        });
        triangles.push(tri);
        inside_faces.push(src);
    };
    for &(t, f) in &tris {
        emit(t, WindowFace::Target(f), &points);
    }
    for &t in &caps {
        emit(t, WindowFace::Cap, &points);
    }
    for &t in &fills {
        emit(t, WindowFace::Fill, &points);
    }
    let mut seam: Vec<usize> = seam_edges
        .iter()
        .flat_map(|((u, v), _)| [*u, *v])
        .filter_map(|p| match p {
            Point::New(i) => Some(i),
            Point::Original(_) => None,
        })
        .collect();
    seam.sort_unstable();
    seam.dedup();
    let inside = KernelMesh::labelled(positions, triangles, 0);
    Ok(Attempt::Done(Box::new(Clipped {
        window: Aabb {
            min: planes.lo,
            max: planes.hi,
        },
        inside,
        inside_faces,
        new_points: points.new,
        seam,
        cut,
        inside_vertices,
        outside_pieces,
    })))
}

/// Whether two distinct points of the window's triangles, at least one of
/// them new, lie within a billionth of the box's size of each other.
fn crowded(tris: &[([Point; 3], FaceId)], points: &Points, planes: &Planes) -> bool {
    let size = (0..3)
        .map(|a| planes.hi[a] - planes.lo[a])
        .fold(0.0, f64::max);
    let eps = size * 1e-9;
    let mut all: Vec<Point> = tris.iter().flat_map(|(t, _)| *t).collect();
    all.sort_unstable_by_key(|p| match p {
        Point::Original(v) => (0, *v as usize),
        Point::New(i) => (1, *i),
    });
    all.dedup();
    // Cells of side 2·eps: points within eps share a cell or neighbour one.
    let cell = |p: [f64; 3]| p.map(|x| (x / (2.0 * eps)).floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<Point>> = HashMap::new();
    for &p in &all {
        grid.entry(cell(points.pos(p))).or_default().push(p);
    }
    for &p in &all {
        if !matches!(p, Point::New(_)) {
            continue;
        }
        let (pp, c) = (points.pos(p), cell(points.pos(p)));
        for d in 0..27 {
            let n = [c[0] + d % 3 - 1, c[1] + d / 3 % 3 - 1, c[2] + d / 9 - 1];
            for &q in grid.get(&n).into_iter().flatten() {
                let qp = points.pos(q);
                if q != p && (0..3).all(|a| (pp[a] - qp[a]).abs() <= eps) {
                    return true;
                }
            }
        }
    }
    false
}

/// The box grown to contain the boundary loops through `vertices`, with
/// the same margin the box had over them.
fn grown_for(
    mesh: &Mesh,
    planes: &Planes,
    vertices: &[u32],
    ctx: &RunContext,
) -> Result<Planes, ClipError> {
    let topo = mesh.topology_with(ctx)?;
    let wanted: HashSet<u32> = vertices.iter().copied().collect();
    let mut out = *planes;
    let mut found = false;
    for l in &topo.boundary_loops {
        if !l.vertices.iter().any(|v| wanted.contains(v)) {
            continue;
        }
        found = true;
        for &v in &l.vertices {
            let p = mesh.corner_position(v);
            for (a, &x) in p.iter().enumerate() {
                out.lo[a] = out.lo[a].min(x);
                out.hi[a] = out.hi[a].max(x);
            }
        }
    }
    if !found {
        // The opening's boundary isn't a traced loop (a non-manifold
        // boundary vertex): it can't be closed for the join.
        return Err(ClipError::OpeningTooLarge);
    }
    for a in 0..3 {
        let pad = (out.hi[a] - out.lo[a]) * 0.02 + (planes.hi[a] - planes.lo[a]) * 0.01;
        out.lo[a] -= pad;
        out.hi[a] += pad;
    }
    Ok(out)
}

/// A closed 3D loop flattened onto its best-fit plane (Newell's normal),
/// counter-clockwise seen from that normal.
fn project_loop(pos: &[[f64; 3]]) -> Vec<[f64; 2]> {
    let mut n = [0.0f64; 3];
    let c = pos.iter().fold([0.0; 3], |acc, p| {
        [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]]
    });
    let c = c.map(|x| x / pos.len() as f64);
    for i in 0..pos.len() {
        let (p, q) = (pos[i], pos[(i + 1) % pos.len()]);
        n[0] += (p[1] - q[1]) * (p[2] + q[2]);
        n[1] += (p[2] - q[2]) * (p[0] + q[0]);
        n[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2])
        .sqrt()
        .max(f64::MIN_POSITIVE);
    let n = n.map(|x| x / len);
    // e1 ⟂ n, e2 = n × e1 (so e1 × e2 = n).
    let helper = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let d = helper[0] * n[0] + helper[1] * n[1] + helper[2] * n[2];
    let e1 = [
        helper[0] - d * n[0],
        helper[1] - d * n[1],
        helper[2] - d * n[2],
    ];
    let l1 = (e1[0] * e1[0] + e1[1] * e1[1] + e1[2] * e1[2]).sqrt();
    let e1 = e1.map(|x| x / l1);
    let e2 = [
        n[1] * e1[2] - n[2] * e1[1],
        n[2] * e1[0] - n[0] * e1[2],
        n[0] * e1[1] - n[1] * e1[0],
    ];
    pos.iter()
        .map(|p| {
            let r = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
            [
                r[0] * e1[0] + r[1] * e1[1] + r[2] * e1[2],
                r[0] * e2[0] + r[1] * e2[1] + r[2] * e2[2],
            ]
        })
        .collect()
}

/// The box face `k`'s 2D coordinates, counter-clockwise seen from outside
/// the box.
fn box_flat(p: [f64; 3], k: usize) -> [f64; 2] {
    let hi = k % 2 == 1;
    match (k / 2, hi) {
        (0, true) => [p[1], p[2]],
        (0, false) => [p[2], p[1]],
        (1, true) => [p[2], p[0]],
        (1, false) => [p[0], p[2]],
        (_, true) => [p[0], p[1]],
        (_, false) => [p[1], p[0]],
    }
}

/// The caps: the parts of the box's surface inside the target, oriented
/// outward (see the module docs). `seam` holds the inside part's open
/// edges on the box (as the inside part runs them) and their plane.
fn box_caps(
    points: &mut Points,
    planes: &Planes,
    seam: &[((Point, Point), usize)],
) -> Result<Vec<[Point; 3]>, ClipError> {
    if seam.is_empty() {
        return Ok(Vec::new());
    }
    // Directed cap edges (the seam run backwards): the cap is on their left.
    let cap_edges: HashSet<(Point, Point)> = seam.iter().map(|&((u, v), _)| (v, u)).collect();
    let undirected =
        |a: Point, b: Point| cap_edges.contains(&(a, b)) || cap_edges.contains(&(b, a));
    let corner = |c: u8| -> [f64; 3] {
        [0, 1, 2].map(|a| {
            if c & (1 << a) != 0 {
                planes.hi[a]
            } else {
                planes.lo[a]
            }
        })
    };
    let seam_points: Vec<Point> = {
        let mut v: Vec<Point> = seam.iter().flat_map(|&((u, w), _)| [u, w]).collect();
        v.sort_by_key(|p| match p {
            Point::New(i) => *i,
            Point::Original(v) => *v as usize,
        });
        v.dedup();
        v
    };
    // Every triangle of every box face: its points, counter-clockwise from
    // outside.
    let mut nodes: Vec<[Point; 3]> = Vec::new();
    for k in 0..6 {
        let mut local: Vec<Point> = Vec::new();
        for c in 0..8u8 {
            let p = corner(c);
            if planes.on(p, k) {
                local.push(Point::New(points.keyed(Key::Corner(c), || p)));
            }
        }
        for &p in &seam_points {
            if planes.on(points.pos(p), k) && !local.contains(&p) {
                local.push(p);
            }
        }
        let mut cdt = ConstrainedDelaunayTriangulation::<Point2<f64>>::new();
        let mut owner: HashMap<usize, Point> = HashMap::new();
        let mut handle = HashMap::new();
        for &p in &local {
            let f = box_flat(points.pos(p), k);
            let h = cdt
                .insert(Point2::new(f[0], f[1]))
                .map_err(|_| ClipError::Section(SectionError::Degenerate))?;
            if owner.insert(h.index(), p).is_some() {
                return Err(ClipError::Degenerate);
            }
            handle.insert(p, h);
        }
        for &((u, v), plane) in seam {
            if plane != k {
                continue;
            }
            let (hu, hv) = (handle[&u], handle[&v]);
            if !cdt.can_add_constraint(hu, hv) {
                return Err(ClipError::Section(SectionError::SelfIntersects));
            }
            cdt.add_constraint(hu, hv);
            if cdt.get_edge_from_neighbors(hu, hv).is_none() {
                return Err(ClipError::Section(SectionError::SelfIntersects));
            }
        }
        for face in cdt.inner_faces() {
            nodes.push(face.vertices().map(|v| owner[&v.fix().index()]));
        }
    }
    // Walk the box's surface: across a seam edge the side flips.
    let mut by_edge: HashMap<(Point, Point), usize> = HashMap::new();
    for (i, t) in nodes.iter().enumerate() {
        for j in 0..3 {
            if by_edge.insert((t[j], t[(j + 1) % 3]), i).is_some() {
                return Err(ClipError::Degenerate);
            }
        }
    }
    let mut label: Vec<Option<bool>> = vec![None; nodes.len()];
    let mut queue = Vec::new();
    for (i, t) in nodes.iter().enumerate() {
        for j in 0..3 {
            let e = (t[j], t[(j + 1) % 3]);
            let seed = if cap_edges.contains(&e) {
                Some(true)
            } else if cap_edges.contains(&(e.1, e.0)) {
                Some(false)
            } else {
                None
            };
            if let Some(s) = seed {
                match label[i] {
                    Some(old) if old != s => {
                        return Err(ClipError::Section(SectionError::SelfIntersects));
                    }
                    Some(_) => {}
                    None => {
                        label[i] = Some(s);
                        queue.push(i);
                    }
                }
            }
        }
    }
    while let Some(i) = queue.pop() {
        let Some(l) = label[i] else { continue };
        let t = nodes[i];
        for j in 0..3 {
            let (a, b) = (t[j], t[(j + 1) % 3]);
            let Some(&o) = by_edge.get(&(b, a)) else {
                // The box's surface didn't close up along its edges.
                return Err(ClipError::Degenerate);
            };
            let lo = if undirected(a, b) { !l } else { l };
            match label[o] {
                Some(old) if old != lo => {
                    return Err(ClipError::Section(SectionError::SelfIntersects));
                }
                Some(_) => {}
                None => {
                    label[o] = Some(lo);
                    queue.push(o);
                }
            }
        }
    }
    let mut caps = Vec::new();
    for (i, t) in nodes.iter().enumerate() {
        match label[i] {
            Some(true) => caps.push(*t),
            Some(false) => {}
            None => return Err(ClipError::Degenerate),
        }
    }
    Ok(caps)
}
