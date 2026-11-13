//! The object-to-world transform (MF-ISSUE-042): every [`mf_document::Object`]
//! carries a 3MF row-major 3x4 affine, and this is the one place that turns
//! it into a matrix the renderer and picker can use.

use glam::{DMat4, DVec4};

/// `transform` is the 3MF row-major 3x4 order: `m00 m01 m02 m10 m11 m12 m20
/// m21 m22 m30 m31 m32`, the last three the translation. 3MF applies it as a
/// row vector times the matrix (`p' = p * M`); this returns the equivalent
/// column-vector matrix (`model * p`), the convention this codebase already
/// uses for the camera (`view_proj * p`).
pub fn model_matrix(transform: &[f64; 12]) -> DMat4 {
    let t = transform;
    DMat4::from_cols(
        DVec4::new(t[0], t[1], t[2], 0.0),
        DVec4::new(t[3], t[4], t[5], 0.0),
        DVec4::new(t[6], t[7], t[8], 0.0),
        DVec4::new(t[9], t[10], t[11], 1.0),
    )
}
