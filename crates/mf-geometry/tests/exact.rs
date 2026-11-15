#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The public exact layer (ADR-0011): the one place "on plane", "same
//! point" and "inside" are decided, with no tolerance.

use mf_fixtures::{cube, scaled, translated, uv_sphere};
use mf_geometry::exact::{Containment, Side, on_plane, point_in_solid, same_point, side_of};

const XY: [[f64; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];

#[test]
fn side_of_follows_the_plane_s_right_handed_normal() {
    assert_eq!(side_of([0.3, 0.3, 1.0], XY), Side::Positive);
    assert_eq!(side_of([0.3, 0.3, -1.0], XY), Side::Negative);
    assert_eq!(side_of([7.0, -3.0, 0.0], XY), Side::On);
}

#[test]
fn on_plane_is_exact_an_ulp_off_is_off() {
    // A plane through three points no axis-aligned shortcut sees.
    let plane = [[0.1, 0.2, 0.3], [1.7, -0.4, 2.9], [-3.3, 5.5, 0.7]];
    for p in plane {
        assert!(on_plane(p, plane));
    }
    let off = [plane[0][0], plane[0][1], f64::next_up(plane[0][2])];
    assert!(!on_plane(off, plane));
    assert!(!on_plane([0.0, 0.0, f64::MIN_POSITIVE], XY));
}

#[test]
fn same_point_is_bitwise_equal_coordinates() {
    let p = [19.318516525781366, 5.176380902050415, 0.0];
    assert!(same_point(p, p));
    assert!(same_point([0.0, 0.0, 0.0], [-0.0, 0.0, 0.0]));
    assert!(!same_point(p, [p[0], f64::next_up(p[1]), p[2]]));
}

#[test]
fn point_in_solid_by_exact_crossings() {
    let c = cube(2.0);
    assert_eq!(
        point_in_solid(&c, [1.0, 1.0, 1.0]),
        Some(Containment::Inside)
    );
    assert_eq!(
        point_in_solid(&c, [3.0, 1.0, 1.0]),
        Some(Containment::Outside)
    );
    // On a face, an edge and a corner.
    assert_eq!(
        point_in_solid(&c, [1.0, 1.0, 2.0]),
        Some(Containment::OnSurface)
    );
    assert_eq!(
        point_in_solid(&c, [2.0, 2.0, 1.0]),
        Some(Containment::OnSurface)
    );
    assert_eq!(
        point_in_solid(&c, [0.0, 0.0, 0.0]),
        Some(Containment::OnSurface)
    );
    // An ulp inside and outside a face.
    assert_eq!(
        point_in_solid(&c, [1.0, 1.0, 2.0f64.next_down()]),
        Some(Containment::Inside)
    );
    assert_eq!(
        point_in_solid(&c, [1.0, 1.0, 2.0f64.next_up()]),
        Some(Containment::Outside)
    );
    let s = translated(&scaled(&uv_sphere(24, 48), 5.0), [10.0, 0.0, 0.0]);
    assert_eq!(
        point_in_solid(&s, [10.0, 0.0, 0.0]),
        Some(Containment::Inside)
    );
    assert_eq!(
        point_in_solid(&s, [0.0, 0.0, 0.0]),
        Some(Containment::Outside)
    );
}
