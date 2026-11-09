# MeshFixer layout and visual language

Status: adopted in M4 Task 2 (Checkpoint A). Code: `crates/mf-desktop/src/theme.rs` (colour tokens), `defects.rs` (plain-language copy), `panels.rs` (layout), `viewport.rs` (camera input).

Someone who has used Meshmixer or Fusion should understand the screen without an engineer. Three rules follow from that:

1. The model is the subject. The 3D view gets most of the window.
2. You act on a target. Pick a hole, a crack or a loose piece, and the operations for it appear next to it. You never pick an operation first and then work out what it will touch.
3. A colour means one thing, everywhere: in the 3D view, the tree and Properties.

## Layout

```
┌ File  Edit  View  Help ───────────────────────────────────────────────────┐
│ Objects & Defects │                                   │ Properties & Tools │
│  scan             │                                   │  Hole 2: small     │
│   Model           │            3D view                │  opening           │
│   ▾ Holes (4)     │       (at least 60% of a          │  12 boundary edges │
│      Hole 1 …     │        1440×900 window)           │  · 26.4 mm …       │
│   ▸ Narrow cracks │                                   │  Tool: Fill hole   │
│   ▾ Loose pieces  │                                   │   Refine ☑         │
│   ▸ Non-manifold  │                                   │   [Apply]          │
│  plate            │                                   │  ▸ Details         │
├───────────────────┴───────────────────────────────────┴────────────────────┤
│ [Undo][Redo] │ Import scan › Import plate › Fill hole        status line   │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Left: "Objects & Defects"** (210 pt, resizable). Every object in the document. The active one is in bold and expands to: **Model**, then **Holes (n)**, **Narrow cracks (n)**, **Loose pieces (n)** and **Non-manifold edges (n)**. A category with something in it opens by default, with its header in its defect colour. An empty one is grey and stays closed. Each category lists up to 200 rows, then "…and N more". To work on another object, click its name or click it in the view.
  - **Every row behaves the same way** (M4 Task 8a; one shared row handler, `panels::tree_row`, which rows added later use too): an object, "Model", and every hole, crack, loose piece and tangled edge.
    - **Click** selects it (an object row makes that object active; "Model" targets the whole object). The camera stays.
    - **Double-click**, or **Enter** on the focused row, selects it and frames it: the camera moves there in 200 ms (eased out; camera maths only, no geometry work), then the selection pulses twice over 600 ms (brighter, outlines wider) so the eye finds it. Ordinary clicks don't pulse. A double-click counts only if both clicks land on the same row (two clicks right after a click on another row, which egui counts as a triple-click, are this row's double-click). Double-clicking an object's row frames the object and doesn't pulse a selection on it. A framing still waiting for what it needs after 10 s gives up and says so in the status line.
    - **Hover** highlights it in the view without moving the camera: an outline for holes, cracks and tangled edges, the hover glow for loose pieces and whole objects (another object's row lights that object up, undimmed). Hovering what is already the target shows nothing more (selection wins over hover; with nothing selected, "Model" is the target).
    - **Rows give sizes**, so a speck is recognisable before you zoom to it: "Hole 3 · 42 edges · 12.4 mm" (the perimeter), "Piece 2 · 1 triangle · 0.213 mm" (its largest dimension; pieces are numbered like holes), "Narrow crack · 8 edges", "Tangled edge · 3 faces". The name is on the left, the size smaller and muted on the right, on one line: when the panel is too narrow the size is cut short with "…" rather than wrapped. The accessible label is both, in full. The tooltip gives the title, detail and next action.
    - After an object's row is clicked, the tree keeps its layout for 0.35 s (longer than egui's 0.3 s double-click window), so the second click of a double-click lands on the same row.
  - **Framing a defect** (`framing.rs`): its bounding sphere fills 40% of the view height, with a floor of 1/10,000 of the scene's radius so a degenerate piece can't send the camera infinitely close. It is centred, and the whole scene stays inside the clipping planes. The camera looks from the way the defect faces when it faces away (a hole on the far side; a hole faces the way the face that would close it would), otherwise from the current direction. If something is in the way, or the eye would end up inside the model (an odd number of surfaces beyond it), it tries the six axes and eight diagonals. If the target is shut inside the model, the eye backs out past the model's last surface along the shortest way out, and the x-ray pass shows the target through it. An object is framed whole (75% of the view height) from the current direction, unless it is flat (its thinnest extent at most half the next) and the view is within 25° of edge-on to it: then the view turns towards its thinnest axis, to 60° off its plane, keeping its heading and the side it looks from (up stays up), so its broad side shows. These line-of-sight checks are a few ray casts through the picking BVHs (O(log n) each). Each defect's bounds and facing are measured on the analysis worker (`defects::Place`), so nothing O(n) runs on the UI thread.
  - Category headers aren't targets: they fold and unfold.
  - A **Legend** is pinned to the bottom of the panel: a swatch and a checkbox for each overlay in the view (Selection, Hover, Holes, Narrow cracks, Loose pieces, Non-manifold edges, Recent repairs), each with a tooltip saying what it means. Unticking one hides that overlay; View ▸ Show defects hides every defect overlay at once (their checkboxes are then disabled). Switching an overlay changes only what the view draws, never the GPU data.
- **Right: "Properties & Tools"** (280 pt, resizable). This panel is contextual to the current target:
  - What the target is: its title and details, from the copy table below.
  - The operations valid for it. A targeted tool is already open: its parameters, then **Apply**.
  - When nothing is selected, it shows the whole object (name, triangle count, size in document units, how many defects) and the whole-object tools ("Fill all holes", "Remove loose pieces"), each opened by clicking it.
  - When the target has no operation yet (a crack, a hole over the fill limit), it says what the target needs instead, using the copy table's next action. It does not offer whole-object tools that wouldn't fix it.
  - When a selection has no operation (a face set, or the main body), the whole-object tools are shown and the panel says they act on the whole object.
  - "Details" folds away every number `inspect` reports.
- **Bottom: the history breadcrumb and status line** (one row).
  - Undo and Redo come first, then the history as steps in plain words (Import scan › Import plate › Fill hole › Remove piece).
  - The current step is highlighted. Steps that Redo would take are greyed. Click any step to jump to it.
  - "Branches…" appears when an undone branch exists and lists the whole history tree.
  - The status line on the right reads "Ready", or shows what's running, or the last result.
  - While an import, operation or export runs, the status line shows the job, its stage in plain words and a percentage ("Fill all holes… · filling 42%"), a neutral progress bar (`PROGRESS`), and a **Cancel** button at the far right. Esc does the same as Cancel while a job runs, before it would clear the selection; otherwise Esc clears the selection as before. A job that reports no progress (replay) shows a spinner and has no Cancel.
  - After Cancel the button gives way to "Cancelling…" until the job stops (within 250 ms), then the status reads exactly "Cancelled: document unchanged" (for an export: "Cancelled: nothing exported"). A job that finished before the cancel reached it keeps its result, and the status starts "Finished before it could be cancelled".
  - Stage words: reading the file, joining corners, finding edges, checking, filling, smoothing the patches, removing pieces, writing the file.
  - Help shows how many cores background work uses and how they are split ("Background work: 11 of 12 cores (default: all cores but one): 8 for the running job, 3 for analysis and display"); `meshfixer-desktop --threads N` changes the total.
  - While a new revision is analysed (after an operation, undo or redo), the 3D view keeps showing the object's previous revision with "Updating view…", without the selection and defect overlays (they belong to the new revision); the new one replaces it in a single frame once its GPU buffers are ready.
  - An operation's result is summarised in words in the status line ("Filled 11 holes with 115181 new faces; 201 left open (too large or can't be filled)"). If an object can't be analysed, an error banner says so and the panels say "Couldn't analyse this object" instead of "Analyzing…".
- **There is no operation drop-down.** You reach operations from the target.
- The default window is 1440 × 900 points.

Targets:

| You do | The target becomes | Properties shows |
|---|---|---|
| Click a hole or crack outline in the view, or its tree row | that loop | its copy and "Fill hole", or what it needs |
| Double-click any tree row (or Enter on it) | as a click, and the view frames it | as a click |
| Click a face in the view | that face's connected piece (the model's main body, or a loose piece) | "Main body" with the whole-object tools, or "Loose piece" and "Remove piece" |
| Click a loose piece's tree row | that piece | "Loose piece" and "Remove piece" |
| Click a tangled edge's tree row | that edge (shown, not selected) | its copy and a "Select the faces involved" button |
| Shift-click | adds to a selection of the same kind | "N openings / pieces selected" |
| Click "Model", Esc, or empty space | nothing (the whole object) | the object summary and the whole-object tools |

Targeted tools take their size limits from the target, which already says which hole or piece:

- "Fill hole" raises `max_loop_edges` to fit the selected hole.
- "Remove piece" sets `max_triangles` just above the piece's size.

These values are recorded in the command log like any other parameter. They are not shown, because they aren't a choice.

## Colour tokens

They are defined once, in `theme.rs`, and used by the panels and the renderer alike. The renderer gets them as an `mf_render::Palette` plus layer colours; it has no colours of its own, apart from a neutral palette for headless renders. One colour never means two things.

| Token | Colour | Means | Where |
|---|---|---|---|
| `SELECTED` | cyan `#28D2FF` | the current target | selected loops and tangled edges (outline), pieces and faces (tint, which replaces any defect tint on them), and the same outline or tint drawn through nearer surfaces at opacity 0.55 (`XRAY_OPACITY`, the x-ray pass) where they hide it; tree rows, the current history step, the targeted tool's name |
| `HOVER` | light `#E8EEF5` | what the pointer is over | the outline (loops, edges) or glow (pieces: strength 0.85 and barely shaded, so it reads near-white at any angle) of the defect under the pointer, in the tree or in the 3D view (where a tooltip gives its size); widget hover outlines |
| `HOLE` | orange `#FF8020` | an open boundary (hole) | every boundary outline in the view; the Holes header |
| `CRACK` | magenta `#E646D2` | a narrow crack (edges almost touch) | crack outlines; the Narrow cracks header |
| `LOOSE_PIECE` | amber `#FFC828` | a piece not connected to the model | tint of every loose piece (every piece but the main body: the same set the tree lists, whatever its size); the Loose pieces header |
| `NON_MANIFOLD` | red `#EB3434` | a tangled (non-manifold) edge | tangled-edge lines, and a light red tint (strength 0.35) on the faces that meet at them; the Non-manifold edges header |
| `RECENT_REPAIR` | green `#50CD78` | faces made by the last 3 commands on the active object | a tint, toggled by View ▸ Highlight recent repairs or the legend (on by default) |
| inactive object | desaturated (saturation 0, brightness 0.55) | not the active object | the whole object, outlines included |

Neutral tokens carry no meaning beyond "the model" and "the room":

| Token | Colour | Use |
|---|---|---|
| `SURFACE` | `#C8C2B8` | front faces |
| `BACK_FACE` | `#4E5464` | back faces, seen through holes or flipped (dark slate, so it can't be confused with a defect colour) |
| `VIEWPORT_BACKGROUND` | `#26292E` | the 3D view's background |
| `TEXT_MUTED` | grey 150 | secondary text, hints, steps Redo would take |
| `PROGRESS` | grey 120 | the running job's progress bar (neutral: progress is neither the selection's cyan nor a defect colour) |
| `ERROR_FILL` | `#6E203A` | the error banner (a dark rose fill for failures, never used in the 3D view) |

Line widths: defect outlines are 2 pt; overlay lines (selection, hover, cracks, tangled edges) are 3 pt. They are drawn as screen-space quads, so they keep that width at any zoom and on Retina displays.

Draw order, bottom to top:

1. the model surface, with its face tints, then hole outlines
2. the x-ray pass for selected faces: drawn only where something nearer hides them (depth test "greater", no depth write), in `SELECTED` at `XRAY_OPACITY`
3. crack lines, tangled-edge lines, selection outline, hover outline
4. the x-ray pass for the selection outline (as 2)

**Hidden selections show through (x-ray).** A selected item hidden behind other geometry, such as a loose piece inside the model or a hole on the far side, is drawn through the surfaces in front of it. It uses the same cyan as the selection, at reduced opacity, because it means the same thing: the current target. The parts in view draw normally, and nothing else is drawn through. While the selection pulses after a double-click framing, the face tint goes up to full strength and is evenly lit, and outlines get up to 40% lighter (same hue) and twice as wide.

Face tints are not separate layers: they are drawn with the surface, from a per-face state buffer (one byte of flags per face, `mf_render::FaceStates`): loose piece, next to a tangled edge, recent repair, selected, hover. One colour, one meaning: a selected face shows only the selection tint, even when it is hovered too or is a loose piece (so it is cyan, never a blend); a hovered face shows only the hover glow (the hover tint, with little shading; the hover outline lines still draw over a selection); otherwise the defect and repair tints apply in that order. Changing the selection or hover rewrites only the changed chunks of that buffer (at most 32 KB per 32,768 faces), never the geometry; it is built on a worker.

Clicking in the view picks, nearest first within 8 pt on screen and not hidden by any object: a boundary loop's edge (selects the loop) or a drawn tangled edge of the active object (selects the faces that meet at it). Otherwise the clicked face's piece is selected: clicking a tinted loose piece selects it.

## Plain-language defects

One function, `defects::defect_copy`, words every defect for the tree, Properties and tooltips. It is unit-tested against this table (`defect_copy_matches_the_table`).

| Kind | Title | Tree row (name · size) | Detail | Next action |
|---|---|---|---|---|
| hole | "Hole N: small opening" (or "large opening" above 200 edges) | "Hole N · 42 edges · 12.4 mm" | "42 boundary edges · 12.4 mm perimeter" | "Fill hole" |
| slit | "Narrow crack" | "Narrow crack · 8 edges" | "Edges almost touch · 8 edges" | "Needs edge stitching (not yet available)" |
| island | "Loose piece" | "Piece N · N triangles · 0.420 mm" (numbered; largest dimension) | "N triangles, 0.420 mm across, not connected to the model" | "Remove piece" |
| non-manifold | "Tangled edge" | "Tangled edge · 3 faces" | "More than two faces share this edge · 3 faces" | "Select the faces involved" |
| over cap | "Very large opening" | "Very large opening · 1500 edges · 88.0 mm" | (as hole) | "Needs the large-hole method (not yet available)" |

- **Measurements** are in the document's units: mm when a file has none (STL, OBJ), or µm, cm, in, ft or m from a 3MF. They are given to three significant figures (12.4, 3.41, 0.500), and as whole numbers from 100 up. A perimeter that overflowed reads "perimeter too large to measure".
- **Classification** runs on the analysis worker, never the UI thread (MF-RT-001):
  - A boundary loop is a **narrow crack** when its vector area is below the degenerate-area threshold `inspect` and fill_holes use (1e-12 × bounding-box diagonal²).
  - A loop with more edges than fill_holes can take (the spec's `max_loop_edges` maximum, 1000) is **over cap**.
  - Every component after the largest is a **loose piece**.
  - Each non-manifold edge is a **tangled edge**, listed with the faces that share it.
- **Numbering:** "Hole N" counts holes in the tree's order.
- **Operation names** come from `defects::operation_label` and use the same words in Properties and the history breadcrumb: Fill hole / Fill holes (n) / Fill all holes, and Remove piece / Remove pieces (n) / Remove loose pieces.

## Metrics (View ▸ Metrics)

A window over the 3D view's top-left corner (M4 Task 10, MF-RT-005). Each line is a plain-language label followed by the engineering numbers; muted text explains what a figure includes.

- **Frame times:** CPU per frame at p50 / p95 / p99 over the last 1000 frames comes first. GPU time is shown only with its counts ("GPU p95 2.2 ms from 28 of 600 frames; 572 unusable"), or as "not supported on this device"; never as a bare figure or a zero. Then the **redraw rate**: fps over frames redrawn back to back (gaps under 100 ms), with the idle gaps it leaves out, labelled as a redraw rate, not a speed figure: the app repaints on demand, so an idle app's frame-to-frame gaps are not slowness.
- **Operations:** the last few jobs (imports, operations, exports) with their duration and how far the heap rose above its level at their start.
- **Memory:** mesh storage, cached topology ("not computed yet" if it isn't), the last operation's peak, peak heap since start, GPU buffers by kind (geometry, face states, outlines, view textures), bytes uploaded by the last change, resident memory (RSS, labelled as excluding swapped and compressed pages) and, on macOS, the memory footprint (Activity Monitor's "Memory": compressed and swapped pages and GPU buffers included), with the reason when the OS gives none.
- **Threads:** the thread budget with its `PoolChoice` reason and the jobs/background split.

It opens over the 3D view's top-left corner, clear of the side panels. It reads a snapshot of cached counters refreshed twice a second while open (the process memory is sampled on its own thread), and asks for a repaint every half second so it keeps updating while the app is otherwise idle; it never does work proportional to the mesh on a UI frame. Job names show the file name, not its folder.

## Camera controls (Help ▸ Controls)

Every move works without a mouse wheel: on a trackpad, with a one-button mouse, or from the keyboard (M4 Task 2a).

| Input | Action |
|---|---|
| Left-drag | orbit, or pan or zoom when the navigation bar's mode is Pan or Zoom |
| Right- or middle-drag, or Shift + left-drag | pan |
| Option/Alt + left-drag (up/down) | zoom toward where the drag started (up zooms in) |
| Mouse wheel | zoom towards the point under the cursor |
| Two-finger drag (trackpad) | orbit |
| Shift + two-finger drag | pan |
| Pinch | zoom towards the cursor |
| + or = / − | zoom in / out, 1.25× toward the view centre |
| F | frame the target (or everything when nothing is targeted), keeping the view direction |
| Double-click a tree row, or Enter on it | select it and frame it (animated; see the layout section) |
| Home, or Cmd/Ctrl+0 | reset the view: the default angle, everything in view (Home needs Fn+← on Mac laptops) |
| Esc | cancel the running job; otherwise clear the selection |

- **Navigation bar.** It sits at the 3D view's bottom-right, inside the view, drawn with the theme tokens. Each control has an accessible name and a tooltip naming its shortcut.
  - "Orbit / Pan / Zoom": a mode switch (default Orbit) that sets what a plain left-drag does. The current mode shows in the selected (cyan) style, because it is the selected option.
  - "Zoom out" (−) and "Zoom in" (+): 1.25× toward the view centre per click. Holding a button repeats every 0.12 s after 0.4 s.
  - "Frame selection" (F) and "Reset view" (Home).
- **Keys stay with text and menus.** Navigation keys only act when no text field or popup (menu) has focus.
- **No UI scaling.** egui's Cmd/Ctrl + / − / 0 UI scaling is turned off, so those keys belong to the 3D view.
- **Mouse versus trackpad.** egui reports a mouse wheel and a two-finger drag both as scroll events. The unit tells them apart:
  - points means a trackpad, so the view orbits (or pans with Shift);
  - lines or pages means a wheel, so the view zooms.
- **Momentum.** Momentum scrolling after the fingers lift is ignored, so the view stops when you stop.
- **Pinch.** Pinch, and Ctrl/Cmd + wheel (which egui turns into zoom), zoom towards the cursor.
- **F** frames the target at 2.5× its size, so its surroundings show. If the target faces away from the camera (for example, a hole on the far side), the view first turns to look at it from outside. The move is instant. Like a double-click framing, it keeps the whole scene inside the clipping planes, so framing a speck doesn't clip away the model around it. A double-click on a tree row animates the move (Task 8a).

## Constraints learned

- egui's bundled fonts have no "▸" or "→". UI strings use ">" and "›" instead (this document may use ▸).
- egui reports a selectable button's state to accessibility as *toggled*. GUI tests read the current history step that way.
- Interactive widgets need accessible labels: the kittest accessibility check runs whenever the UI settles.
