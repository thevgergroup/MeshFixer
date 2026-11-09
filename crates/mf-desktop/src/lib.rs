//! MeshFixer desktop app: a thin client of the command/session core.
//! Geometry work never runs on the UI thread (MF-RT-001).

pub mod analysis;
pub mod app;
pub mod defects;
pub mod framing;
pub mod gpu;
pub mod metrics;
mod panels;
pub mod theme;
pub mod viewport;
pub mod worker;

pub use app::{MeshFixerApp, Settings, TreeItem};
