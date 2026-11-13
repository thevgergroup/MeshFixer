//! The orbit benchmark (MF-RENDER-002, MF-RT-005): a full 360° orbit of a
//! mesh rendered offscreen with the desktop's render path (per-chunk GPU
//! mesh uploaded in paced chunks, its face-state buffer bound, boundary
//! outlines on, 4× MSAA), each frame waited for on the GPU, so a frame's time is
//! what a user waits for: CPU encode and submit through GPU completion.
//!
//! GPU timestamps are recorded too where the device has them, with their
//! counts ([`FrameTimes`]): on Metal most samples of a heavy pass are
//! unusable (see [`GpuTimer`]), so they are never the headline figure.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mf_geometry::Mesh;
use mf_metrics::{FrameTimes, GpuBufferBytes, GpuTime};
use mf_runtime::{Cancelled, RunContext};

use crate::{
    DrawItem, FaceStates, GpuMesh, GpuTimer, MeshRenderer, OrbitCamera, Palette, RenderError,
    RenderTarget, render_origin,
};

/// As the desktop view.
pub const SAMPLES: u32 = 4;

/// What an orbit's frame time covers, and what it leaves out.
pub const MEASURES: &str = "offscreen 3D-view render (desktop path, 4x MSAA, boundary outlines, \
     no overlay layers); excludes egui UI/paint, swapchain present and vsync";
/// As the desktop's uploads (`mf-desktop` `UPLOAD_CHUNK_BYTES`).
const UPLOAD_CHUNK_BYTES: u64 = 16 << 20;

/// What to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrbitSpec {
    /// Frames in the orbit (after one warm-up frame, which isn't counted).
    pub frames: u32,
    pub width: u32,
    pub height: u32,
}

/// What an orbit measured.
pub struct OrbitRun {
    pub adapter: wgpu::AdapterInfo,
    /// Whether the device has GPU timestamp queries.
    pub timestamps: bool,
    /// Building and uploading the GPU mesh (paced, as the desktop does).
    pub upload: Duration,
    /// The warm-up frame (pipeline and first-draw costs), not counted.
    pub first_frame: Duration,
    /// Each counted frame, from the start of its encode to the GPU
    /// reporting its work done.
    pub frames: Vec<Duration>,
    /// CPU (encode and submit) and GPU (timestamps) per counted frame.
    pub times: FrameTimes,
    /// GPU allocations while orbiting: the mesh's buffers and the target's
    /// textures.
    pub buffers: GpuBufferBytes,
    /// The process's memory right after the last frame, while the GPU
    /// mesh is still held (or why there's none).
    pub process: Result<mf_metrics::ProcessMemory, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OrbitError {
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("cancelled")]
    Cancelled,
    #[error("the mesh is empty: nothing to orbit")]
    Empty,
}

impl From<Cancelled> for OrbitError {
    fn from(_: Cancelled) -> Self {
        OrbitError::Cancelled
    }
}

/// A device of its own (not [`crate::offscreen::shared_device`]'s), with
/// GPU timestamps where the adapter has them, and the desktop's limits.
pub fn timing_device() -> Result<(wgpu::AdapterInfo, wgpu::Device, wgpu::Queue), RenderError> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|_| RenderError::NoAdapter)?;
    let info = adapter.get_info();
    let base_limits = if info.backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("mf orbit benchmark"),
        required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
        required_limits: wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..base_limits
        },
        ..Default::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .map_err(|e| RenderError::Device(e.to_string()))?;
    Ok((info, device, queue))
}

/// Wait until the GPU has finished everything submitted so far, confirmed
/// by [`wgpu::Queue::on_submitted_work_done`].
fn wait_for_gpu(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(), RenderError> {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    queue.on_submitted_work_done(move || flag.store(true, Ordering::Release));
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|e| RenderError::Device(format!("waiting for the GPU: {e}")))?;
    if !done.load(Ordering::Acquire) {
        return Err(RenderError::Device(
            "the GPU didn't report its work done after a blocking wait".into(),
        ));
    }
    Ok(())
}

/// Orbit `mesh` once around its centre in `spec.frames` steps. The
/// mesh's topology must be computable (boundary outlines are drawn);
/// `ctx` is checked between frames.
pub fn orbit(mesh: &Mesh, spec: OrbitSpec, ctx: &RunContext) -> Result<OrbitRun, OrbitError> {
    let bounds = mesh.bounding_box().ok_or(OrbitError::Empty)?;
    let (adapter, device, queue) = timing_device()?;
    let target = RenderTarget::new(&device, spec.width, spec.height, SAMPLES)?;
    let mut renderer = MeshRenderer::new(&device, RenderTarget::format(SAMPLES), Palette::HEADLESS);
    let mut timer = GpuTimer::new(&device, &queue);
    let timestamps = timer.is_some();

    mesh.topology_with(ctx)?;
    let started = Instant::now();
    let origin = render_origin(None, Some(bounds));
    let gpu = GpuMesh::from_mesh_paced(&device, &queue, mesh, origin, &[], UPLOAD_CHUNK_BYTES);
    wait_for_gpu(&device, &queue)?;
    let upload = started.elapsed();
    ctx.checkpoint()?;

    let items = [DrawItem {
        hole: false,
        ghost: false,
        mesh: &gpu,
        model: glam::DMat4::IDENTITY,
        inactive: false,
        shown_states: u32::from(FaceStates::ALL),
        object_color: None,
    }];
    let base = OrbitCamera::fit(&bounds);
    let camera = |i: u32| {
        let mut c = base;
        c.orbit(
            std::f64::consts::TAU * f64::from(i) / f64::from(spec.frames.max(1)),
            0.0,
        );
        c
    };

    let warm = Instant::now();
    target.draw(
        &device,
        &queue,
        &mut renderer,
        Some(&camera(0)),
        &items,
        true,
        &[],
    );
    wait_for_gpu(&device, &queue)?;
    let first_frame = warm.elapsed();

    let mut times = FrameTimes::new();
    let mut frames = Vec::with_capacity(spec.frames as usize);
    for i in 0..spec.frames {
        ctx.checkpoint()?;
        let cam = camera(i);
        let start = Instant::now();
        let index = times.begin(start);
        let timed = target.draw_timed(
            &device,
            &queue,
            &mut renderer,
            Some(&cam),
            &items,
            true,
            &[],
            timer.as_mut().map(|t| (t, index)),
        );
        let cpu = start.elapsed();
        wait_for_gpu(&device, &queue)?;
        frames.push(start.elapsed());
        let gpu_time = match (&timer, timed) {
            (None, _) => GpuTime::Unsupported,
            (Some(_), true) => GpuTime::Pending,
            // Every read-back was busy: no time to be had for this frame.
            (Some(_), false) => GpuTime::Unusable,
        };
        times.end(index, cpu, gpu_time);
        if let Some(t) = timer.as_mut() {
            for (frame, time) in t.collect(&device) {
                match time {
                    Some(d) => times.set_gpu(frame, d),
                    None => times.set_gpu_unusable(frame),
                }
            }
        }
    }
    let process = mf_metrics::process_memory();
    let mut buffers = gpu.resident_buffers();
    buffers.textures = target.texture_bytes();
    Ok(OrbitRun {
        adapter,
        timestamps,
        upload,
        first_frame,
        frames,
        times,
        buffers,
        process,
    })
}
