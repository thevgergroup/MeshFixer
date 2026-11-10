# ADR-0002: Boolean kernel

Status: **No kernel qualifies (re-qualified in Task 7b, 2026-11-10).** `manifold-rust` 0.19.2 fails gates 1, 3 and 5. C++ Manifold 3.5.3 passes gates 1–4 but fails gate 5: its exact operand check refuses a gated window. The outline plan for our own kernel is ADR-0002b (`0002b-home-grown-kernel.md`), with two cheaper alternatives for the owner to rule on. Booleans don't ship until one of them passes these gates.

## Context

D1 (Phase 1 plan) picked `manifold-rust` 0.19.2's robust engine as the boolean kernel, behind the `mf-boolean::Kernel` trait, if it passed the Task 7 qualification. D4 says every result is validated independently, and an invalid result is refused by name. D5 says booleans cost what the overlap costs: the kernel only ever sees a local window.

Task 7 rejected `manifold-rust` (history below). The controller found that two of its gates didn't match the plan's own contracts:
- Gate 1 counted the input's *own* self-intersections against the kernel. D4 asks for no *new* ones.
- Gates 4 and 5 timed whole 1M × 1M operands, a case D5 never runs.

Task 7b fixed the gates and qualified a second kernel through the same harness.

## What Task 7b changed

- **Validator** (`validate_boolean`, `crates/mf-boolean/src/validate.rs`). A self-intersection in a result counts only if it is *new*. Two output faces that meet are excused only when both lie on faces of the same operand that meet each other in that operand. Origins are mapped back through the operands' `face_ids`. Faces of the two operands meeting is always new, and so are two pieces of one input face meeting. Everything else `validate_solid` checks is unchanged. `self_intersecting_faces` flags an input's own crossing faces, and the provenance completeness check skips them.
- **Degenerate post-pass** (`remove_created_degenerates`, `crates/mf-boolean/src/cleanup.rs`). It removes zero-area triangles that the kernel created on input faces with area. Each decision uses exact `orient2d`, and each edit leaves the surface's point set unchanged:
  - **flip:** a collinear cap is flipped against its neighbour, and both new triangles take the neighbour's provenance;
  - **collapse:** coincident vertices are collapsed, under the link condition.

  The pass doesn't touch what it can't remove; D4 refuses it by name. Nothing is decided with a tolerance.
- **Windows** (gate 4 and 5 cases). A window operand is cut from the real model (untimed) and validated before use:
  - **Closed targets:** `target ∩ (tool box + margin)`. This is the D5 window geometry: the target's faces clipped at the box, plus caps from the box faces.
  - **The open Archaeopteryx scan:** the connected patch inside the box, closed into a prism (the patch, a copy translated along its mean normal, and side walls). This stands in for D15's local closure. The first seed whose prism validates is used, and the rejected ones are recorded.

  "End to end" counts the linear scan of the whole target for faces in the window, the kernel, the post-pass and `validate_boolean`. D5's clip, cap triangulation and weld are Task 8 and aren't measured.
- **The C++ kernel** (`ManifoldCpp`, `crates/mf-boolean/src/manifold_cpp.rs`, feature `manifold-cpp`). It runs through our own minimal binding, `crates/mf-manifold-sys`: CMake builds C++ Manifold v3.5.3 and oneTBB, and a 160-line C++ shim sits on top. The `manifold3d` / `manifold-csg` crates were rejected because upstream's C API can't set `MeshGL64::faceID`. Without it, Manifold merges coplanar input triangles, so per-triangle provenance is lost. The C API also can't bound the thread count.
  - **Provenance:** every input triangle is its own `faceID`.
  - **Cancel:** import and boolean both observe Manifold's `ExecutionContext` (added in v3.5). A watcher forwards our token every 2 ms. The C++ call returns, so nothing is detached.
  - **Threads:** the boolean runs in a oneTBB arena as wide as the current `rayon` pool.
  - **Two variants were measured.** `cpp` checks each operand exactly first and refuses one that intersects itself (`KernelError::SelfIntersecting`, code `self_intersecting_operand`); an open operand is still `not_closed`. `cpp-unchecked` has no check (`ManifoldCpp::unchecked()`), and measures what the check prevents.

## Gate results (Task 7b)

Measured on an Apple M2 Max (12 cores, 32 GB) in the test harness:
- `manifold-rust` and `dashu` at opt-level 3, with no debug assertions;
- C++ Manifold and oneTBB as Release (-O3);
- `mf-boolean` at opt-level 2.

The machine was shared and heavily loaded: the load average was **9–41** during the runs, and swap was ~97% used. Every timing in `artifacts/phase1/p1-m2/qualify.json` (under `kernels.<rust|cpp|cpp-unchecked>.<gate>`, git-ignored) has its load average next to it. Times are pessimistic for all three kernels alike. Logs: `/tmp/mf7b-{windows,windows-cpp,corpus,corpus-cpp,1m}.log`.

### `manifold-rust` 0.19.2 (robust engine)

| # | Gate | Result | Verdict |
|---|---|---|---|
| 1 | Corpus (15 messy models × 3 tools) | 33 refused `not_closed` (11 open models). Of the 12 operations on the 4 closed models: **3 valid** (all of puritan; the post-pass flipped 2, 57 and 0 slivers). **8 invalid with new self-intersections:** bunny-peel 97/453/192 pairs, sullivan 6,346/88,463/45,871, winged victory 2 and 3. Winged victory ∪ itself shifted (5.7M + 5.7M) **passed the 12 GB heap limit** at 295 s. The post-pass left no degenerate face anywhere. 0 panics. | **FAIL** |
| 2 | Determinism | Same hash at 1 and 11 threads: 20k spheres (3 ops), every corpus result under 500k triangles, every window case (3 runs plus 1 thread). | PASS |
| 3 | Provenance | Synthetic cases ~1e-17. Excluding the scans' own crossing faces removes Task 7's "untouched" errors (0 everywhere but sullivan − box, 0.32). Two failures remain: output vertices off their face's plane (bunny-peel 6e-7, sullivan 7e-6 of the diagonal), and over-coverage on sullivan (up to 3.0×). | **FAIL** (real) |
| 4 | Cancel ≤ 250 ms (largest gated window, winged victory, 123k + 50k) | worst **65 ms** over 10 cancels (load 26) | PASS |
| 5 | Time through D5 windows | Below. | **FAIL** |

### C++ Manifold 3.5.3, with the operand check (`cpp`)

| # | Gate | Result | Verdict |
|---|---|---|---|
| 1 | Corpus | Every operation is refused by name: 24 `not_closed`, 21 `self_intersecting_operand`. Every closed model in the corpus intersects itself somewhere; even winged victory has 1,006 crossing faces, and its whole-model check takes 11.8 s. No invalid output and 0 panics, but **nothing is computed**. | PASS (by refusal) |
| 2 | Determinism | Same hash at 1 and 11 threads (oneTBB arena): 20k spheres (3 ops), every window case. | PASS |
| 3 | Provenance | Every case it computes: ≤ 3e-15 (synthetic and windows). | PASS |
| 4 | Cancel ≤ 250 ms (winged victory window) | worst **25 ms** over 7 cancels (load 14). It includes the exact operand check, which is cancellable. | PASS |
| 5 | Time through D5 windows | Below. **Archaeopteryx is refused**: the window holds 50 faces of the scan's own crossings (under the part, and their copies in the prism). Every other case passes, by 2–6×. | **FAIL** |

### C++ Manifold 3.5.3, unchecked (`cpp-unchecked`, measured only to show what the check prevents)

| # | Gate | Result | Verdict |
|---|---|---|---|
| 1 | Corpus | 24 `not_closed` and 9 `kernel_internal` refusals. The 9 come from Manifold's `NotManifold`: closed by index but wound inconsistently (cube-achim, walking-man, cleopatras-needle). **9 invalid** results (puritan, bunny-peel, sullivan): up to 384,163 new intersecting pairs, overlapping sheets (coverage up to 5,131×), vertices up to 0.6% of the diagonal off their faces, and an edge on 4 faces. Winged victory: 3 results valid by D4, but they miss gate 3's 1e-9 plane check at 1.8e-9, as `manifold-rust` does on the same model; that is probably the float plane test on near-degenerate source faces (see concerns in the Task 7b report). Every result ran in 0.02–12.6 s. | **FAIL** |
| 2–4 | | As `cpp`. Cancel worst 5 ms. | PASS |
| 5 | Windows | All five pass, Archaeopteryx included (0.16 s, valid, provenance exact). | PASS |

### Gate 5 through D5 windows (median of 3 runs; seconds; load is the 1-min average at the first run)

| Case (limit) | Window + tool (tris) | `manifold-rust` | `cpp` | `cpp-unchecked` |
|---|---|---|---|---|
| 50k sphere ∪ winged victory, 5.7M (3 s) | 123,404 + 50,176 | **4.18** (kernel 3.80), load 9 | 0.68 (check 0.38, kernel 0.04), load 18 | 0.31, load 14 |
| 50k sphere ∪ Archaeopteryx, 1M, open (3 s) | 29,268 + 50,176 | 1.80, load 19 | **refused: self-intersecting window** | 0.16, load 13 |
| Slab cut through a 1M model (10 s) (1M UV sphere; no closed real model of that size) | 23,996 + 12 | 2.24, load 19 | 1.58, load 16 | 1.13, load 13 |
| 50k × 50k overlapping spheres (0.5 s, kernel time) | 50,176 + 50,176 | **1.70**, load 22 | 0.24, load 14 | 0.03, load 12 |
| Slab cut through winged victory (informational) | 163,606 + 12 | 6.30 | 1.07 | 0.56 |
| Whole 1M × 1M union (informational since 7b) | 1M + 1M | 30.6, 1.94 GB, load 33 | 4.85 (check ~4.4), 0.14 GB, load 40 | 0.44, 0.14 GB, load 39 |

On every window, every kernel's result was valid with exact provenance and identical across runs and at 1 thread. One exception: the `cpp` Archaeopteryx case, which it refused. The validation share is large for the C++ kernel. `validate_boolean` on the slab window takes 1.1 s, because its big cap triangles leave the grid; that is the validator's cost, and Task 8 bounds it to created faces (D4).

## Decision

**Neither kernel passes every gate, so neither is accepted.** ADR-0002b gives the outline plan for our own exact local-window kernel. The ruling is D1's own rule, applied to corrected gates:
- **`manifold-rust`** is the only kernel that handles self-intersecting scans, but it still emits new self-intersections on three of the four closed scans. It is also 1.4–3.4× too slow on gated windows, even at D5 sizes.
- **C++ Manifold** is fast (unchecked, 10–300× inside the limits; with its exact operand check, 2–6×), cancellable, deterministic and exact in its provenance, but only on operands that don't intersect themselves. Unchecked, it produces garbage on scans that do. Checked, it is safe, but it refuses booleans wherever a scan crosses itself near the tool, and the one open scan in the window gates does.

What a kernel must do is exact handling of self-intersecting operands, at window sizes, in well under a second. That is what ADR-0002b specifies.

ADR-0002b also gives the owner two cheaper options:
- **A.** Profile and speed up `manifold-rust` upstream; its corpus defects would need fixing too.
- **B.** Ship C++ Manifold with the exact operand check as an interim. Booleans would be valid or refused, with "repair the self-intersections near the tool first" as the next action. That changes what MF-MODEL-001 promises on real scans, so it is the owner's call, not this task's.

## Cost of the C++ option (if B is chosen)

- **Licences:** Manifold, Apache-2.0; oneTBB, Apache-2.0; `cmake` and `cc` crates, MIT/Apache. No `NOTICE` files. Recorded in `THIRD_PARTY_NOTICES.md`. `cargo deny` doesn't see the C++ sources.
- **Build.**
  - A clean build of `mf-manifold-sys` takes **1 min 08 s at `-j 2`** on the M2 Max (load ~5): git clone, CMake configure, the oneTBB fetch, and C++ compile. `OUT_DIR` grows by 180 MB.
  - Needs CMake ≥ 3.18, a C++17 compiler, git, and network access at build time.
  - Pinning: the Manifold tag is checked against commit `0edd9d5`, and Manifold's CMake pins oneTBB to v2022.3.0.
  - Offline builds: set `MF_MANIFOLD_SRC` to point at a cached checkout. oneTBB's FetchContent still needs network access, unless it is pre-seeded via `FETCHCONTENT_SOURCE_DIR_TBB`.
- **CI.**
  - **macOS:** measured locally.
  - **Linux:** expected to work with the runner's GCC and CMake.
  - **Windows:** not tried. The `cmake` crate with MSVC multi-config needs the Release runtime to match Rust's, and static oneTBB on MSVC is "not recommended" by oneTBB.
  - Expect ~3–6 min per OS per cold cache, cached by `rust-cache` afterwards.
  - Today the crate sits outside the workspace (`exclude`), and the feature is off by default, so `--workspace` builds and every existing CI job compile no C++. Only the on-demand `kernel-qualification` job builds it (macOS).
- **Cancel:** acceptable without a detached thread. Manifold 3.5's `ExecutionContext` is checked between phases, and stop-to-return was ≤ 25 ms on the largest gated window. The call returns, so nothing keeps running in the background.

## History: Task 7 (superseded gates)

Task 7, on the original gates (whole-model 1M × 1M; all self-intersections counted; machine at load 15–35):
- gate 1: 1 valid result, 8 `degenerate_face`, 2 `self_intersecting`, 1 timeout;
- gate 2: pass;
- gate 3: fail on real models;
- gate 4: fail, 695 ms during the uncancellable import;
- gate 5: fail, 1M × 1M in 31.6 s and 50k × 50k in 1.83 s.

The adapter's API deviations from the plan still stand:
- **Import:** `from_mesh_gl64_robust`, then `boolean_with_engine_rule_and_progress(Robust, Positive, …)`.
- **Provenance:** from the `TriRef` runs.
- **Cancel:** a 2 ms watcher. The import takes no token; on a window it costs tens of milliseconds, not seconds.
- **Progress:** mapped onto a cloned `Progress`.

Task 7's raw JSON was overwritten by the Task 7b layout. Its logs are kept in `artifacts/phase1/p1-m2/task7-logs/` (git-ignored).

## Consequences

- MF-MODEL-001 and P1-M2 Tasks 8–10 stay blocked, pending the owner's choice between ADR-0002b, A and B.
- The `Kernel` trait, both adapters, `validate_boolean`, the degenerate post-pass and the qualification suite stay. Any kernel, including our own, is qualified with the same `qualify.rs`.
- The qualification no longer runs in the milestone heavy workflow's required jobs. It has its own on-demand job (`kernel-qualification` in heavy.yml), started by the `run-qualify` label or a dispatch with `qualify` ticked.
