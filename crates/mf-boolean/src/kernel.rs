//! The boolean kernel contract (D1): what any kernel behind `mesh.boolean`
//! takes, returns and refuses. The adapter for `manifold-rust`'s exact
//! engine is [`crate::ManifoldRobust`]; a replacement kernel implements the
//! same trait.

use mf_runtime::RunContext;

/// Which boolean to compute: `a ∪ b`, `a − b` or `a ∩ b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

/// One closed operand. `face_ids[t]` is the label triangle `t` is reported
/// under in [`KernelResult::origin`]: `(operand, face)`, chosen by the
/// caller (the window of D5 labels its temporary caps so it can remove
/// them by provenance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KernelMesh {
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub face_ids: Vec<(u8 /* operand */, u32 /* face */)>,
}

impl KernelMesh {
    /// A mesh whose faces are labelled `(operand, 0..n)`.
    pub fn labelled(positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>, operand: u8) -> Self {
        let face_ids = (0..triangles.len() as u32).map(|f| (operand, f)).collect();
        KernelMesh {
            positions,
            triangles,
            face_ids,
        }
    }
}

/// A kernel's output: an indexed triangle mesh, and for each output
/// triangle the label of the input face it lies on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KernelResult {
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub origin: Vec<(u8, u32)>,
}

/// Why a kernel produced no result. Every refusal is named; an invalid
/// result is never returned in place of one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KernelError {
    /// The operand (0 = `a`, 1 = `b`) isn't a closed, orientable surface,
    /// so it doesn't bound a solid.
    #[error("operand {operand} is not closed, so it does not bound a solid")]
    NotClosed { operand: u8 },
    #[error("cancelled by the user")]
    Cancelled,
    /// Anything else the kernel reported, verbatim.
    #[error("boolean kernel failed: {0}")]
    Internal(String),
}

impl KernelError {
    /// A stable, machine-readable code for logs and refusals.
    pub fn code(&self) -> &'static str {
        match self {
            KernelError::NotClosed { .. } => "not_closed",
            KernelError::Cancelled => "cancelled",
            KernelError::Internal(_) => "kernel_internal",
        }
    }
}

/// A boolean kernel. Implementations must be deterministic at any thread
/// count, check `ctx` for cancellation and report progress on it.
pub trait Kernel: Send + Sync {
    fn boolean(
        &self,
        a: &KernelMesh,
        b: &KernelMesh,
        op: BoolOp,
        ctx: &RunContext,
    ) -> Result<KernelResult, KernelError>;

    /// A stable identifier recorded with every result (e.g. in provenance).
    fn id(&self) -> &'static str;
}
