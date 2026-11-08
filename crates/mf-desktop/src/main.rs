use mf_desktop::MeshFixerApp;

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MeshFixer")
            .with_inner_size([1400.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MeshFixer",
        options,
        Box::new(move |cc| Ok(Box::new(MeshFixerApp::new(cc, initial)?))),
    )
}
