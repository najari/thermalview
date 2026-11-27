#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ThermalView")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0]),
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    eframe::run_native(
        "ThermalView",
        options,
        Box::new(|cc| Ok(Box::new(thermalview::app::ThermalApp::new(cc)))),
    )
}

/// Prefers DirectX 12 over Vulkan. With Vulkan (wgpu's default pick) on an Intel Iris Xe the
/// window stayed white while the app ran normally; DX12 draws it. Other backends stay as a
/// fallback, and `WGPU_BACKEND` still limits the choice. Among adapters of the same backend a
/// discrete GPU comes first, as with wgpu's default high-performance preference.
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew};
    use eframe::wgpu;
    use std::sync::Arc;

    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
        let rank = |a: &wgpu::Adapter| {
            let info = a.get_info();
            let backend = match info.backend {
                wgpu::Backend::Dx12 => 0,
                wgpu::Backend::Vulkan => 1,
                _ => 2,
            };
            let device = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                _ => 2,
            };
            (backend, device)
        };
        let chosen = adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
            .min_by_key(|a| rank(a))
            .cloned()
            .ok_or_else(|| "no graphics adapter can draw to the window".to_owned())?;
        let info = chosen.get_info();
        log::info!("graphics: {} via {:?}", info.name, info.backend);
        Ok(chosen)
    }));
    eframe::WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}
