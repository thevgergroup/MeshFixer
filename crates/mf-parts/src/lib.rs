//! Parts, connectors and print fit for MeshFixer (Phase 1 D16, D18, D20,
//! D22).
//!
//! - The print fit profile ([`FitProfile`], D20) and the config-directory
//!   adapter ([`dirs`], MF-PLAT-001).
//! - The parts library (D16, Task 21): built-ins generated in code and the
//!   user's folder ([`Library`]), [`SurfaceFrame`] and [`place`].

mod builtin;
pub mod dirs;
mod fit;
mod frame;
mod library;
mod manifest;

pub use builtin::builtins;
pub use fit::{BUILTIN_NAME, FIT_PROFILES_FILE, FitError, FitProfile, FitProfiles};
pub use frame::{SurfaceFrame, place};
pub use library::{
    Library, LibraryPart, LibraryWarning, MANIFEST_FILE, PartError, save_object_as_part,
    user_parts_dir,
};
pub use manifest::{Attach, PartKind, PartManifest};
