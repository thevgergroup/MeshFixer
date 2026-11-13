//! The wgpu device the app (and its GUI tests) run on.

use std::sync::Arc;

use eframe::egui_wgpu;

/// egui-wgpu's device defaults, plus GPU timestamps where the adapter has
/// them, so the 3D view's GPU time can be measured (MF-RT-001). Without
/// them the app still runs and reports GPU time as unsupported.
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let base_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    wgpu::DeviceDescriptor {
        label: Some("MeshFixer device"),
        required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
        required_limits: wgpu::Limits {
            // As egui-wgpu: a depth buffer the size of a 4K+ display.
            max_texture_dimension_2d: 8192,
            ..base_limits
        },
        ..Default::default()
    }
}

/// eframe's wgpu configuration with [`device_descriptor`].
pub fn wgpu_configuration() -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.device_descriptor = Arc::new(device_descriptor);
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}
