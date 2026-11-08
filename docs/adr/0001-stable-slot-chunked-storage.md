# ADR-0001: Stable-slot chunked mesh storage

Status: Proposed (M2 Task 3) · 2026-11-08

## Measurements (M2 Task 1, before any redesign)

Release build, Apple M2 Max, `meshfixer … --metrics`. Heap bytes are from the counting allocator. "Peak" is the peak above the level at the start of the phase.

| Mesh | Phase | Wall | Peak | Retained |
|---|---|---|---|---|
| 0.96M tri (46 MB STL) | import | 0.36 s | 202 MB | 22 MB |
| | topology | 0.18 s | 91 MB | 4 MB |
| | remove_small (operation) | 0.20 s | 91 MB | 28 MB |
| | export (verified) | 0.35 s | 202 MB | 0 |
| 4.8M tri (230 MB STL) | import | 1.93 s | **1017 MB** | 110 MB |
| | topology | 0.98 s | 468 MB | 32 MB |
| | remove_small (operation) | 1.21 s | 468 MB | 224 MB |
| | export (verified) | 2.23 s | **1017 MB** | 0 |

The process-wide heap peak for the 4.8M `remove-small-components` run was 1383 MB.

**Retained working set at 4.8M: mesh 110 MB + topology 32 MB = 142 MB.** That's within the 400 MB budget (MF-GEO-002), so the gate passes.

The 1.1 GB figure from M1 came almost entirely from transient scratch:
1. **Import:** the 230 MB file buffer, plus f32 corners, f64 corners and three index arrays for welding.
2. **Export verification:** re-parsing and re-welding the encoded file, which costs the same as an import.
3. **Topology construction:** a 16-byte half-edge per corner, plus a fan array and corner union-find (468 MB at 4.8M).
4. **remove_small output:** `Vec` growth overshoot (192 MB instead of ~110 MB).

Items 1 and 2 are addressed in Task 2, and item 4 by `MeshEdit` in Task 3. Item 3 is tracked: topology scratch can drop to roughly a third by keying half-edges as `u64` and using in-place union-find (MF-ISSUE-019).
