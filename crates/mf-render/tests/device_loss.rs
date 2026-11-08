#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Device loss in its own test binary: destroying the shared device would
//! break GPU tests running concurrently in the same process.

use mf_fixtures::cube;
use mf_render::{RenderError, offscreen};

#[test]
fn render_recovers_after_the_shared_device_is_lost() {
    // MF-ISSUE-024: a lost device must not be cached for the process lifetime.
    let (device, _) = match offscreen::shared_device() {
        Err(RenderError::NoAdapter) => {
            mf_fixtures::skip::gpu_unavailable("render_recovers_after_the_shared_device_is_lost");
            return;
        }
        other => other.unwrap(),
    };
    device.destroy();
    let img = offscreen::render_image(&cube(1.0), 32, 32, false).unwrap();
    assert_ne!(
        img.get_pixel(16, 16).0,
        offscreen::BACKGROUND_RGBA,
        "rendered with a fresh device"
    );
}
