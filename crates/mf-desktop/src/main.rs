use mf_desktop::{MeshFixerApp, Settings};
use mf_metrics::CountingAlloc;

/// Counts heap bytes for View ▸ Metrics (peak heap, each operation's peak).
#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// The window size used when nothing overrides it: fits the panels and the
/// view at a comfortable desktop size (M4 demo script, `docs/milestones/m4-demo.md`).
const DEFAULT_WINDOW_SIZE: (f32, f32) = (1440.0, 900.0);

/// Parses a `WIDTHxHEIGHT` window size, as given to `--window-size` or
/// `MF_WINDOW_SIZE` (for example `"1440x900"`). Both dimensions must be
/// positive numbers; anything else is named in the error so the demo
/// script's launch command fails loudly rather than opening some other size.
fn parse_window_size(s: &str) -> Result<(f32, f32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("window size {s:?} must look like WIDTHxHEIGHT, e.g. 1440x900"))?;
    let parse_dim = |v: &str| -> Result<f32, String> {
        match v.parse::<f32>() {
            Ok(n) if n.is_finite() && n > 0.0 => Ok(n),
            _ => Err(format!("window size {s:?} has a bad dimension {v:?}")),
        }
    };
    Ok((parse_dim(w)?, parse_dim(h)?))
}

/// What `parse_args` extracts from the command line, beyond [`Settings`].
struct Launch {
    initial: Option<std::path::PathBuf>,
    window_size: (f32, f32),
}

/// `meshfixer-desktop [--threads N] [--window-size WxH] [file]`.
///
/// `--window-size` (or the `MF_WINDOW_SIZE` env var, which the flag
/// overrides) sets the initial window size instead of the 1440×900 default;
/// the demo script (`scripts/demo.sh`) relies on the default, so this exists
/// for anyone whose display can't open that size, not for ordinary use.
fn parse_args() -> Result<(Settings, Launch), String> {
    let mut settings = Settings {
        // People opening a scan are asked for its unit (MF-ISSUE-045).
        unit_prompt: true,
        ..Settings::default()
    };
    let mut window_size = match std::env::var("MF_WINDOW_SIZE") {
        Ok(v) => parse_window_size(&v)?,
        Err(_) => DEFAULT_WINDOW_SIZE,
    };
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
        } else if arg == "--window-size" {
            let v = args
                .next()
                .ok_or_else(|| "--window-size needs WIDTHxHEIGHT".to_string())?;
            let v = v
                .to_str()
                .ok_or_else(|| "--window-size must be valid UTF-8".to_string())?;
            window_size = parse_window_size(v)?;
        } else if initial.is_none() {
            initial = Some(std::path::PathBuf::from(arg));
        } else {
            return Err(format!("unexpected argument {}", arg.to_string_lossy()));
        }
    }
    Ok((
        settings,
        Launch {
            initial,
            window_size,
        },
    ))
}

fn main() -> eframe::Result {
    let (settings, launch) = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: meshfixer-desktop [--threads N] [--window-size WxH] [file]"
            );
            std::process::exit(2);
        }
    };
    let (width, height) = launch.window_size;
    let initial = launch.initial;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MeshFixer")
            .with_inner_size([width, height])
            // A laptop window still fits the panels and the view (M4 final
            // UX I5); the panels collapse below that (View > Hide side panels).
            .with_min_inner_size([960.0, 600.0])
            // The app icon (assets/icon/meshfixer.svg, rendered at 256 px):
            // the Dock/taskbar icon when run outside a bundle.
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../assets/icon/meshfixer-256.png"
                ))
                .expect("the embedded app icon is a valid PNG"),
            )),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_window_size_accepts_widthxheight() {
        assert_eq!(parse_window_size("1440x900"), Ok((1440.0, 900.0)));
        assert_eq!(parse_window_size("1024X640"), Ok((1024.0, 640.0)));
    }

    #[test]
    fn parse_window_size_names_a_missing_separator() {
        let err = parse_window_size("1440").expect_err("no WIDTHxHEIGHT separator");
        assert!(err.contains("WIDTHxHEIGHT"), "{err}");
    }

    #[test]
    fn parse_window_size_rejects_non_positive_or_non_finite_dimensions() {
        for bad in ["0x900", "1440x0", "-1x900", "NaNx900", "1440xinf", "ax900"] {
            let err = parse_window_size(bad).expect_err("bad dimension");
            assert!(err.contains(bad), "{bad:?} -> {err}");
        }
    }

    #[test]
    fn default_window_size_matches_the_demo_script() {
        // scripts/demo.sh and docs/milestones/m4-demo.md both assume the
        // app opens at 1440x900 with no flags.
        assert_eq!(DEFAULT_WINDOW_SIZE, (1440.0, 900.0));
    }
}
