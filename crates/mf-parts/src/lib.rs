//! Parts, connectors and print fit for MeshFixer (Phase 1 D16, D18, D20,
//! D22).
//!
//! So far (Task 12a): the print fit profile ([`FitProfile`], D20), the
//! source of every printing tolerance's default, and the marked
//! config-directory adapter ([`dirs`], MF-PLAT-001). The parts library and
//! connectors arrive in P1-M5 and P1-M6.

pub mod dirs;
mod fit;

pub use fit::{BUILTIN_NAME, FIT_PROFILES_FILE, FitError, FitProfile, FitProfiles};
