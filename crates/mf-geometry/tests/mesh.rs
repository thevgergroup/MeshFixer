#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::{cube, translated};
use mf_geometry::{GeometryError, Mesh};

#[test]
fn new_rejects_out_of_range() {
    let err = Mesh::new(
        vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 3]],
    )
    .unwrap_err();
    assert_eq!(
        err,
        GeometryError::IndexOutOfRange {
            triangle: 0,
            index: 3
        }
    );
}

#[test]
fn new_rejects_nan() {
    let err = Mesh::new(
        vec![[0.0; 3], [f64::NAN, 0.0, 0.0], [0.0, 1.0, 0.0]],
        vec![[0, 1, 2]],
    )
    .unwrap_err();
    assert_eq!(err, GeometryError::NonFinite { vertex: 1 });
}

#[test]
fn hash_stable() {
    let a = cube(1.0);
    assert_eq!(a.content_hash(), cube(1.0).content_hash());
    assert_ne!(
        a.content_hash(),
        translated(&a, [1e-9, 0.0, 0.0]).content_hash()
    );
}

#[test]
fn hash_order_sensitive() {
    let a = cube(1.0);
    let mut tris = a.triangles().to_vec();
    tris.swap(0, 1);
    let b = Mesh::new(a.positions().to_vec(), tris).unwrap();
    assert_ne!(a.content_hash(), b.content_hash());
}

#[test]
fn bbox_cube() {
    let bb = cube(1.0).bounding_box().unwrap();
    assert_eq!(bb.min, [0.0, 0.0, 0.0]);
    assert_eq!(bb.max, [1.0, 1.0, 1.0]);
    assert_eq!(bb.center(), [0.5, 0.5, 0.5]);
    assert!(Mesh::empty().bounding_box().is_none());
}

#[test]
fn clone_is_cheap_and_shares_hash() {
    let a = cube(1.0);
    let h = a.content_hash();
    let b = a.clone();
    assert_eq!(b.content_hash(), h);
    assert_eq!(b.triangle_count(), 12);
    assert_eq!(b.vertex_count(), 8);
}

#[test]
fn heap_bytes_cube() {
    let m = cube(1.0);
    let buffers = 8 * 24 + 12 * 12;
    assert!(
        m.heap_bytes() >= buffers && m.heap_bytes() < buffers + 512,
        "{}",
        m.heap_bytes()
    );
    let t = m.topology();
    assert!(t.heap_bytes() >= 12 * 4, "components hold every face id");
}
