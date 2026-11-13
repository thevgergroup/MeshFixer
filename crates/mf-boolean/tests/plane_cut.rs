#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mf_boolean::plane_cut`'s section-region identity and region
//! filtering (Phase 1b: cross-section picking, bounded cut plane).

use mf_boolean::{Bound, CutError, Plane, Side, cut_half, cut_half_with_regions};
use mf_fixtures::{merge, translated, uv_sphere};
use mf_geometry::Mesh;
use mf_runtime::RunContext;

/// Two spheres of radius 1 far enough apart (centres 6 apart) that a
/// plane through both x=centres crosses each in a disjoint section, with
/// no shared faces: two separate outer loops, never nested.
fn two_lobes() -> Mesh {
    let sphere = uv_sphere(12, 16);
    merge(&[
        translated(&sphere, [-3.0, 0.0, 0.0]),
        translated(&sphere, [3.0, 0.0, 0.0]),
    ])
}

fn plane() -> Plane {
    Plane {
        point: [0.0, 0.0, 0.0],
        normal: [0.0, 0.0, 1.0],
    }
}

#[test]
fn section_regions_get_a_stable_seed_face() {
    let mesh = two_lobes();
    let ctx = RunContext::none();
    let a = cut_half(&mesh, &plane(), Side::Above, &ctx).unwrap();
    assert_eq!(
        a.section_regions.len(),
        2,
        "two disjoint lobes, two regions"
    );
    let seeds: Vec<u32> = a.section_regions.iter().map(|r| r.seed_face.0).collect();
    assert_ne!(
        seeds[0], seeds[1],
        "distinct regions get distinct seed faces"
    );

    // Re-running the same cut gives the identical seed faces (replay
    // stability): input face ids don't change run to run.
    let a2 = cut_half(&mesh, &plane(), Side::Above, &ctx).unwrap();
    let seeds2: Vec<u32> = a2.section_regions.iter().map(|r| r.seed_face.0).collect();
    assert_eq!(seeds, seeds2);
}

#[test]
fn ticking_one_region_leaves_the_other_lobe_whole() {
    let mesh = two_lobes();
    let ctx = RunContext::none();
    let unrestricted = cut_half(&mesh, &plane(), Side::Above, &ctx).unwrap();
    assert_eq!(unrestricted.section_regions.len(), 2);
    let keep_seed = unrestricted.section_regions[0].seed_face;

    let restricted = cut_half_with_regions(
        &mesh,
        &plane(),
        Side::Above,
        true,
        Some(&[keep_seed]),
        None,
        &ctx,
    )
    .unwrap();
    // Only the ticked region actually got cut/capped.
    assert_eq!(restricted.section_regions.len(), 1);
    assert_eq!(restricted.section_regions[0].seed_face, keep_seed);

    // The other lobe is present whole (every one of its vertices, never
    // a partial subset): with Side::Above kept, the untouched lobe
    // (whichever one's seed face isn't `keep_seed`) must be either
    // fully present (unioned in, uncut) or fully absent — never a
    // smaller, capped remainder. We can't directly label which lobe is
    // "the other one" by vertex id post-cut, so instead check an
    // invariant that would fail if that lobe were sliced: total kept
    // face count equals the ticked lobe's cut result's face count plus
    // exactly one whole sphere's worth of untouched faces (the input
    // sphere's own face count, since a whole lobe keeps every face).
    let sphere_faces = uv_sphere(12, 16).face_count();
    // The side that keeps the untouched lobe whole must contain at
    // least one lobe with exactly `sphere_faces` original faces still
    // present (no cap faces added for it, since region-count is 1, not
    // 2). We approximate "whole lobe kept" by checking total face count
    // is strictly more than the single-region unrestricted cut would
    // give for just the ticked lobe alone (i.e. the other lobe's
    // untouched faces are present on top), without asserting exact
    // counts that depend on the kernel's internal cap triangulation.
    assert!(
        restricted.mesh.face_count() > sphere_faces,
        "the kept side should still carry the whole untouched lobe's faces \
         ({sphere_faces}) on top of the ticked lobe's cut result, got {}",
        restricted.mesh.face_count()
    );
}

#[test]
fn unknown_seed_face_in_regions_list_is_ignored_not_a_panic() {
    let sphere = mf_fixtures::scaled(&uv_sphere(12, 16), 3.0);
    let ctx = RunContext::none();
    let bogus = mf_geometry::FaceId(999_999);
    // One real region; the allow-list names a face that doesn't exist
    // anywhere in the mesh, so nothing matches: the region isn't cut
    // (left whole), exactly as if it had been unticked.
    let result = cut_half_with_regions(
        &sphere,
        &plane(),
        Side::Above,
        true,
        Some(&[bogus]),
        None,
        &ctx,
    );
    match result {
        Ok(half) => assert!(
            half.section_regions.is_empty(),
            "no region's seed face matched, so nothing should have been cut: {:?}",
            half.section_regions
        ),
        Err(e) => panic!("expected Ok with zero regions cut, not a refusal: {e:?}"),
    }
}

#[test]
fn bounded_disc_cuts_only_the_region_it_covers() {
    let mesh = two_lobes();
    let ctx = RunContext::none();
    // Disc of radius 1.5 centred at the plane's point (0,0): covers the
    // lobe at x=-3's section (radius ~1 there) but not the lobe at
    // x=+3 (its section sits entirely outside a disc of radius 1.5
    // centred at the origin, since it's centred near x=3).
    //
    // The plane frame's origin is `plane.point` = (0,0,0); to make the
    // bound actually sit over just one lobe we instead center the bound
    // by moving the plane's point to the lobe we want covered, keeping
    // the normal the same, and size the disc so only that lobe's
    // section (radius ~1 around its own centre, which is now the
    // frame's origin) is inside.
    let shifted = Plane {
        point: [-3.0, 0.0, 0.0],
        normal: [0.0, 0.0, 1.0],
    };
    let bound = Bound::Disc { radius: 1.5 };
    let restricted =
        cut_half_with_regions(&mesh, &shifted, Side::Above, true, None, Some(&bound), &ctx)
            .unwrap();
    assert_eq!(
        restricted.section_regions.len(),
        1,
        "only the lobe under the disc should be cut"
    );
    let sphere_faces = uv_sphere(12, 16).face_count();
    assert!(
        restricted.mesh.face_count() > sphere_faces,
        "the other lobe's whole faces should still be present"
    );
}

#[test]
fn bound_crossing_a_region_is_refused_by_name() {
    let sphere = mf_fixtures::scaled(&uv_sphere(12, 16), 3.0);
    let ctx = RunContext::none();
    // Off the sphere's vertex rings (z=0.37, not z=0), same as the
    // two-lobe tests' "off the seam of vertices" cuts, so the plane's
    // own point can move without landing exactly on a different ring.
    let cut_plane = Plane {
        point: [0.0, 0.0, 0.37],
        normal: [0.0, 0.0, 1.0],
    };
    let unbounded = cut_half(&sphere, &cut_plane, Side::Above, &ctx).unwrap();
    assert_eq!(unbounded.section_regions.len(), 1);
    let seed = unbounded.section_regions[0].seed_face;
    // The section's radius there is sqrt(3^2 - 0.37^2) ≈ 2.977. A disc
    // of radius 1.5 centred 2 units off the section's own centre (in
    // the plane) partially overlaps that circle: some of its points
    // fall inside the disc, some outside.
    let offset = Plane {
        point: [2.0, 0.0, 0.37],
        normal: [0.0, 0.0, 1.0],
    };
    let bound = Bound::Disc { radius: 1.5 };
    let result = cut_half_with_regions(
        &sphere,
        &offset,
        Side::Above,
        true,
        None,
        Some(&bound),
        &ctx,
    );
    match result {
        Err(CutError::RegionCrossesBound { seed_face }) => {
            assert_eq!(seed_face, seed);
        }
        other => panic!("expected RegionCrossesBound, got {other:?}"),
    }
}
