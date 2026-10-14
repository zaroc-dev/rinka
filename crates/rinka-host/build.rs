//! Embeds the Windows application manifest.
//!
//! Wallpaper windows on Windows 11 24H2 and later are layered child windows,
//! which Windows only allows for processes whose manifest declares Windows 8
//! or later. The manifest also makes the host per-monitor DPI aware, so
//! monitor rectangles and surfaces are in physical pixels.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::{embed_manifest, manifest::DpiAwareness, new_manifest};
        embed_manifest(new_manifest("Rinka.Host").dpi_awareness(DpiAwareness::PerMonitorV2Only))
            .expect("embedding the Windows manifest");
    }
}
