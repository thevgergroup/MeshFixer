//! Booleans for MeshFixer (MF-MODEL-001, Phase 1 D1, D4, D18).
//!
//! - [`Kernel`]: the kernel contract, with [`ManifoldRobust`], the adapter
//!   for `manifold-rust`'s exact (rational-arithmetic) engine.
//!   Behind the `manifold-cpp` feature, `ManifoldCpp`: the C++ Manifold
//!   library, qualified alongside it in Task 7b (ADR-0002).
//! - [`validate_solid`]: the independent output check (D4), with exact
//!   predicates (D2), and [`validate_boolean`], the same check for a
//!   boolean result, counting only *new* self-intersections (Task 7b).
//!   Phase 1 Task 7 builds the minimal form; Task 8 extends it.
//! - [`plane_cut`] and [`cut_half`]: the exact plane cut with capped parts
//!   (D11, Task 12), our own code rather than the kernel.
//! - [`remove_created_degenerates`]: the exact post-pass that removes
//!   zero-area triangles a kernel created (Task 7b).

mod cleanup;
mod clip;
mod kernel;
#[cfg(feature = "manifold-rust")]
mod manifold;
#[cfg(feature = "manifold-cpp")]
mod manifold_cpp;
mod plane_cut;
pub mod primitives;
mod section;
mod validate;
mod weld;
mod window;

pub use cleanup::{DegenerateCleanup, remove_created_degenerates};
pub use clip::{ClipError, ClipOptions, Clipped, Point, WindowFace, clip_to_box};
pub use kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};
#[cfg(feature = "manifold-rust")]
pub use manifold::{BridgeGuard, ManifoldRobust, bridge};
#[cfg(feature = "manifold-cpp")]
pub use manifold_cpp::ManifoldCpp;
pub use plane_cut::{
    Bound, BoundCheck, CAPPING, CHECKING, CUTTING, CutError, CutParts, FINDING_THE_CUT, HalfCut,
    Plane, PlaneFrame, SectionRegion, Side, check_closed, classify_region, cut_half, cut_half_with,
    cut_half_with_regions, plane_cut,
};
pub use section::{SectionError, triangulate_section};
pub use validate::{SolidError, self_intersecting_faces, validate_boolean, validate_solid};
pub use weld::weld_exact;
pub use window::{
    BooleanError, BooleanOutput, STAGES, apply, boolean, compose, inverse, operand_in_frame,
};
