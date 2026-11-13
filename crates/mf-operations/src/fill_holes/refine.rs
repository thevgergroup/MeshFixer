//! Patch refinement (Liepa 2003 §3): split patch triangles at their
//! centroids where the local density says they are too big, then relax
//! interior edges towards Delaunay by flipping, until nothing splits.
//!
//! Works on a local patch (boundary vertices first, then new vertices) so
//! every decision is deterministic and nothing outside the patch can change.

use std::collections::BTreeSet;

use mf_runtime::{Cancelled, RunContext};

/// A hole patch in local indices: `0..n_boundary` are the loop's vertices.
pub struct Patch {
    pub positions: Vec<[f64; 3]>,
    /// Target edge length ("scale attribute") per vertex.
    pub sigma: Vec<f64>,
    pub tris: Vec<[usize; 3]>,
    pub n_boundary: usize,
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}

/// Circumcentre and radius of triangle abc (in its plane), if not degenerate.
fn circumsphere(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Option<([f64; 3], f64)> {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let n = cross(ab, ac);
    let nn = dot(n, n);
    if nn == 0.0 {
        return None;
    }
    // centre = a + ((|ac|² (n × ab) + |ab|² (ac × n)) / (2 |n|²))
    let t1 = cross(n, ab).map(|x| x * dot(ac, ac));
    let t2 = cross(ac, n).map(|x| x * dot(ab, ab));
    let off = [0, 1, 2].map(|k| (t1[k] + t2[k]) / (2.0 * nn));
    let centre = [a[0] + off[0], a[1] + off[1], a[2] + off[2]];
    Some((centre, dot(off, off).sqrt()))
}

fn edge(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

impl Patch {
    /// Smallest triangle area in the patch.
    pub fn min_area(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|v| self.positions[v]);
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Split pass: returns true if any triangle was split.
    fn split(&mut self) -> bool {
        let sqrt2 = std::f64::consts::SQRT_2;
        let mut any = false;
        let count = self.tris.len();
        for t in 0..count {
            let [a, b, c] = self.tris[t];
            let p = [a, b, c].map(|v| self.positions[v]);
            let centre = [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0);
            let sigma_c = (self.sigma[a] + self.sigma[b] + self.sigma[c]) / 3.0;
            let split = [a, b, c].iter().all(|&v| {
                let d = sqrt2 * dist(centre, self.positions[v]);
                d > sigma_c && d > self.sigma[v]
            });
            if split {
                let m = self.positions.len();
                self.positions.push(centre);
                self.sigma.push(sigma_c);
                // Same winding as the original triangle.
                self.tris[t] = [a, b, m];
                self.tris.push([b, c, m]);
                self.tris.push([c, a, m]);
                any = true;
            }
        }
        any
    }

    /// Area vector (twice the area, along the normal) of triangle `t`.
    fn area_vector(&self, t: [usize; 3]) -> [f64; 3] {
        let [a, b, c] = t.map(|v| self.positions[v]);
        cross(sub(b, a), sub(c, a))
    }

    /// Whether flipping (u, v, c) + (v, u, d) to (c, u, d) + (c, d, v)
    /// keeps the patch from folding (MF-ISSUE-037). On a non-planar patch
    /// (a saddle, say) the circumsphere test can ask for a flip across a
    /// quad that isn't convex seen from the patch, which turns a triangle
    /// upside down. Rejected when either new triangle faces against the
    /// two it replaces (their summed area vector), or when more of the new
    /// pair than of the old pair face against the patch normal `patch`.
    fn flip_keeps_orientation(&self, [u, v, c, d]: [usize; 4], patch: [f64; 3]) -> bool {
        let old = [self.area_vector([u, v, c]), self.area_vector([v, u, d])];
        let new = [self.area_vector([c, u, d]), self.area_vector([c, d, v])];
        let quad = [0, 1, 2].map(|k| old[0][k] + old[1][k]);
        let against = |pair: &[[f64; 3]; 2]| pair.iter().filter(|&&n| dot(n, patch) <= 0.0).count();
        new.iter().all(|&n| dot(n, quad) > 0.0) && against(&new) <= against(&old)
    }

    /// Relax pass: flip interior edges whose opposite vertex lies inside the
    /// circumsphere of the neighbouring triangle. Never creates an edge that
    /// already exists (in the patch or in `existing`), nor a fold
    /// ([`Self::flip_keeps_orientation`]). Bounded by `max_flips`, a
    /// termination guard for non-planar patches.
    fn relax(
        &mut self,
        existing: &BTreeSet<(usize, usize)>,
        ctx: &RunContext,
    ) -> Result<(), Cancelled> {
        let max_flips = 100 * self.tris.len().max(1);
        // The patch normal: the sum of its triangles' area vectors, which
        // is the rim's vector area, so no flip or split changes it.
        let patch = self
            .tris
            .iter()
            .map(|&t| self.area_vector(t))
            .fold([0.0; 3], |s, n| [s[0] + n[0], s[1] + n[1], s[2] + n[2]]);
        let mut flips = 0;
        loop {
            // One pass is a sort and a sweep over the patch's edges.
            ctx.checkpoint()?;
            let mut flipped = false;
            // Interior edges: those shared by exactly two patch triangles.
            let mut by_edge: Vec<((usize, usize), usize, usize)> = Vec::new();
            for (t, tri) in self.tris.iter().enumerate() {
                for k in 0..3 {
                    by_edge.push((edge(tri[k], tri[(k + 1) % 3]), t, k));
                }
            }
            by_edge.sort_unstable();
            let mut edges: BTreeSet<(usize, usize)> = by_edge.iter().map(|(e, _, _)| *e).collect();
            let mut i = 0;
            while i + 1 < by_edge.len() {
                let (e, t1, k1) = by_edge[i];
                if by_edge[i + 1].0 != e || (i + 2 < by_edge.len() && by_edge[i + 2].0 == e) {
                    i += 1;
                    continue;
                }
                let (_, t2, k2) = by_edge[i + 1];
                i += 2;
                let (ta, tb) = (self.tris[t1], self.tris[t2]);
                // Skip if either triangle was already changed this pass.
                if edge(ta[k1], ta[(k1 + 1) % 3]) != e || edge(tb[k2], tb[(k2 + 1) % 3]) != e {
                    continue;
                }
                let (u, v) = (ta[k1], ta[(k1 + 1) % 3]);
                let c = ta[(k1 + 2) % 3];
                let d = tb[(k2 + 2) % 3];
                if c == d || edges.contains(&edge(c, d)) || existing.contains(&edge(c, d)) {
                    continue;
                }
                let Some((centre, r)) =
                    circumsphere(self.positions[u], self.positions[v], self.positions[c])
                else {
                    continue;
                };
                if dist(centre, self.positions[d]) < r * (1.0 - 1e-12)
                    && self.flip_keeps_orientation([u, v, c, d], patch)
                {
                    // Triangles (u, v, c) and (v, u, d) become (c, u, d) and
                    // (c, d, v), keeping the patch's orientation.
                    self.tris[t1] = [c, u, d];
                    self.tris[t2] = [c, d, v];
                    edges.remove(&e);
                    edges.insert(edge(c, d));
                    flipped = true;
                    flips += 1;
                    if flips >= max_flips {
                        return Ok(());
                    }
                }
            }
            if !flipped {
                return Ok(());
            }
        }
    }

    pub fn refine(
        &mut self,
        existing: &BTreeSet<(usize, usize)>,
        ctx: &RunContext,
    ) -> Result<(), Cancelled> {
        self.relax(existing, ctx)?;
        loop {
            ctx.checkpoint()?;
            if !self.split() {
                return Ok(());
            }
            self.relax(existing, ctx)?;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    fn normal(p: &Patch, t: [usize; 3]) -> [f64; 3] {
        let [a, b, c] = t.map(|v| p.positions[v]);
        cross(sub(b, a), sub(c, a))
    }

    /// MF-ISSUE-037: a saddle-shaped hole (five rim points on the unit
    /// circle, lifted onto z = 3 (x² − y²)). Starting from a fan whose
    /// triangles all face along the patch normal, refinement's Delaunay
    /// flips used to fold one triangle upside down; every triangle must
    /// still face along the patch normal afterwards.
    #[test]
    fn refining_a_saddle_hole_never_reverses_a_triangle() {
        let n = 5;
        let positions: Vec<[f64; 3]> = (0..n)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                let (x, y) = (a.cos(), a.sin());
                [x, y, 3.0 * (x * x - y * y)]
            })
            .collect();
        // A fan in the winding `triangulate` emits ([m, i, j]).
        let tris: Vec<[usize; 3]> = (1..n - 1).map(|j| [0, j + 1, j]).collect();
        let mut p = Patch {
            sigma: vec![0.3; n],
            positions,
            tris,
            n_boundary: n,
        };
        // Splits and flips both keep the sum of the triangles' area vectors
        // (it is the rim's vector area), so this is the patch normal before
        // and after.
        let patch = |p: &Patch| {
            p.tris
                .iter()
                .map(|&t| normal(p, t))
                .fold([0.0; 3], |s, v| [s[0] + v[0], s[1] + v[1], s[2] + v[2]])
        };
        let before = patch(&p);
        assert!(p.tris.iter().all(|&t| dot(normal(&p, t), before) > 0.0));

        p.refine(&BTreeSet::new(), &RunContext::none()).unwrap();
        assert!(p.tris.len() > 3, "refinement added vertices");
        let after = patch(&p);
        let reversed: Vec<[usize; 3]> = p
            .tris
            .iter()
            .copied()
            .filter(|&t| dot(normal(&p, t), after) <= 0.0)
            .collect();
        assert!(
            reversed.is_empty(),
            "{} of {} triangles face against the patch: {reversed:?}",
            reversed.len(),
            p.tris.len()
        );
    }
}
