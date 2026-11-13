# Task 4 report: the transform gizmo and the build plate (MF-OBJ-001)

**Worktree:** `/Users/patrick/Projects/thevgergroup/MeshFixer/.claude/worktrees/agent-a2fd3b86523da1ce5`
**Branch:** `p1-m1-gizmo`

## What was built

- `crates/mf-render/src/overlay.rs` (new): `GizmoPalette { axis_x, axis_y, axis_z, neutral }` with `color_for(Handle)` (D8: axis/plane/ring handles take their axis's colour by index; the centre, view ring and uniform-scale handles, which have no single axis, take `neutral`); `Plate { size_mm: [f64;2], grid_mm: f64 }` with `Default` (256 × 256 mm, 10 mm grid); pure CPU functions `gizmo_vertices` (screen-space segments, tessellating ring handles, baking the highlighted handle's 40%-lighter/2×-wider rule per vertex) and `plate_vertices` (world-space grid lines at z = 0, snapped to multiples of `grid_mm` so a line always crosses the origin, not just the plate's own centre).
- `crates/mf-render/src/renderer.rs`: `Palette` gains a `plate: [f32;4]` field (D9 `PLATE`; the renderer still has no colours of its own — `HEADLESS` and `mf-desktop`'s `theme::palette()` both set it). `MeshRenderer::draw_plate(&mut self, device, queue, pass, plate: &Plate)` — world-space grid, depth-tested and depth-written like opaque geometry but alpha-blended so it fades with distance from the camera; callers draw it before `paint()` so a resting object's opaque surface overdraws the grid beneath it. `MeshRenderer::draw_gizmo(&mut self, device, pass, shapes: &[HandleShape], highlight: Option<Handle>, palette: &GizmoPalette)` — screen-space pixel quads (like the existing 3 pt overlay lines) through a pipeline with depth compare `Always`/no write, so it draws last and is never hidden. Added a `DepthMode` enum (`Opaque`/`XRay`/`Blended`/`NoTest`) generalising the old `xray: bool` pipeline switch to cover the plate's new blended-but-depth-tested case and the gizmo's no-test case.
- `crates/mf-render/src/shader.wgsl`: `Scene` gains `eye_pos` (the camera eye, shifted into the same `world_origin`-relative frame as vertex positions) and `plate` (the grid colour); `vs_plate`/`fs_plate` (world-space line widening like `vs_line`, fading alpha by `distance(world, eye_pos)` against the camera's own framing distance); `vs_gizmo`/`fs_gizmo` (pixel-space quad widening, the inverse of `screen_position`'s NDC mapping, with colour/width already baked per vertex on the CPU).
- `crates/mf-render/src/target.rs`: two small read-only accessors, `RenderTarget::depth_view()` and `RenderTarget::color_attachment()`, so the offscreen test can open its own render pass (needed because `draw_gizmo`/`draw_plate` take the pass directly, unlike `RenderTarget::draw`/`draw_timed`, which own the pass internally).
- `crates/mf-desktop/src/theme.rs`: new tokens `AXIS_X #E5484D` / `AXIS_Y #46A758` / `AXIS_Z #3E8BF0` / `PLATE` grey 70 (D9, exact values from the plan), `theme::gizmo_palette()` building `mf_render::GizmoPalette` from them (neutral = white), and `theme::palette()` now also sets `plate: rgba(PLATE)`.
- `docs/design/visual-language.md`: added `AXIS_X`/`AXIS_Y`/`AXIS_Z` and `PLATE` to the colour-token tables, and extended the "Draw order" list with the plate (bottom, under the model) and the gizmo (top, no depth test, 40% lighter/2× wider when hovered).
- `crates/mf-render/tests/gizmo_render.rs` (new): the brief's three offscreen tests, plus five small CPU-only unit tests for the pure helpers (no GPU needed): `plate_vertices_default_is_256mm_with_a_10mm_grid_through_the_origin`, `plate_vertices_degenerate_input_is_empty_not_a_panic`, `gizmo_palette_color_for_follows_axis_index_d8`, `gizmo_vertices_highlighted_handle_is_lighter_and_wider`, `gizmo_vertices_ring_is_a_closed_polyline`.

## TDD

Wrote the three named tests first; they failed to compile (`GizmoPalette`/`Plate`/`draw_gizmo`/`draw_plate` didn't exist), then against a first implementation with two real bugs surfaced by the RED→GREEN cycle:

1. **Gizmo pipeline/pass depth-format mismatch.** The gizmo pipeline was built with `depth_stencil: None` for "no depth test", but wgpu requires a pipeline's depth-stencil state to match the render pass's attachment format even when the test always passes. Fixed by always declaring the attachment and using `depth_compare: Always`, `depth_write_enabled: false` for the gizmo's `DepthMode::NoTest` instead of omitting it.
2. **Plate grid not centred on multiples of `grid_mm`.** The first `plate_vertices` centred the grid on the plate's own bounds (`-size/2 + i*grid`), which for 256 mm / 10 mm never lands a line exactly on x = 0 or y = 0 (256 isn't a multiple of 10). `plate_is_hidden_behind_objects_resting_on_it` sampled a point on the origin's grid line and got background, not grid colour. Fixed by snapping grid lines to `k * grid_mm` for integer `k`, guaranteeing a line through the origin — where an object placed at the origin (the common case) actually meets the plate.

Both offscreen tests drive `MeshRenderer`/`RenderTarget` directly (as `bench.rs` does) rather than through `offscreen::render_image`, since `draw_gizmo`/`draw_plate` need the render pass itself.

## Gate result

```
export PATH=$HOME/.rustup/toolchains/1.99.1-aarch64-apple-darwin/bin:$PATH CARGO_INCREMENTAL=0
cargo fmt --all && CARGO_BUILD_JOBS=2 scripts/check.sh
```

Clean: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the swallowed/fixtures/licence checks, and the full fast test suite all pass — **581 verified passes, 0 failed, 0 skipped for a missing GPU/fixture, 16 heavy/timing groups skipped by fast mode** (unchanged from before this task; heavy/5M/timing tests run on demand only, per the owner's ruling, not here).

`cargo test -p mf-render --test gizmo_render` on its own: **8 passed, 0 failed** — `gizmo_draws_over_geometry`, `plate_is_hidden_behind_objects_resting_on_it`, `highlighted_handle_is_lighter` (the brief's three), plus the five pure-helper unit tests.

## Test images

Captured from a throwaway instrumented run (not part of the committed test file) and saved under `.superpowers-report/task-4-images/`:

- `gizmo_draws_over_geometry.png` — the gizmo's red/green/blue axes drawn over a grey cube.
- `plate_is_hidden_behind_objects_resting_on_it.png` — the plate's grid visible around a cube, hidden under its footprint.
- `highlighted_handle_is_lighter_plain.png` / `highlighted_handle_is_lighter_highlighted.png` — the X axis handle at its plain width/colour versus highlighted (lighter, twice as wide).

## Rulings and concerns

- **Where the plate's colour lives vs `GizmoPalette`.** The brief gives `draw_plate(&mut self, pass, plate: &Plate)` with no palette parameter, while `draw_gizmo` takes `palette: &GizmoPalette` explicitly, and `Plate` itself (`size_mm`, `grid_mm`) has no colour field. Read this as: the plate's colour lives on the main `Palette` (which the renderer already stores from construction, exactly how `boundary`/`surface`/etc. work), and `GizmoPalette` is a separate, small struct only for the gizmo's own D8 axis colours (plus a `neutral` for the centre/view-ring/uniform-scale handles, which D9's table doesn't name — chosen as plain white, not tied to any existing one-meaning token, since reusing e.g. `HOVER` there would violate "one colour, one meaning" when the handle isn't actually hovered).
- **`RenderTarget::depth_view()`/`color_attachment()` were added even though `target.rs` isn't in the brief's file list**, because `draw_gizmo`/`draw_plate` need the pass directly and `RenderTarget::draw`/`draw_timed` own the pass internally with no way to extend it from outside. Kept to two small read-only accessors mirroring the existing `view()`/`texture()`, rather than duplicating MSAA/depth texture setup in the test file.
- **Plate fade formula.** The brief says "fading with distance" without a formula. Implemented as alpha fading linearly to zero at twice the camera's own distance from its target (a conventional "infinite floor" fade), tested only qualitatively (a bare-plate grid line must still be visible, not literally background) rather than against an exact curve, since none is specified.
- **Ring/plane/view-ring/uniform-scale handles** are implemented and covered by the CPU-only unit tests (tessellated circles, axis-indexed colouring) even though the brief's three named tests only exercise Move-mode axis handles; left in because `Gizmo::handles` (Task 1) already produces all of them and silently mis-rendering Rotate/Scale mode would be a real gap.
- Did not wire `draw_gizmo`/`draw_plate` into `mf-desktop`'s viewport — that's Task 5 ("The Transform tool in the desktop"), out of scope here.

## Fix round 1

Review came back "Needs fixes" on five points. Fixed test-first, in the same worktree/branch.

### 1. `gizmo_draws_over_geometry` tested nothing

The original sampled a background pixel (117, 77); any depth mode, including `DepthMode::Opaque`, still passed. Rewrote it to sample the move gizmo's **centre disc** at the pivot (the cube's own geometric centre — provably inside its silhouette and strictly behind its front face). Verified directly: temporarily forced the gizmo line/fill pipelines to `DepthMode::Opaque` — the test failed, reading the cube's shaded surface colour instead of the disc's — then reverted and confirmed it passes again. A second, related finding surfaced during this verification: the gizmo's vertex shaders hardcoded `clip.z = 0.0` (the near plane), which under `CompareFunction::LessEqual` would have passed almost any real depth test trivially, making "Opaque" not a meaningful check. Changed both `vs_gizmo_line` and `vs_gizmo_fill` to write `clip.z = 1.0` (the far plane) instead — irrelevant to the real `DepthMode::NoTest` pipeline (`CompareFunction::Always`, no depth write), but it is what makes the Opaque-mode verification genuine: at the far plane the gizmo loses a real depth test against anything else in the scene, so drawing over geometry there can only mean `NoTest` is actually in effect.

### 2. Point handles (Centre, ScaleUniform) never drew

Root cause: they were zero-length segments (`a == b`), which the renderer's line-quad shader degenerates to a zero-width, invisible quad. Fixed as part of the `HandleGeometry` contract change below: `Handle::Centre`/`ScaleUniform` now carry `HandleGeometry::Point { center, radius_px: POINT_HANDLE_RADIUS_PX }` (8 pt), drawn as a filled disc (triangle fan) through the new fill pipeline.

### 3. Rings and plane handles had no real geometry

Extended `HandleShape` in `crates/mf-render/src/gizmo.rs`: it now carries a `HandleGeometry` enum (`Arrow`, `Ring`, `Circle`, `Plane`, `Point`) instead of a bare segment/circle pair, so drawing and hit-testing always agree:

- **Axis arrows** (`Axis`/`ScaleAxis`): a shaft from an inset start (see below) to 75% of the arm, then a head polygon to the tip — a triangle for `Axis`, a quad (a cube's screen outline) for `ScaleAxis`.
- **Rings** (`Handle::Ring`): the true projected 3D circle, perpendicular to the ring's own axis, as a 64-point closed polyline. Each point is tagged front/back by `front_facing` (is its direction from the ring's centre pointing towards the camera's own in-plane direction); only the front half draws (as a line) and is hittable. The world radius is solved along the ring's own in-plane basis directions (`plane_basis`'s `u`/`v`, the smaller of the two, capped at 30% of the camera's distance) — not along the ring's normal, which it never extends along at all and which was the root cause of a bug surfaced while building this: using the normal direction for the radius solve could blow the realised ellipse up far past the requested `size_pt`, and even sweep points close enough to the camera to blow up their perspective-divided screen coordinates.
- **Plane handles**: a small filled square between 22% and 38% of each axis's own arm, as four screen corners.
- **ViewRing**: unchanged, a plain screen circle (it has no single world axis to project).

`Gizmo::hit` now measures against this same geometry: the arrow's full centreline (start to tip, not just the shaft — the head extends past where the shaft itself stops), a ring's front-facing polyline segments only, a plane square's interior (point-in-polygon, zero distance when inside) or nearest edge, and a point handle's distance to its centre. Priority order is unchanged (axis > plane > ring > centre).

Added `ARROW_SHAFT_INSET_PX` (`POINT_HANDLE_RADIUS_PX + 4`): every axis shaft now starts a few pixels out from the pivot, not exactly at it. Without this, a shaft beginning exactly at the pivot is always within the centre disc's own hit radius of it, and the axis-beats-centre priority rule would make the centre handle practically unreachable — this surfaced as a real test failure (`centre_disc_is_hittable`) while writing the new tests, not a hypothetical.

New `overlay.rs` vertex kinds: `GizmoLineVertex` (renamed from `GizmoVertex`) for shafts and ring polylines through the existing instanced-quad line pipeline (`gizmo_line_vertices`), and `GizmoFillVertex` for filled triangles (arrowheads, scale cubes, plane squares, point discs) through a new plain-triangle-list pipeline (`gizmo_fill_vertices`, `vs_gizmo_fill`/`fs_gizmo_fill`, `gizmo_fill_pipeline`). `draw_gizmo` draws fill first, then lines over it, so a shaft visibly reaches into its own arrowhead.

Updated Task 1's tests in `crates/mf-render/tests/gizmo.rs` for the changed contract (`end_on_axis_is_not_hittable`, `handles_keep_screen_size_at_any_zoom` now read `HandleGeometry::Arrow`'s fields) and added the three requested: `x_ring_and_y_ring_are_distinct_and_hittable_from_an_oblique_view`, `plane_handle_hit_inside_its_square`, `centre_disc_is_hittable`.

### 4. The plate had no depth bias

`vs_plate` lacked `vs_line`'s depth pull. Added `clip.z = clip.z + 0.0005 * clip.w` — pushed *away* from the camera (the opposite direction from the overlay lines' own camera-ward pull), so an object resting exactly coplanar with the plate at z = 0 always wins the depth comparison rather than racing it. Added the requested test: `plate_is_hidden_behind_objects_resting_on_it` now also samples the cube's own base face at z = 0 (not just its taller top face) and asserts it reads as the object's colour, not the plate's grey. Note: on this worktree's GPU backend the un-biased version still passed this specific test (`CompareFunction::LessEqual` already favours the later-drawn fragment on an exact tie here), so the test could not be made to reproduce a visible failure pre-fix; the bias is kept as standard, directionally-correct defensive practice against z-fighting that could appear differently on other GPU backends/drivers, and the test stands as a regression guard for the end state the review asked for (object colour wins at the coplanar base face).

### 5. GPU coverage for every shape kind, in every mode

Added three new offscreen tests: `move_mode_arrowhead_and_centre_disc_draw` (arrowhead, plane square, centre disc), `rotate_mode_rings_draw_as_distinct_ellipses` (two rings sampled at their own front-facing point nearest the view centre, each resolving to its own colour), `scale_mode_cube_head_and_uniform_disc_draw` (cube head, uniform-scale disc).

Captured 512×512 renders of each mode (from a throwaway instrumented run, removed after capture) into `.superpowers-report/task-4-images-round1/`:

- `gizmo_move_mode_512.png` — arrows with real arrowheads, small filled plane squares, white centre disc.
- `gizmo_rotate_mode_512.png` — three distinct coloured ellipses at different angles (not the same circle) plus the white ViewRing.
- `gizmo_scale_mode_512.png` — arrows with cube heads and a white uniform-scale disc.

### Gate result (fix round 1)

```
export PATH=$HOME/.rustup/toolchains/1.99.1-aarch64-apple-darwin/bin:$PATH CARGO_INCREMENTAL=0
cargo fmt --all && CARGO_BUILD_JOBS=2 scripts/check.sh
```

Clean: **590 verified passes, 0 failed, 0 skipped for a missing GPU/fixture, 16 heavy/timing groups skipped by fast mode** (up from 581 before this round, reflecting the new tests: 3 new tests in `gizmo.rs`, 3 new GPU-coverage tests in `gizmo_render.rs`, plus `gizmo_fill_vertices`/ring-skips-back-half unit tests).

`cargo test -p mf-render --test gizmo --test gizmo_render`: **28 passed, 0 failed** (14 + 14).
