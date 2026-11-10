//! Booleans for MeshFixer (MF-MODEL-001, Phase 1 D1, D4, D18).
//!
//! - [`Kernel`]: the kernel contract, with [`ManifoldRobust`], the adapter
//!   for `manifold-rust`'s exact (rational-arithmetic) engine.
//! - [`validate_solid`]: the independent output check (D4), with exact
//!   predicates (D2). Phase 1 Task 7 builds its minimal form; Task 8 extends it.

mod kernel;
mod manifold;
mod validate;

pub use kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};
pub use manifold::{BridgeGuard, ManifoldRobust, bridge};
pub use validate::{SolidError, validate_solid};
