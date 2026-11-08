//! MeshFixer desktop app: a thin client of the command/session core.
//! Geometry work never runs on the UI thread (MF-RT-001).

pub mod app;
mod panels;
pub mod viewport;
mod worker;

pub use app::MeshFixerApp;
