// A minimal C ABI over the C++ Manifold library (elalish/manifold), for
// mf-manifold-sys (Phase 1 Task 7b, ADR-0002). Upstream's own C binding
// can't set MeshGL64::faceID, which per-triangle provenance needs, and
// can't bound the thread count, which the determinism gate needs.
//
// One call does the whole boolean: import both operands through a
// cancellable ExecutionContext, evaluate the boolean observing it, and
// export the result with each output triangle's (operand, input triangle).
// No C++ exception crosses the boundary.

#include <atomic>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <numeric>
#include <string>
#include <vector>

#include "manifold/manifold.h"
#include "tbb/task_arena.h"

using manifold::ExecutionContext;
using manifold::Manifold;
using manifold::MeshGL64;
using manifold::OpType;

struct MfmCtx {
  ExecutionContext ec;
};

struct MfmOut {
  std::vector<double> positions;   // 3 per vertex
  std::vector<uint64_t> triangles; // 3 per triangle
  std::vector<uint8_t> operand;    // per triangle
  std::vector<uint64_t> face;      // per triangle
};

// Status codes returned by mfm_boolean. 0..=255 are Manifold::Error values
// (with *failed_operand set to 0 or 1 when an import failed, else -1).
static const int MFM_EXCEPTION = 1000;
static const int MFM_BAD_RUN = 1001;
static const int MFM_BAD_FACE = 1002;

static thread_local std::string last_error;

extern "C" {

MfmCtx* mfm_ctx_new() {
  try {
    return new MfmCtx();
  } catch (...) {
    return nullptr;
  }
}

void mfm_ctx_free(MfmCtx* ctx) { delete ctx; }

void mfm_ctx_cancel(MfmCtx* ctx) { ctx->ec.Cancel(); }

double mfm_ctx_progress(const MfmCtx* ctx) { return ctx->ec.Progress(); }

const char* mfm_last_error() { return last_error.c_str(); }

size_t mfm_out_num_vert(const MfmOut* out) { return out->positions.size() / 3; }

size_t mfm_out_num_tri(const MfmOut* out) { return out->operand.size(); }

void mfm_out_copy(const MfmOut* out, double* positions, uint64_t* triangles,
                  uint8_t* operand, uint64_t* face) {
  std::memcpy(positions, out->positions.data(),
              out->positions.size() * sizeof(double));
  std::memcpy(triangles, out->triangles.data(),
              out->triangles.size() * sizeof(uint64_t));
  std::memcpy(operand, out->operand.data(), out->operand.size());
  std::memcpy(face, out->face.data(), out->face.size() * sizeof(uint64_t));
}

void mfm_out_free(MfmOut* out) { delete out; }

}  // extern "C"

static MeshGL64 to_mesh(const double* pos, size_t nv, const uint64_t* tri,
                        size_t nt, uint32_t original_id) {
  MeshGL64 m;
  m.numProp = 3;
  m.vertProperties.assign(pos, pos + 3 * nv);
  m.triVerts.assign(tri, tri + 3 * nt);
  m.runOriginalID = {original_id};
  // Each triangle its own face ID: provenance per input triangle, and
  // Manifold never merges two input triangles into one output triangle.
  m.faceID.resize(nt);
  std::iota(m.faceID.begin(), m.faceID.end(), 0);
  return m;
}

static int run(MfmCtx* ctx, const double* pa, size_t nva, const uint64_t* ta,
               size_t nta, const double* pb, size_t nvb, const uint64_t* tb,
               size_t ntb, int op, MfmOut** out, int* failed_operand) {
  *failed_operand = -1;
  const uint32_t first = Manifold::ReserveIDs(2);
  const uint32_t ids[2] = {first, first + 1};
  const Manifold ma = ctx->ec.FromMeshGL(to_mesh(pa, nva, ta, nta, ids[0]));
  if (ma.Status() != Manifold::Error::NoError) {
    *failed_operand = 0;
    return static_cast<int>(ma.Status());
  }
  const Manifold mb = ctx->ec.FromMeshGL(to_mesh(pb, nvb, tb, ntb, ids[1]));
  if (mb.Status() != Manifold::Error::NoError) {
    *failed_operand = 1;
    return static_cast<int>(mb.Status());
  }
  const OpType kind =
      op == 0 ? OpType::Add : (op == 1 ? OpType::Subtract : OpType::Intersect);
  const Manifold r = ma.Boolean(mb, kind).WithContext(ctx->ec);
  const Manifold::Error status = r.Status();
  if (status != Manifold::Error::NoError) return static_cast<int>(status);
  if (ctx->ec.Cancelled()) return static_cast<int>(Manifold::Error::Cancelled);

  const MeshGL64 gl = r.GetMeshGL64();
  auto result = std::make_unique<MfmOut>();
  const size_t stride = gl.numProp;
  const size_t nv = gl.vertProperties.size() / stride;
  result->positions.resize(3 * nv);
  for (size_t v = 0; v < nv; ++v)
    for (size_t k = 0; k < 3; ++k)
      result->positions[3 * v + k] = gl.vertProperties[stride * v + k];
  result->triangles = gl.triVerts;
  const size_t nt = gl.triVerts.size() / 3;
  if (gl.faceID.size() != nt) return MFM_BAD_FACE;
  result->operand.resize(nt);
  result->face.resize(nt);
  for (size_t r_ = 0; r_ < gl.runOriginalID.size(); ++r_) {
    const uint32_t id = gl.runOriginalID[r_];
    if (id != ids[0] && id != ids[1]) return MFM_BAD_RUN;
    const uint8_t operand = id == ids[0] ? 0 : 1;
    const size_t start = gl.runIndex[r_] / 3;
    const size_t end = gl.runIndex[r_ + 1] / 3;
    for (size_t t = start; t < end; ++t) {
      result->operand[t] = operand;
      result->face[t] = gl.faceID[t];
    }
  }
  *out = result.release();
  return 0;
}

extern "C" int mfm_boolean(MfmCtx* ctx, int threads, const double* pa,
                           size_t nva, const uint64_t* ta, size_t nta,
                           const double* pb, size_t nvb, const uint64_t* tb,
                           size_t ntb, int op, MfmOut** out,
                           int* failed_operand) {
  *out = nullptr;
  try {
    auto call = [&] {
      return run(ctx, pa, nva, ta, nta, pb, nvb, tb, ntb, op, out,
                 failed_operand);
    };
    if (threads > 0) {
      tbb::task_arena arena(threads);
      return arena.execute(call);
    }
    return call();
  } catch (const std::exception& e) {
    last_error = e.what();
    return MFM_EXCEPTION;
  } catch (...) {
    last_error = "unknown C++ exception";
    return MFM_EXCEPTION;
  }
}
