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
