//! A bounding volume hierarchy over a mesh revision's live faces, for
//! picking at interactive rates (MF-RENDER-004: a face pick at 5M
//! triangles in < 16 ms).
//!
//! Binned-SAH build (16 bins on each axis), leaves of at most
//! [`LEAF_MAX`] faces, nodes in depth-first order (a node's left child
//! follows it). Node bounds are `f32` relative to the mesh's bounding-box
//! centre, rounded outward and padded, so a box never cuts off a face the
//! exact `f64` triangle test would hit; the triangle test itself is the
//! same `f64` Möller–Trumbore code as the brute-force cast in `pick.rs`,
//! so both find the same face at the same distance (ties: the lower face
//! ID). 32 bytes per node plus 4 per face: about 18 bytes per face at 5M
//! triangles (≤ 64 B/face is the budget). Building also holds 28 bytes
//! per face of scratch until it returns.
//!
//! The build is deterministic (a pure function of the mesh, however rayon
//! splits the work: subtrees are joined in a fixed order) and cancellable
//! (it checkpoints at least every [`TICK_INTERVAL`] items of work). Its
//! partitions are in place, so it needs no sort.

use glam::{DMat4, DVec3, DVec4};
use mf_geometry::{FaceId, Mesh};
use mf_runtime::{Cancelled, RunContext, TICK_INTERVAL};

use crate::pick::{HIT_MIN, intersect};

/// Leaves hold at most this many faces.
pub const LEAF_MAX: usize = 8;
/// SAH bins per axis.
const BINS: usize = 16;
/// Cost of visiting a node, relative to one triangle test. Measured at
/// 5M triangles, 3 (rather than the textbook 1) halves the node count, to
/// about 18 bytes per face, with picks still in microseconds.
const TRAVERSAL_COST: f32 = 3.0;
/// Below this depth splits are SAH; from it on, halves (see [`split`]).
const SAH_DEPTH: u32 = 48;
/// Subtrees with more items than this are built on another rayon thread.
const PARALLEL_MIN: usize = 1 << 15;

/// One node: its bounds (relative to [`Bvh`]'s origin) and either a leaf's
/// faces (`count > 0`: `faces[first..first + count]`) or, for an interior
/// node (`count == 0`), its right child's index (the left child is the
/// next node).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Node {
    min: [f32; 3],
    max: [f32; 3],
    first_or_right: u32,
    count: u32,
}

/// A face's bounds (relative to the origin, rounded outward and padded)
/// while building.
#[derive(Clone, Copy)]
struct Item {
    min: [f32; 3],
    max: [f32; 3],
    face: u32,
}

impl Item {
    fn centroid(&self, axis: usize) -> f32 {
        (self.min[axis] + self.max[axis]) * 0.5
    }
}

/// A BVH over one mesh revision's live faces (see the module docs). It
/// stores face IDs, not geometry: query it with the mesh it was built
/// for.
#[derive(Clone, PartialEq)]
pub struct Bvh {
    /// Object-space point the node bounds are relative to.
    origin: DVec3,
    nodes: Vec<Node>,
    /// Face IDs in leaf order.
    faces: Vec<u32>,
    /// How far a box is padded beyond its faces (object units), also the
    /// slack allowed when comparing a box's entry distance with a hit.
    pad: f64,
}

impl std::fmt::Debug for Bvh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bvh")
            .field("origin", &self.origin)
            .field("nodes", &self.nodes.len())
            .field("faces", &self.faces.len())
            .finish()
    }
}

/// `x` as an `f32` no greater than it.
fn down(x: f64) -> f32 {
    let f = x as f32;
    if f64::from(f) > x { f.next_down() } else { f }
}

/// `x` as an `f32` no less than it.
fn up(x: f64) -> f32 {
    let f = x as f32;
    if f64::from(f) < x { f.next_up() } else { f }
}

fn area(min: &[f32; 3], max: &[f32; 3]) -> f32 {
    let d = [0, 1, 2].map(|k| (max[k] - min[k]).max(0.0));
    d[0] * d[1] + d[1] * d[2] + d[2] * d[0]
}

/// Bounds of a run of items, and of their centroids.
struct Extent {
    min: [f32; 3],
    max: [f32; 3],
    cmin: [f32; 3],
    cmax: [f32; 3],
}

impl Extent {
    fn of(items: &[Item]) -> Extent {
        let mut e = Extent {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            cmin: [f32::INFINITY; 3],
            cmax: [f32::NEG_INFINITY; 3],
        };
        for it in items {
            for k in 0..3 {
                e.min[k] = e.min[k].min(it.min[k]);
                e.max[k] = e.max[k].max(it.max[k]);
                let c = it.centroid(k);
                e.cmin[k] = e.cmin[k].min(c);
                e.cmax[k] = e.cmax[k].max(c);
            }
        }
        e
    }
}

/// Which of [`BINS`] bins along an axis a centroid falls in.
#[derive(Clone, Copy)]
struct Binning {
    axis: usize,
    lo: f32,
    scale: f32,
}

impl Binning {
    fn bin(&self, it: &Item) -> usize {
        let b = ((it.centroid(self.axis) - self.lo) * self.scale) as usize;
        b.min(BINS - 1)
    }
}

/// Counts work done and checkpoints every [`TICK_INTERVAL`] items.
struct Work<'a> {
    ctx: &'a RunContext,
    done: usize,
}

impl Work<'_> {
    fn add(&mut self, items: usize) -> Result<(), Cancelled> {
        self.done += items;
        if self.done >= TICK_INTERVAL as usize {
            self.done = 0;
            self.ctx.checkpoint()?;
        }
        Ok(())
    }
}

/// The best SAH split of `items` (whose bounds are `e`), as a binning and
/// the last bin that goes left, if any split has items on both sides.
fn best_split(
    items: &[Item],
    e: &Extent,
    work: &mut Work<'_>,
) -> Result<Option<(Binning, usize, f32)>, Cancelled> {
    let mut best: Option<(Binning, usize, f32)> = None;
    for axis in 0..3 {
        let extent = e.cmax[axis] - e.cmin[axis];
        // (Also skips a NaN extent, from non-finite positions.)
        if extent.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            continue;
        }
        let binning = Binning {
            axis,
            lo: e.cmin[axis],
            scale: BINS as f32 / extent,
        };
        let mut count = [0usize; BINS];
        let mut bmin = [[f32::INFINITY; 3]; BINS];
        let mut bmax = [[f32::NEG_INFINITY; 3]; BINS];
        for chunk in items.chunks(TICK_INTERVAL as usize) {
            for it in chunk {
                let b = binning.bin(it);
                count[b] += 1;
                for k in 0..3 {
                    bmin[b][k] = bmin[b][k].min(it.min[k]);
                    bmax[b][k] = bmax[b][k].max(it.max[k]);
                }
            }
            work.add(chunk.len())?;
        }
        // Areas and counts left of each split (after bin i), then right.
        let mut left = [(0.0f32, 0usize); BINS];
        let (mut lo, mut hi, mut n) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3], 0);
        for i in 0..BINS {
            for k in 0..3 {
                lo[k] = lo[k].min(bmin[i][k]);
                hi[k] = hi[k].max(bmax[i][k]);
            }
            n += count[i];
            left[i] = (if n > 0 { area(&lo, &hi) } else { 0.0 }, n);
        }
        let (mut lo, mut hi, mut n) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3], 0);
        for i in (1..BINS).rev() {
            for k in 0..3 {
                lo[k] = lo[k].min(bmin[i][k]);
                hi[k] = hi[k].max(bmax[i][k]);
            }
            n += count[i];
            let (left_area, left_n) = left[i - 1];
            if n == 0 || left_n == 0 {
                continue;
            }
            let cost = left_area * left_n as f32 + area(&lo, &hi) * n as f32;
            // Ties keep the split found first: deterministic.
            if best.is_none_or(|(.., c)| cost < c) {
                best = Some((binning, i - 1, cost));
            }
        }
    }
    Ok(best)
}

/// Moves the items in bins `..=last` to the front; returns how many.
fn partition(
    items: &mut [Item],
    binning: Binning,
    last: usize,
    work: &mut Work<'_>,
) -> Result<usize, Cancelled> {
    let (mut i, mut j) = (0, items.len());
    let mut since = 0usize;
    while i < j {
        if binning.bin(&items[i]) <= last {
            i += 1;
        } else {
            j -= 1;
            items.swap(i, j);
        }
        since += 1;
        if since == TICK_INTERVAL as usize {
            work.add(since)?;
            since = 0;
        }
    }
    work.add(since)?;
    Ok(i)
}

/// How a node's items are divided: as a leaf, or into two halves at `mid`.
enum Split {
    Leaf,
    At(usize),
}

fn split(
    items: &mut [Item],
    e: &Extent,
    depth: u32,
    work: &mut Work<'_>,
) -> Result<Split, Cancelled> {
    let k = items.len();
    if k <= 1 {
        return Ok(Split::Leaf);
    }
    if depth >= SAH_DEPTH {
        // Lopsided splits this deep: halve, so the depth stays bounded
        // (and the build's recursion with it).
        return Ok(if k <= LEAF_MAX {
            Split::Leaf
        } else {
            Split::At(k / 2)
        });
    }
    match best_split(items, e, work)? {
        Some((binning, last, cost)) => {
            let parent = area(&e.min, &e.max);
            let split_cost = if parent > 0.0 {
                TRAVERSAL_COST + cost / parent
            } else {
                f32::INFINITY
            };
            if k <= LEAF_MAX && k as f32 <= split_cost {
                return Ok(Split::Leaf);
            }
            Ok(Split::At(partition(items, binning, last, work)?))
        }
        // Every centroid in one place: halve by position.
        None if k <= LEAF_MAX => Ok(Split::Leaf),
        None => Ok(Split::At(k / 2)),
    }
}

/// The subtree over `items` (which start at `first` in the leaf order),
/// nodes indexed from 0 (its root).
fn build_subtree(
    items: &mut [Item],
    first: u32,
    depth: u32,
    ctx: &RunContext,
) -> Result<Vec<Node>, Cancelled> {
    if items.len() <= PARALLEL_MIN {
        // About 0.44 nodes per face are made (leaves average ~4.5 faces):
        // half a node per face rarely needs to grow.
        let mut nodes = Vec::with_capacity(items.len() / 2 + 1);
        let mut work = Work { ctx, done: 0 };
        build_into(items, first, depth, &mut nodes, &mut work)?;
        ctx.checkpoint()?;
        return Ok(nodes);
    }
    let mut work = Work { ctx, done: 0 };
    let e = Extent::of(items);
    work.add(items.len())?;
    let mid = match split(items, &e, depth, &mut work)? {
        Split::At(mid) => mid,
        Split::Leaf => unreachable!("only small runs become leaves"),
    };
    let (left_items, right_items) = items.split_at_mut(mid);
    let (left, right) = rayon::join(
        || build_subtree(left_items, first, depth + 1, ctx),
        || build_subtree(right_items, first + mid as u32, depth + 1, ctx),
    );
    let (mut nodes, right) = (left?, right?);
    // Merged into the left subtree's buffer (no third copy): its interior
    // nodes move down one for the root, the right subtree follows.
    for n in &mut nodes {
        if n.count == 0 {
            n.first_or_right += 1;
        }
    }
    let right_at = 1 + nodes.len() as u32;
    nodes.reserve_exact(1 + right.len());
    nodes.insert(
        0,
        Node {
            min: e.min,
            max: e.max,
            first_or_right: right_at,
            count: 0,
        },
    );
    ctx.checkpoint()?;
    nodes.extend(right.iter().map(|n| Node {
        first_or_right: if n.count == 0 {
            n.first_or_right + right_at
        } else {
            n.first_or_right
        },
        ..*n
    }));
    drop(right);
    ctx.checkpoint()?;
    Ok(nodes)
}

fn build_into(
    items: &mut [Item],
    first: u32,
    depth: u32,
    nodes: &mut Vec<Node>,
    work: &mut Work<'_>,
) -> Result<(), Cancelled> {
    let e = Extent::of(items);
    work.add(items.len())?;
    let at = nodes.len();
    nodes.push(Node {
        min: e.min,
        max: e.max,
        first_or_right: first,
        count: items.len() as u32,
    });
    let Split::At(mid) = split(items, &e, depth, work)? else {
        return Ok(());
    };
    nodes[at].count = 0;
    let (left, right) = items.split_at_mut(mid);
    build_into(left, first, depth + 1, nodes, work)?;
    nodes[at].first_or_right = nodes.len() as u32;
    build_into(right, first + mid as u32, depth + 1, nodes, work)
}

/// A ray in the BVH's frame: relative to its origin, unit direction.
struct Ray {
    /// Object space (for the exact triangle test).
    o: DVec3,
    d: DVec3,
    /// Relative to the BVH origin (for the box test).
    rel: DVec3,
    inv: DVec3,
}

impl Ray {
    /// Distance along the ray at which it enters the box, if it does
    /// before `limit`.
    fn enters(&self, n: &Node, pad: f64, limit: f64) -> Option<f64> {
        let (mut t0, mut t1) = (0.0f64, limit);
        for k in 0..3 {
            let (lo, hi) = (f64::from(n.min[k]) - pad, f64::from(n.max[k]) + pad);
            if self.d[k] == 0.0 {
                if self.rel[k] < lo || self.rel[k] > hi {
                    return None;
                }
                continue;
            }
            let (a, b) = (
                (lo - self.rel[k]) * self.inv[k],
                (hi - self.rel[k]) * self.inv[k],
            );
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }
}

impl Bvh {
    /// Build over `mesh`'s live faces. O(n log n), in parallel on the
    /// current rayon pool; returns [`Cancelled`] (having built nothing)
    /// once `ctx` is cancelled. Deterministic.
    pub fn build(mesh: &Mesh, ctx: &RunContext) -> Result<Bvh, Cancelled> {
        ctx.checkpoint()?;
        let Some(bb) = mesh.bounding_box_with(ctx)? else {
            return Ok(Bvh {
                origin: DVec3::ZERO,
                nodes: Vec::new(),
                faces: Vec::new(),
                pad: 0.0,
            });
        };
        let origin = DVec3::from(bb.center());
        let reach = DVec3::from(bb.max)
            .abs()
            .max(DVec3::from(bb.min).abs())
            .max_element();
        // Covers f64 rounding of positions relative to the origin (and of
        // the ray's), many times over; far below anything visible.
        let pad = (bb.diagonal() + reach) * 1e-9;
        let mut items = Vec::with_capacity(mesh.face_count());
        let mut ticker = ctx.ticker();
        for (id, t) in mesh.faces() {
            ticker.tick()?;
            let p = t.map(|v| DVec3::from(mesh.corner_position(v)) - origin);
            let (lo, hi) = (
                p[0].min(p[1]).min(p[2]) - pad,
                p[0].max(p[1]).max(p[2]) + pad,
            );
            items.push(Item {
                min: [down(lo.x), down(lo.y), down(lo.z)],
                max: [up(hi.x), up(hi.y), up(hi.z)],
                face: id.0,
            });
        }
        let nodes = build_subtree(&mut items, 0, 0, ctx)?;
        let mut faces = Vec::with_capacity(items.len());
        for chunk in items.chunks(TICK_INTERVAL as usize) {
            ctx.checkpoint()?;
            faces.extend(chunk.iter().map(|it| it.face));
        }
        drop(items);
        let mut nodes = nodes;
        nodes.shrink_to_fit();
        Ok(Bvh {
            origin,
            nodes,
            faces,
            pad,
        })
    }

    /// Nodes in the tree.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Faces it was built over (the mesh's live faces then).
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Heap memory held (nodes and the leaf face order).
    pub fn heap_bytes(&self) -> u64 {
        (self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.faces.capacity() * std::mem::size_of::<u32>()) as u64
    }

    /// Walk the nodes the ray enters before `limit()` (re-read after each
    /// leaf, so it can shrink), nearest first, calling `leaf` with each
    /// leaf's faces; stops when `leaf` returns true.
    fn walk(&self, ray: &Ray, limit: impl Fn() -> f64, mut leaf: impl FnMut(&[u32]) -> bool) {
        if self.nodes.is_empty() {
            return;
        }
        // (node, where the ray enters it)
        let mut stack: Vec<(u32, f64)> = Vec::with_capacity(64);
        let Some(t) = ray.enters(&self.nodes[0], self.pad, limit()) else {
            return;
        };
        stack.push((0, t));
        while let Some((i, t)) = stack.pop() {
            // A hit found since it was pushed may be nearer than it.
            if t > limit() {
                continue;
            }
            let n = &self.nodes[i as usize];
            if n.count > 0 {
                let first = n.first_or_right as usize;
                if leaf(&self.faces[first..first + n.count as usize]) {
                    return;
                }
                continue;
            }
            let lim = limit();
            let (a, b) = (i + 1, n.first_or_right);
            let ta = ray.enters(&self.nodes[a as usize], self.pad, lim);
            let tb = ray.enters(&self.nodes[b as usize], self.pad, lim);
            match (ta, tb) {
                (Some(ta), Some(tb)) => {
                    // Nearer on top.
                    let (near, far) = if tb < ta {
                        ((b, tb), (a, ta))
                    } else {
                        ((a, ta), (b, tb))
                    };
                    stack.push(far);
                    stack.push(near);
                }
                (Some(ta), None) => stack.push((a, ta)),
                (None, Some(tb)) => stack.push((b, tb)),
                (None, None) => {}
            }
        }
    }

    fn ray(&self, o: DVec3, d: DVec3) -> Ray {
        Ray {
            o,
            d,
            rel: o - self.origin,
            inv: d.recip(),
        }
    }

    /// Nearest live face hit by the world-space ray `origin` + t·`dir`
    /// (`dir` a unit vector) on `mesh` placed by `model`, and the
    /// world-space distance: exactly what the brute-force
    /// [`crate::ray_pick`] returns, ties to the lower face ID. `mesh` must
    /// be the revision this was built for.
    pub fn ray_pick(
        &self,
        mesh: &Mesh,
        origin: [f64; 3],
        dir: [f64; 3],
        model: &DMat4,
    ) -> Option<(FaceId, f64)> {
        let inv = model.inverse();
        let o = (inv * DVec4::from((DVec3::from(origin), 1.0))).truncate();
        let d_obj = (inv * DVec4::from((DVec3::from(dir), 0.0))).truncate();
        let scale = d_obj.length();
        if scale < 1e-18 {
            return None;
        }
        let ray = self.ray(o, d_obj / scale);
        // (face, world distance, object distance)
        let best = std::cell::Cell::new(None::<(u32, f64, f64)>);
        let slack = self.pad;
        self.walk(
            &ray,
            || {
                best.get().map_or(f64::INFINITY, |(_, _, t)| {
                    // Room for a farther-looking box face that still holds
                    // an equal-distance hit with a lower ID.
                    t * (1.0 + 1e-12) + slack
                })
            },
            |faces| {
                for &f in faces {
                    let Some(t) = mesh.face(FaceId(f)) else {
                        continue;
                    };
                    let [a, b, c] = t.map(|v| DVec3::from(mesh.corner_position(v)));
                    let Some(raw) = intersect(ray.o, ray.d, a, b, c) else {
                        continue;
                    };
                    let dist = raw / scale;
                    if dist > HIT_MIN
                        && best
                            .get()
                            .is_none_or(|(bf, bd, _)| dist < bd || (dist == bd && f < bf))
                    {
                        best.set(Some((f, dist, raw)));
                    }
                }
                false
            },
        );
        best.get().map(|(f, dist, _)| (FaceId(f), dist))
    }

    /// True if a face of `mesh` for which `skip` is false lies strictly
    /// between the object-space points `from` and `to` (see
    /// [`crate::edge_occluded`], which does this in world space). Stops at
    /// the first such face.
    pub fn occluded(
        &self,
        mesh: &Mesh,
        from: DVec3,
        to: DVec3,
        skip: impl Fn(&[u32; 3]) -> bool,
    ) -> bool {
        let Some((d, len)) = crate::pick::segment(from, to) else {
            return false;
        };
        let ray = self.ray(from, d);
        let mut hidden = false;
        let reach = len + self.pad;
        self.walk(
            &ray,
            || reach,
            |faces| {
                hidden = faces.iter().any(|&f| {
                    mesh.face(FaceId(f))
                        .is_some_and(|t| crate::pick::blocks(mesh, &t, from, d, len, &skip))
                });
                hidden
            },
        );
        hidden
    }
}
