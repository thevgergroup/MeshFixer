#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The per-face state buffer (MF-RENDER-003 selection, MF-RENDER-005): a
//! selection, hover or defect tint is one byte per face in a small storage
//! buffer per face chunk, so changing it uploads no geometry.

use glam::DMat4;
use image::RgbaImage;
use mf_fixtures::{grid, uv_sphere};
use mf_geometry::{CHUNK, FaceId, Mesh};
use mf_render::{
    DrawItem, FaceStates, GpuMesh, MeshRenderer, MeshVertices, OrbitCamera, Palette, RenderTarget,
    offscreen, render_origin, screen_position,
};

/// What the desktop paces uploads at.
const PACE: u64 = 16 << 20;
/// Bytes of one full face chunk's states (one byte per face).
const STATE_CHUNK_BYTES: u64 = CHUNK as u64;

fn gpu(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    match offscreen::shared_device() {
        Ok(d) => Some(d),
        Err(_) => {
            mf_fixtures::skip::gpu_unavailable(test);
            None
        }
    }
}

/// One byte per face slot of `mesh`: `bit` on every face of `faces`.
fn flags(mesh: &Mesh, faces: impl IntoIterator<Item = u32>, bit: u8) -> Vec<u8> {
    let mut out = vec![0u8; mesh.face_slots() as usize];
    for f in faces {
        out[f as usize] |= bit;
    }
    out
}

#[test]
fn selection_change_uploads_no_geometry() {
    let Some((device, queue)) = gpu("selection_change_uploads_no_geometry") else {
        return;
    };
    for (label, rings, segments) in [("1M", 500, 1000), ("5M", 1250, 2000)] {
        let mesh = uv_sphere(rings, segments);
        let origin = render_origin(None, mesh.bounding_box());
        let gpu = GpuMesh::from_mesh_paced(&device, &queue, &mesh, origin, &[], PACE);
        let geometry = gpu.last_upload().surface_bytes;

        // Select 500 faces in face chunk 3.
        let base = 3 * CHUNK as u32;
        let first = FaceStates::from_face_flags(
            &mesh,
            &flags(&mesh, base + 100..base + 600, FaceStates::SELECTED),
        );
        let stats = gpu.set_face_states(&queue, &first);
        eprintln!(
            "{label} ({} triangles): selection change uploaded {} state bytes, {} surface bytes \
             (the geometry is {geometry} surface bytes)",
            mesh.triangle_count(),
            stats.state_bytes,
            stats.surface_bytes,
        );
        assert_eq!(stats.surface_bytes, 0, "{label}: no geometry: {stats:?}");
        assert_eq!(stats.line_bytes, 0, "{label}: no outlines: {stats:?}");
        assert!(stats.state_bytes > 0, "{label}: the states were written");
        assert!(
            stats.state_bytes <= STATE_CHUNK_BYTES,
            "{label}: only the chunk holding the selection: {stats:?}"
        );

        // Moving the selection to chunk 5 rewrites chunks 3 (cleared) and 5.
        let base = 5 * CHUNK as u32;
        let second =
            FaceStates::from_face_flags(&mesh, &flags(&mesh, [base + 7], FaceStates::SELECTED));
        let stats = gpu.set_face_states(&queue, &second);
        assert_eq!(stats.surface_bytes, 0, "{label}: {stats:?}");
        assert!(
            stats.state_bytes <= 2 * STATE_CHUNK_BYTES,
            "{label}: two chunks changed: {stats:?}"
        );
        // The same states again write nothing.
        assert_eq!(gpu.set_face_states(&queue, &second).total(), 0, "{label}");
        // What is on the GPU is what was set.
        for i in [2, 3, 5] {
            assert_eq!(
                gpu.read_face_states(&device, &queue, i).unwrap(),
                second.chunks[i],
                "{label}: chunk {i}"
            );
        }
        assert_eq!(
            gpu.read_face_states(&device, &queue, 5).unwrap()[1] & 0xff00_0000,
            u32::from(FaceStates::SELECTED) << 24,
            "{label}: face 7 of chunk 5 is byte 3 of word 1"
        );
    }
}

/// A grid of 80,000 faces (three chunks), with the first 1,000 faces of
/// chunk 1 deleted so drawn order and face IDs differ there.
fn holed_grid() -> (Mesh, u32) {
    let n = 200;
    let mut e = grid(n).edit();
    for f in CHUNK as u32..CHUNK as u32 + 1000 {
        e.remove_face(FaceId(f)).unwrap();
    }
    (e.finish().unwrap().0, n)
}

/// Faces of the cells `i0..i1` × `j0..j1` of an `n`-cell grid, and the
/// patch's centre.
fn patch(n: u32, i: std::ops::Range<u32>, j: std::ops::Range<u32>) -> (Vec<u32>, [f64; 3]) {
    let centre = [
        f64::from(i.start + i.end) / 2.0,
        f64::from(j.start + j.end) / 2.0,
        0.0,
    ];
    let faces = j
        .flat_map(|j| {
            i.clone()
                .flat_map(move |i| [2 * (j * n + i), 2 * (j * n + i) + 1])
        })
        .collect();
    (faces, centre)
}

fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    gpu: &GpuMesh,
    cam: &OrbitCamera,
    shown: u32,
) -> RgbaImage {
    let target = RenderTarget::new(device, 400, 400, 4).unwrap();
    let mut renderer = MeshRenderer::new(device, RenderTarget::format(4), Palette::HEADLESS);
    let items = [DrawItem {
        hole: false,
        ghost: false,
        mesh: gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: shown,
        object_color: None,
    }];
    target.draw(device, queue, &mut renderer, Some(cam), &items, false, &[]);
    offscreen::read_back(device, queue, &target).unwrap()
}

fn pixel(image: &RgbaImage, cam: &OrbitCamera, world: [f64; 3]) -> [i32; 3] {
    let (x, y) = screen_position(cam, cam.target, (400.0, 400.0), world).unwrap();
    let p = image.get_pixel(x as u32, y as u32).0;
    [0, 1, 2].map(|k| i32::from(p[k]))
}

fn cyan(p: [i32; 3]) -> bool {
    p[2] - p[0] > 60 && p[1] - p[0] > 40
}

fn amber(p: [i32; 3]) -> bool {
    p[0] - p[2] > 60 && p[1] - p[2] > 40
}

#[test]
fn state_bits_tint_the_right_faces() {
    let Some((device, queue)) = gpu("state_bits_tint_the_right_faces") else {
        return;
    };
    let (mesh, n) = holed_grid();
    assert_eq!(mesh.face_chunk_count(), 3);
    // Selected: a patch in chunk 1, after its deleted faces. Small
    // component: a patch in chunk 2. Plain: a patch in chunk 0.
    let (selected, at_selected) = patch(n, 120..140, 150..160);
    let (small, at_small) = patch(n, 30..50, 180..195);
    let (_, at_plain) = patch(n, 40..60, 40..60);
    assert!(selected.iter().all(|&f| (f as usize) / CHUNK == 1));
    assert!(small.iter().all(|&f| (f as usize) / CHUNK == 2));
    let mut bits = flags(&mesh, selected, FaceStates::SELECTED);
    for f in small {
        bits[f as usize] |= FaceStates::LOOSE_PIECE;
    }
    let states = FaceStates::from_face_flags(&mesh, &bits);

    let mut cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    cam.pitch = 80f64.to_radians();
    let all = u32::from(FaceStates::ALL);
    let vertices = MeshVertices::build(&mesh);
    // One buffer per chunk, and chunks split into several buffers (a tiny
    // buffer limit): the state index must follow the face across both.
    for limit in [u64::MAX, 3 * 24 * 5000] {
        let limit = limit.min(device.limits().max_buffer_size);
        let gpu = GpuMesh::from_vertices_with_limit(&device, &vertices, limit);
        assert_eq!(gpu.set_face_states(&queue, &states).surface_bytes, 0);
        let image = draw(&device, &queue, &gpu, &cam, all);
        let (s, a, p) = (
            pixel(&image, &cam, at_selected),
            pixel(&image, &cam, at_small),
            pixel(&image, &cam, at_plain),
        );
        assert!(cyan(s), "limit {limit}: the selected patch is cyan: {s:?}");
        assert!(amber(a), "limit {limit}: the small piece is amber: {a:?}");
        assert!(
            !cyan(p) && !amber(p),
            "limit {limit}: the rest is untinted: {p:?}"
        );

        // A tint that isn't shown isn't drawn (the overlay toggles).
        let image = draw(
            &device,
            &queue,
            &gpu,
            &cam,
            all & !u32::from(FaceStates::LOOSE_PIECE),
        );
        let a = pixel(&image, &cam, at_small);
        assert!(!amber(a), "limit {limit}: hidden tint: {a:?}");
        assert!(cyan(pixel(&image, &cam, at_selected)), "limit {limit}");
    }
}

/// A face's state is found by its chunk and its index among the chunk's
/// drawn (live) faces.
#[test]
fn faces_are_located_in_drawn_order() {
    let (mesh, _) = holed_grid();
    assert_eq!(FaceStates::locate(&mesh, FaceId(5)), Some((0, 5)));
    assert_eq!(FaceStates::locate(&mesh, FaceId(CHUNK as u32 + 10)), None);
    assert_eq!(
        FaceStates::locate(&mesh, FaceId(CHUNK as u32 + 1000)),
        Some((1, 0))
    );
    let states = FaceStates::from_face_flags(
        &mesh,
        &flags(&mesh, [CHUNK as u32 + 1001], FaceStates::HOVER),
    );
    assert_eq!(states.get(1, 1), FaceStates::HOVER);
    assert_eq!(states.get(1, 0), 0);
    assert_eq!(states.chunks[1].len(), (CHUNK - 1000).div_ceil(4));
}

/// States built from a few flagged faces equal the dense ones in the
/// chunks they touch and are empty (clear) elsewhere; writing them clears
/// what an earlier write set.
#[test]
fn sparse_states_match_dense_ones() {
    let (mesh, _) = holed_grid();
    let picked = [
        5,
        CHUNK as u32 + 1001,
        CHUNK as u32 + 1001,
        2 * CHUNK as u32,
    ];
    let mut flagged: Vec<(u32, u8)> = picked
        .iter()
        .zip([
            FaceStates::SELECTED,
            FaceStates::HOVER,
            FaceStates::RECENT_REPAIR,
            0,
        ])
        .map(|(&f, b)| (f, b))
        .chain([(CHUNK as u32 + 10, FaceStates::SELECTED)]) // deleted: skipped
        .collect();
    let mut dense = vec![0u8; mesh.face_slots() as usize];
    for &(f, b) in &flagged {
        if mesh.is_face_live(FaceId(f)) {
            dense[f as usize] |= b;
        }
    }
    let dense = FaceStates::from_face_flags(&mesh, &dense);
    let sparse = FaceStates::from_flagged(&mesh, &mut flagged);
    assert_eq!(sparse.chunks[0], dense.chunks[0]);
    assert_eq!(sparse.chunks[1], dense.chunks[1]);
    assert_eq!(
        sparse.get(1, 1),
        FaceStates::HOVER | FaceStates::RECENT_REPAIR
    );
    assert!(sparse.chunks[2].is_empty(), "nothing set in chunk 2");

    let Some((device, queue)) = gpu("sparse_states_match_dense_ones") else {
        return;
    };
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    assert_eq!(
        gpu.set_face_states(&queue, &FaceStates::empty(&mesh))
            .total(),
        0
    );
    let stats = gpu.set_face_states(&queue, &sparse);
    assert_eq!(
        stats.state_bytes,
        4 * (dense.chunks[0].len() + dense.chunks[1].len()) as u64
    );
    for i in 0..3 {
        assert_eq!(
            gpu.read_face_states(&device, &queue, i).unwrap(),
            dense.chunks[i]
        );
    }
    // Clearing writes zeros over the two chunks that had flags.
    let stats = gpu.set_face_states(&queue, &FaceStates::empty(&mesh));
    assert_eq!(
        stats.state_bytes,
        4 * (dense.chunks[0].len() + dense.chunks[1].len()) as u64
    );
    assert!(
        gpu.read_face_states(&device, &queue, 1)
            .unwrap()
            .iter()
            .all(|&w| w == 0)
    );
}

/// One colour, one meaning (fix round 1, P2): a face that is both
/// selected and hovered (different targets) shows the selection colour
/// exactly, not a blend with the hover tint.
#[test]
fn selection_wins_over_hover() {
    let Some((device, queue)) = gpu("selection_wins_over_hover") else {
        return;
    };
    let mesh = grid(40);
    let (selected, at_selected) = patch(40, 5..15, 5..15);
    let (both, at_both) = patch(40, 25..35, 25..35);
    let (hovered, at_hovered) = patch(40, 5..15, 25..35);
    let mut bits = flags(&mesh, selected, FaceStates::SELECTED);
    for f in both {
        bits[f as usize] |= FaceStates::SELECTED | FaceStates::HOVER;
    }
    for f in hovered {
        bits[f as usize] |= FaceStates::HOVER;
    }
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    gpu.set_face_states(&queue, &FaceStates::from_face_flags(&mesh, &bits));
    let mut cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    cam.pitch = 80f64.to_radians();
    let image = draw(&device, &queue, &gpu, &cam, u32::from(FaceStates::ALL));
    let (s, b, h) = (
        pixel(&image, &cam, at_selected),
        pixel(&image, &cam, at_both),
        pixel(&image, &cam, at_hovered),
    );
    assert!(cyan(s), "{s:?}");
    assert!(
        (0..3).all(|k| (s[k] - b[k]).abs() <= 3),
        "selected and hovered {b:?} looks like selected {s:?}"
    );
    assert!(
        (0..3).any(|k| (h[k] - s[k]).abs() > 20),
        "hover alone is its own colour: {h:?}"
    );
}

/// UI plan §11 (owner report): a selection is an outline and a light
/// tint, never a solid fill, so a selected object's own shading shows. With
/// the app's light tint (18%), the centre of a selected surface stays close
/// to its unselected shaded colour, and is not the selection's cyan.
#[test]
fn a_selected_surface_keeps_its_own_shading() {
    let Some((device, queue)) = gpu("a_selected_surface_keeps_its_own_shading") else {
        return;
    };
    let mesh = grid(40);
    let (all, centre) = patch(40, 0..40, 0..40);
    let mut palette = Palette::HEADLESS;
    palette.tints.selected[3] = 0.18;
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    let mut cam = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    cam.pitch = 60f64.to_radians();
    let draw_with = |states: &FaceStates| {
        gpu.set_face_states(&queue, states);
        let target = RenderTarget::new(&device, 400, 400, 4).unwrap();
        let mut renderer = MeshRenderer::new(&device, RenderTarget::format(4), palette);
        let items = [DrawItem {
            hole: false,
            ghost: false,
            mesh: &gpu,
            model: DMat4::IDENTITY,
            inactive: false,
            shown_states: u32::from(FaceStates::ALL),
            object_color: None,
        }];
        target.draw(
            &device,
            &queue,
            &mut renderer,
            Some(&cam),
            &items,
            false,
            &[],
        );
        offscreen::read_back(&device, &queue, &target).unwrap()
    };
    let plain = pixel(
        &draw_with(&FaceStates::from_face_flags(&mesh, &flags(&mesh, [], 0))),
        &cam,
        centre,
    );
    let selected = pixel(
        &draw_with(&FaceStates::from_face_flags(
            &mesh,
            &flags(&mesh, all, FaceStates::SELECTED),
        )),
        &cam,
        centre,
    );
    let diff = (0..3)
        .map(|k| (plain[k] - selected[k]).abs())
        .max()
        .unwrap();
    assert!(
        diff <= 40,
        "the selected centre {selected:?} stays near its shading {plain:?} ({diff})"
    );
    assert!(diff > 3, "but is visibly tinted: {selected:?} vs {plain:?}");
    assert!(!cyan(selected), "not a solid cyan fill: {selected:?}");
}
