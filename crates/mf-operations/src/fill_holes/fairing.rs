//! Patch fairing (MF-OP-002, after Liepa 2003 §4): move a refined patch's
//! new vertices so the patch meets the surface around the hole smoothly.
//!
//! `smooth` solves Δ²x = 0 and `curvature` Δ³x = 0 at the patch's interior
//! vertices, with the scale-dependent umbrella Laplacian
//! Δx_i = (2 / E_i) Σ_j (x_j − x_i) / |e_ij|, where E_i is the summed length
//! of the edges at i. The hole's rim and the existing rings around it (one
//! for Δ², two for Δ³) are read-only: they give the patch the surface's
//! position and slope (C1), and for Δ³ its curvature too (C2).
//!
//! Writing Δ = D⁻¹K (K the weighted graph Laplacian, symmetric; D the
//! diagonal E_i / 2), the equations scaled by D at each row are
//! N (D⁻¹N)^(m−1) x = 0 with N = −K, a symmetric matrix that is positive
//! definite on the interior once the rim is fixed: so a conjugate gradient
//! solve, preconditioned by its diagonal (Jacobi), one coordinate at a
//! time. Everything runs single-threaded per patch over vertices and edges
//! in a fixed order, with plain IEEE arithmetic (+ − × ÷ √, no fused
//! multiply-add), so the result is bit-identical on every machine and at
//! every thread count.

use std::collections::{BTreeMap, BTreeSet};

use mf_geometry::Mesh;
use mf_runtime::{Cancelled, RunContext};

use super::refine::Patch;

/// How a fill's patch meets the surface around it (`fill_style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Flat,
    Smooth,
    Curvature,
}

impl Style {
    pub fn parse(word: &str) -> Option<Style> {
        Some(match word {
            "flat" => Style::Flat,
            "smooth" => Style::Smooth,
            "curvature" => Style::Curvature,
            _ => return None,
        })
    }

    /// Power of the Laplacian solved for: 0 for flat (no solve).
    fn power(self) -> usize {
        match self {
            Style::Flat => 0,
            Style::Smooth => 2,
            Style::Curvature => 3,
        }
    }

    /// Existing rings around the hole the solve reads (and never moves).
    pub fn rings(self) -> usize {
        self.power().saturating_sub(1)
    }
}

/// Relative residual a solve must reach.
const TOLERANCE: f64 = 1e-10;

/// A solve that didn't reach [`TOLERANCE`] within its iteration budget
/// (4 × the patch's interior vertex count).
#[derive(Debug, Clone, PartialEq)]
pub struct NotConverged {
    pub iterations: usize,
    pub residual: f64,
}

/// The patch as a local graph: its own vertices first (boundary, then
/// interior, as in [`Patch`]), then the existing ring vertices.
struct Graph {
    /// Per vertex: (neighbour, 1 / edge length), neighbours ascending.
    adjacency: Vec<Vec<(usize, f64)>>,
    /// Σ 1 / |e| per vertex: N's diagonal.
    weight_sum: Vec<f64>,
    /// 2 / E_i (D⁻¹), 0 where a vertex has no edge of positive length.
    inv_area: Vec<f64>,
}

impl Graph {
    fn new(positions: &[[f64; 3]], edges: &BTreeSet<(usize, usize)>) -> Graph {
        let n = positions.len();
        let mut adjacency = vec![Vec::new(); n];
        let mut length_sum = vec![0.0; n];
        for &(a, b) in edges {
            let (p, q) = (positions[a], positions[b]);
            let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            // A zero-length edge (welded-in-place duplicates in a scan's
            // ring) has no direction to weigh: it is left out.
            if len > 0.0 {
                adjacency[a].push((b, 1.0 / len));
                adjacency[b].push((a, 1.0 / len));
                length_sum[a] += len;
                length_sum[b] += len;
            }
        }
        for list in &mut adjacency {
            list.sort_by_key(|&(v, _)| v);
        }
        let weight_sum = adjacency
            .iter()
            .map(|l| l.iter().map(|&(_, w)| w).sum())
            .collect();
        let inv_area = length_sum
            .iter()
            .map(|&e| if e > 0.0 { 2.0 / e } else { 0.0 })
            .collect();
        Graph {
            adjacency,
            weight_sum,
            inv_area,
        }
    }

    /// out = N v.
    fn apply_n(&self, v: &[f64], out: &mut [f64]) {
        for (i, list) in self.adjacency.iter().enumerate() {
            let mut s = self.weight_sum[i] * v[i];
            for &(j, w) in list {
                s -= w * v[j];
            }
            out[i] = s;
        }
    }

    /// out = N (D⁻¹N)^(power−1) v, over the whole local graph. Rows far
    /// from the patch are incomplete (their outer ring isn't loaded) but
    /// never reach an interior row: see the module docs.
    fn apply(&self, power: usize, v: &[f64], out: &mut [f64], scratch: &mut [f64]) {
        self.apply_n(v, out);
        for _ in 1..power {
            for (s, (&o, &d)) in scratch.iter_mut().zip(out.iter().zip(&self.inv_area)) {
                *s = d * o;
            }
            self.apply_n(scratch, out);
        }
    }

    /// The operator's diagonal at vertex `i`.
    fn diagonal(&self, power: usize, i: usize) -> f64 {
        // c = N e_i, nonzero on i and its neighbours (sorted, i included).
        let mut c: Vec<(usize, f64)> = self.adjacency[i].iter().map(|&(j, w)| (j, -w)).collect();
        let at = c.partition_point(|&(j, _)| j < i);
        c.insert(at, (i, self.weight_sum[i]));
        match power {
            // e_iᵀ N D⁻¹ N e_i = Σ_k c_k² / D_k.
            2 => c.iter().map(|&(k, ck)| ck * ck * self.inv_area[k]).sum(),
            // e_iᵀ N D⁻¹ N D⁻¹ N e_i = aᵀ N a with a = D⁻¹ c.
            _ => {
                let a: Vec<(usize, f64)> = c
                    .iter()
                    .map(|&(k, ck)| (k, self.inv_area[k] * ck))
                    .collect();
                let value = |v: usize| {
                    a.binary_search_by_key(&v, |&(k, _)| k)
                        .map_or(0.0, |x| a[x].1)
                };
                a.iter()
                    .map(|&(k, ak)| {
                        let mut na = self.weight_sum[k] * ak;
                        for &(l, w) in &self.adjacency[k] {
                            na -= w * value(l);
                        }
                        ak * na
                    })
                    .sum()
            }
        }
    }
}

/// Fair `patch` in place: its interior vertices (`n_boundary..`) solve
/// Δ^m x = 0 with everything else fixed. `ring` lists the existing faces
/// around the hole in mesh vertex IDs (those touching the rim, and for
/// `curvature` those touching the ring around it too); `rim` is the loop's
/// mesh vertex IDs, `patch.positions[..n_boundary]` in order. Returns the
/// most iterations any coordinate's solve took.
pub fn fair(
    patch: &mut Patch,
    rim: &[u32],
    ring: &[[u32; 3]],
    mesh: &Mesh,
    style: Style,
    ctx: &RunContext,
) -> Result<Result<usize, NotConverged>, Cancelled> {
    let power = style.power();
    let nb = patch.n_boundary;
    let np = patch.positions.len();
    let interior = np - nb;
    if power == 0 || interior == 0 {
        return Ok(Ok(0));
    }

    // Local graph: the patch, then ring vertices in order of first use.
    let mut positions = patch.positions.clone();
    let mut local: BTreeMap<u32, usize> = rim.iter().enumerate().map(|(k, &v)| (v, k)).collect();
    let mut edges = BTreeSet::new();
    let mut add = |a: usize, b: usize| {
        if a != b {
            edges.insert((a.min(b), a.max(b)));
        }
    };
    for t in &patch.tris {
        for k in 0..3 {
            add(t[k], t[(k + 1) % 3]);
        }
    }
    for t in ring {
        let ids = t.map(|v| {
            *local.entry(v).or_insert_with(|| {
                positions.push(mesh.corner_position(v));
                positions.len() - 1
            })
        });
        for k in 0..3 {
            add(ids[k], ids[(k + 1) % 3]);
        }
    }
    ctx.checkpoint()?;
    let graph = Graph::new(&positions, &edges);

    // Solve about the rim's centroid, so the tolerance is relative to the
    // hole's own extent rather than its distance from the origin.
    let mut centre = [0.0; 3];
    for p in &positions[..nb] {
        for c in 0..3 {
            centre[c] += p[c];
        }
    }
    let centre = centre.map(|c| c / nb as f64);
    let mut x: [Vec<f64>; 3] =
        [0, 1, 2].map(|c| positions[nb..np].iter().map(|p| p[c] - centre[c]).collect());

    // From the refined (flat) patch. Starting Δ³ from the Δ² solution was
    // tried: it saved under 1% of the Δ³ iterations, for the cost of a
    // whole Δ² solve.
    let iterations = match solve(&graph, power, &positions, &centre, nb, &mut x, ctx)? {
        Ok(k) => k,
        Err(e) => return Ok(Err(e)),
    };
    // Only once every coordinate has converged.
    for (c, solution) in x.iter().enumerate() {
        for (k, &v) in solution.iter().enumerate() {
            patch.positions[nb + k][c] = v + centre[c];
        }
    }
    Ok(Ok(iterations))
}

/// Solve Δ^power x = 0 for the interior (`nb..` of the patch's vertices),
/// each coordinate from the given `x` (centred on `centre`). Returns the
/// most iterations a coordinate took.
fn solve(
    graph: &Graph,
    power: usize,
    positions: &[[f64; 3]],
    centre: &[f64; 3],
    nb: usize,
    x: &mut [Vec<f64>; 3],
    ctx: &RunContext,
) -> Result<Result<usize, NotConverged>, Cancelled> {
    let interior = x[0].len();
    let np = nb + interior;
    let n = positions.len();
    let mut inv_diag = Vec::with_capacity(interior);
    for i in nb..np {
        let d = graph.diagonal(power, i);
        if !(d > 0.0 && d.is_finite()) {
            return Ok(Err(NotConverged {
                iterations: 0,
                residual: f64::NAN,
            }));
        }
        inv_diag.push(1.0 / d);
    }
    let max_iterations = 4 * interior;
    let mut most = 0;
    let mut full = vec![0.0; n];
    let mut out = vec![0.0; n];
    let mut scratch = vec![0.0; n];
    for (c, xc) in x.iter_mut().enumerate() {
        // b = −(S x_F) at the interior rows, x_F the fixed coordinates.
        for (f, p) in full.iter_mut().zip(positions) {
            *f = p[c] - centre[c];
        }
        full[nb..np].fill(0.0);
        graph.apply(power, &full, &mut out, &mut scratch);
        let b: Vec<f64> = out[nb..np].iter().map(|&v| -v).collect();
        let b_norm = norm(&b);
        if b_norm == 0.0 {
            // The fixed coordinates are all at the centroid: S is positive
            // definite, so x = 0 is the exact solution.
            xc.fill(0.0);
            continue;
        }
        let mut matvec = |v: &[f64], into: &mut [f64]| {
            full.fill(0.0);
            full[nb..np].copy_from_slice(v);
            graph.apply(power, &full, &mut out, &mut scratch);
            into.copy_from_slice(&out[nb..np]);
        };
        match conjugate_gradient(&mut matvec, &b, b_norm, &inv_diag, xc, max_iterations, ctx)? {
            Ok(k) => most = most.max(k),
            Err(e) => return Ok(Err(e)),
        }
    }
    Ok(Ok(most))
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

/// Jacobi-preconditioned conjugate gradient for S x = b from the given x,
/// to ‖r‖ ≤ [`TOLERANCE`] ‖b‖ within `max_iterations`.
fn conjugate_gradient(
    matvec: &mut impl FnMut(&[f64], &mut [f64]),
    b: &[f64],
    b_norm: f64,
    inv_diag: &[f64],
    x: &mut [f64],
    max_iterations: usize,
    ctx: &RunContext,
) -> Result<Result<usize, NotConverged>, Cancelled> {
    let n = b.len();
    let mut r = vec![0.0; n];
    matvec(x, &mut r);
    for (ri, &bi) in r.iter_mut().zip(b) {
        *ri = bi - *ri;
    }
    let goal = TOLERANCE * b_norm;
    let mut residual = norm(&r);
    let mut z: Vec<f64> = r.iter().zip(inv_diag).map(|(r, d)| r * d).collect();
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut sp = vec![0.0; n];
    for k in 0..max_iterations {
        if residual <= goal {
            return Ok(Ok(k));
        }
        if !residual.is_finite() {
            break;
        }
        ctx.checkpoint()?;
        matvec(&p, &mut sp);
        let alpha = rz / dot(&p, &sp);
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * sp[i];
        }
        residual = norm(&r);
        for i in 0..n {
            z[i] = r[i] * inv_diag[i];
        }
        let rz_next = dot(&r, &z);
        let beta = rz_next / rz;
        rz = rz_next;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    if residual <= goal {
        return Ok(Ok(max_iterations));
    }
    Ok(Err(NotConverged {
        iterations: max_iterations,
        residual: residual / b_norm,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A 1-D chain Laplacian (tridiagonal 2, −1) of size n: SPD.
    fn chain(v: &[f64], out: &mut [f64]) {
        let n = v.len();
        for i in 0..n {
            let left = if i > 0 { v[i - 1] } else { 0.0 };
            let right = if i + 1 < n { v[i + 1] } else { 0.0 };
            out[i] = 2.0 * v[i] - left - right;
        }
    }

    #[test]
    fn conjugate_gradient_solves_an_spd_system() {
        let n = 50;
        let b = vec![1.0; n];
        let mut x = vec![0.0; n];
        let it = conjugate_gradient(
            &mut chain,
            &b,
            norm(&b),
            &vec![0.5; n],
            &mut x,
            4 * n,
            &RunContext::none(),
        )
        .unwrap()
        .unwrap();
        assert!(it <= n + 5, "{it} iterations");
        let mut r = vec![0.0; n];
        chain(&x, &mut r);
        let err: f64 = r.iter().map(|v| (v - 1.0).abs()).fold(0.0, f64::max);
        assert!(err < 1e-8, "{err}");
    }

    /// Out of iterations: a named failure with the residual reached, never
    /// a quietly unfinished answer.
    #[test]
    fn conjugate_gradient_reports_not_converging() {
        let n = 50;
        let b = vec![1.0; n];
        let mut x = vec![0.0; n];
        let e = conjugate_gradient(
            &mut chain,
            &b,
            norm(&b),
            &vec![0.5; n],
            &mut x,
            3,
            &RunContext::none(),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(e.iterations, 3);
        assert!(e.residual > TOLERANCE, "{e:?}");
    }
}
