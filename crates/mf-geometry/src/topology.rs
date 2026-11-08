//! Derived topology, computed lazily and cached per mesh (MF-GEO-003).
//!
//! Everything here is derived from the triangle list. It is never
//! authoritative state. Sorted arrays are used instead of hash maps so the
//! results are deterministic (MF-CMD-005).

use rayon::slice::ParallelSliceMut;

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
    fn new(n: usize) -> Self {
        UnionFind((0..n as u32).collect())
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
        let face_slots = mesh.face_slots() as usize;
        let vertex_count = mesh.vertex_slots() as usize;
        // Faces with a repeated index (collapsed onto an edge or point) have
        // no area and no well-defined edges. They're reported as degenerate
        // by diagnostics and excluded here so they can't fake non-manifold
        // edges or vertices.
        let collapsed = |t: &[u32; 3]| t[0] == t[1] || t[1] == t[2] || t[0] == t[2];
        let mut edges: Vec<HalfEdge> = Vec::with_capacity(mesh.face_count() * 3);
        for (FaceId(f), t) in mesh.faces() {
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
        edges.par_sort_unstable_by_key(|e| (e.lo, e.hi, e.face, e.corner));

        let mut faces = UnionFind::new(face_slots);
        let mut corners = UnionFind::new(face_slots * 3);
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

        let mut i = 0;
        while i < edges.len() {
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
        let mut groups: Vec<(u32, u32)> = mesh
            .faces()
            .map(|(FaceId(f), _)| (faces.find(f), f))
            .collect();
        groups.par_sort_unstable();
        let mut components: Vec<Vec<u32>> = Vec::new();
        let mut last_root = u32::MAX;
        for (root, f) in groups {
            if root != last_root {
                components.push(Vec::new());
                last_root = root;
            }
            if let Some(c) = components.last_mut() {
                c.push(f);
            }
        }
        components.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        topo.components = components;

        // Non-manifold vertices: more than one corner fan, or on a >2-face edge.
        let mut fans: Vec<(u32, u32)> = Vec::with_capacity(mesh.face_count() * 3);
        for (FaceId(f), t) in mesh.faces().filter(|(_, t)| !collapsed(t)) {
            for (c, &v) in t.iter().enumerate() {
                fans.push((v, corners.find(f * 3 + c as u32)));
            }
        }
        fans.par_sort_unstable();
        fans.dedup();
        let mut fan_count = vec![0u32; vertex_count];
        for (v, _) in &fans {
            fan_count[*v as usize] += 1;
        }
        topo.non_manifold_vertices = (0..vertex_count)
            .filter(|&v| fan_count[v] > 1 || on_bad_edge[v])
            .count();

        // Boundary loops. Trace edge to edge: at an ordinary boundary vertex
        // the next edge is the only outgoing one. At a pinched vertex (k > 1
        // holes meeting) each incoming edge continues along the outgoing edge
        // across the same gap: the next outgoing edge counter-clockwise
        // around the vertex (MF-ISSUE-007). Vertices on non-manifold edges,
        // or with unequal in/out degree, stay untraced.
        boundary.par_sort_unstable();
        let mut out_deg = vec![0u32; vertex_count];
        let mut in_deg = vec![0u32; vertex_count];
        for &(a, b, _, _) in &boundary {
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
            pinched_pairing(mesh, &boundary, &out_deg, &in_deg, &traceable, &mut corners);
        const NONE: u32 = u32::MAX;
        let next_edge: Vec<u32> = boundary
            .iter()
            .enumerate()
            .map(|(e, &(_, v, _, _))| {
                if !traceable(v) {
                    NONE
                } else if out_deg[v as usize] == 1 {
                    out_range(v).start as u32
                } else {
                    pinched_next
                        .binary_search_by_key(&(e as u32), |&(i, _)| i)
                        .map_or(NONE, |k| pinched_next[k].1)
                }
            })
            .collect();

        let mut visited = vec![false; boundary.len()];
        let mut traced = 0usize;
        for start in 0..boundary.len() {
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
        topo
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
) -> Vec<(u32, u32)> {
    let mut pinched: Vec<u32> = (0..out_deg.len() as u32)
        .filter(|&v| {
            out_deg[v as usize] > 1 && in_deg[v as usize] == out_deg[v as usize] && traceable(v)
        })
        .collect();
    if pinched.is_empty() {
        return Vec::new();
    }
    pinched.sort_unstable();

    // Per pinched vertex, its fans: (fan root, incoming edges, outgoing edges).
    type Fan = (u32, Vec<u32>, Vec<u32>);
    let mut fans: Vec<Vec<Fan>> = vec![Vec::new(); pinched.len()];
    let fan_entry =
        |k: usize, root: u32, edge: u32, outgoing: bool, fans: &mut Vec<Vec<Fan>>| {
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
    pairs.sort_unstable();
    pairs
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
