//! MeshFixer desktop app: a thin client of the command/session core.
//! Geometry work never runs on the UI thread (MF-RT-001).

pub mod analysis;
pub mod app;
pub mod defects;
pub mod framing;
pub mod gpu;
pub mod i18n;
pub mod icons;
pub mod import_dialog;
pub mod metrics;
mod panels;
pub mod sculpt_tool;
pub mod shape_tools;
pub mod shell;
pub mod theme;
mod tool_panels;
pub mod tools;
pub mod transform_tool;
pub mod viewport;
pub mod widgets;
pub mod worker;

pub use app::{MeshFixerApp, NarrowFocus, Settings, TreeItem};
