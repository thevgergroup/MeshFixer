//! Derived topology, computed lazily and cached per mesh (MF-GEO-003).
//!
//! Everything here is derived from the triangle list. It is never
//! authoritative state. Sorted arrays are used instead of hash maps so the
//! results are deterministic (MF-CMD-005).

use mf_runtime::{Cancelled, RunContext, indices, par_sort_unstable_by_key};

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

#[derive(Clone, Copy)]
struct HalfEdge {
    lo: u32,
    hi: u32,
    face: u32,
    /// Corner index within the face of the edge's start vertex.
    corner: u8,
    /// True if the face traverses lo -> hi.
    forward: bool,
}

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
        const STAGE: &str = "topology";
        let faces_total = mesh.face_count();
        let face_slots = mesh.face_slots() as usize;
        let vertex_count = mesh.vertex_slots() as usize;
        // Faces with a repeated index (collapsed onto an edge or point) have
        // no area and no well-defined edges. They're reported as degenerate
        // by diagnostics and excluded here so they can't fake non-manifold
        // edges or vertices.
        let collapsed = |t: &[u32; 3]| t[0] == t[1] || t[1] == t[2] || t[0] == t[2];
        let mut edges: Vec<HalfEdge> = Vec::with_capacity(mesh.face_count() * 3);
        let span = ctx.span(STAGE, 0.0, 0.1, faces_total);
        for (k, (FaceId(f), t)) in mesh.faces().enumerate() {
            span.at(k)?;
            if collapsed(&t) {
                continue;
            }
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                edges.push(HalfEdge {
                    lo: a.min(b),
                    hi: a.max(b),
                    face: f,
                    corner: i as u8,
                    forward: a < b,
                });
            }
        }
        // Parallel and deterministic: the key is a total order.
        ctx.progress.report(STAGE, 0.1);
        par_sort_unstable_by_key(&mut edges, ctx, |e| (e.lo, e.hi, e.face, e.corner))?;

        let mut faces = UnionFind::new(face_slots, ctx)?;
        let mut corners = UnionFind::new(face_slots * 3, ctx)?;
        let mut on_bad_edge = vec![false; vertex_count];
        // Directed boundary half-edges a -> b, with the corner IDs of a and b
        // in their face (for fan-based pairing at pinched vertices).
        let mut boundary: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut topo = Topology::default();

        // A half-edge starts at corner `c` and ends at corner `c + 1`, so the
        // corners of its low and high vertices follow from its direction.
        let lo_corner = |e: &HalfEdge| {
            e.face * 3
                + if e.forward {
                    e.corner as u32
                } else {
                    (e.corner as u32 + 1) % 3
                }
        };
        let hi_corner = |e: &HalfEdge| {
            e.face * 3
                + if e.forward {
                    (e.corner as u32 + 1) % 3
                } else {
                    e.corner as u32
                }
        };

        let span = ctx.span(STAGE, 0.35, 0.5, edges.len());
        let mut i = 0;
        let mut groups_seen = 0usize;
        while i < edges.len() {
            span.at(groups_seen)?;
            groups_seen += 1;
            let mut j = i + 1;
            while j < edges.len() && edges[j].lo == edges[i].lo && edges[j].hi == edges[i].hi {
                j += 1;
            }
            let group = &edges[i..j];
            topo.edge_count += 1;
            let (lo, hi) = (group[0].lo, group[0].hi);
            match group.len() {
                1 => {
                    let e = group[0];
                    boundary.push(if e.forward {
                        (lo, hi, lo_corner(&e), hi_corner(&e))
                    } else {
                        (hi, lo, hi_corner(&e), lo_corner(&e))
                    });
                }
                2 => {
                    let (a, b) = (group[0], group[1]);
                    faces.union(a.face, b.face);
                    if a.forward == b.forward {
                        topo.inconsistent_edges += 1;
                    }
                    corners.union(lo_corner(&a), lo_corner(&b));
                    corners.union(hi_corner(&a), hi_corner(&b));
                }
                _ => {
                    for e in &group[1..] {
                        faces.union(group[0].face, e.face);
                    }
                    topo.non_manifold_edges.push([lo, hi]);
                    on_bad_edge[lo as usize] = true;
                    on_bad_edge[hi as usize] = true;
                }
            }
            i = j;
        }
        topo.boundary_edges = boundary.len();

        // Components: group faces by root, in face order.
        let span = ctx.span(STAGE, 0.5, 0.55, faces_total);
        let mut groups: Vec<(u32, u32)> = Vec::with_capacity(faces_total);
        for (k, (FaceId(f), _)) in mesh.faces().enumerate() {
            span.at(k)?;
            groups.push((faces.find(f), f));
        }
        par_sort_unstable_by_key(&mut groups, ctx, |g| *g)?;
        let span = ctx.span(STAGE, 0.65, 0.7, groups.len());
        let mut components: Vec<Vec<u32>> = Vec::new();
        let mut last_root = u32::MAX;
        for (k, (root, f)) in groups.into_iter().enumerate() {
            span.at(k)?;
            if root != last_root {
                components.push(Vec::new());
                last_root = root;
            }
            if let Some(c) = components.last_mut() {
                c.push(f);
            }
        }
        // Largest first, ties by lowest first face (a total order: every
        // face is in exactly one component).
        par_sort_unstable_by_key(&mut components, ctx, |c| (std::cmp::Reverse(c.len()), c[0]))?;
        topo.components = components;

        // Non-manifold vertices: more than one corner fan, or on a >2-face edge.
        let span = ctx.span(STAGE, 0.7, 0.75, faces_total);
        let mut fans: Vec<(u32, u32)> = Vec::with_capacity(mesh.face_count() * 3);
        for (k, (FaceId(f), t)) in mesh.faces().enumerate() {
            span.at(k)?;
            if collapsed(&t) {
                continue;
            }
            for (c, &v) in t.iter().enumerate() {
                fans.push((v, corners.find(f * 3 + c as u32)));
            }
        }
        par_sort_unstable_by_key(&mut fans, ctx, |x| *x)?;
        ctx.checkpoint()?;
        fans.dedup();
        let span = ctx.span(STAGE, 0.9, 0.95, fans.len() + vertex_count);
        let mut fan_count = vec![0u32; vertex_count];
        for (k, (v, _)) in fans.iter().enumerate() {
            span.at(k)?;
            fan_count[*v as usize] += 1;
        }
        let mut tick = ctx.ticker();
        let mut non_manifold_vertices = 0;
        for v in 0..vertex_count {
            tick.tick()?;
            if fan_count[v] > 1 || on_bad_edge[v] {
                non_manifold_vertices += 1;
            }
        }
        topo.non_manifold_vertices = non_manifold_vertices;

        // Boundary loops. Trace edge to edge: at an ordinary boundary vertex
        // the next edge is the only outgoing one. At a pinched vertex (k > 1
        // holes meeting) each incoming edge continues along the outgoing edge
        // across the same gap: the next outgoing edge counter-clockwise
        // around the vertex (MF-ISSUE-007). Vertices on non-manifold edges,
        // or with unequal in/out degree, stay untraced.
        ctx.progress.report(STAGE, 0.95);
        par_sort_unstable_by_key(&mut boundary, ctx, |b| *b)?;
        let mut out_deg = vec![0u32; vertex_count];
        let mut in_deg = vec![0u32; vertex_count];
        let mut tick = ctx.ticker();
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
        let pinched_next = pinched_pairing(
            mesh,
            &boundary,
            &out_deg,
            &in_deg,
            &traceable,
            &mut corners,
            ctx,
        )?;
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
        ctx.progress.report(STAGE, 1.0);
        Ok(topo)
    }
}

/// At each pinched boundary vertex (k > 1 incident fans), pair every
/// incoming boundary edge with the outgoing edge across the same gap.
///
/// Fans come from the corner union-find, so the pairing is combinatorial
/// first (review I7): each fan contributes one incoming and one outgoing
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
    corners: &mut UnionFind,
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
            let root = corners.find(ca);
            fan_entry(k, root, e as u32, true, &mut fans);
        }
        if let Ok(k) = pinched.binary_search(&b) {
            let root = corners.find(cb);
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
