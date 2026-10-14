//! Wallpaper Engine import: reads workshop items and converts the supported
//! subset into native Rinka projects with a structured report.

pub mod assets;
pub mod convert;
mod particles;
pub mod pkg;
pub mod preprocess;
pub mod report;
pub mod shader;
pub mod tex;
pub mod value;

pub use convert::{Options, import};
pub use report::{Report, Status};
