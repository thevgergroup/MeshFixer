//! A minimal, safe binding to the C++ Manifold library (elalish/manifold
//! v3.5.3, Apache-2.0) for `mf-boolean`'s `ManifoldCpp` kernel (Phase 1
//! Task 7b, ADR-0002). It exists so `mf-boolean` stays free of `unsafe`.
//!
//! The whole boolean is one call ([`boolean`]): import both operands,
//! evaluate, export. Each output triangle comes back with the operand and
//! input triangle it lies on (Manifold's `faceID`, which upstream's C API
//! can't set). [`Context`] carries Manifold's `ExecutionContext`: cancel
//! it from any thread and the call returns [`Error::Cancelled`] at the
//! kernel's next check; read its progress while it runs.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr::NonNull;

#[repr(C)]
struct MfmCtx {
    _private: [u8; 0],
}

#[repr(C)]
struct MfmOut {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn mfm_ctx_new() -> *mut MfmCtx;
    fn mfm_ctx_free(ctx: *mut MfmCtx);
    fn mfm_ctx_cancel(ctx: *mut MfmCtx);
    fn mfm_ctx_progress(ctx: *const MfmCtx) -> f64;
    fn mfm_last_error() -> *const c_char;
    fn mfm_out_num_vert(out: *const MfmOut) -> usize;
    fn mfm_out_num_tri(out: *const MfmOut) -> usize;
    fn mfm_out_copy(
        out: *const MfmOut,
        positions: *mut f64,
        triangles: *mut u64,
        operand: *mut u8,
        face: *mut u64,
    );
    fn mfm_out_free(out: *mut MfmOut);
    #[allow(clippy::too_many_arguments)]
    fn mfm_boolean(
        ctx: *mut MfmCtx,
        threads: c_int,
        pa: *const f64,
        nva: usize,
        ta: *const u64,
        nta: usize,
        pb: *const f64,
        nvb: usize,
        tb: *const u64,
        ntb: usize,
        op: c_int,
        out: *mut *mut MfmOut,
        failed_operand: *mut c_int,
    ) -> c_int;
}

/// Manifold's `ExecutionContext`: cancel and progress for one boolean.
pub struct Context {
    ptr: NonNull<MfmCtx>,
}

// SAFETY: Manifold documents `ExecutionContext::Cancel`, `Cancelled` and
// `Progress` as callable from any thread (atomics inside); the context is
// freed only by `Drop`, which needs exclusive ownership.
unsafe impl Send for Context {}
// SAFETY: as above; every method takes `&self` and is thread-safe in C++.
unsafe impl Sync for Context {}

impl Context {
    pub fn new() -> Context {
        // SAFETY: no preconditions; returns null only if allocation failed.
        let ptr = unsafe { mfm_ctx_new() };
        Context {
            ptr: NonNull::new(ptr).expect("could not allocate a Manifold ExecutionContext"),
        }
    }

    /// Requests cancellation. Sticky; callable from any thread.
    pub fn cancel(&self) {
        // SAFETY: `ptr` is live until `Drop`; `Cancel` is thread-safe.
        unsafe { mfm_ctx_cancel(self.ptr.as_ptr()) }
    }

    /// Manifold's progress in `[0, 1]` (1 when nothing is scheduled).
    pub fn progress(&self) -> f64 {
        // SAFETY: `ptr` is live until `Drop`; `Progress` is thread-safe.
        unsafe { mfm_ctx_progress(self.ptr.as_ptr()) }
    }
}

impl Default for Context {
    fn default() -> Self {
        Context::new()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: allocated by `mfm_ctx_new`, freed exactly once here.
        unsafe { mfm_ctx_free(self.ptr.as_ptr()) }
    }
}

/// An indexed triangle mesh going in.
pub struct MeshIn<'a> {
    pub positions: &'a [[f64; 3]],
    pub triangles: &'a [[u64; 3]],
}

/// The result: an indexed mesh, and per triangle the operand (0 or 1) and
/// input triangle index it lies on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshOut {
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u64; 3]>,
    pub operand: Vec<u8>,
    pub face: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Union,
    Subtract,
    Intersect,
}

/// Why there is no result. Manifold's own status codes keep their names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    /// An operand failed to import (`operand` 0 or 1), with Manifold's
    /// status name (e.g. `NotManifold`).
    Import {
        operand: u8,
        status: &'static str,
    },
    /// The boolean failed with Manifold's status name.
    Boolean {
        status: &'static str,
    },
    /// A C++ exception, with its message.
    Exception(String),
    /// The output's run or face IDs didn't name an input triangle.
    BadProvenance(&'static str),
}

fn status_name(code: c_int) -> &'static str {
    match code {
        1 => "NonFiniteVertex",
        2 => "NotManifold",
        3 => "VertexOutOfBounds",
        4 => "PropertiesWrongLength",
        5 => "MissingPositionProperties",
        6 => "MergeVectorsDifferentLengths",
        7 => "MergeIndexOutOfBounds",
        8 => "TransformWrongLength",
        9 => "RunIndexWrongLength",
        10 => "FaceIDWrongLength",
        11 => "InvalidConstruction",
        12 => "ResultTooLarge",
        13 => "InvalidTangents",
        14 => "Cancelled",
        _ => "Unknown",
    }
}

/// Computes `a op b`. `threads`: `Some(n)` runs in a oneTBB arena of `n`
/// threads, `None` uses oneTBB's default.
pub fn boolean(
    a: &MeshIn<'_>,
    b: &MeshIn<'_>,
    op: Op,
    threads: Option<usize>,
    ctx: &Context,
) -> Result<MeshOut, Error> {
    let op = match op {
        Op::Union => 0,
        Op::Subtract => 1,
        Op::Intersect => 2,
    };
    let threads = threads.map_or(0, |n| n.clamp(1, c_int::MAX as usize) as c_int);
    let mut out: *mut MfmOut = std::ptr::null_mut();
    let mut failed: c_int = -1;
    // SAFETY: the slices are valid for the lengths passed ([f64; 3] and
    // [u64; 3] are contiguous); `ctx.ptr` is live; the shim catches every
    // C++ exception; `out` and `failed` are valid for writes.
    let code = unsafe {
        mfm_boolean(
            ctx.ptr.as_ptr(),
            threads,
            a.positions.as_ptr().cast(),
            a.positions.len(),
            a.triangles.as_ptr().cast(),
            a.triangles.len(),
            b.positions.as_ptr().cast(),
            b.positions.len(),
            b.triangles.as_ptr().cast(),
            b.triangles.len(),
            op,
            &mut out,
            &mut failed,
        )
    };
    match code {
        0 => {}
        14 => return Err(Error::Cancelled),
        1000 => {
            // SAFETY: the shim returns a pointer to its thread-local,
            // NUL-terminated message, valid until the next call on this thread.
            let msg = unsafe { CStr::from_ptr(mfm_last_error()) };
            return Err(Error::Exception(msg.to_string_lossy().into_owned()));
        }
        1001 => return Err(Error::BadProvenance("an output run names neither operand")),
        1002 => return Err(Error::BadProvenance("face IDs don't match the triangles")),
        code if failed == 0 || failed == 1 => {
            return Err(Error::Import {
                operand: failed as u8,
                status: status_name(code),
            });
        }
        code => {
            return Err(Error::Boolean {
                status: status_name(code),
            });
        }
    }
    let out = NonNull::new(out).ok_or(Error::Exception("no result returned".into()))?;
    // SAFETY: `out` was returned by `mfm_boolean` and is freed below.
    let (nv, nt) = unsafe {
        (
            mfm_out_num_vert(out.as_ptr()),
            mfm_out_num_tri(out.as_ptr()),
        )
    };
    let mut r = MeshOut {
        positions: vec![[0.0; 3]; nv],
        triangles: vec![[0; 3]; nt],
        operand: vec![0; nt],
        face: vec![0; nt],
    };
    // SAFETY: each buffer holds exactly what the shim reported; `out` is
    // live, then freed exactly once.
    unsafe {
        mfm_out_copy(
            out.as_ptr(),
            r.positions.as_mut_ptr().cast(),
            r.triangles.as_mut_ptr().cast(),
            r.operand.as_mut_ptr(),
            r.face.as_mut_ptr(),
        );
        mfm_out_free(out.as_ptr());
    }
    Ok(r)
}
