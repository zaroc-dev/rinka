//! Shared Rinka engine.
//!
//! Owns project semantics and rendering. It must not depend on Electron, Node,
//! Win32 or Wayland: hosts hand it a wgpu device and a render target view.

pub mod backdrop;
pub mod capture;
pub mod color;
pub mod gpu;
pub mod image;
pub mod mesh;
pub mod particles;
pub mod project;
pub mod render;
pub mod scene;
pub mod video;

pub use backdrop::BackdropRenderer;
pub use color::Srgb8;
pub use gpu::{FrameOutcome, FrameViews, GpuInfo, SurfaceSetup};
pub use image::{ImageData, ImageError};
pub use project::{Content, Fit, Project, ProjectError};
pub use render::{Frame, GpuImage, Renderer, Source, Target, VideoTexture};
pub use scene::{GpuScene, SceneAssets};
pub use video::{ColorMatrix, ColorRange, YuvFrame};

/// Engine build version, reported to hosts and recorded with reference captures.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
