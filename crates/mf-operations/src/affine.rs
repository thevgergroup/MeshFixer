//! Small helpers for 3MF row-vector affines (`p' = p·M + t`, D7).

/// Inverse of a 3MF row-vector affine (`p' = p·M + t`), `None` if
/// singular.
pub(crate) fn inverse(t: &[f64; 12]) -> Option<[f64; 12]> {
    let m = [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
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
    let it: [f64; 3] =
        std::array::from_fn(|c| -(tr[0] * inv[0][c] + tr[1] * inv[1][c] + tr[2] * inv[2][c]));
    Some([
        inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1],
        inv[2][2], it[0], it[1], it[2],
    ])
}

/// `p·M + t`.
pub(crate) fn apply(t: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|c| t[c] * p[0] + t[3 + c] * p[1] + t[6 + c] * p[2] + t[9 + c])
}

/// A direction under the linear part only: `d·M`.
pub(crate) fn apply_linear(t: &[f64; 12], d: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|c| t[c] * d[0] + t[3 + c] * d[1] + t[6 + c] * d[2])
}

/// Determinant of the linear part.
pub(crate) fn determinant(t: &[f64; 12]) -> f64 {
    t[0] * (t[4] * t[8] - t[5] * t[7]) - t[1] * (t[3] * t[8] - t[5] * t[6])
        + t[2] * (t[3] * t[7] - t[4] * t[6])
}

/// The identity placement.
pub(crate) const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
