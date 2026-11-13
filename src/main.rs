#![windows_subsystem = "windows"]

mod app;
mod gfx;
mod layout;
mod tree;
mod win;

use eframe::egui_wgpu::{SurfaceConfig, WgpuSetup, WgpuSetupCreateNew};
use std::sync::Arc;

fn main() -> eframe::Result {
    let mut opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("SpaceSniffer-rs")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    // Start on Fifo (always supported); App switches to Mailbox + DwmFlush pacing if the surface allows it.
    opts.wgpu_options.surface = SurfaceConfig { present_mode: wgpu::PresentMode::Fifo, desired_maximum_frame_latency: Some(1) };
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    // Vulkan only (DX12 support costs ~0.7 MB of exe); discrete GPU first.
    setup.instance_descriptor.backends = wgpu::Backends::VULKAN;
    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
        adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .max_by_key(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
            .cloned()
            .ok_or_else(|| "No Vulkan adapter found".to_string())
    }));
    opts.wgpu_options.wgpu_setup = WgpuSetup::CreateNew(setup);
    eframe::run_native("SpaceSniffer-rs", opts, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
