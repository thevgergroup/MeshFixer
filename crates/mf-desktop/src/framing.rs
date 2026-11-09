//! Where the camera goes to show an item found from the tree (M4 Task 8a):
//! a defect fills a sensible part of the view, seen from outside the model,
//! turning to face it when it's on the far side. Camera maths plus a few
//! ray casts through the picking BVHs (O(log n) each); everything O(n) was
//! done on the analysis worker (`defects::Place`).

use glam::DVec3;
use mf_geometry::Aabb;
use mf_render::OrbitCamera;

use crate::defects::Place;

/// A defect's bounding sphere fills this much of the view height: big
/// enough to see what it is, with its surroundings for context.
pub const DEFECT_FILL: f64 = 0.4;
/// An object's bounding sphere fills this much: all of it, with a margin.
pub const OBJECT_FILL: f64 = 0.75;
/// A target facing the camera less than this (the cosine between the way
/// it faces and the way to the eye) is looked at from the way it faces
/// instead (the hole on the far side).
pub const FACING_MIN: f64 = 0.3;
/// However small (or degenerate) a target, it's framed as if its radius
/// were at least this fraction of the scene's: a zero-size piece doesn't
/// send the camera infinitely close.
pub const MIN_RADIUS_OF_SCENE: f64 = 1e-4;
/// How many surfaces a sight line is followed through, at most, to get
/// the eye out of a model that encloses the target.
const MAX_CROSSINGS: usize = 32;

/// The camera that shows `place` (a defect, world space), starting from
/// `cam`: its centre at the view centre, its bounding sphere filling
/// [`DEFECT_FILL`] of the view height, every part of `scene` within the
/// clipping planes.
///
/// The view direction is the first of these with a clear line of sight
/// from the target to an eye outside the model (an even number of
/// surfaces beyond it): the way it faces when it faces away from the
/// camera (or else the current direction, then the way it faces), then
/// the six axes and eight diagonals. If none will do (the target is shut
/// inside the model), the eye backs out along the one that leaves the
/// model soonest, past its last surface: never inside the model (the
/// selection's x-ray pass shows the target through it).
///
/// `hit(origin, dir)` is the distance to the nearest surface along a
/// world-space ray (`dir` a unit vector), if any.
pub fn defect_view(
    cam: &OrbitCamera,
    place: &Place,
    scene: &Aabb,
    hit: impl Fn(DVec3, DVec3) -> Option<f64>,
) -> OrbitCamera {
    let centre = DVec3::from(place.bounds.center());
    let radius = (place.bounds.diagonal() * 0.5)
        .max(scene.diagonal() * 0.5 * MIN_RADIUS_OF_SCENE)
        .max(1e-9);
    let distance = cam.distance_to_fill(radius, DEFECT_FILL);
    let current = cam.view_dir();
    let facing = place.facing.filter(|f| f.is_finite() && f.length() > 0.5);
    let mut tries: Vec<DVec3> = match facing {
        Some(f) if f.dot(current) < FACING_MIN => vec![f, current],
        Some(f) => vec![current, f],
        None => vec![current],
    };
    for d in fallback_directions() {
        if tries.iter().all(|t| t.dot(d) < 0.999) {
            tries.push(d);
        }
    }
    // Rays start just outside the target, so they don't hit it, and just
    // past each surface they cross.
    let start = radius * 1.05;
    let step = (radius * 1e-3).max(scene.diagonal() * 1e-9);
    // The surfaces crossed along `d` from `from` on: how many, and how far
    // the last one is.
    let crossings = |from: DVec3, d: DVec3| {
        let (mut s, mut n) = (0.0, 0);
        while n < MAX_CROSSINGS {
            match hit(from + d * s, d) {
                Some(t) => {
                    s += t + step;
                    n += 1;
                }
                None => break,
            }
        }
        (n, s)
    };
    // A clear line of sight, and an eye outside the model: an even number
    // of surfaces beyond it (a closed model's inside has an odd number).
    let good = |d: DVec3| {
        hit(centre + d * start, d).is_none_or(|t| start + t >= distance)
            && crossings(centre + d * distance, d).0 % 2 == 0
    };
    let (dir, distance) = match tries.iter().copied().find(|&d| good(d)) {
        Some(d) => (d, distance),
        None => {
            // How far along `d` the last surface is.
            let exit = |d: DVec3| start + crossings(centre + d * start, d).1;
            let (d, out) = tries
                .iter()
                .map(|&d| (d, exit(d)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .expect("there are always directions to try");
            (d, distance.max(out + radius))
        }
    };
    let mut out = cam.framing(&place.bounds, dir, DEFECT_FILL, scene);
    out.distance = distance;
    out
}

/// The camera that shows all of `bounds` (an object, world space) from
/// the current direction.
pub fn object_view(cam: &OrbitCamera, bounds: &Aabb, scene: &Aabb) -> OrbitCamera {
    cam.framing(bounds, cam.view_dir(), OBJECT_FILL, scene)
}

/// The six axes and the eight diagonals, unit length.
fn fallback_directions() -> Vec<DVec3> {
    let mut out = vec![
        DVec3::X,
        DVec3::NEG_X,
        DVec3::Y,
        DVec3::NEG_Y,
        DVec3::Z,
        DVec3::NEG_Z,
    ];
    for x in [-1.0, 1.0] {
        for y in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                out.push(DVec3::new(x, y, z).normalize());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(min: f64, max: f64) -> Aabb {
        Aabb {
            min: [min; 3],
            max: [max; 3],
        }
    }

    /// The distance along a ray to a sphere's surface (from inside or
    /// outside).
    fn sphere(centre: DVec3, r: f64) -> impl Fn(DVec3, DVec3) -> Option<f64> {
        move |o: DVec3, d: DVec3| {
            let oc = o - centre;
            let b = oc.dot(d);
            let c = oc.length_squared() - r * r;
            let disc = b * b - c;
            if disc < 0.0 {
                return None;
            }
            let s = disc.sqrt();
            [-b - s, -b + s].into_iter().find(|&t| t > 1e-9)
        }
    }

    fn scene_cam() -> OrbitCamera {
        OrbitCamera::fit(&cube(-50.0, 50.0))
    }

    #[test]
    fn a_target_facing_the_camera_is_zoomed_to_without_turning() {
        let cam = scene_cam();
        let place = Place {
            bounds: cube(9.0, 11.0),
            facing: Some(cam.view_dir()),
        };
        let out = defect_view(&cam, &place, &cube(-50.0, 50.0), |_, _| None);
        assert!((out.view_dir() - cam.view_dir()).length() < 1e-9);
        assert_eq!(out.target, DVec3::splat(10.0));
        // Its sphere (radius √3) fills 40% of the view height.
        let fill = 3f64.sqrt() / (out.distance * (out.fov_y * 0.5).tan());
        assert!((fill - DEFECT_FILL).abs() < 1e-9, "{fill}");
        // The whole scene stays between the clipping planes.
        assert!(out.distance + 2.0 * out.radius > (DVec3::splat(-50.0) - out.eye()).length());
    }

    #[test]
    fn a_target_on_the_far_side_is_seen_from_the_way_it_faces() {
        let cam = scene_cam();
        // On a ball's far side, facing away from the camera.
        let away = -cam.view_dir();
        let at = away * 20.0;
        let place = Place {
            bounds: Aabb {
                min: (at - 1.0).to_array(),
                max: (at + 1.0).to_array(),
            },
            facing: Some(away),
        };
        let out = defect_view(&cam, &place, &cube(-50.0, 50.0), sphere(DVec3::ZERO, 19.0));
        assert!(out.view_dir().dot(away) > 0.99, "{}", out.view_dir());
        assert!(out.eye().length() > 20.0, "outside the ball");
    }

    #[test]
    fn a_target_shut_inside_is_seen_from_outside() {
        let cam = scene_cam();
        // A speck at the centre of a ball of radius 30.
        let place = Place {
            bounds: cube(-0.5, 0.5),
            facing: None,
        };
        let out = defect_view(&cam, &place, &cube(-50.0, 50.0), sphere(DVec3::ZERO, 30.0));
        assert!(out.eye().length() > 30.0, "eye {} is outside", out.eye());
        assert_eq!(out.target, DVec3::ZERO);
    }

    #[test]
    fn a_blocked_view_turns_to_a_clear_one() {
        let cam = scene_cam();
        let place = Place {
            bounds: cube(-0.5, 0.5),
            facing: None,
        };
        // A ball between the target and the camera.
        let wall = sphere(cam.view_dir() * 5.0, 2.0);
        let out = defect_view(&cam, &place, &cube(-50.0, 50.0), &wall);
        let eye = out.eye();
        assert!(
            wall(DVec3::ZERO + out.view_dir() * 0.9, out.view_dir())
                .is_none_or(|t| t > eye.length()),
            "a clear line of sight from {eye}"
        );
    }

    #[test]
    fn a_degenerate_target_is_framed_at_a_minimum_size() {
        let cam = scene_cam();
        let place = Place {
            bounds: cube(1.0, 1.0),
            facing: None,
        };
        let scene = cube(-50.0, 50.0);
        let out = defect_view(&cam, &place, &scene, |_, _| None);
        assert!(out.distance.is_finite() && out.distance > 0.0);
        let min = scene.diagonal() * 0.5 * MIN_RADIUS_OF_SCENE;
        assert!((out.distance - cam.distance_to_fill(min, DEFECT_FILL)).abs() < 1e-9);
    }
}
