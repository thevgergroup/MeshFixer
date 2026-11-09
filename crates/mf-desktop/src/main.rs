use mf_desktop::{MeshFixerApp, Settings};

/// `meshfixer-desktop [--threads N] [file]`.
fn parse_args() -> Result<(Settings, Option<std::path::PathBuf>), String> {
    let mut settings = Settings::default();
    let mut initial = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--threads" {
            let n = match args
                .next()
                .as_ref()
                .and_then(|v| v.to_str())
                .map(str::parse)
            {
                Some(Ok(n)) => n,
                _ => return Err("--threads needs a number of threads".into()),
            };
            settings.threads = Some(n);
        } else if initial.is_none() {
            initial = Some(std::path::PathBuf::from(arg));
        } else {
            return Err(format!("unexpected argument {}", arg.to_string_lossy()));
        }
    }
    Ok((settings, initial))
}

fn main() -> eframe::Result {
    let (settings, initial) = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {e}\nusage: meshfixer-desktop [--threads N] [file]");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MeshFixer")
            .with_inner_size([1440.0, 900.0]),
        wgpu_options: mf_desktop::gpu::wgpu_configuration(),
        ..Default::default()
    };
    eframe::run_native(
        "MeshFixer",
        options,
        Box::new(move |cc| {
            Ok(Box::new(MeshFixerApp::with_settings(
                cc, initial, settings,
            )?))
        }),
    )
}
