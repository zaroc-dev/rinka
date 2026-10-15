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
        copy_video_runtime();
    }
}

/// Keep development executables and packaged hosts independent of the
/// user's PATH. The installer copies these DLLs beside the host as well.
fn copy_video_runtime() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let Some(sdk) = std::env::var_os("FFMPEG_DIR") else {
        return;
    };
    let bin = std::path::PathBuf::from(sdk).join("bin");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let profile = out.ancestors().nth(3).expect("Cargo profile directory");
    for entry in std::fs::read_dir(bin).expect("reading the FFmpeg runtime directory") {
        let entry = entry.expect("FFmpeg runtime entry");
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !text.ends_with(".dll")
            || ![
                "avcodec-",
                "avformat-",
                "avutil-",
                "swscale-",
                "swresample-",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix))
        {
            continue;
        }
        let source = entry.path();
        println!("cargo:rerun-if-changed={}", source.display());
        let destination = profile.join(name);
        let bytes = std::fs::read(&source).expect("reading an FFmpeg runtime DLL");
        // A running host may have the DLL loaded. Avoid overwriting an
        // identical file, which Windows would refuse.
        if std::fs::read(&destination).ok().as_ref() != Some(&bytes) {
            std::fs::copy(source, destination).expect("copying an FFmpeg runtime DLL");
        }
    }
}
