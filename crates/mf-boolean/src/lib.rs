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
//! - [`remove_created_degenerates`]: the exact post-pass that removes
//!   zero-area triangles a kernel created (Task 7b).

mod cleanup;
mod kernel;
mod manifold;
#[cfg(feature = "manifold-cpp")]
mod manifold_cpp;
mod validate;

pub use cleanup::{DegenerateCleanup, remove_created_degenerates};
pub use kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};
pub use manifold::{BridgeGuard, ManifoldRobust, bridge};
#[cfg(feature = "manifold-cpp")]
pub use manifold_cpp::ManifoldCpp;
pub use validate::{SolidError, self_intersecting_faces, validate_boolean, validate_solid};
