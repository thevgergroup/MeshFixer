//! 3MF row-major affine transforms (D7): 12 values, the 3×3 linear part
//! row by row, then the translation; a point is placed as `p · M + t`.
//! One implementation for the boolean (D10's tool-in-target frame) and
//! intersection detection, so both place every point identically.

use glam::DMat4;

/// `a * b` (`p * a * b`: `a` first).
pub fn compose(a: &[f64; 12], b: &[f64; 12]) -> [f64; 12] {
    let mut out = [0.0; 12];
    for r in 0..4 {
        for c in 0..3 {
            let row = |k: usize| if r < 3 { a[3 * r + k] } else { a[9 + k] };
            out[3 * r + c] = row(0) * b[c]
                + row(1) * b[3 + c]
                + row(2) * b[6 + c]
                + if r == 3 { b[9 + c] } else { 0.0 };
        }
    }
    out
}

/// The linear part's determinant (negative: the transform mirrors).
pub fn determinant(t: &[f64; 12]) -> f64 {
    t[0] * (t[4] * t[8] - t[5] * t[7]) - t[1] * (t[3] * t[8] - t[5] * t[6])
        + t[2] * (t[3] * t[7] - t[4] * t[6])
}

/// The inverse, `None` if singular.
pub fn inverse(t: &[f64; 12]) -> Option<[f64; 12]> {
    let m = [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]];
    let det = determinant(t);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let mut inv = [[0.0; 3]; 3];
    for (r, row) in inv.iter_mut().enumerate() {
        for (c, x) in row.iter_mut().enumerate() {
            *x = cof(c, r) / det;
        }
    }
    let tr = [t[9], t[10], t[11]];
    let it: Vec<f64> = (0..3)
        .map(|c| -(tr[0] * inv[0][c] + tr[1] * inv[1][c] + tr[2] * inv[2][c]))
        .collect();
    Some([
        inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1],
        inv[2][2], it[0], it[1], it[2],
    ])
}

/// `p` placed by `t`.
pub fn apply(t: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    [
        t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9],
        t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
        t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11],
    ]
}

/// The same transform as a column-vector `DMat4` (exact: entries copied).
pub fn to_dmat4(t: &[f64; 12]) -> DMat4 {
    DMat4::from_cols_array(&[
        t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10], t[11],
        1.0,
    ])
}

/// The 12 values of an affine `DMat4` (exact: entries copied; the
/// projective row is ignored).
pub fn from_dmat4(m: &DMat4) -> [f64; 12] {
    let c = m.to_cols_array();
    [
        c[0], c[1], c[2], c[4], c[5], c[6], c[8], c[9], c[10], c[12], c[13], c[14],
    ]
}

/// Where the tool's points land in the target's frame (D10): the tool's
/// placement followed by the inverse of the target's. `None` if the
/// target's can't be inverted.
pub fn tool_to_target(tool: &[f64; 12], target: &[f64; 12]) -> Option<[f64; 12]> {
    Some(compose(tool, &inverse(target)?))
}

/// A world-space normal `n`, carried into `t`'s object space (`t` is
/// object→world, i.e. `p_world = p_object · t + translation`, [`apply`]'s
/// convention): `M · n`, `M` being `t`'s linear part's rows taken
/// directly — never `t`'s inverse applied to `n` as if it were a point.
/// Points and planes both land correctly under a uniform scale or a pure
/// rotation (inverse-linear and this agree there), but a non-uniform
/// scale needs this one: a plane's normal doesn't scale the way its
/// points do (MF-SPLIT-PARITY-001, split_parity.rs). Every caller that
/// needs a plane's normal in object space from a world one — Apply
/// (`mf_operations::split::normal_to_object`), Split's region preview
/// and the peg preview worker — must go through this, so the preview
/// always cuts the same regions Apply would.
pub fn world_normal_to_object(t: &[f64; 12], n: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| t[3 * r] * n[0] + t[3 * r + 1] * n[1] + t[3 * r + 2] * n[2])
}
