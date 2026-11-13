//! Parts, connectors and print fit for MeshFixer (Phase 1 D16, D18, D20,
//! D22).
//!
//! - The print fit profile ([`FitProfile`], D20) and the config-directory
//!   adapter ([`dirs`], MF-PLAT-001).
//! - The parts library (D16, Task 21): built-ins generated in code and the
//!   user's folder ([`Library`]), [`SurfaceFrame`] and [`place`].

mod builtin;
mod builtin_shapes;
mod connector;
pub mod connectors;
pub mod dirs;
mod fit;
mod fit_check;
mod frame;
pub mod generator;
mod library;
mod machine;
mod manifest;
pub mod shape;
pub mod swing;

pub use builtin::builtins;
pub use connector::{ConnectorError, JoinKind, Side};
pub use fit::{
    BUILTIN_NAME, Compensation, FIT_PROFILES_FILE, FitClass, FitClasses, FitError, FitProfile,
    FitProfiles, fill_fit_role_params, resolve,
};
pub use fit_check::{FitCheck, FitExpectation, MatingSurfaces, check_fit};
pub use frame::{SurfaceFrame, place};
pub use generator::RegisterError as GeneratorRegisterError;
pub use generator::{
    Anchor, FitProbe, Fix, GenCheck, GenContext, Generator, Handles, ParamPatch, PlacementHint,
    ShapePart, TargetSlot, Targets, cut_generators, find_generator, generators, register_generator,
};
pub use library::{
    Library, LibraryPart, LibraryWarning, MANIFEST_FILE, PartError, delete_user_part,
    save_connector_as_part, save_object_as_part, update_user_part, user_parts_dir,
};
pub use machine::{
    MACHINE_PROFILES_FILE, MachineError, MachineProfile, MachineProfiles, PRINT_VOLUME_PRESETS,
    UNCALIBRATED_NAME,
};
pub use manifest::{Attach, PartKind, PartManifest};
pub use shape::{GeneratorRecord, ShapeRole, builtin_shapes, new_record, record_spec, regenerate};
