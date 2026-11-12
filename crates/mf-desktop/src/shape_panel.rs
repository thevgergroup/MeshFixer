//! Editable shapes in the desktop (parametric-shapes brief): the helpers
//! the Parts library, Connectors, Connect and the selection panel share.

/// The linear part of a 3MF 3×4 placement (row-vector convention: its
/// first three values are where local x goes), as a matrix.
pub fn rotation(t: &[f64; 12]) -> glam::DMat3 {
    glam::DMat3::from_cols(
        glam::DVec3::new(t[0], t[1], t[2]),
        glam::DVec3::new(t[3], t[4], t[5]),
        glam::DVec3::new(t[6], t[7], t[8]),
    )
}
