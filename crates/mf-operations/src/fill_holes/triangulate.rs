//! Minimum-weight triangulation of a hole's boundary polygon (Liepa 2003).
//!
//! The weight of a triangulation is the pair (largest dihedral angle between
//! adjacent triangles, total area), compared lexicographically: avoid folds
//! first, then prefer small area. Dihedrals include the existing faces
//! across the boundary edges, so the patch meets its surroundings smoothly.

/// Weight of a (partial) triangulation.
#[derive(Clone, Copy, PartialEq)]
struct Weight {
    angle: f64,
    area: f64,
}

impl Weight {
    const ZERO: Weight = Weight {
        angle: 0.0,
        area: 0.0,
    };
    const INFINITE: Weight = Weight {
        angle: f64::INFINITY,
        area: f64::INFINITY,
    };

    fn better_than(&self, other: &Weight) -> bool {
        match self.angle.total_cmp(&other.angle) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.area < other.area,
        }
    }
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

/// Unit normal (zero for a degenerate triangle) and area.
fn normal_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> ([f64; 3], f64) {
    let n = cross(sub(b, a), sub(c, a));
    let len = dot(n, n).sqrt();
    if len > 0.0 {
        (n.map(|x| x / len), 0.5 * len)
    } else {
        ([0.0; 3], 0.0)
    }
}

/// Angle between two unit normals; a degenerate (zero) normal counts as a
/// right angle so degenerate triangles are never preferred.
fn dihedral(n1: [f64; 3], n2: [f64; 3]) -> f64 {
    if dot(n1, n1) == 0.0 || dot(n2, n2) == 0.0 {
        return std::f64::consts::FRAC_PI_2;
    }
    dot(n1, n2).clamp(-1.0, 1.0).acos()
}

/// Triangulate the polygon `p[0..n]` (the loop's vertices in loop order).
/// `outer[k]` is the unit normal of the existing face across boundary edge
/// `k → k+1` (`k = n-1` is the closing edge `n-1 → 0`).
///
/// Returns triangles as index triples into `p`, emitted as `[m, i, j]` so
/// each boundary edge `k → k+1` appears reversed (`k+1 → k`): the patch is
/// oriented consistently with the faces it meets.
pub fn triangulate(p: &[[f64; 3]], outer: &[[f64; 3]]) -> Vec<[usize; 3]> {
    let n = p.len();
    assert!(n >= 3 && outer.len() == n);
    // w[i][j], choice[i][j] for 0 <= i < j < n.
    let idx = |i: usize, j: usize| i * n + j;
    let mut w = vec![Weight::INFINITE; n * n];
    let mut choice = vec![usize::MAX; n * n];
    let mut tri_normal = vec![[0.0; 3]; n * n];
    for i in 0..n - 1 {
        w[idx(i, i + 1)] = Weight::ZERO;
    }
    // Normal of the triangle on the far side of chord (i, j): either the
    // existing boundary face (j = i + 1) or the chosen sub-triangle.
    let side_normal = |i: usize, j: usize, tri_normal: &Vec<[f64; 3]>| -> [f64; 3] {
        if j == i + 1 {
            outer[i]
        } else {
            tri_normal[idx(i, j)]
        }
    };
    for len in 2..n {
        for i in 0..n - len {
            let j = i + len;
            let mut best = Weight::INFINITE;
            let mut best_m = usize::MAX;
            let mut best_n = [0.0; 3];
            for m in i + 1..j {
                let (a, b) = (w[idx(i, m)], w[idx(m, j)]);
                if a.angle.is_infinite() || b.angle.is_infinite() {
                    continue;
                }
                // Emitted winding [m, i, j].
                let (nrm, area) = normal_area(p[m], p[i], p[j]);
                let mut angle = a
                    .angle
                    .max(b.angle)
                    .max(dihedral(nrm, side_normal(i, m, &tri_normal)))
                    .max(dihedral(nrm, side_normal(m, j, &tri_normal)));
                if i == 0 && j == n - 1 {
                    // The closing boundary edge n-1 → 0.
                    angle = angle.max(dihedral(nrm, outer[n - 1]));
                }
                let cand = Weight {
                    angle,
                    area: a.area + b.area + area,
                };
                if best_m == usize::MAX || cand.better_than(&best) {
                    best = cand;
                    best_m = m;
                    best_n = nrm;
                }
            }
            w[idx(i, j)] = best;
            choice[idx(i, j)] = best_m;
            tri_normal[idx(i, j)] = best_n;
        }
    }
    let mut out = Vec::with_capacity(n - 2);
    let mut stack = vec![(0usize, n - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let m = choice[idx(i, j)];
        out.push([m, i, j]);
        stack.push((i, m));
        stack.push((m, j));
    }
    out
}
