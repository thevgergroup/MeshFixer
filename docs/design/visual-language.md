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
  - A **Legend** is pinned to the bottom of the panel: a swatch and a checkbox for each overlay in the view (Selection, Hover, Holes, Hole markers, Narrow cracks, Loose pieces, Tangled edges, Recent repairs, plus Intentional openings without a switch), each with a tooltip saying what it means. Unticking one hides that overlay; View ▸ Show defects hides every defect overlay at once (their checkboxes are then disabled). Switching an overlay changes only what the view draws, never the GPU data. The legend is data-driven (`tools::LegendRow`): an open interactive tool supplies its own rows instead. It folds away (click "Legend"), so the tree, and later the Parts library, get the room on a laptop screen.
  - **Room for Parts** (M4 final UX review I8): the panel's body is one of `tools::LeftTab`'s sections. Today there is only Objects, so no tab strip shows; when the Parts library arrives (Phase 1 Task 23) a tab row "Objects | Parts" appears above the legend.
- **Right: "Properties & Tools"** (280 pt, resizable). This panel is contextual to the current target:
  - What the target is: its title and details, from the copy table below.
  - The operations valid for it. A targeted tool is already open: its parameters, then **Apply**.
  - When nothing is selected, it shows the whole object (name, triangle count, size in document units, how many defects) and the whole-object tools, each opened by clicking it. They come from a curated list (`tools::CATALOGUE`, M4 final UX review I8), never straight from the spec registry, grouped under small headings in this order: **Modify** (Transform; later Split, Sculpt), **Repair** (Fill all holes, Remove loose pieces), **Combine** (booleans; later Parts), **Housekeeping** (Compact). An operation that isn't listed is never offered as a button.
  - A tool is **one-shot** (its parameters, then Apply: one command) or **interactive** (it works in the 3D view while it is open). An open interactive tool gets the view's primary drag where it claims the press (its handles), ahead of the camera, box selection and clicks; Option/Alt and Shift camera drags always win. It has its own keys, and supplies the legend's rows. **Esc** works in this order, one step per press: cancel the running job; cancel the tool's drag (nothing committed); leave the tool; clear the selection.
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
  - Stage words: reading the file, joining corners, finding edges, checking, filling, adding detail to the patches, shaping the patches to the surface, removing pieces, writing the file.
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
| `HOLE` | orange `#FF8020` | an open boundary (hole) | every boundary outline in the view, except intentional openings; the Holes header; the hole markers |
| `OPENING` | slate `#8492A0` | an intentional opening (a boundary marked as meant to be open) | its outline in the view, drawn over the hole outline (and over a marked crack's line); the Intentional openings header and legend row |
| `CRACK` | magenta `#E646D2` | a narrow crack (edges almost touch) | crack outlines; the Narrow cracks header |
| `LOOSE_PIECE` | amber `#FFC828` | a piece not connected to the model | tint of every loose piece (every piece but the main body: the same set the tree lists, whatever its size); while "Remove loose pieces" is open, only the pieces its Apply would remove (state bit `LOOSE_PIECE`); the Loose pieces header |
| `NON_MANIFOLD` | red `#EB3434` | a tangled (non-manifold) edge | tangled-edge lines, and a light red tint (strength 0.35) on the faces that meet at them; the Tangled edges header (tooltip: non-manifold edges) |
| `RECENT_REPAIR` | green `#50CD78` | faces made by the last 3 commands on the active object | a tint, toggled by View ▸ Highlight recent repairs or the legend (on by default) |
| `AXIS_X` / `AXIS_Y` / `AXIS_Z` | `#E5484D` / `#46A758` / `#3E8BF0` | the conventional X/Y/Z axis colours (D8) | the transform gizmo's handles only, while the Transform tool is open (its legend shows the axes in these colours in place of the defect legend) |
| `GIZMO_FREE` | grey 180 | a gizmo handle with no single axis (D9) | the move centre, the view ring (drawn only while hovered or dragged) and the uniform-scale handle only; its legend row is "Free handles". Distinct from `HOVER` (near-white), so a free handle never reads as hovered |
| inactive object | desaturated (saturation 0, brightness 0.55) | not the active object | the whole object, outlines included |

Neutral tokens carry no meaning beyond "the model" and "the room":

| Token | Colour | Use |
|---|---|---|
| `SURFACE` | `#C8C2B8` | front faces |
| `BACK_FACE` | `#4E5464` | back faces, seen through holes or flipped (dark slate, so it can't be confused with a defect colour) |
| `VIEWPORT_BACKGROUND` | `#26292E` | the 3D view's background |
| `TEXT` | grey 225 | primary text drawn over the 3D view: the open tool's line at the view's top-left ("Transform · W move · …") |
| `TEXT_MUTED` | grey 150 | secondary text, hints, steps Redo would take |
| `PROGRESS` | grey 120 | the running job's progress bar (neutral: progress is neither the selection's cyan nor a defect colour) |
| `ERROR_FILL` | `#6E203A` | the error banner (a dark rose fill for failures, never used in the 3D view) |
| `PLATE` / `PLATE_MAJOR` | grey 100 / grey 140 | the build plate's grid (256 × 256 mm, 10 mm spacing, a stronger line every 50 mm), drawn at world z = 0 while Transform is open, depth-tested against the model and fading out by four times the camera's distance; light enough to read on the dark background, darker than the model so it never competes with geometry |

Line widths: defect outlines are 2 pt; overlay lines (selection, hover, cracks, tangled edges) are 3 pt. They are drawn as screen-space quads, so they keep that width at any zoom and on Retina displays.

Draw order, bottom to top:

1. the build plate's grid (`PLATE`), depth-tested against the model so an object resting on it hides the plate underneath, fading with distance from the camera
2. the model surface, with its face tints, then hole outlines
3. the x-ray pass for selected faces: drawn only where something nearer hides them (depth test "greater", no depth write), in `SELECTED` at `XRAY_OPACITY`
4. crack lines, tangled-edge lines, selection outline, hover outline
5. the x-ray pass for the selection outline (as 3)
6. while the Transform tool is open, the gizmo's handles, last and with no depth test, so they're never hidden by the model (D8); the hovered handle draws 40% lighter and twice as wide, as the selection pulse does

**Hidden selections show through (x-ray).** A selected item hidden behind other geometry, such as a loose piece inside the model or a hole on the far side, is drawn through the surfaces in front of it. It uses the same cyan as the selection, at reduced opacity, because it means the same thing: the current target. The parts in view draw normally, and nothing else is drawn through. While the selection pulses after a double-click framing, the face tint goes up to full strength and is evenly lit, and outlines get up to 40% lighter (same hue) and twice as wide.

Face tints are not separate layers: they are drawn with the surface, from a per-face state buffer (one byte of flags per face, `mf_render::FaceStates`): loose piece, next to a tangled edge, recent repair, selected, hover. One colour, one meaning: a selected face shows only the selection tint, even when it is hovered too or is a loose piece (so it is cyan, never a blend); a hovered face shows only the hover glow (the hover tint, with little shading; the hover outline lines still draw over a selection); otherwise the defect and repair tints apply in that order. Changing the selection or hover rewrites only the changed chunks of that buffer (at most 32 KB per 32,768 faces), never the geometry; it is built on a worker.

Clicking in the view picks, nearest first within 8 pt on screen and not hidden by any object: a boundary loop's edge (selects the loop) or a drawn tangled edge of the active object (selects the faces that meet at it). Otherwise the clicked face's piece is selected: clicking a tinted loose piece selects it. Clicking an object's main body while no defect is targeted makes the whole object the target (Properties shows its whole-object tools, Transform first); with a defect targeted, it selects the main body as before. While Transform is open, a click on any object makes that object the target, and a click on a gizmo handle is the tool's (no pick).

## The Transform tool (MF-OBJ-001, Phase 1 Task 5)

- **Opening it.** Click an object's tree row, or click the object in the view while no defect is targeted: the whole object is the target, and Properties lists **Transform** first (under Modify). **T** opens it from anywhere (it clears a defect selection); while it is open, **W**, **E** and **R** pick Move, Rotate and Scale, as in Meshmixer. Those three keys are the tool's only while it is open (the framework routes keys to the open tool); otherwise nothing takes them. The view's top-left says so: "Transform · W move · E rotate · R scale · Cmd/Ctrl: snap off · Esc: done". With Transform open, a click on any object makes it the target.
- **The gizmo** sits at the object's bounding-box centre, 110 pt across whatever the zoom, drawn last so the model never hides it. X red, Y green, Z blue (D8, gizmo only); `GIZMO_FREE` grey for the handles with no single axis (the centre, the view ring, uniform scale). The hovered or dragged handle draws lighter and twice as wide.
  - **Rotate's handles** (Phase 1 design stream, owner review). Each axis ring carries a **grip**: a short curved arrow on the ring, in the ring's colour, with an arrowhead at each end pointing along the ring, so the rings say "drag along here" the way the move arrows do. It is a band 3× the ring's line width, 18 pt either side of its centre, with 12 pt arrowheads 15 pt wide (`mf_render::GRIP_*`). It sits at the point of the ring nearest the viewer, where the ring's tangent lies flat to the screen so the arc reads at full length. Seen face-on (looking along the axis) the grip rests at the ring's lower-right, blending into the nearest point as the view tilts; seen edge-on (where the nearest point would sit on the pivot, on top of the other handles) it slides along the ring towards the screen's bottom until it is at least 35% of the ring's screen radius out (`GRIP_CLEARANCE`). Grips are drawn after all three rings, so no ring's line crosses a grip. The grip is the ring's own handle (`Handle::Ring`): pressing either the grip or anywhere on the ring's front half turns about that axis, and the grip wins over another ring that passes near it.
  - **No view ring at rest.** The grey ring around the rings (turn in the view's plane) read as a mistake, so it isn't drawn until the pointer is on it: point just outside the axis rings (1.15× the gizmo's size) and it appears, highlighted, and a drag there turns the object in the screen's plane. `Gizmo::shapes(…, highlight)` adds it only while it's hovered or dragged.
  - **Draw/hit parity.** `Gizmo::hit` tests exactly the shapes `Gizmo::shapes` draws (with the view ring included, because a press can only land on it while it's hovered, and so drawn). A test sweeps every pixel around the gizmo in each mode from several views: wherever `hit` returns a handle, a shape of that handle is drawn within the 8 pt tolerance.
- **Dragging** a handle moves only the object's model matrix, so a 30M-triangle object drags as smoothly as it orbits. Releasing commits one `object.set_transform` with the absolute matrix (D7): one undo per drag. **Esc** (or Cmd/Ctrl+Z) mid-drag puts it back and commits nothing (the next Esc leaves the tool). The camera holds still while a handle is dragged: no orbit, pan, wheel or pinch. Snapping is on by default (1 mm grid, 15°, 10% steps, and the plate within 8 pt); **Cmd/Ctrl** while dragging inverts it; the Snap checkbox switches the default. Option/Alt and Shift drags still move the camera.
- **Fields.** Position (the bounding-box centre, in document units), Rotation (degrees, extrinsic XYZ) and Scale (%, with a Uniform lock). Type and press **Enter** to commit one command (Enter on the value the field already shows commits nothing); leaving a field without Enter puts its value back. W, E and R typed into a field are text. A transform with shear from the file shows "Transform includes shear (from the file)": Rotation and Scale stay read-only (they show "—"), Rotate and Scale modes are off, Move still works (Review Focus 2). A mirrored object shows "Mirrored"; its scale fields show sizes, and it stays mirrored.
- **Buttons.** **Drop to plate** runs on the worker (progress and Cancel in the status bar on a big model) and moves the object straight down or up until its lowest point rests at z = 0. **Reset** takes rotation and scale back to none and 100%, leaving the object where it is. **Done** (or Esc) closes the tool.
- **While it is open** the defect and recent-repair tints and outlines, the hole markers, and hovering a defect (its outline and its "Click to select" tooltip) are hidden, and the legend shows the axes instead (D8), so on screen a colour still has one meaning. The build plate (`PLATE`, a 256 × 256 mm grid at z = 0, 10 mm cells with a `PLATE_MAJOR` line every 50 mm, in document units) shows under the model. An intersection overlay, once it exists (P1-M2), stays.
- **Status line.** "Moved scan by 12.0, 0, 0 mm", "Rotated plate 90° about Z", "Scaled part by 150%", "Dropped part onto the plate: moved 7.50 mm down". The history step reads "Transform" (or "Drop to plate").
- **A move keeps the analysis.** What the analysis finds about the mesh (diagnostics, which holes, cracks, pieces and tangled edges there are, the render mesh, face states and outline lines) belongs to the mesh and is kept across a move, a turn, or an undo of one. Only the measurements (perimeters, sizes, where each defect is) are made again, on a worker: a pure move shifts them (O(defects)); a turn or scale measures again without inspecting or uploading anything. Meanwhile the panels, the tree and hovering keep the previous values; nothing goes blank and "Analysing…" doesn't show.

## Plain-language defects

One function, `defects::defect_copy`, words every defect for the tree, Properties and tooltips. It is unit-tested against this table (`defect_copy_matches_the_table`).

| Kind | Title | Tree row (name · size) | Detail | Next action |
|---|---|---|---|---|
| hole | "Hole N: small opening" (or "large opening" above 200 edges) | "Hole N · 42 edges · 12.4 mm" | "42 boundary edges · 12.4 mm perimeter" | "Fill hole" |
| slit | "Narrow crack" | "Narrow crack · 8 edges" | "Edges almost touch · 8 edges" | "Slit: needs welding (not yet available)" |
| island | "Loose piece" | "Piece N · N triangles · 0.420 mm" (numbered; largest dimension) | "N triangles, 0.420 mm across, not connected to the model" | "Remove piece" |
| non-manifold | "Tangled edge" | "Tangled edge · 3 faces" | "More than two faces share this edge · 3 faces" | "Select the faces involved" |
| over cap | "Very large opening" | "Very large opening · 1500 edges · 88.0 mm" | (as hole) | "Needs a large-hole method (not yet available)" |

- **Measurements** are in the document's units: the unit chosen when a unitless file (STL, OBJ) was opened (see "Units" below), or µm, cm, in, ft or m from a 3MF. They are given to three significant figures (12.4, 3.41, 0.500), and as whole numbers from 100 up. A perimeter that overflowed reads "perimeter too large to measure".
- **Classification** runs on the analysis worker, never the UI thread (MF-RT-001):
  - A boundary loop is a **narrow crack** when its vector area is below the degenerate-area threshold `inspect` and fill_holes use (1e-12 × bounding-box diagonal²).
  - A loop with more edges than fill_holes can take (the spec's `max_loop_edges` maximum, 1000) is **over cap**.
  - Every component after the largest is a **loose piece**.
  - Each non-manifold edge is a **tangled edge**, listed with the faces that share it.
- **Numbering:** holes are listed largest first (by edge count), and a first analysis numbers them in that order. Numbers are stable (M4 Task 12): a hole is identified across revisions by its lowest corner and edge count, keeps its number after other holes are filled (fill Hole 1 and Hole 3 is still "Hole 3"), and gets it back when an undo reopens it; new holes get the next number.
- **Operation names** come from `defects::operation_label` and use the same words in Properties and the history breadcrumb: Fill hole / Fill holes (n) / Fill all holes, and Remove piece / Remove pieces (n) / Remove loose pieces.

## The defect browser at scale (M4 Task 12)

Sculpts and scans often have hundreds of holes. The tree's **Holes** section (counted without the intentional openings) starts with **Previous / Next** (Shift+N / N), then a line per size group with its count and actions, then each group's rows:

| Group | Edges | Actions |
|---|---|---|
| Too large for the current method | over 1000 | none: "Needs a large-hole method (not yet available)" (MF-ISSUE-043) |
| Large holes | 201–1000 | "Fill all N large holes", "Mark intentional" |
| Medium holes | 31–200 | as above |
| Small holes | 9–30 | as above |
| Tiny holes | up to 8 | as above |

- A group fill is one `fill_holes` command on those loops: one Undo brings them all back. A hole that passes through one point twice (MF-ISSUE-051) is listed but left out of fills.
- N / Shift+N scroll the tree to the hole's row as well as framing it. A group's "Fill all N" counts exactly the holes it fills.
- Every hole row has **Fill** at its right (that hole alone, one step); a loose piece's row has **Remove island**. Rows are virtualised: only the rows in view are laid out, so every one of thousands is reachable by scrolling and a frame costs the same however many there are.
- **Fill all holes** (Properties, nothing selected) has a **Fill holes up to [N] edges** slider and a live preview ("353 holes · 4 left open"); Apply fills those holes as one command on their loops. (Before a big model's holes are listed, an object with no marked openings fills them as a whole-object run.)
- **Intentional openings:** "Mark as intentional opening" (a selected hole or several, or a group's "Mark intentional") is an undoable step that changes no geometry. Marked openings are stored on the object and follow later edits like a selection; they're left out of hole counts, groups, the slider, Fill all holes and the overview's markers, and listed under **Intentional openings** with **Unmark**. The command log records each mark, so a session replays with them.
- **Selecting several:** Shift-click adds a hole; B (or the navigation bar's **Select** mode) then a drag selects every visible hole whose centre is in the box (until every object's picking data is built it says "Picking isn't ready yet" instead of guessing). Properties then says "N holes selected" with one **Fill holes (N)**.
- **Hole markers (on by default; View ▸ Show hole markers, or the legend):** each listed hole under 4 points across on screen, and not hidden behind the model, gets a fixed-size orange marker, so a scan's tiny holes show in the view without turning anything on (M4 final UX review I6).
- **View ▸ Show hole overview:** faces within one ring of a hole get a faint hole tint (state bit `HOLE_NEIGHBOURHOOD`, `HOLE` at strength 0.3, under every other tint), and each listed hole under 4 points across on screen, and not hidden behind the model, gets a fixed-size orange marker, so tiny holes show when zoomed out. The markers are worked out on a worker whenever the camera moves.
- Tool summaries lead with plain words ("Closes holes with new triangles matched to the mesh around them.") and the spec's engineering summary follows, smaller; every parameter's tooltip is its spec description.

## Units (MF-ISSUE-045)

STL and OBJ files don't say what their numbers measure. Opening one asks, in a dialog titled "Units for <file>": one radio row per unit, each giving the model's size read that way, with millimetres alongside ("Metres · 0.495 × 0.410 × 0.300 m (495 × 410 × 300 mm)"). The unit that makes the model between 10 mm and 2 m is preselected, trying millimetres, metres, centimetres and inches in that order (millimetres if none does), so a scan in metres opens at metre scale. Import records the choice in the document and 3MF export writes it; Cancel or Esc opens nothing. 3MF files carry their unit and aren't asked about. Without the prompt (tests, the CLI without `--units mm|cm|m|in`) a unitless file reads as millimetres, with a warning saying so.

## Removing loose pieces safely (MF-ISSUE-052)

"Remove loose pieces" (the whole object) only removes a piece that is under **Max triangles** *and* no bigger than **Max relative size** (default 1%) of the largest piece, so a design made of many parts keeps them. While the tool is open, Properties says what Apply would do ("Removes 1,050 of 1,100 pieces (6.4% of triangles)") and the view tints just those pieces. If that is more than 3% of the triangles, or more than half of the pieces and more than 2% of the triangles, Apply asks first, in a modal dialog: "This would delete 1,050 of 1,100 parts of 'verlan dress' (6.4% of its triangles). These may be part of the design, not debris." with **Remove them** and **Keep them** (Esc keeps them). Clearing a scan's specks (most pieces, about 1% of the triangles) doesn't ask, so the question stays rare enough to be read.

## Messages

Every error ends with what to do next ("…Split it in another tool, or ask us to raise the limit."; "…Choose another location and try again."). An empty document says how to start: "Open a mesh (Cmd+O) or drop a file here". Shortcuts are written the platform's way, by name (Cmd on macOS, Ctrl elsewhere; the bundled fonts have no ⌘ glyph). The unit and removal dialogs are modal: nothing behind them answers until they're answered. A file dropped on the window opens, one at a time.

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
| F | frame the target (or everything when nothing is targeted), keeping the view direction, with the same 200 ms eased move as the tree |
| N / Shift+N | select and frame the next / previous hole in the list's order |
| B, then drag | select the visible holes whose centre is inside the box (Shift adds); the navigation bar's Select mode makes every drag do this |
| Cmd+O | open a mesh (or drop a file on the window) |
| Double-click a tree row, or Enter on it | select it and frame it (animated; see the layout section) |
| Home, or Cmd/Ctrl+0 | reset the view: the default angle, everything in view (Home needs Fn+← on Mac laptops) |
| Esc | cancel the running job; otherwise clear the selection. Esc that closes a menu or dialog does only that (MF-ISSUE-041) |

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
