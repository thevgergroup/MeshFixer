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
| Licence | CC0 1.0 (Smithsonian Open Access): public domain. Copying, modification, distribution and commercial reuse are allowed without contacting the Smithsonian or attributing. |
| Licence evidence | The official 3D record (https://3d.si.edu/object/3d/archaeopteryx-siemensii-dames:391660da-7c49-499c-91f5-88a298686c09) identifies the 3D media as public domain, with metadata marked CC0. The project owner checked it on 2026-11-08. The record's package ID is the one the download URL above is served from. Policy: https://www.si.edu/openaccess |
| Archive SHA-256 | `87ff968b527bf093c69a739ab712f8adaba7743a5b9bcf211853f89461bafcb8` (36.1 MB) |
| File | `USNM_PAL_509743_archaeopteryx_999k.obj`, SHA-256 `8f484483d2a4fe1c7863f7aa19f2f8381b59d9ae333df7a3c62764126031b85d` (111.8 MB) |
| Accessed | 2026-11-08 |

Why this scan: 997,141 triangles, 101 real boundary loops (481 boundary
edges, the largest 12 edges), 4 components (3 islands) and 2 non-manifold
vertices, straight from the published full-resolution mesh. Most Smithsonian
"master" meshes are watertight after processing; this one isn't. Units are
metres (bounding box about 0.50 × 0.41 × 0.03).

Courtesy attribution (not required by CC0): 3D model of *Archaeopteryx
siemensii* Dames (USNM PAL 509743), Smithsonian Institution, National Museum
of Natural History, Department of Paleobiology, via the Smithsonian 3D
Digitization Program.

## Messy-model collection (M4 Task 11a)

Fifteen real files with the damage people bring to a repair tool, used by
`crates/mf-operations/tests/messy_corpus.rs` (results in
`artifacts/m4/messy/results.json`). All come from
[Thingi10K](https://ten-thousand-models.appspot.com/) (Zhou & Jacobson,
arXiv:1605.04797), whose raw meshes are served as plain files from the
dataset's Hugging Face repository
(https://huggingface.co/datasets/Thingi10K/Thingi10K, pinned to commit
`2d5d3b2f3cd3711028ad75b12788c13b25559ec6`); the manifest's `-` member means
the download is the file, and its SHA-256 is the repository's LFS hash.

Licence policy: only CC BY, CC0 or public domain; no non-commercial,
share-alike or no-derivatives licences. Each model's licence is taken from
the dataset's own per-model record (`metadata/contextual_data.csv` and
`metadata/input_summary.csv` at the commit above) and cross-checked against
the model's Thingiverse page as archived by the Wayback Machine (the live
pages answer only a Cloudflare bot challenge). Defect counts quoted below
are MeshFixer's `inspect`, except self-intersections, which MeshFixer
doesn't detect yet (MF-ISSUE-049): those are the dataset's
`num_self_intersections` (`metadata/geometry_data.csv`).

Coverage: open sculpts with many holes (verlan-dress, site3-scan,
moon-shot, inopos), non-manifold edges and vertices (aztec-chief,
moon-shot, inopos-131443, site3-scan, duplo-crossing, cleopatras-needle,
puritan), flipped orientation (walking-man, cube-achim), many loose
pieces (verlan-dress, duplo-crossing, openrc-differential, bunny-peel,
site3-scan), duplicate and degenerate faces (cleopatras-needle, puritan,
duplo-crossing), self-intersecting exports (sullivan, cube-achim,
bunny-peel, verlan-dress, cleopatras-needle, winged-victory), unwelded
triangle soup (openrc-differential) and a model of at least 5M triangles
(winged-victory, 5,672,212). No coverage gaps; the one caveat is that
self-intersection is evidenced by the dataset, not by MeshFixer.

### `messy-inopos-131487.stl`

| | |
|---|---|
| Object | The Inopos (Thingiverse thing 49168, Thingi10K file 131487) |
| Author | CosmoWenman (https://www.thingiverse.com/CosmoWenman) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 49168: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170720023806/http://www.thingiverse.com/thing:49168) carries the RDFa licence block "CosmoWenman is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/131487.stl |
| SHA-256 | `d70bd99627700f6860fd6bf641c5d344c5423fa84b809d2fddf31fa70f15943d` (41.8 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Open sculpt: 4 holes, one larger than max_loop_edges (500), so a whole-object fill refuses it by name (too_large).

Attribution (required by CC BY): "The Inopos" by CosmoWenman (https://www.thingiverse.com/thing:49168), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-inopos-131443.obj`

| | |
|---|---|
| Object | The Inopos (Thingiverse thing 49168, Thingi10K file 131443) |
| Author | CosmoWenman (https://www.thingiverse.com/CosmoWenman) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 49168: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170720023806/http://www.thingiverse.com/thing:49168) carries the RDFa licence block "CosmoWenman is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/131443.obj |
| SHA-256 | `363c02000c68e684a458c10fb957fd69b265c23669049a9b26cc44267b12edc1` (43.8 MB, plain file) |
| Accessed | 2026-11-09 |

Why: The same scan exported as OBJ: 18 holes, a non-manifold edge, 7 non-manifold vertices; exercises the OBJ reader on a real export.

Attribution (required by CC BY): "The Inopos" by CosmoWenman (https://www.thingiverse.com/thing:49168), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-verlan-dress-285440.obj`

| | |
|---|---|
| Object | Verlan Dress from New Skins with Francis Bitonti Studio (Thingiverse thing 146293, Thingi10K file 285440) |
| Author | MakerBot (https://www.thingiverse.com/MakerBot) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 146293: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170716101335/http://www.thingiverse.com/thing:146293) carries the RDFa licence block "MakerBot is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/285440.obj |
| SHA-256 | `65fd2fa3d0764db068fdb76f7651099bfceb89dcc7108608a7d601e52124db13` (45.9 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Open garment of 1,100 separate pieces with 1,479 holes and 27,829 near-duplicate vertices (dataset: 175,314 self-intersections): the many-holes and many-pieces stress case.

Attribution (required by CC BY): "Verlan Dress from New Skins with Francis Bitonti Studio" by MakerBot (https://www.thingiverse.com/thing:146293), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-aztec-chief-384098.stl`

| | |
|---|---|
| Object | Aztec Chief (Thingiverse thing 205869, Thingi10K file 384098) |
| Author | MakerBot (https://www.thingiverse.com/MakerBot) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 205869: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170211162534/http://www.thingiverse.com/thing:205869) carries the RDFa licence block "MakerBot is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/384098.stl |
| SHA-256 | `208e659d723505f8ee372355c6dc17b72357b597b59f070f6e06893e0c1898fe` (16.4 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Non-manifold edges (9) and vertices (19) plus 3 duplicate faces on an otherwise closed sculpt.

Attribution (required by CC BY): "Aztec Chief" by MakerBot (https://www.thingiverse.com/thing:205869), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-moon-shot-45809.stl`

| | |
|---|---|
| Object | Moon Shot (Thingiverse thing 14011, Thingi10K file 45809) |
| Author | WilliamAAdams (https://www.thingiverse.com/WilliamAAdams) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 14011: "Public Domain"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170719010132/http://www.thingiverse.com/thing:14011) carries the RDFa licence block "WilliamAAdams is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. The dataset says public domain, the archived page CC BY 3.0: treated as CC BY 3.0 (the stricter of the two) and attributed. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/45809.stl |
| SHA-256 | `c1cd31ae30f6292ce884b720ae2bdc4e26a722dba31890f025fe3288372e5bbf` (25.9 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Non-manifold edges (15) and vertices (56) with 65 holes, 6 boundary edges untraced at pinched vertices.

Attribution (required by CC BY): "Moon Shot" by WilliamAAdams (https://www.thingiverse.com/thing:14011), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-walking-man-75275.stl`

| | |
|---|---|
| Object | The Walking Man; by Auguste Rodin (Thingiverse thing 24997, Thingi10K file 75275) |
| Author | CosmoWenman (https://www.thingiverse.com/CosmoWenman) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 24997: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20161123182423/http://www.thingiverse.com/thing:24997) carries the RDFa licence block "CosmoWenman is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/75275.stl |
| SHA-256 | `838d1c6d18b61b56b6b5251968f0da0c45e91e6d23d09adb8e688b85c9d78d44` (18.1 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Closed sculpt whose faces are inconsistently oriented (846 edges traversed the same way by both faces): the flipped-orientation case.

Attribution (required by CC BY): "The Walking Man; by Auguste Rodin" by CosmoWenman (https://www.thingiverse.com/thing:24997), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-cube-achim-252785.stl`

| | |
|---|---|
| Object | Cube (Thingiverse thing 123366, Thingi10K file 252785) |
| Author | Achim (https://www.thingiverse.com/Achim) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 123366: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170716074208/http://www.thingiverse.com/thing:123366) carries the RDFa licence block "Achim is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/252785.stl |
| SHA-256 | `b6fac5b7e3c72236ee58e99d6d76b7b80967df2efb7b55518df6c26300574f25` (8.0 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Flipped orientation (1,302 edges) and heavy self-intersection (dataset: 322,904 intersecting pairs), 1,919 non-manifold vertices.

Attribution (required by CC BY): "Cube" by Achim (https://www.thingiverse.com/thing:123366), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-site3-scan-85538.stl`

| | |
|---|---|
| Object | Site 3 open house 3D scans from 2012-08-16 (Thingiverse thing 28601, Thingi10K file 85538) |
| Author | techknight (https://www.thingiverse.com/techknight) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 28601: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20140511144532/http://www.thingiverse.com/thing:28601) carries the RDFa licence block "techknight is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/85538.stl |
| SHA-256 | `19f3eb83330d64ffb364af7b49958572df9661b4e9c524fce9022c62c9b06e4b` (14.3 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Raw 3D scan from an open house: 354 pieces, 332 holes (35 pass through a pinched vertex), non-manifold edges and vertices.

Attribution (required by CC BY): "Site 3 open house 3D scans from 2012-08-16" by techknight (https://www.thingiverse.com/thing:28601), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-duplo-crossing-111959.stl`

| | |
|---|---|
| Object | Duplo hexagon railroad crossing (Thingiverse thing 39466, Thingi10K file 111959) |
| Author | satoer (https://www.thingiverse.com/satoer) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 39466: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170720084333/http://www.thingiverse.com/thing:39466) carries the RDFa licence block "satoer is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/111959.stl |
| SHA-256 | `d33f4af93a69dca89d9ac7ce7be344825d73a91a6f8e22548e0ad8ba221befd1` (1.3 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Small CAD export with everything wrong: 932 pieces, 85 holes, non-manifold edges, 15 duplicate and 306 degenerate faces.

Attribution (required by CC BY): "Duplo hexagon railroad crossing" by satoer (https://www.thingiverse.com/thing:39466), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-bunny-peel-298323.obj`

| | |
|---|---|
| Object | Bunny Peel with meshmixer Support (Thingiverse thing 131054, Thingi10K file 298323) |
| Author | meshmixer (https://www.thingiverse.com/meshmixer) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 131054: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20161101061645/http://www.thingiverse.com/thing:131054) carries the RDFa licence block "meshmixer is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/298323.obj |
| SHA-256 | `f928738a2e5e74656991aadbc14bb6a46b89a0d2fdec63251709143b4ad3e7de` (7.7 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Meshmixer export with tree supports: 418 pieces, 208 degenerate faces, dataset self-intersections 209,830.

Attribution (required by CC BY): "Bunny Peel with meshmixer Support" by meshmixer (https://www.thingiverse.com/thing:131054), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-cleopatras-needle-63942.stl`

| | |
|---|---|
| Object | Cleopatra's Needle (Thingiverse thing 20271, Thingi10K file 63942) |
| Author | MakerBot (https://www.thingiverse.com/MakerBot) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 20271: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20160711135934/http://www.thingiverse.com/thing:20271) carries the RDFa licence block "MakerBot is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/63942.stl |
| SHA-256 | `19b479698a61ea0c17394c57e5a5d0e67a3a45544f6e9dfc166b98aad6e7f61b` (52.9 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Duplicate-face export: 139,905 duplicate faces (every face stored about three times), 702 degenerate, 104,913 non-manifold edges.

Attribution (required by CC BY): "Cleopatra's Needle" by MakerBot (https://www.thingiverse.com/thing:20271), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-puritan-91945.stl`

| | |
|---|---|
| Object | The Puritan (Thingiverse thing 31644, Thingi10K file 91945) |
| Author | MakerBot (https://www.thingiverse.com/MakerBot) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 31644: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20170719222842/http://www.thingiverse.com/thing:31644) carries the RDFa licence block "MakerBot is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/91945.stl |
| SHA-256 | `ff2395999055153f302c6b90964837c0b930b1f2666e471d7257561385341e27` (3.2 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Degenerate-face export: 1,608 degenerate and 8,512 duplicate faces in 12,768 triangles.

Attribution (required by CC BY): "The Puritan" by MakerBot (https://www.thingiverse.com/thing:31644), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-sullivan-1716279.stl`

| | |
|---|---|
| Object | TWENTY SOMETHING SULLIVAN (Thingiverse thing 1092050, Thingi10K file 1716279) |
| Author | tomburtonwood (https://www.thingiverse.com/tomburtonwood) |
| Licence | CC0 1.0 (http://creativecommons.org/publicdomain/zero/1.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 1092050: "Creative Commons - Public Domain Dedication"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20160914013032/http://www.thingiverse.com/thing:1092050) carries the RDFa licence block "tomburtonwood is licensed under the Creative Commons - Public Domain Dedication licence" linking http://creativecommons.org/publicdomain/zero/1.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/1716279.stl |
| SHA-256 | `3b52bf71521e5d139499de33deba916ed05967307caf014fb67258963b488595` (19.5 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Self-intersecting closed export (dataset: 128,435 intersecting pairs) under CC0; 849 non-manifold vertices.

### `messy-openrc-differential-100727.stl`

| | |
|---|---|
| Object | OpenRC 1:10 RC Differential (Thingiverse thing 35104, Thingi10K file 100727) |
| Author | barspin (https://www.thingiverse.com/barspin) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 35104: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20161122003952/http://www.thingiverse.com/thing:35104) carries the RDFa licence block "barspin is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/100727.stl |
| SHA-256 | `4f1451e20de0ee9f0a4d86b81d080afd7f0cd36e46ba07d308a4110d4d2955ee` (0.9 MB, plain file) |
| Accessed | 2026-11-09 |

Why: Unwelded triangle soup STL: 3,244 triangles in 2,208 pieces with 7,640 open edges; 4,315 vertices lie within tolerance of another but are not exactly equal, so the exact weld on import leaves them apart.

Attribution (required by CC BY): "OpenRC 1:10 RC Differential" by barspin (https://www.thingiverse.com/thing:35104), licensed under CC BY 3.0; used unmodified as a test input.

### `messy-winged-victory-376253.obj`

| | |
|---|---|
| Object | Winged Victory of Samothrace (Thingiverse thing 196038, Thingi10K file 376253) |
| Author | CosmoWenman (https://www.thingiverse.com/CosmoWenman) |
| Licence | CC BY 3.0 (http://creativecommons.org/licenses/by/3.0/) |
| Licence evidence | Thingi10K `metadata/contextual_data.csv` row for thing 196038: "Creative Commons - Attribution"; the thing page as archived by the Wayback Machine (http://web.archive.org/web/20180121124333/http://www.thingiverse.com/thing:196038) carries the RDFa licence block "CosmoWenman is licensed under the Creative Commons - Attribution licence" linking http://creativecommons.org/licenses/by/3.0/. Live page check: thingiverse.com unreachable (Cloudflare challenge) on 2026-11-09. |
| Download | https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/2d5d3b2f3cd3711028ad75b12788c13b25559ec6/raw_meshes/376253.obj |
| SHA-256 | `f831cf74044a956a844714b671bd5f81832bb2a45cdd7dd653278c3e5fc34ae1` (244.7 MB, plain file) |
| Accessed | 2026-11-09 |

Why: The large model: 5,672,212 triangles (dataset: 707 self-intersections, 50 degenerate faces); a 244.7 MB OBJ.

Attribution (required by CC BY): "Winged Victory of Samothrace" by CosmoWenman (https://www.thingiverse.com/thing:196038), licensed under CC BY 3.0; used unmodified as a test input.

## `stanford-bunny.obj`

| | |
|---|---|
| Object | The Stanford Bunny (Turk and Levoy, SIGGRAPH 1994), the `bun_zipper` reconstruction: 35,947 vertices, 69,451 triangles, with the holes in its base |
| Publisher | Stanford University Computer Graphics Laboratory, The Stanford 3D Scanning Repository (http://graphics.stanford.edu/data/3Dscanrep/) |
| Mirror | Alec Jacobson's common-3d-test-models (https://github.com/alecjacobson/common-3d-test-models), commit `8a4f8642acaf43f9cd7b67858a1502e1055ef202`, `data/stanford-bunny.obj` (the original converted to OBJ by MeshLab, unmodified otherwise); the repository's own table credits Stanford University as its source |
| Download | https://raw.githubusercontent.com/alecjacobson/common-3d-test-models/8a4f8642acaf43f9cd7b67858a1502e1055ef202/data/stanford-bunny.obj |
| Licence | The Stanford 3D Scanning Repository's terms: the models may be used freely for research (and other non-commercial) purposes, with credit to the Stanford Computer Graphics Laboratory; not redistributed here (fetched for tests only, never committed) |
| SHA-256 | `1eb35d1e21ce99e5ce911353b6be278990713448dd9e8f5c9387f9de39b32205` (2.4 MB, plain file) |
| Accessed | 2026-11-14 |

Why: Meshmixer's own classic Hollow test, the canonical organic model for
`mesh.hollow`'s acceptance tests (`crates/mf-command/tests/hollow_acceptance.rs`
and `core_workflows.rs`): Repair (fill its base holes), then Hollow. Units
are metres (about 0.16 × 0.15 × 0.12); the tests scale it to 100 mm.

Credit: The Stanford Bunny, Stanford University Computer Graphics
Laboratory, The Stanford 3D Scanning Repository.
