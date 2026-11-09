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
    let mesh = uv_sphere(64, 128);
    let target = RenderTarget::new(&device, 256, 256, 4).unwrap();
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(4), Palette::HEADLESS);
    let gpu = GpuMesh::from_vertices(&device, &MeshVertices::build(&mesh));
    let camera = OrbitCamera::fit(&mesh.bounding_box().unwrap());
    let items = [DrawItem {
        mesh: &gpu,
        model: DMat4::IDENTITY,
        inactive: false,
    }];
    let mut times = Vec::new();
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
        times.extend(timer.collect(&device));
    }
    // Every frame is accounted for: timed, or (Metal sometimes skips a
    // write) counted as failed. Never a zero time. The pass is small, which
    // Metal times fine (see timer.rs on heavy passes).
    eprintln!(
        "{} of 20 frames timed, {} unusable",
        times.len(),
        timer.failed
    );
    assert_eq!(times.len() as u64 + timer.failed, 20);
    assert!(times.len() >= 10, "most frames are timed: {times:?}");
    assert!(times.windows(2).all(|w| w[0].0 < w[1].0), "in frame order");
    assert!(times.iter().all(|(_, t)| !t.is_zero()), "{times:?}");
    assert!(
        times
            .iter()
            .all(|(_, t)| *t < std::time::Duration::from_secs(1)),
        "{times:?}"
    );
}
