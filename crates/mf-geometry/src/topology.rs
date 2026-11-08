//! Derived topology, computed lazily and cached per mesh (MF-GEO-003).
//!
//! Everything here is derived from the triangle list. It is never
//! authoritative state. Sorted arrays are used instead of hash maps so the
//! results are deterministic (MF-CMD-005).

#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryLoop {
    pub id: u32,
    /// Ordered loop vertices, following the boundary half-edge direction.
    pub vertices: Vec<u32>,
    pub edge_count: usize,
}

#[derive(Debug, Clone, Default)]
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
    pub fn compute(vertex_count: usize, triangles: &[[u32; 3]]) -> Topology {
        let mut edges: Vec<HalfEdge> = Vec::with_capacity(triangles.len() * 3);
        for (f, t) in triangles.iter().enumerate() {
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                if a == b {
                    continue;
                }
                edges.push(HalfEdge {
                    lo: a.min(b),
                    hi: a.max(b),
                    face: f as u32,
                    corner: i as u8,
                    forward: a < b,
                });
            }
        }
        edges.sort_unstable_by_key(|e| (e.lo, e.hi, e.face, e.corner));

        let mut faces = UnionFind::new(triangles.len());
        let mut corners = UnionFind::new(triangles.len() * 3);
        let mut on_bad_edge = vec![false; vertex_count];
        let mut boundary: Vec<(u32, u32)> = Vec::new(); // directed a -> b
        let mut topo = Topology::default();

        let corner_of = |face: u32, v: u32| -> u32 {
            let t = triangles[face as usize];
            let i = t.iter().position(|&x| x == v).unwrap_or(0);
            face * 3 + i as u32
        };

        let mut i = 0;
        while i < edges.len() {
            let mut j = i + 1;
            while j < edges.len() && edges[j].lo == edges[i].lo && edges[j].hi == edges[i].hi {
                j += 1;
            }
            let group = &edges[i..j];
            let (lo, hi) = (group[0].lo, group[0].hi);
            match group.len() {
                1 => {
                    let e = group[0];
                    boundary.push(if e.forward { (lo, hi) } else { (hi, lo) });
                }
                2 => {
                    let (a, b) = (group[0], group[1]);
                    faces.union(a.face, b.face);
                    if a.forward == b.forward {
                        topo.inconsistent_edges += 1;
                    }
                    corners.union(corner_of(a.face, lo), corner_of(b.face, lo));
                    corners.union(corner_of(a.face, hi), corner_of(b.face, hi));
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
        let mut groups: Vec<(u32, u32)> = (0..triangles.len() as u32)
            .map(|f| (faces.find(f), f))
            .collect();
        groups.sort_unstable();
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
        let mut fans: Vec<(u32, u32)> = Vec::with_capacity(triangles.len() * 3);
        for (f, t) in triangles.iter().enumerate() {
            for (c, &v) in t.iter().enumerate() {
                fans.push((v, corners.find((f * 3 + c) as u32)));
            }
        }
        fans.sort_unstable();
        fans.dedup();
        let mut fan_count = vec![0u32; vertex_count];
        for (v, _) in &fans {
            fan_count[*v as usize] += 1;
        }
        topo.non_manifold_vertices = (0..vertex_count)
            .filter(|&v| fan_count[v] > 1 || on_bad_edge[v])
            .count();

        // Boundary loops through vertices with exactly one outgoing and one
        // incoming boundary half-edge.
        let mut out_deg = vec![0u32; vertex_count];
        let mut in_deg = vec![0u32; vertex_count];
        for &(a, b) in &boundary {
            out_deg[a as usize] += 1;
            in_deg[b as usize] += 1;
        }
        let traceable = |v: u32| out_deg[v as usize] == 1 && in_deg[v as usize] == 1;
        boundary.sort_unstable();
        let next = |v: u32| -> Option<u32> {
            boundary
                .binary_search_by_key(&v, |&(a, _)| a)
                .ok()
                .map(|k| boundary[k].1)
        };
        let mut visited = vec![false; vertex_count];
        let mut traced = 0usize;
        for &(start, _) in &boundary {
            if visited[start as usize] || !traceable(start) {
                continue;
            }
            let mut verts = vec![start];
            let mut cur = start;
            let closed = loop {
                match next(cur) {
                    Some(n) if n == start => break true,
                    Some(n) if traceable(n) && !visited[n as usize] => {
                        verts.push(n);
                        cur = n;
                    }
                    _ => break false,
                }
            };
            // Every vertex on a broken chain leads to the same dead end, so
            // mark it visited either way to keep tracing linear.
            for &v in &verts {
                visited[v as usize] = true;
            }
            if closed {
                traced += verts.len();
                topo.boundary_loops.push(BoundaryLoop {
                    id: topo.boundary_loops.len() as u32,
                    edge_count: verts.len(),
                    vertices: verts,
                });
            }
        }
        topo.untraced_boundary_edges = topo.boundary_edges - traced;
        topo
    }
}
