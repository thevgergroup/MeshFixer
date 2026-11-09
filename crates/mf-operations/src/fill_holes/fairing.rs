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
//! N (D⁻¹N)^(m−1) x = 0 with N = −K: restricted to the interior, a
//! symmetric positive definite matrix S. It is assembled explicitly and
//! solved directly (MF-ISSUE-053: Jacobi-preconditioned CG needed minutes
//! on the largest holes): a fill-reducing ordering (approximate minimum
//! degree, from `faer`, integer-only and deterministic), then a
//! supernodal sparse Cholesky factorisation and a solve per coordinate,
//! checked against the operator itself and refined once if needed.
//!
//! The numeric work is our own scalar code, run single-threaded per patch
//! in a fixed order with plain IEEE arithmetic (+ − × ÷ √, no fused
//! multiply-add), so the result is bit-identical on every machine and at
//! every thread count; faer's SIMD kernels would choose a vector width per
//! CPU and with it a summation order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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

/// Relative residual a solve must reach (‖S x − b‖ ≤ this × ‖b‖).
const TOLERANCE: f64 = 1e-10;

/// Why a patch couldn't be faired. Named (`fairing_did_not_converge`) and
/// never replaced by a flat patch.
#[derive(Debug, Clone, PartialEq)]
pub enum Failed {
    /// A pivot of the Cholesky factorisation was not positive (`row` in
    /// the factorisation's order).
    NotPositiveDefinite { row: usize, pivot: f64 },
    /// The solve produced a NaN or an infinity.
    NotFinite,
    /// Still above [`TOLERANCE`] after one step of refinement.
    Residual(f64),
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failed::NotPositiveDefinite { row, pivot } => write!(
                f,
                "factorisation failed: matrix not positive definite at row {row} (pivot {pivot:e})"
            ),
            Failed::NotFinite => write!(f, "solution not finite"),
            Failed::Residual(r) => write!(
                f,
                "relative residual {r:e} after refinement, above the {TOLERANCE:e} required"
            ),
        }
    }
}

/// A faired patch: how many vertices moved and the factor's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Faired {
    pub vertices: usize,
    pub factor_nonzeros: usize,
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
}

/// Fair `patch` in place: its interior vertices (`n_boundary..`) solve
/// Δ^m x = 0 with everything else fixed. `ring` lists the existing faces
/// around the hole in mesh vertex IDs (those touching the rim, and for
/// `curvature` those touching the ring around it too); `rim` is the loop's
/// mesh vertex IDs, `patch.positions[..n_boundary]` in order.
pub fn fair(
    patch: &mut Patch,
    rim: &[u32],
    ring: &[[u32; 3]],
    mesh: &Mesh,
    style: Style,
    ctx: &RunContext,
) -> Result<Result<Faired, Failed>, Cancelled> {
    let power = style.power();
    let nb = patch.n_boundary;
    let np = patch.positions.len();
    let interior = np - nb;
    if power == 0 || interior == 0 {
        return Ok(Ok(Faired {
            vertices: 0,
            factor_nonzeros: 0,
        }));
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

    // Solve about the rim's centroid, so the residual is relative to the
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

    let factor_nonzeros = match solve(&graph, power, &positions, &centre, nb, &mut x, ctx)? {
        Ok(k) => k,
        Err(e) => return Ok(Err(e)),
    };
    // Only once every coordinate has solved.
    for (c, solution) in x.iter().enumerate() {
        for (k, &v) in solution.iter().enumerate() {
            patch.positions[nb + k][c] = v + centre[c];
        }
    }
    Ok(Ok(Faired {
        vertices: interior,
        factor_nonzeros,
    }))
}

impl Graph {
    /// Rows of R · D⁻¹ for each needed row, where R is given row-wise as
    /// sparse (column, value) lists: out row i = Σ_k N_ik d_k R_k.
    /// Columns are interior indices; rows are local vertices. Rows whose
    /// `keep` is false are left empty. Accumulates through a dense scratch
    /// in a fixed order, so the sums are deterministic.
    fn times_n(
        &self,
        r: &[Vec<(usize, f64)>],
        scale: bool,
        keep: &[bool],
        width: usize,
        ctx: &RunContext,
    ) -> Result<Vec<Vec<(usize, f64)>>, Cancelled> {
        let mut acc = vec![0.0; width];
        let mut seen = vec![false; width];
        let mut out = vec![Vec::new(); r.len()];
        for (i, row) in out.iter_mut().enumerate() {
            if i % 256 == 0 {
                ctx.checkpoint()?;
            }
            if !keep[i] {
                continue;
            }
            let mut cols: Vec<usize> = Vec::new();
            let mut add = |k: usize, w: f64| {
                let d = if scale { self.inv_area[k] } else { 1.0 };
                for &(j, v) in &r[k] {
                    if !seen[j] {
                        seen[j] = true;
                        cols.push(j);
                    }
                    acc[j] += w * (d * v);
                }
            };
            add(i, self.weight_sum[i]);
            for &(k, w) in &self.adjacency[i] {
                add(k, -w);
            }
            cols.sort_unstable();
            *row = cols
                .iter()
                .map(|&j| {
                    let v = acc[j];
                    acc[j] = 0.0;
                    seen[j] = false;
                    (j, v)
                })
                .collect();
        }
        Ok(out)
    }
}

/// S = N (D⁻¹N)^(power−1) restricted to the interior (`nb..np`), as rows
/// of (interior column, value) with columns ascending.
fn assemble(
    graph: &Graph,
    power: usize,
    nb: usize,
    np: usize,
    ctx: &RunContext,
) -> Result<Vec<Vec<(usize, f64)>>, Cancelled> {
    let n = graph.adjacency.len();
    let interior = np - nb;
    // R = N[:, I]: column j of N for interior j.
    let mut r: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for j in nb..np {
        r[j].push((j - nb, graph.weight_sum[j]));
        for &(i, w) in &graph.adjacency[j] {
            r[i].push((j - nb, -w));
        }
    }
    for row in &mut r {
        row.sort_unstable_by_key(|&(j, _)| j);
    }
    // Each further factor needs rows one ring nearer the patch: compute
    // the rows within (power − 1 − step) rings of the interior.
    let mut near = vec![false; n];
    near[nb..np].fill(true);
    let mut rings = vec![near];
    for step in 1..power {
        let prev = &rings[step - 1];
        let mut next = prev.clone();
        for (i, on) in prev.iter().enumerate() {
            if *on {
                for &(k, _) in &graph.adjacency[i] {
                    next[k] = true;
                }
            }
        }
        rings.push(next);
    }
    for step in 1..power {
        r = graph.times_n(&r, true, &rings[power - 1 - step], interior, ctx)?;
    }
    Ok(r.drain(nb..np).collect())
}

const NONE: usize = usize::MAX;

/// Elimination tree and column counts (strictly below the diagonal) of the
/// Cholesky factor of a symmetric matrix given by its upper triangle by
/// columns (`upper[k]` = rows i < k of column k). After Davis's LDL.
fn etree(upper: &[Vec<usize>], ctx: &RunContext) -> Result<(Vec<usize>, Vec<usize>), Cancelled> {
    let n = upper.len();
    let mut parent = vec![NONE; n];
    let mut flag = vec![NONE; n];
    let mut below = vec![0usize; n];
    for k in 0..n {
        if k % 1024 == 0 {
            ctx.checkpoint()?;
        }
        flag[k] = k;
        for &i0 in &upper[k] {
            let mut i = i0;
            while flag[i] != k {
                if parent[i] == NONE {
                    parent[i] = k;
                }
                below[i] += 1;
                flag[i] = k;
                i = parent[i];
            }
        }
    }
    Ok((parent, below))
}

/// A postorder of a forest given by `parent` (children visited in
/// increasing order), so every subtree is a contiguous range.
fn postorder(parent: &[usize]) -> Vec<usize> {
    let n = parent.len();
    let mut head = vec![NONE; n];
    let mut next = vec![NONE; n];
    // Push children in decreasing order so the lists come out increasing.
    for j in (0..n).rev() {
        if parent[j] != NONE {
            next[j] = head[parent[j]];
            head[parent[j]] = j;
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut stack = Vec::new();
    for root in (0..n).filter(|&j| parent[j] == NONE) {
        stack.push(root);
        while let Some(&top) = stack.last() {
            let child = head[top];
            if child == NONE {
                stack.pop();
                order.push(top);
            } else {
                head[top] = next[child];
                stack.push(child);
            }
        }
    }
    order
}

/// The lower part of C = A Bᵀ, where A is rows `p1..p1 + h` and B rows
/// `p1..p1 + cw` of the column-major block `l` (`m` rows; its first `w`
/// columns): C[r][c] = Σ_t l[p1 + r][t] · l[p1 + c][t] for r ≥ c, into
/// `out` (h × cw, column-major; entries above the diagonal are not
/// written). Each entry is summed over t in increasing order from 0, by
/// element-wise column updates the compiler may vectorise without
/// reordering any sum. (4×4 register tiles were tried: slower here, the
/// work being bound by reading the block.)
fn lower_product(l: &[f64], m: usize, w: usize, p1: usize, h: usize, cw: usize, out: &mut [f64]) {
    for c in 0..cw {
        let dst = &mut out[c * h + c..(c + 1) * h];
        dst.fill(0.0);
        for t in 0..w {
            let a = l[t * m + p1 + c];
            let src = &l[t * m + p1 + c..t * m + p1 + h];
            for (d, sv) in dst.iter_mut().zip(src) {
                *d += sv * a;
            }
        }
    }
}

/// Relaxed supernodes (after CHOLMOD): merge a fundamental supernode into
/// its parent when the parent follows it directly, at the cost of a few
/// explicit zeros, so the dense kernels work on bigger blocks. Returns the
/// first column of each merged supernode, then n.
fn relax(fundamental: &[usize], parent: &[usize], below: &[usize]) -> Vec<usize> {
    // (first column, width, rows, explicit zeros)
    let mut merged: Vec<(usize, usize, usize, usize)> = Vec::new();
    for w in fundamental.windows(2) {
        let (f, end) = (w[0], w[1]);
        let mut cur = (f, end - f, below[f] + 1, 0usize);
        while let Some(&prev) = merged.last() {
            let last = prev.0 + prev.1 - 1;
            if parent[last] != cur.0 {
                break;
            }
            let width = prev.1 + cur.1;
            let rows = prev.1 + cur.2;
            let zeros = prev.3 + cur.3 + prev.1 * (prev.1 + cur.2 - prev.2);
            let share = zeros as f64 / (width * rows) as f64;
            // CHOLMOD's defaults. (Fundamental supernodes only, or other
            // limits, measured within ±10% on a 1000-edge hole.)
            let ok = width <= 4
                || (width <= 16 && share < 0.8)
                || (width <= 48 && share < 0.1)
                || share < 0.05;
            if !ok {
                break;
            }
            merged.pop();
            cur = (prev.0, width, rows, zeros);
        }
        merged.push(cur);
    }
    let mut first: Vec<usize> = merged.iter().map(|m| m.0).collect();
    // `fundamental` ends with n (the column count).
    first.push(fundamental[fundamental.len() - 1]);
    first
}

/// A supernodal Cholesky factorisation L Lᵀ of P S Pᵀ (P: a fill-reducing
/// ordering, then an elimination-tree postorder). Left-looking: each
/// supernode (a run of columns sharing one row structure) is a dense
/// column-major block, updated by its descendants in a fixed order. The
/// dense kernels are element-wise column updates (`a[r] -= b[r] * c`), so
/// the compiler may vectorise them without changing any sum's order.
struct Cholesky {
    /// perm[new] = old.
    perm: Vec<usize>,
    /// First column of each supernode (and n at the end).
    first: Vec<usize>,
    /// Row structure of each supernode (sorted; starts with its columns).
    rows: Vec<Vec<usize>>,
    /// Dense blocks, rows.len() × width, column-major.
    blocks: Vec<Vec<f64>>,
}

impl Cholesky {
    fn factor(
        s: &[Vec<(usize, f64)>],
        ctx: &RunContext,
    ) -> Result<Result<Cholesky, Failed>, Cancelled> {
        let n = s.len();
        let upper_of = |inv: &[usize]| -> Vec<Vec<usize>> {
            let mut upper = vec![Vec::new(); n];
            for (old_i, row) in s.iter().enumerate() {
                let i = inv[old_i];
                for &(old_j, _) in row {
                    let j = inv[old_j];
                    if i < j {
                        upper[j].push(i);
                    }
                }
            }
            upper
        };
        let invert = |perm: &[usize]| {
            let mut inv = vec![0; n];
            for (new, &old) in perm.iter().enumerate() {
                inv[old] = new;
            }
            inv
        };
        let amd = amd_order(s);
        ctx.checkpoint()?;
        let upper = upper_of(&invert(&amd));
        ctx.checkpoint()?;
        let (parent0, below0) = etree(&upper, ctx)?;
        drop(upper);
        // Renumber in postorder: the tree and counts carry over.
        let post = postorder(&parent0);
        let post_inv = invert(&post);
        let perm: Vec<usize> = post.iter().map(|&k| amd[k]).collect();
        let inv = invert(&perm);
        let mut parent = vec![NONE; n];
        let mut below = vec![0; n];
        for (new, &old) in post.iter().enumerate() {
            parent[new] = match parent0[old] {
                NONE => NONE,
                p => post_inv[p],
            };
            below[new] = below0[old];
        }
        ctx.checkpoint()?;

        // Fundamental supernodes: j joins j−1's when j−1's only child
        // path continues into j with one row fewer below.
        let mut children = vec![0u32; n];
        for &p in &parent {
            if p != NONE {
                children[p] += 1;
            }
        }
        let mut fundamental = vec![0];
        for j in 1..n {
            let joins = parent[j - 1] == j && below[j - 1] == below[j] + 1 && children[j] == 1;
            if !joins {
                fundamental.push(j);
            }
        }
        fundamental.push(n);
        let first = relax(&fundamental, &parent, &below);
        let count = first.len() - 1;
        let mut snode_of = vec![0; n];
        for sn in 0..count {
            snode_of[first[sn]..first[sn + 1]].fill(sn);
        }

        // Lower triangle of C = P S Pᵀ by columns, rows ascending.
        let mut lower: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        for (old_i, row) in s.iter().enumerate() {
            let i = inv[old_i];
            for &(old_j, v) in row {
                let j = inv[old_j];
                if i >= j {
                    lower[j].push((i, v));
                }
            }
        }
        for col in &mut lower {
            col.sort_unstable_by_key(|&(i, _)| i);
        }
        ctx.checkpoint()?;

        // Row structures: the supernode's own entries and its children's
        // rows below its first column (children come first: postorder).
        let mut rows: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut mark = vec![NONE; n];
        let mut kids: Vec<Vec<usize>> = vec![Vec::new(); count];
        for sn in 0..count {
            let last = first[sn + 1] - 1;
            if parent[last] != NONE {
                kids[snode_of[parent[last]]].push(sn);
            }
        }
        for sn in 0..count {
            if sn % 256 == 0 {
                ctx.checkpoint()?;
            }
            let (f, end) = (first[sn], first[sn + 1]);
            let mut r: Vec<usize> = (f..end).collect();
            for &i in &r {
                mark[i] = sn;
            }
            for col in &lower[f..end] {
                for &(i, _) in col {
                    if mark[i] != sn {
                        mark[i] = sn;
                        r.push(i);
                    }
                }
            }
            for &k in &kids[sn] {
                for &i in &rows[k] {
                    if i >= f && mark[i] != sn {
                        mark[i] = sn;
                        r.push(i);
                    }
                }
            }
            r.sort_unstable();
            rows[sn] = r;
        }
        drop(kids);
        ctx.checkpoint()?;

        // Numeric, left-looking. `link[J]` lists the supernodes whose next
        // rows fall in J, with `cursor[K]` the position of that next row.
        let mut blocks: Vec<Vec<f64>> = Vec::with_capacity(count);
        let mut link_head = vec![NONE; count];
        let mut link_next = vec![NONE; count];
        let mut cursor = vec![0usize; count];
        let mut pos = vec![0usize; n];
        let mut update: Vec<f64> = Vec::new();
        for sn in 0..count {
            ctx.checkpoint()?;
            let (f, end) = (first[sn], first[sn + 1]);
            let w = end - f;
            let r = &rows[sn];
            let m = r.len();
            for (p, &i) in r.iter().enumerate() {
                pos[i] = p;
            }
            let mut block = vec![0.0; m * w];
            for (c, col) in lower[f..end].iter().enumerate() {
                for &(i, v) in col {
                    block[c * m + pos[i]] += v;
                }
            }
            // Updates from each descendant K, in list order.
            let mut k = link_head[sn];
            link_head[sn] = NONE;
            while k != NONE {
                let next_k = link_next[k];
                let rk = &rows[k];
                let mk = rk.len();
                let wk = first[k + 1] - first[k];
                let p1 = cursor[k];
                let mut p2 = p1;
                while p2 < mk && rk[p2] < end {
                    p2 += 1;
                }
                // C = L_K[p1.., :] · L_K[p1..p2, :]ᵀ, then subtract into
                // the block at the matching rows and columns.
                let (h, cw) = (mk - p1, p2 - p1);
                update.clear();
                update.resize(h * cw, 0.0);
                let lk = &blocks[k];
                lower_product(lk, mk, wk, p1, h, cw, &mut update);
                for c in 0..cw {
                    let col = rk[p1 + c] - f;
                    let dst = &mut block[col * m..(col + 1) * m];
                    for (rr, u) in update[c * h + c..(c + 1) * h].iter().enumerate() {
                        dst[pos[rk[p1 + c + rr]]] -= u;
                    }
                }
                cursor[k] = p2;
                if p2 < mk {
                    let target = snode_of[rk[p2]];
                    link_next[k] = link_head[target];
                    link_head[target] = k;
                }
                k = next_k;
                ctx.checkpoint()?;
            }
            // Dense Cholesky of the block: left-looking in panels of four
            // columns (each panel first takes the earlier columns' update
            // through the tiled product, then is factored on its own).
            let mut jb = 0;
            while jb < w {
                ctx.checkpoint()?;
                let bw = (w - jb).min(4);
                let h = m - jb;
                if jb > 0 {
                    update.clear();
                    update.resize(h * bw, 0.0);
                    lower_product(&block, m, jb, jb, h, bw, &mut update);
                    for c in 0..bw {
                        let col = &mut block[(jb + c) * m + jb..(jb + c + 1) * m];
                        for (rr, u) in update[c * h..(c + 1) * h].iter().enumerate().skip(c) {
                            col[rr] -= u;
                        }
                    }
                }
                for j in jb..jb + bw {
                    let d = block[j * m + j];
                    if !(d > 0.0 && d.is_finite()) {
                        return Ok(Err(Failed::NotPositiveDefinite {
                            row: f + j,
                            pivot: d,
                        }));
                    }
                    let d = d.sqrt();
                    block[j * m + j] = d;
                    for v in &mut block[j * m + j + 1..(j + 1) * m] {
                        *v /= d;
                    }
                    for c in j + 1..jb + bw {
                        let a = block[j * m + c];
                        let (left, right) = block.split_at_mut(c * m);
                        let src = &left[j * m + c..(j + 1) * m];
                        for (dv, sv) in right[c..m].iter_mut().zip(src) {
                            *dv -= sv * a;
                        }
                    }
                }
                jb += bw;
            }
            blocks.push(block);
            cursor[sn] = w;
            if w < m {
                let target = snode_of[r[w]];
                link_next[sn] = link_head[target];
                link_head[target] = sn;
            }
        }
        Ok(Ok(Cholesky {
            perm,
            first,
            rows,
            blocks,
        }))
    }

    fn nonzeros(&self) -> usize {
        self.blocks.iter().map(Vec::len).sum()
    }

    /// x = S⁻¹ b.
    fn solve(&self, b: &[f64]) -> Vec<f64> {
        let n = b.len();
        let mut x: Vec<f64> = self.perm.iter().map(|&old| b[old]).collect();
        let count = self.rows.len();
        for sn in 0..count {
            let (f, w) = (self.first[sn], self.first[sn + 1] - self.first[sn]);
            let (r, l) = (&self.rows[sn], &self.blocks[sn]);
            let m = r.len();
            for j in 0..w {
                let xj = x[f + j] / l[j * m + j];
                x[f + j] = xj;
                for p in j + 1..m {
                    x[r[p]] -= l[j * m + p] * xj;
                }
            }
        }
        for sn in (0..count).rev() {
            let (f, w) = (self.first[sn], self.first[sn + 1] - self.first[sn]);
            let (r, l) = (&self.rows[sn], &self.blocks[sn]);
            let m = r.len();
            for j in (0..w).rev() {
                let mut xj = x[f + j];
                for p in j + 1..m {
                    xj -= l[j * m + p] * x[r[p]];
                }
                x[f + j] = xj / l[j * m + j];
            }
        }
        let mut out = vec![0.0; n];
        for (new, &old) in self.perm.iter().enumerate() {
            out[old] = x[new];
        }
        out
    }
}

/// Approximate minimum degree ordering of S's pattern (faer's AMD: integer
/// only, no randomness, so the same pattern always gives the same order).
fn amd_order(s: &[Vec<(usize, f64)>]) -> Vec<usize> {
    use faer::dyn_stack::{MemBuffer, MemStack};
    use faer::sparse::SymbolicSparseColMat;
    use faer::sparse::linalg::amd;
    let n = s.len();
    let mut col_ptr = Vec::with_capacity(n + 1);
    let mut row_idx = Vec::new();
    col_ptr.push(0usize);
    for row in s {
        // S is symmetric: its rows are its columns.
        row_idx.extend(row.iter().map(|&(j, _)| j));
        col_ptr.push(row_idx.len());
    }
    let nnz = row_idx.len();
    let pattern = SymbolicSparseColMat::new_checked(n, n, col_ptr, None, row_idx);
    let mut perm = vec![0usize; n];
    let mut perm_inv = vec![0usize; n];
    let mut buffer = MemBuffer::new(amd::order_scratch::<usize>(n, nnz));
    match amd::order(
        &mut perm,
        &mut perm_inv,
        pattern.as_ref(),
        amd::Control::default(),
        MemStack::new(&mut buffer),
    ) {
        Ok(_) => perm,
        // Only an index overflow, impossible at fill_holes' sizes (at most
        // ~60k unknowns): the natural order is correct, just slower.
        Err(e) => panic!("AMD ordering failed on a {n}×{n} fairing matrix: {e:?}"),
    }
}

/// Solve S x = b for each coordinate (b = −S_IF x_F, the fixed vertices'
/// pull), verify against the operator, refine once if needed.
fn solve(
    graph: &Graph,
    power: usize,
    positions: &[[f64; 3]],
    centre: &[f64; 3],
    nb: usize,
    x: &mut [Vec<f64>; 3],
    ctx: &RunContext,
) -> Result<Result<usize, Failed>, Cancelled> {
    let interior = x[0].len();
    let np = nb + interior;
    let n = positions.len();
    let s = assemble(graph, power, nb, np, ctx)?;
    ctx.checkpoint()?;
    let chol = match Cholesky::factor(&s, ctx)? {
        Ok(f) => f,
        Err(e) => return Ok(Err(e)),
    };
    ctx.checkpoint()?;
    let mut full = vec![0.0; n];
    let mut out = vec![0.0; n];
    let mut scratch = vec![0.0; n];
    for (c, xc) in x.iter_mut().enumerate() {
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
        let mut residual = |v: &[f64]| -> Vec<f64> {
            full.fill(0.0);
            full[nb..np].copy_from_slice(v);
            graph.apply(power, &full, &mut out, &mut scratch);
            b.iter().zip(&out[nb..np]).map(|(bi, si)| bi - si).collect()
        };
        let mut sol = chol.solve(&b);
        let mut r = residual(&sol);
        if norm(&r) > TOLERANCE * b_norm {
            // One step of iterative refinement.
            let fix = chol.solve(&r);
            for (s, f) in sol.iter_mut().zip(&fix) {
                *s += f;
            }
            r = residual(&sol);
        }
        if !sol.iter().all(|v| v.is_finite()) {
            return Ok(Err(Failed::NotFinite));
        }
        let rel = norm(&r) / b_norm;
        if rel.is_nan() || rel > TOLERANCE {
            return Ok(Err(Failed::Residual(rel)));
        }
        *xc = sol;
        ctx.checkpoint()?;
    }
    Ok(Ok(chol.nonzeros()))
}

fn norm(a: &[f64]) -> f64 {
    a.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A 1-D chain Laplacian (tridiagonal 2, −1) of size n, as rows.
    fn chain(n: usize) -> Vec<Vec<(usize, f64)>> {
        (0..n)
            .map(|i| {
                let mut row = Vec::new();
                if i > 0 {
                    row.push((i - 1, -1.0));
                }
                row.push((i, 2.0));
                if i + 1 < n {
                    row.push((i + 1, -1.0));
                }
                row
            })
            .collect()
    }

    fn times(s: &[Vec<(usize, f64)>], x: &[f64]) -> Vec<f64> {
        s.iter()
            .map(|row| row.iter().map(|&(j, v)| v * x[j]).sum())
            .collect()
    }

    #[test]
    fn cholesky_solves_an_spd_system() {
        let s = chain(50);
        let b: Vec<f64> = (0..50).map(|i| (i % 7) as f64 - 3.0).collect();
        let ldl = Cholesky::factor(&s, &RunContext::none()).unwrap().unwrap();
        let x = ldl.solve(&b);
        let r = times(&s, &x);
        let err = r
            .iter()
            .zip(&b)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(err < 1e-10, "{err}");
    }

    /// A matrix that isn't positive definite fails by name, at its row.
    #[test]
    fn cholesky_names_a_non_positive_pivot() {
        let mut s = chain(5);
        s[3][1].1 = -2.0; // diagonal of row 3
        let e = Cholesky::factor(&s, &RunContext::none())
            .unwrap()
            .err()
            .unwrap();
        assert!(matches!(e, Failed::NotPositiveDefinite { .. }), "{e:?}");
        assert!(
            e.to_string().contains("not positive definite at row"),
            "{e}"
        );
    }
}
