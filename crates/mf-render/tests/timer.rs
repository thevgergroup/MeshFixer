#![allow(clippy::unwrap_used)] // tests may panic on failure

//! GPU time of the 3D view's pass (MF-RT-001, MF-RT-005).

use glam::DMat4;
use mf_fixtures::uv_sphere;
use mf_render::{
    DrawItem, GpuMesh, GpuTimer, MeshRenderer, MeshVertices, OrbitCamera, Palette, RenderTarget,
};

fn device(features: wgpu::Features) -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let features = adapter.features() & features;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        ..Default::default()
    }))
    .ok()
}

#[test]
fn a_device_without_timestamps_has_no_timer() {
    let Some((device, queue)) = device(wgpu::Features::empty()) else {
        mf_fixtures::skip::gpu_unavailable("a_device_without_timestamps_has_no_timer");
        return;
    };
    assert!(GpuTimer::new(&device, &queue).is_none());
}

#[test]
fn timed_draws_report_a_gpu_time_per_frame() {
    let Some((device, queue)) = device(wgpu::Features::TIMESTAMP_QUERY) else {
        mf_fixtures::skip::gpu_unavailable("timed_draws_report_a_gpu_time_per_frame");
        return;
    };
    let Some(mut timer) = GpuTimer::new(&device, &queue) else {
        eprintln!("this adapter has no GPU timestamps; nothing to check");
        return;
    };
    // A small pass: Metal's begin/end timestamps come back out of order
    // once a pass has real fragment work (see timer.rs), and the studio
    // lighting (UI plan step 3) pushed the old 64x128 sphere at 256 px
    // over that edge; what this checks is the timer's bookkeeping.
    let mesh = uv_sphere(16, 32);
    let target = RenderTarget::new(&device, 96, 96, 4).unwrap();
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(4), Palette::HEADLESS);
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    let camera = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    let items = [DrawItem {
        hole: false,
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(mf_render::FaceStates::ALL),
    }];
    let mut samples = Vec::new();
    for frame in 0..20 {
        let timed = target.draw_timed(
            &device,
            &queue,
            &mut renderer,
            Some(&camera),
            &items,
            true,
            &[],
            Some((&mut timer, frame)),
        );
        assert!(timed, "a read-back is free for frame {frame}");
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        samples.extend(timer.collect(&device));
    }
    // Every frame comes back, with a time or marked unusable (Metal
    // sometimes skips a write); never a zero time. The pass is small,
    // which Metal times fine (see timer.rs on heavy passes).
    let frames: Vec<u64> = samples.iter().map(|(f, _)| *f).collect();
    assert_eq!(frames, (0..20).collect::<Vec<_>>(), "every frame, in order");
    let times: Vec<_> = samples.iter().filter_map(|(_, t)| *t).collect();
    let unusable = samples.len() - times.len();
    eprintln!("{} of 20 frames timed, {unusable} unusable", times.len());
    assert_eq!(unusable as u64, timer.failed);
    assert!(times.len() >= 10, "most frames are timed: {samples:?}");
    assert!(times.iter().all(|t| !t.is_zero()), "{times:?}");
    assert!(
        times.iter().all(|t| *t < std::time::Duration::from_secs(1)),
        "{times:?}"
    );
}

/// Generating and uploading a chunk at a time (for workers) gives the same
/// vertices as building them all first, in buffers no bigger than asked.
#[test]
fn paced_upload_from_the_mesh_matches_the_whole_upload() {
    let Some((device, queue)) = device(wgpu::Features::empty()) else {
        mf_fixtures::skip::gpu_unavailable("paced_upload_from_the_mesh_matches_the_whole_upload");
        return;
    };
    let mesh = mf_fixtures::cube_with_missing_face();
    let whole = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    let origin = glam::DVec3::from(mesh.bounding_box().unwrap().center());
    let stride = std::mem::size_of::<mf_render::SurfaceVertex>() as u64;
    // Two faces per buffer.
    let paced = GpuMesh::from_mesh_paced(&device, &queue, &mesh, origin, &[], 6 * stride);
    assert_eq!(paced.origin, whole.origin);
    assert_eq!(paced.triangle_count, whole.triangle_count);
    assert_eq!(paced.line_vertex_count(), whole.line_vertex_count());
    let (vertices, buffers) = paced.surface_vertex_count();
    assert_eq!(vertices, whole.surface_vertex_count().0);
    assert_eq!(vertices, 3 * mesh.triangle_count());
    assert_eq!(buffers, mesh.triangle_count().div_ceil(2));
}
