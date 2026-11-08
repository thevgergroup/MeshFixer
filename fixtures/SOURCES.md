# Fixture sources

Generated fixtures (`fixtures/generated/`, `mf-fixtures`) are made by this
project. Real scans are not committed: `scripts/fetch-fixtures.sh` downloads
them into `fixtures/real/` (git-ignored) and verifies every SHA-256 listed in
`fixtures/manifest.txt`. Tests that need one use `mf_fixtures::real::path`,
which skips by policy when it's missing; `MF_REQUIRE_FIXTURES=1` (set in CI's
Linux and evidence jobs) turns that skip into a failure.

## `archaeopteryx.obj`

| | |
|---|---|
| Object | *Archaeopteryx siemensii* Dames, USNM PAL 509743 (NMNH Paleobiology) |
| Publisher | Smithsonian Institution, 3D Digitization Program |
| 3D package | `3d_package:391660da-7c49-499c-91f5-88a298686c09` (https://3d.si.edu/object/3d/archaeopteryx-siemensii-dames:391660da-7c49-499c-91f5-88a298686c09) |
| Record | `edanmdm:nmnhpaleobiology_3370783` |
| Download | https://3d-api.si.edu/content/document/3d_package:391660da-7c49-499c-91f5-88a298686c09/resources/USNM_PAL_509743-full_resolution-obj_std.zip ("Full resolution 3D mesh, obj") |
| Licence | CC0 1.0 (Smithsonian Open Access). Record-level confirmation pending, MF-ISSUE-030 |
| Archive SHA-256 | `87ff968b527bf093c69a739ab712f8adaba7743a5b9bcf211853f89461bafcb8` (36.1 MB) |
| File | `USNM_PAL_509743_archaeopteryx_999k.obj`, SHA-256 `8f484483d2a4fe1c7863f7aa19f2f8381b59d9ae333df7a3c62764126031b85d` (111.8 MB) |
| Accessed | 2026-11-08 |

Why this scan: 997,141 triangles, 101 real boundary loops (481 boundary
edges, the largest 12 edges), 4 components (3 islands) and 2 non-manifold
vertices, straight from the published full-resolution mesh. Most Smithsonian
"master" meshes are watertight after processing; this one isn't. Units are
metres (bounding box about 0.50 × 0.41 × 0.03).
