# ADR-0002b: A home-grown exact boolean kernel for D5 windows (outline plan)

Status: **Proposed**: Phase 1 Task 7b, 2026-11-10. Written because no kernel passed every gate of the Task 7b re-qualification (ADR-0002). Needs the owner's ruling before any of it is built.

## Why

ADR-0002 measured two kernels through the same harness (`crates/mf-boolean/tests/qualify.rs`), with corrected gates (new self-intersections only; D5 windows instead of whole 1M operands):

- **`manifold-rust` 0.19.2 (robust, exact).** It takes self-intersecting scans and resolves them; the degenerate post-pass removes every sliver it creates (puritan now gives 3 valid results). But it still returns new self-intersections on 8 of the 11 corpus results it finishes (bunny-peel, sullivan, winged victory), runs out of the heap limit on winged victory ∪ itself, and is too slow through D5 windows: the 50k part on winged victory takes 4.2 s against 3 s, and 50k × 50k 1.7 s against 0.5 s.
- **C++ Manifold 3.5.3 (float, symbolic perturbation).** 10–300× inside every time limit (2–6× with the exact operand check), cancels in milliseconds, deterministic, exact provenance, on operands that don't intersect themselves. On scans that do, it returns invalid output (overlapping sheets, vertices up to 0.6% of the model's size off their source faces, up to 384k new intersecting pairs). With an exact precondition that refuses such operands by name, every result is valid or refused, but it refuses the Archaeopteryx window (50 window faces: the scan's own crossings under the part).

The gap is precise: **exact handling of self-intersecting operands (what the Rust engine does) at window sizes in well under a second (what the C++ engine does).** Real scans intersect themselves; D5 makes the work local, so the kernel only needs to be fast at 10⁴–10⁵ triangles, not 10⁶–10⁷.

## Scope

A kernel behind the existing `mf_boolean::Kernel` trait, fed D5 windows: two closed operands of up to ~300k triangles each, possibly self-intersecting, possibly with coplanar overlaps. Not a general mesh-arrangement library: no open input (the window caps close it, D5/D15), no Minkowski, no 2D.

Targets (the Task 7b gates, unchanged): window cases ≤ 3 s end to end on the M2 Max (aim for ≤ 0.5 s of kernel time at 150k + 50k), 50k × 50k ≤ 0.5 s, cancel ≤ 250 ms, bit-identical at any thread count, provenance exact, corpus valid-or-refused.

## Design outline

The approach of Cherchi, Pellacini, Attene and Livesu, *Interactive and Robust Mesh Booleans* (SIGGRAPH Asia 2022), building on *Fast and Robust Mesh Arrangements using Floating-point Arithmetic* (2020): exact decisions with **indirect predicates** on implicit points, so nothing is constructed in rationals.

1. **Input.** f64 vertices are exact. Each output triangle keeps the label of the input triangle it lies on (`KernelResult::origin`, as now).
2. **Broad phase.** A grid or BVH over both operands' triangles (the grid in `validate.rs` is the starting point), including pairs within one operand: self-intersections are resolved, not assumed away.
3. **Exact intersection.** Triangle–triangle intersection with Shewchuk's `orient3d` (the `robust` crate, D2). New points are **implicit**: LPI (line–plane) and TPI (three planes), stored as the input triangles/edges that define them, never rounded during the computation.
4. **Indirect predicates.** `orient2d`/`orient3d` on implicit points evaluated with a floating-point filter, then an interval filter, then exact expansion arithmetic (Attene 2020, *Indirect predicates for geometric constructions*). This is the core work: a Rust port of the predicate set (there is no maintained crate), fuzzed against `dashu` rationals.
5. **Per-triangle retriangulation.** Each intersected triangle is cut by its intersection segments with a constrained Delaunay triangulation in the triangle's own plane, on the indirect predicates. `spade` (D3) needs explicit coordinates, so this is our own incremental CDT, or `spade` behind a coordinate shim if its predicate hooks allow it (to check).
6. **Coplanar overlaps.** Coplanar triangle pairs are merged into one planar arrangement per plane group before step 5.
7. **Classification by winding number.** Patches between intersection curves are labelled by generalised winding number with respect to each operand, propagated across patches and decided once per connected region with an exact ray test. The `{w ≥ 1}` rule handles self-overlapping operands the way `manifold-rust`'s robust engine does.
8. **Output.** Implicit points are rounded to f64 once, at the end. Rounding can create new intersections or zero-area triangles; the Task 7b degenerate post-pass and D4's `validate_boolean` catch both, and a rounding failure is refused by name (`rounding_created_intersection`), never hidden. Snap rounding is the later fix if measurements show it matters.
9. **Runtime.** Steps 2–5 and 7 are data-parallel on `rayon` with deterministic reductions (sorted keys, no float reduction order dependence). Every loop checks `RunContext` (cancel ≤ 250 ms), and phases report progress.

## Plan (each step qualified with `qualify.rs` before the next)

| # | Step | Exit criterion | Estimate |
|---|---|---|---|
| 1 | Indirect predicates (LPI/TPI, orient2d/3d) + fuzz against rationals | 10⁹ random and 10⁶ adversarial cases agree with `dashu` | 4–6 weeks |
| 2 | Broad phase, exact tri–tri, implicit intersection segments | segments match `manifold-rust`'s on the corpus windows | 3–4 weeks |
| 3 | Per-triangle CDT on implicit points, coplanar groups | every corpus window's arrangement is a valid closed surface | 5–7 weeks |
| 4 | Winding classification, output, provenance | gates 1–3 on the corpus and windows | 3–4 weeks |
| 5 | Parallelism, cancel, progress, profiling | gates 4–5 | 2–4 weeks |

Total 4–6 months of specialist work, as D1 estimated. Booleans stay labelled limited, with an issue, until step 5 passes.

## Alternatives the owner can rule on instead

- **A. Speed up `manifold-rust`'s robust engine upstream** (it misses by 1.4–3.4× on the gated windows, measured at load 9–22, and still emits new self-intersections on three scans, which upstream would also have to fix). Profile the 50k × 50k case, contribute fixes (Apache-2.0, sponsored maintainer). Cheaper than this plan if the gap is a few hot spots; unknowable without profiling. Measure the gated windows again on an unloaded machine first: the Task 7b runs were at load 9–22.
- **B. C++ Manifold with the exact operand check, as an interim** (ADR-0002, "cpp"). Every result is valid or refused by name; booleans refuse where the scan intersects itself near the tool, with "repair self-intersections first" as the next action. It costs a C++ toolchain, CMake and oneTBB in CI on three OSes (ADR-0002). It is not a fallback: one kernel, one precondition, no retry. It changes what MF-MODEL-001 promises on real scans, so it is the owner's call.
- **C. Either kernel chosen per window by an exact self-intersection test** (C++ for clean windows, `manifold-rust` for the rest). Rejected here: it is two kernels with two failure modes, and close to the "silently try another method" that D4 rules out.

## Consequences

- MF-MODEL-001 and P1-M2 Tasks 8–10 stay blocked on the owner's choice between this plan, A and B.
- The trait, the adapters, `validate_boolean`, the degenerate post-pass and the qualification harness stay; a home-grown kernel is qualified with the same `qualify.rs`.
