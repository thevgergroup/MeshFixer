//! Embeds the app icon and version details in the Windows `.exe`, so
//! Explorer, the taskbar and Alt-Tab show the rabbit. Nothing to do on other
//! platforms (macOS gets its icon from the app bundle; see
//! scripts/bundle-macos.sh).
fn main() {
    println!("cargo:rerun-if-changed=assets/icon/MeshFixer.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/MeshFixer.ico")
            .set("ProductName", "MeshFixer")
            .set("FileDescription", "MeshFixer");
        if let Err(e) = res.compile() {
            panic!("embedding the Windows icon resource failed: {e}");
        }
    }
}
