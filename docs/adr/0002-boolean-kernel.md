# ADR-0002: Boolean kernel

Status: **Rejected for shipping (NO-GO)**: Phase 1 Task 7 qualification, 2026-11-10. `manifold-rust` =0.19.2 failed gates 1, 3 (on real models), 4 and 5. Booleans don't ship on it. Per D1, work on booleans stops until there is a replacement-kernel plan (ADR-0002b).

## Context

D1 (Phase 1 plan) picked `manifold-rust` 0.19.2's robust engine as the boolean kernel. It runs exact rational arithmetic on a mesh arrangement. It sits behind the `mf-boolean::Kernel` trait, on the condition that it passes the Task 7 qualification gate. D4 says every result is validated independently and an invalid result is refused by name. D5 says booleans cost what the overlap costs.

Task 7 built:
- the adapter (`ManifoldRobust`, `crates/mf-boolean/src/manifold.rs`);
- the cancel and progress bridge;
- a minimal exact `validate_solid` (`crates/mf-boolean/src/validate.rs`). It checks that the mesh is closed, edge-manifold and consistently oriented, that no face is degenerate, and that it doesn't self-intersect. It uses `robust` predicates over a grid broad phase.
- the five gates (`crates/mf-boolean/tests/qualify.rs`).

## Gate results

Measured on an Apple M2 Max (12 cores, 32 GB) in the test harness. `manifold-rust` and `dashu` ran at opt-level 3 without debug assertions; there was no LTO. The machine was shared with another agent's work: load average 15–35, and swap almost full. That probably inflates the times, but by less than the margins they missed by. 1M × 1M ran *faster* on 1 thread (21.8 s) than on 11 (31.6 s). Raw results: `artifacts/phase1/p1-m2/qualify.json`. The corpus run hit a bug in the test harness on its last model and was finished by re-running that model alone (the note in the JSON says how).

| # | Gate | Result | Verdict |
|---|---|---|---|
| 1 | Corpus: each of the 15 messy models, unioned with a sphere at its centroid, unioned with itself shifted 10%, and minus a box. Every result must be valid or a named error. | 11 models are open, so their 33 operations were refused with `not_closed` (correct). The 4 closed models (puritan, bunny-peel, sullivan, winged victory) gave 12 operations: **1 valid, 8 invalid for `degenerate_face`, 2 invalid for `self_intersecting`, 1 timed out.** The degenerate faces are zero-area triangles the kernel *created* on input faces that had area: 2–54 per result, none passed through from degenerate input. On puritan, every self-intersection found once they are set aside touches one of those slivers. The two self-intersecting results are winged victory ∪ sphere (6 pairs) and winged victory − box (13 pairs), both with no degenerate faces. The timeout was winged victory ∪ itself shifted (5.7M + 5.7M triangles), still running at 300 s with a 7.6 GB peak. 0 panics. | **FAIL** |
| 2 | Determinism: 1 vs N threads, and two runs | Identical hashes everywhere it was measured: 20k spheres (all 3 operations, threads 1/11/11/1), 1M × 1M (two runs at 11 threads, and 1 thread), every computed corpus result under 500k triangles (1 thread vs 11), and winged victory ∪ sphere across the two corpus runs (5.7M input). | PASS |
| 3 | Provenance: `origin` names a real face; coverage within 1e-9 | Synthetic cases pass at ~1e-17: spheres, box ∩ sphere, coplanar boxes, 50k and 1M spheres. On real models it fails. The worst output vertex is 7.4e-6 of the bounding-box diagonal off its named face's plane (sullivan, bunny-peel, winged victory at 1.8e-9). Output on one face covers up to 4× its area (sullivan). Every `origin` named a real input face. One caveat: the test's "untouched face" check expects a face outside the other operand's box to be kept whole or dropped. That is wrong for faces cut by their own model's self-intersections, so its 0.37–1.06 errors on real scans overstate the problem. The plane-distance and over-coverage findings don't depend on that check. Neither has been root-caused. | **FAIL** (real models) |
| 4 | Cancel-to-stop ≤ 250 ms on 1M × 1M | Inside the engine: 12–71 ms. During operand import, `Manifold::from_mesh_gl64_robust` takes no cancel token and runs about 1.3 s per 1M operand, so the measured worst case is **695 ms**. | **FAIL** |
| 5 | Time: 1M × 1M ≤ 10 s, 50k × 50k ≤ 0.5 s | 1M × 1M: **31.6 s** (29.4 s on a second run, 21.8 s on 1 thread), 1.94 GB peak heap, 1.36M-triangle output, valid. 50k × 50k: **1.83 s** median of 3, 0.10 GB peak. | **FAIL** |

On synthetic, clean, closed input (spheres, boxes) every result was valid, exact (cube volumes to 1e-12), deterministic, and carried exact provenance. The engine's design is sound. What fails is speed, cancelling during import, and output quality on real scans.

## Decision

**NO-GO.** `manifold-rust` 0.19.2 does not qualify as the D1 kernel:
- **Gate 1:** it produced invalid output (zero-area slivers and self-intersections) on 10 of the 11 real-model booleans it finished. It timed out on the 12th (5.7M × 5.7M).
- **Gate 3:** its provenance doesn't hold on real models.
- **Gate 4:** its import isn't cancellable.
- **Gate 5:** it is 3–4× over the time budget at 50k and 1M.

D4 would refuse every one of those invalid results by name, so shipping it would not produce bad models. But the boolean would mostly refuse on real scans, which defeats MF-MODEL-001.

The `mf-boolean::Kernel` trait, the adapter, the bridge, `validate_solid` and the qualification suite stay. A replacement kernel is qualified with the same `qualify.rs`.

## Deviations from the plan's reading of the API (checked against the 0.19.2 source)

- **`robust::boolean_with_progress`** takes `&ManifoldImpl`, not `MeshGL64`. The adapter imports with `Manifold::from_mesh_gl64_robust`. It then calls `Manifold::boolean_with_engine_rule_and_progress(…, BooleanEngine::Robust, WindingRule::Positive, Some(&token), Some(&reporter))`, which goes straight to `robust::boolean_with_rule`, so it is the same pipeline. The float engine is never called.
- **Provenance** works as planned. `run_original_id` is set per operand (from `Manifold::reserve_ids(2)`) and `face_id` per triangle. `get_mesh_gl64` exports each output triangle's `TriRef` as its run's original ID plus its `face_id`. Subtracted-operand runs and flipped siblings keep the original ID.
- **Cancel.** `manifold_rust::cancel::CancelToken` is its own `Arc<AtomicBool>` and can't wrap ours. So `bridge` runs a watcher thread that polls our token every 2 ms (unit-tested to forward within 10 ms). The import (`from_mesh_gl64_robust`) and export take no token. The adapter checks for cancellation between the stages, and that gap is what failed gate 4.
- **Progress.** `ProgressReporter::new` takes a `'static` callback, so it can't hold a borrowed `mf_runtime::Span`. The bridge maps the 9 robust phases onto 0.05–0.95 of a cloned `Progress` instead, as stages named `"boolean: <phase>"`, and keeps it monotonic. Import and export take the bands either side.

## Alternatives (for ADR-0002b)

As D1: a home-grown kernel behind the same trait (indirect predicates, Cherchi et al. 2022), or re-qualify a later `manifold-rust` once:
- its sliver clean-up is fixed;
- its import takes a token;
- its robust engine is faster.

The zero-area slivers alone could be removed by a deterministic, exact edge-flip pass after the kernel. That is a design change for the owner to rule on under D4, and it wouldn't fix gates 3–5.

## Consequences

- MF-MODEL-001 and P1-M2 Tasks 8–10 are blocked on ADR-0002b.
- The heavy gates (`qualify_corpus`, `qualify_1m_union`, `qualify_cancel`) are in the `scripts/run-tests.sh` timing list, and they fail until a kernel passes.
