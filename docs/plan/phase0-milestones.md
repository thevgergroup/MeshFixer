# Phase 0 Milestones

Spec: `docs/spec/phase0-spec.md` (v0.3). Each milestone ends with a **demonstration**, **pass/fail gates** and **evidence**: a screen recording, benchmark JSON with device info, and a sample model, stored in `artifacts/<milestone>/` and summarized in `docs/milestones/<milestone>.md`. A milestone isn't closed by passing tests alone.

Detailed task plans are written when each milestone starts, so they build on what the previous milestone taught us. M1: `docs/superpowers/plans/2026-11-08-m1-vertical-slice.md`.

Commits are small and reference requirement IDs (e.g. `feat(io): binary STL reader [MF-IO-001]`).

**Every milestone closes with:**
- An independent whole-branch review. Its job is to challenge shortcuts that pass acceptance tests without advancing the long-term architecture (localized editing, provenance, history).
- Green cross-platform CI. Skipped GPU tests are reported separately and never counted as passes.
- Evidence uploaded by CI (`milestone-evidence` artifact), plus a human-driven demo recording.
- Deferred findings logged in `docs/issues.md`, each with an ID, severity, milestone and resolution criteria.

---

## M1: Thin vertical slice

**Demo:** Open a damaged STL in the desktop app and orbit it. The inspect panel shows diagnostics, and boundary loops are drawn as an overlay. Run `remove_small_components` from a panel generated from its YAML spec. Islands vanish; undo brings them back. Run the same operation from the CLI with the same spec-derived arguments and export STL/OBJ.

**Gates:**
- G1.1 CI is green on Windows, macOS and Linux (build, test, `cargo deny`). [MF-PLAT-001, MF-ARCH-LEGAL-001]
- G1.2 Diagnostics fixture tests pass for every metric implemented so far. [MF-GEO-004]
- G1.3 The GUI panel and CLI args for `remove_small_components` come from one spec. A spec/implementation parity test passes. [MF-SPEC-001/002]
- G1.4 The operation runs off the UI thread, prepared then committed, even before the full runtime exists. [MF-RT-001 groundwork, MF-CMD-006]
- G1.5 Undo/redo works in the GUI and in a headless test. [MF-CMD-003]
- G1.6 OBJ high-precision round-trip passes. [MF-GEO-001]

**Deliberately temporary:** plain `Arc<Vec>` storage (chunks come in M2), linear-stack history (DAG in M2), operations on whole objects only (targets in M3).

## M2: Core contracts

**Scope:**
- **First, memory instrumentation (MF-ISSUE-013):** a counting allocator and per-category breakdown (mesh storage, topology, import scratch, history, GPU), reported by `--metrics` and CI. The chunked storage design must be justified against these numbers before history and selections build on it.
- Tolerance duplicate vertices (MF-ISSUE-012), and pinched-loop splitting (MF-ISSUE-007, which must close before M3).
- Chunked copy-on-write storage, revision DAG with retained branches, full command records, face provenance maps, selection model and `translate`, command-log save/load/replay with `Exact`/`Equivalent`/`Mismatch`, canonical hash vs geometric-equivalence comparator, `Geometry` kind enum, strict-determinism tests at 1 and N threads, mutation-path visibility.

**Demo:** Make several edits in the GUI, save the log, quit, then `meshfixer replay` reproduces the result and prints a per-command `Exact` report. Tamper with the log and it reports `Mismatch`. A debug view shows the DAG with an abandoned branch retained.

**Gates:** memory breakdown reported, with a storage model whose projected 5M-triangle working set is ≤ 400 MB (MF-GEO-002), MF-DOC-001/002/003/005, MF-HIST-001, MF-CMD-001/002/004/005, MF-SEL-001/002/003 (provenance-totality property tests), CI green.

## M3: Real, surgical repair

**Scope:** targeted, localized editing is the point of this milestone, not whole-mesh cleanup. That means `fill_holes` on a chosen loop (minimum-weight DP triangulation plus refinement), targeted selections (component, boundary loop, face set), clickable overlays and loop picking, `untouched_outside_target` checks, the declarative YAML test runner, 3MF read/write, real scan fixture fetched by hash, and the HIST-002 re-execution test.

**Demo (exit criterion 9):** Open a real scan, click one hole, fill only that hole, inspect it, undo, change `max_loop_edges` or the refinement setting, fill again, and export 3MF that opens in a slicer.

**Gates:** MF-OP-001/002/003, MF-SEL-003/004 (selection across `fill_holes` maps fully; a closed-loop selection fails explicitly), MF-SPEC-004, MF-IO-001 (3MF), MF-DOC-004, MF-HIST-002.

## M4: Scale and reliability

**Scope:** `mf-runtime` (job pool with a thread budget, cancellation tokens, progress, telemetry), incremental GPU uploads by chunk, a per-face selection state buffer, picking acceleration, a counting-allocator memory benchmark, damaged 1M/5M/10M orbit benchmarks with device info, and `cargo-fuzz` targets per reader.

**Demo:** Load a 5M-triangle damaged mesh, orbit while `fill_holes` runs with live progress, cancel mid-run (the document is unchanged), run it again to completion, then view the metrics panel.

**Gates:** MF-RT-001…005, MF-RENDER-002 (preliminary, M2 Max), MF-RENDER-003/004, MF-GEO-002/003, MF-DOC-002 memory test, MF-IO-002.

## M5: Architecture validation and Phase 0 exit

**Scope:** `meshfixer schema --mcp` with a golden CLI/MCP consistency test, the spec-propagation test (add a parameter, see it in all three interfaces), a cross-OS equivalence CI job, `simplify` (stretch goal), and the exit report.

**Demo:** Run through all nine exit criteria end to end in one recording.

**Gates:** MF-SPEC-003, MF-CMD-005 (cross-platform level), every exit criterion in spec §12, and OD-2 status reported honestly.
