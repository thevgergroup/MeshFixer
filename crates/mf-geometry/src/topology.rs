//! Derived topology, computed lazily and cached per mesh (MF-GEO-003).
//!
//! Everything here is derived from the triangle list. It is never
//! authoritative state. Sorted arrays are used instead of hash maps so the
//! results are deterministic (MF-CMD-005).

use mf_runtime::{Cancelled, RunContext, TICK_INTERVAL, Ticker, indices, par_sort_unstable_by_key};
use rayon::prelude::*;

use crate::ids::FaceId;
use crate::mesh::Mesh;

#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryLoop {
    pub id: u32,
    /// Ordered loop vertices, following the boundary half-edge direction.
    pub vertices: Vec<u32>,
    pub edge_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Topology {
    /// Face IDs per edge-connected component, each sorted ascending.
    /// Ordered largest first, ties broken by lowest first face.
    pub components: Vec<Vec<u32>>,
    pub boundary_loops: Vec<BoundaryLoop>,
    /// Edges with exactly one incident face.
    pub boundary_edges: usize,
    /// Boundary edges that touch a non-manifold boundary vertex and so are
    /// not part of any traced loop.
    pub untraced_boundary_edges: usize,
    /// Undirected edges with more than two incident faces.
    pub non_manifold_edges: Vec<[u32; 2]>,
    pub non_manifold_vertices: usize,
    /// Two-face edges whose faces traverse the edge in the same direction.
    pub inconsistent_edges: usize,
    /// Distinct undirected edges of live, non-collapsed faces.
    pub edge_count: usize,
}

struct UnionFind(Vec<u32>);

impl UnionFind {
    fn new(n: usize, ctx: &RunContext) -> Result<Self, Cancelled> {
        Ok(UnionFind(indices(n as u32, ctx)?))
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.0[x as usize] != x {
            let p = self.0[x as usize];
            self.0[x as usize] = self.0[p as usize];
            x = p;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // Smaller root wins so the result doesn't depend on union order.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.0[hi as usize] = lo;
        }
    }
}

/// Faces with a repeated index (collapsed onto an edge or point) have no
/// area and no well-defined edges. They're reported as degenerate by
/// diagnostics and excluded here so they can't fake non-manifold edges or
/// vertices.
fn collapsed(t: &[u32; 3]) -> bool {
    t[0] == t[1] || t[1] == t[2] || t[0] == t[2]
}

/// The next corner of the same face. Corner `f·3 + c` is corner `c` of
/// face `f`, and also names the half-edge that starts there: from that
/// corner's vertex to the next corner's.
fn next_corner(h: u32) -> u32 {
    h - h % 3 + (h % 3 + 1) % 3
}

fn prev_corner(h: u32) -> u32 {
    h - h % 3 + (h % 3 + 2) % 3
}

/// Every half-edge of the live, non-collapsed faces, grouped by undirected
/// edge: bucketed by the edge's low vertex (a counting sort), each bucket
/// sorted by (high vertex, half-edge). Nothing but the half-edge IDs is
/// stored (the vertices are read back from the mesh), so it costs 4 bytes
/// per half-edge plus 4 per vertex: about 70 MB at 5M triangles, where a
/// sorted list of (low, high, face, corner) records was 240 MB, plus a
/// union-find over corners and a sorted list of corner fans (MF-ISSUE-019).
/// An edge's half-edges are adjacent, found by a binary search in its low
/// vertex's bucket. It lives only while the topology is built.
struct EdgeIndex<'m> {
    mesh: &'m Mesh,
    ctx: &'m RunContext,
    /// Vertex `v`'s bucket is `half_edges[offsets[v]..offsets[v + 1]]`.
    offsets: Vec<u32>,
    half_edges: Vec<u32>,
}

impl<'m> EdgeIndex<'m> {
    fn build(mesh: &'m Mesh, ctx: &'m RunContext) -> Result<EdgeIndex<'m>, Cancelled> {
        let faces = &mesh.inner().faces;
        let vertex_slots = mesh.vertex_slots() as usize;
        let faces_total = mesh.face_count();
        let low = |t: &[u32; 3], c: usize| t[c].min(t[(c + 1) % 3]) as usize;

        // Count each bucket (shifted by one), then prefix-sum into starts.
        let mut offsets = vec![0u32; vertex_slots + 1];
        let mut total = 0usize;
        let span = ctx.span(STAGE, 0.0, 0.1, faces_total);
        for (k, (_, t)) in mesh.faces().enumerate() {
            span.at(k)?;
            if collapsed(&t) {
                continue;
            }
            for c in 0..3 {
                offsets[low(&t, c) + 1] += 1;
            }
            total += 3;
        }
        let mut tick = ctx.ticker();
        for v in 0..vertex_slots {
            tick.tick()?;
            offsets[v + 1] += offsets[v];
        }

        // Scatter in face order, so each bucket starts out sorted by
        // half-edge. `offsets[v]` is v's write cursor, and ends up at v's
        // end, i.e. the next bucket's start: shifted back afterwards.
        let mut half_edges = vec![0u32; total];
        let span = ctx.span(STAGE, 0.1, 0.2, faces_total);
        for (k, (FaceId(f), t)) in mesh.faces().enumerate() {
            span.at(k)?;
            if collapsed(&t) {
                continue;
            }
            for c in 0..3 {
                let cursor = &mut offsets[low(&t, c)];
                half_edges[*cursor as usize] = f * 3 + c as u32;
                *cursor += 1;
            }
        }
        for v in (1..=vertex_slots).rev() {
            tick.tick()?;
            offsets[v] = offsets[v - 1];
        }
        offsets[0] = 0;

        // Sort each bucket by (high vertex, half-edge), blocks of buckets
        // in parallel. Every key is distinct, so the order is the same
        // however the blocks are split across threads.
        const BLOCK: usize = 1 << 14;
        let high = |h: u32| {
            let t = faces.get((h / 3) as usize);
            let c = (h % 3) as usize;
            t[c].max(t[(c + 1) % 3])
        };
        let mut blocks: Vec<(usize, &mut [u32])> = Vec::with_capacity(vertex_slots / BLOCK + 1);
        let mut rest: &mut [u32] = &mut half_edges;
        let mut v0 = 0;
        while v0 < vertex_slots {
            let v1 = (v0 + BLOCK).min(vertex_slots);
            let len = (offsets[v1] - offsets[v0]) as usize;
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(len);
            blocks.push((v0, head));
            rest = tail;
            v0 = v1;
        }
        ctx.progress.report(STAGE, 0.2);
        let offsets_ref = &offsets;
        // A vertex can carry most of the mesh's edges (a hub): its bucket
        // is sorted with the cancellable sort, and small buckets
        // checkpoint every `TICK_INTERVAL` half-edges sorted (MF-RT-002).
        const BIG_BUCKET: usize = TICK_INTERVAL as usize;
        blocks.into_par_iter().try_for_each(|(v0, block)| {
            ctx.checkpoint()?;
            let base = offsets_ref[v0] as usize;
            let v1 = (v0 + BLOCK).min(vertex_slots);
            let mut since_checkpoint = 0;
            for v in v0..v1 {
                let (s, e) = (
                    offsets_ref[v] as usize - base,
                    offsets_ref[v + 1] as usize - base,
                );
                let bucket = &mut block[s..e];
                if bucket.len() > BIG_BUCKET {
                    par_sort_unstable_by_key(bucket, ctx, |&h| (high(h), h))?;
                } else if bucket.len() > 1 {
                    bucket.sort_unstable_by_key(|&h| (high(h), h));
                    since_checkpoint += bucket.len();
                    if since_checkpoint >= BIG_BUCKET {
                        ctx.checkpoint()?;
                        since_checkpoint = 0;
                    }
                }
            }
            Ok(())
        })?;
        Ok(EdgeIndex {
            mesh,
            ctx,
            offsets,
            half_edges,
        })
    }

    fn tri(&self, f: u32) -> [u32; 3] {
        self.mesh.inner().faces.get(f as usize)
    }

    /// Start and end vertex of half-edge `h`.
    fn ends(&self, h: u32) -> (u32, u32) {
        let t = self.tri(h / 3);
        let c = (h % 3) as usize;
        (t[c], t[(c + 1) % 3])
    }

    fn high(&self, h: u32) -> u32 {
        let (a, b) = self.ends(h);
        a.max(b)
    }

    fn bucket(&self, v: u32) -> &[u32] {
        &self.half_edges[self.offsets[v as usize] as usize..self.offsets[v as usize + 1] as usize]
    }

    /// The half-edges of undirected edge `{a, b}`, by half-edge ID.
    fn edge(&self, a: u32, b: u32) -> &[u32] {
        let (lo, hi) = (a.min(b), a.max(b));
        let bucket = self.bucket(lo);
        let start = bucket.partition_point(|&h| self.high(h) < hi);
        let len = bucket[start..].partition_point(|&h| self.high(h) == hi);
        &bucket[start..start + len]
    }

    /// The other half-edge of `h`'s edge, if exactly two faces share it.
    fn mate(&self, h: u32) -> Option<u32> {
        let (a, b) = self.ends(h);
        match self.edge(a, b) {
            &[x, y] => Some(if x == h { y } else { x }),
            _ => None,
        }
    }

    /// Walk the corner fan of `corner`'s vertex, starting across `via` (one
    /// of the corner's two edges) and crossing two-face edges only: the
    /// corners joined this way are one fan (a disk or a wedge of one). Each
    /// corner reached is passed to `visit`; the walk stops at an edge that
    /// isn't shared by exactly two faces, or when `visit` returns false.
    fn walk(
        &self,
        corner: u32,
        via: u32,
        tick: &mut Ticker,
        mut visit: impl FnMut(u32) -> bool,
    ) -> Result<(), Cancelled> {
        let v = self.ends(corner).0;
        let mut via = via;
        let mut steps = 0u32;
        loop {
            tick.tick()?;
            // A step is a binary search in a bucket that can be huge (a
            // hub vertex's): checkpoint more often than the ticker does.
            steps += 1;
            if steps.is_multiple_of(WALK_CHECKPOINT) {
                self.ctx.checkpoint()?;
            }
            let Some(m) = self.mate(via) else {
                return Ok(());
            };
            // The mate's face has v at the mate's start or end.
            let k = if self.ends(m).0 == v {
                m
            } else {
                next_corner(m)
            };
            if !visit(k) {
                return Ok(());
            }
            // Leave k's face across its other edge at v.
            via = if m == k { prev_corner(k) } else { k };
        }
    }

    /// How many corners the fan of `corner` has (itself included).
    fn fan_size(&self, corner: u32, tick: &mut Ticker) -> Result<u32, Cancelled> {
        let (mut size, mut closed) = (1, false);
        for via in [corner, prev_corner(corner)] {
            if closed {
                break;
            }
            self.walk(corner, via, tick, |k| {
                closed = k == corner;
                size += u32::from(!closed);
                !closed
            })?;
        }
        Ok(size)
    }

    /// The fan of `corner`, named by its lowest corner.
    fn fan_root(&self, corner: u32, tick: &mut Ticker) -> Result<u32, Cancelled> {
        let mut root = corner;
        for via in [corner, prev_corner(corner)] {
            self.walk(corner, via, tick, |k| {
                root = root.min(k);
                k != corner
            })?;
        }
        Ok(root)
    }
}

const STAGE: &str = "topology";

/// Fan-walk steps between checkpoints (see [`EdgeIndex::walk`]).
const WALK_CHECKPOINT: u32 = 2048;

impl Topology {
    /// Heap bytes retained by this topology.
    pub fn heap_bytes(&self) -> u64 {
        let comps: usize = self
            .components
            .iter()
            .map(|c| c.capacity() * 4 + std::mem::size_of::<Vec<u32>>())
            .sum();
        let loops: usize = self
            .boundary_loops
            .iter()
            .map(|l| l.vertices.capacity() * 4 + std::mem::size_of::<BoundaryLoop>())
            .sum();
        (std::mem::size_of::<Topology>() + comps + loops + self.non_manifold_edges.capacity() * 8)
            as u64
    }

    /// Topology of the mesh's live faces. Face and vertex IDs are slots.
    pub fn compute(mesh: &Mesh) -> Topology {
        Topology::compute_with(mesh, &RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`Topology::compute`], cancellable, reporting progress as stage
    /// `"topology"`.
    pub fn compute_with(mesh: &Mesh, ctx: &RunContext) -> Result<Topology, Cancelled> {
        let faces_total = mesh.face_count();
        let face_slots = mesh.face_slots() as usize;
        let vertex_count = mesh.vertex_slots() as usize;
        let index = EdgeIndex::build(mesh, ctx)?;

        // One pass over the edges, in (low, high) order: face connectivity,
        // boundary half-edges, non-manifold and inconsistent edges.
        let mut faces = UnionFind::new(face_slots, ctx)?;
        let mut on_bad_edge = vec![false; vertex_count];
        // Directed boundary half-edges a -> b, with the corner IDs of a and b
        // in their face (for fan-based pairing at pinched vertices).
        let mut boundary: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut topo = Topology::default();
        let span = ctx.span(STAGE, 0.3, 0.45, vertex_count);
        let mut tick = ctx.ticker();
        for lo in 0..vertex_count as u32 {
            span.at(lo as usize)?;
            let bucket = index.bucket(lo);
            let mut i = 0;
            while i < bucket.len() {
                tick.tick()?;
                let hi = index.high(bucket[i]);
                let mut j = i + 1;
                while j < bucket.len() && index.high(bucket[j]) == hi {
                    j += 1;
                }
                let group = &bucket[i..j];
                topo.edge_count += 1;
                // A half-edge runs from its corner to the next one; which
                // of those is at `lo` follows from its direction.
                let forward = |h: u32| index.ends(h).0 == lo;
                let lo_corner = |h: u32| if forward(h) { h } else { next_corner(h) };
                let hi_corner = |h: u32| if forward(h) { next_corner(h) } else { h };
                match *group {
                    [h] => boundary.push(if forward(h) {
                        (lo, hi, lo_corner(h), hi_corner(h))
                    } else {
                        (hi, lo, hi_corner(h), lo_corner(h))
                    }),
                    [a, b] => {
                        faces.union(a / 3, b / 3);
                        if forward(a) == forward(b) {
                            topo.inconsistent_edges += 1;
                        }
                    }
                    _ => {
                        for &h in &group[1..] {
                            faces.union(group[0] / 3, h / 3);
                        }
                        topo.non_manifold_edges.push([lo, hi]);
                        on_bad_edge[lo as usize] = true;
                        on_bad_edge[hi as usize] = true;
                    }
                }
                i = j;
            }
        }
        topo.boundary_edges = boundary.len();

        // Non-manifold vertices: more than one corner fan, or on a >2-face
        // edge. A vertex has one fan if walking the fan of any one of its
        // corners reaches all of them: one walk per vertex, from its first
        // corner, the vertices split into blocks walked in parallel.
        let span = ctx.span(STAGE, 0.45, 0.5, faces_total);
        let mut corner_count = vec![0u32; vertex_count];
        let mut first_corner = vec![u32::MAX; vertex_count];
        for (k, (FaceId(f), t)) in mesh.faces().enumerate() {
            span.at(k)?;
            if collapsed(&t) {
                continue;
            }
            for (c, &v) in t.iter().enumerate() {
                let v = v as usize;
                corner_count[v] += 1;
                if first_corner[v] == u32::MAX {
                    first_corner[v] = f * 3 + c as u32;
                }
            }
        }
        ctx.progress.report(STAGE, 0.5);
        const BLOCK: usize = 1 << 14;
        let non_manifold_vertices = (0..vertex_count.div_ceil(BLOCK))
            .into_par_iter()
            .map(|b| {
                ctx.checkpoint()?;
                let mut tick = ctx.ticker();
                let mut bad = 0usize;
                for v in b * BLOCK..((b + 1) * BLOCK).min(vertex_count) {
                    let several_fans = corner_count[v] > 0
                        && index.fan_size(first_corner[v], &mut tick)? < corner_count[v];
                    if several_fans || on_bad_edge[v] {
                        bad += 1;
                    }
                }
                Ok(bad)
            })
            .try_reduce(|| 0, |a, b| Ok(a + b))?;
        drop((corner_count, first_corner));
        ctx.progress.report(STAGE, 0.6);
        topo.non_manifold_vertices = non_manifold_vertices;

        // Boundary loops. Trace edge to edge: at an ordinary boundary vertex
        // the next edge is the only outgoing one. At a pinched vertex (k > 1
        // holes meeting) each incoming edge continues along the outgoing edge
        // across the same gap: the next outgoing edge counter-clockwise
        // around the vertex (MF-ISSUE-007). Vertices on non-manifold edges,
        // or with unequal in/out degree, stay untraced.
        ctx.progress.report(STAGE, 0.6);
        par_sort_unstable_by_key(&mut boundary, ctx, |b| *b)?;
        let mut out_deg = vec![0u32; vertex_count];
        let mut in_deg = vec![0u32; vertex_count];
        for &(a, b, _, _) in &boundary {
            tick.tick()?;
            out_deg[a as usize] += 1;
            in_deg[b as usize] += 1;
        }
        let traceable = |v: u32| {
            let v = v as usize;
            out_deg[v] >= 1 && out_deg[v] == in_deg[v] && !on_bad_edge[v]
        };
        // Outgoing edges of v are contiguous in the sorted list.
        let out_range = |v: u32| {
            let lo = boundary.partition_point(|&(a, ..)| a < v);
            let hi = boundary.partition_point(|&(a, ..)| a <= v);
            lo..hi
        };
        let pinched_next =
            pinched_pairing(mesh, &boundary, &out_deg, &in_deg, &traceable, &index, ctx)?;
        drop(index);
        ctx.progress.report(STAGE, 0.7);
        const NONE: u32 = u32::MAX;
        let mut next_edge: Vec<u32> = Vec::with_capacity(boundary.len());
        for (e, &(_, v, _, _)) in boundary.iter().enumerate() {
            tick.tick()?;
            next_edge.push(if !traceable(v) {
                NONE
            } else if out_deg[v as usize] == 1 {
                out_range(v).start as u32
            } else {
                pinched_next
                    .binary_search_by_key(&(e as u32), |&(i, _)| i)
                    .map_or(NONE, |k| pinched_next[k].1)
            });
        }

        let mut visited = vec![false; boundary.len()];
        let mut traced = 0usize;
        for start in 0..boundary.len() {
            tick.tick()?;
            if visited[start] {
                continue;
            }
            let mut edges = vec![start];
            visited[start] = true;
            let mut cur = start;
            let closed = loop {
                let n = next_edge[cur];
                if n == NONE {
                    break false;
                }
                let n = n as usize;
                if n == start {
                    break true;
                }
                if visited[n] {
                    break false;
                }
                tick.tick()?;
                visited[n] = true;
                edges.push(n);
                cur = n;
            };
            if closed {
                traced += edges.len();
                topo.boundary_loops.push(BoundaryLoop {
                    id: topo.boundary_loops.len() as u32,
                    edge_count: edges.len(),
                    vertices: edges.iter().map(|&e| boundary[e].0).collect(),
                });
            }
        }
        topo.untraced_boundary_edges = topo.boundary_edges - traced;
        drop((next_edge, visited, out_deg, in_deg, boundary, on_bad_edge));

        // Components: group faces by root, in face order.
        let span = ctx.span(STAGE, 0.75, 0.85, faces_total);
        let mut groups: Vec<(u32, u32)> = Vec::with_capacity(faces_total);
        for (k, (FaceId(f), _)) in mesh.faces().enumerate() {
            span.at(k)?;
            groups.push((faces.find(f), f));
        }
        drop(faces);
        par_sort_unstable_by_key(&mut groups, ctx, |g| *g)?;
        let span = ctx.span(STAGE, 0.9, 0.95, groups.len());
        let mut components: Vec<Vec<u32>> = Vec::new();
        let mut i = 0;
        while i < groups.len() {
            span.at(i)?;
            let root = groups[i].0;
            let mut j = i + 1;
            while j < groups.len() && groups[j].0 == root {
                tick.tick()?;
                j += 1;
            }
            // Exactly sized: the components are most of what's retained.
            components.push(groups[i..j].iter().map(|g| g.1).collect());
            i = j;
        }
        drop(groups);
        // Largest first, ties by lowest first face (a total order: every
        // face is in exactly one component).
        par_sort_unstable_by_key(&mut components, ctx, |c| (std::cmp::Reverse(c.len()), c[0]))?;
        topo.components = components;
        ctx.progress.report(STAGE, 1.0);
        Ok(topo)
    }
}

/// At each pinched boundary vertex (k > 1 incident fans), pair every
/// incoming boundary edge with the outgoing edge across the same gap.
///
/// Fans come from walking the corners around the vertex (see
/// [`EdgeIndex::fan_root`]), so the pairing is combinatorial first (review
/// I7): each fan contributes one incoming and one outgoing
/// boundary edge, and a fan is never paired with itself. With two fans the
/// pairing is forced. With more, geometry is used only to order the fans
/// around the vertex (by the angle of each fan's outgoing edge in the tangent
/// plane), and each fan's incoming edge continues along the next fan's
/// outgoing edge. Vertices whose fans aren't disk-like (one in, one out) are
/// left untraced rather than guessed. Returns `(incoming edge index,
/// outgoing edge index)` sorted by incoming index.
fn pinched_pairing(
    mesh: &Mesh,
    boundary: &[(u32, u32, u32, u32)],
    out_deg: &[u32],
    in_deg: &[u32],
    traceable: &dyn Fn(u32) -> bool,
    index: &EdgeIndex,
    ctx: &RunContext,
) -> Result<Vec<(u32, u32)>, Cancelled> {
    let mut tick = ctx.ticker();
    let mut pinched: Vec<u32> = Vec::new();
    for v in 0..out_deg.len() as u32 {
        tick.tick()?;
        if out_deg[v as usize] > 1 && in_deg[v as usize] == out_deg[v as usize] && traceable(v) {
            pinched.push(v);
        }
    }
    if pinched.is_empty() {
        return Ok(Vec::new());
    }
    pinched.sort_unstable();

    // Per pinched vertex, its fans: (fan root, incoming edges, outgoing edges).
    type Fan = (u32, Vec<u32>, Vec<u32>);
    let mut fans: Vec<Vec<Fan>> = vec![Vec::new(); pinched.len()];
    let fan_entry = |k: usize, root: u32, edge: u32, outgoing: bool, fans: &mut Vec<Vec<Fan>>| {
        let list = &mut fans[k];
        let i = match list.iter().position(|f| f.0 == root) {
            Some(i) => i,
            None => {
                list.push((root, Vec::new(), Vec::new()));
                list.len() - 1
            }
        };
        if outgoing {
            list[i].2.push(edge);
        } else {
            list[i].1.push(edge);
        }
    };
    for (e, &(a, b, ca, cb)) in boundary.iter().enumerate() {
        tick.tick()?;
        if let Ok(k) = pinched.binary_search(&a) {
            let root = index.fan_root(ca, &mut tick)?;
            fan_entry(k, root, e as u32, true, &mut fans);
        }
        if let Ok(k) = pinched.binary_search(&b) {
            let root = index.fan_root(cb, &mut tick)?;
            fan_entry(k, root, e as u32, false, &mut fans);
        }
    }

    // Tangent-plane normals, needed only to order three or more fans.
    let needs_geometry: Vec<bool> = fans.iter().map(|f| f.len() > 2).collect();
    let mut normals = vec![[0.0f64; 3]; pinched.len()];
    if needs_geometry.iter().any(|&g| g) {
        for (_, t) in mesh.faces() {
            tick.tick()?;
            let [a, b, c] = t.map(|v| mesh.corner_position(v));
            let n = cross(sub(b, a), sub(c, a));
            for v in t {
                if let Ok(k) = pinched.binary_search(&v)
                    && needs_geometry[k]
                {
                    for i in 0..3 {
                        normals[k][i] += n[i];
                    }
                }
            }
        }
    }

    let mut pairs = Vec::new();
    for (k, &v) in pinched.iter().enumerate() {
        tick.tick()?;
        let mut vertex_fans = std::mem::take(&mut fans[k]);
        // Each fan must be disk-like at v: exactly one edge in, one out.
        if vertex_fans.len() < 2 || vertex_fans.iter().any(|f| f.1.len() != 1 || f.2.len() != 1) {
            continue;
        }
        if vertex_fans.len() > 2 {
            let n = normals[k];
            let len = dot(n, n).sqrt();
            if len == 0.0 || !len.is_finite() {
                continue;
            }
            let n = n.map(|x| x / len);
            let axis = if n[0].abs() < 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            let e1 = {
                let t = cross(n, axis);
                let l = dot(t, t).sqrt();
                t.map(|x| x / l)
            };
            let e2 = cross(n, e1);
            let p = mesh.corner_position(v);
            let angle = |w: u32| {
                let d = sub(mesh.corner_position(w), p);
                dot(d, e2).atan2(dot(d, e1))
            };
            // Order fans by their outgoing edge's direction; ties by vertex.
            vertex_fans.sort_by(|x, y| {
                let (wx, wy) = (boundary[x.2[0] as usize].1, boundary[y.2[0] as usize].1);
                angle(wx).total_cmp(&angle(wy)).then(wx.cmp(&wy))
            });
        }
        let m = vertex_fans.len();
        for i in 0..m {
            // Fan i spans counter-clockwise from its outgoing edge to its
            // incoming edge; across the gap after it is fan i + 1's outgoing.
            pairs.push((vertex_fans[i].1[0], vertex_fans[(i + 1) % m].2[0]));
        }
    }
    par_sort_unstable_by_key(&mut pairs, ctx, |p| *p)?;
    Ok(pairs)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
