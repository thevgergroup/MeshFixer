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
  - The status line on the right reads "Ready", or shows what's running (with a spinner), or the last result.
- **There is no operation drop-down.** You reach operations from the target.
- The default window is 1440 × 900 points.

Targets:

| You do | The target becomes | Properties shows |
|---|---|---|
| Click a hole or crack outline in the view, or its tree row | that loop | its copy and "Fill hole", or what it needs |
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
| `SELECTED` | cyan `#28D2FF` | the current target | selected loops (outline), pieces and faces (tint), tree rows, the current history step, the targeted tool's name |
| `HOVER` | light `#E8EEF5` | what the pointer is over | the outline of the defect under the pointer in the tree, drawn in the view; widget hover outlines |
| `HOLE` | orange `#FF8020` | an open boundary (hole) | every boundary outline in the view; the Holes header |
| `CRACK` | magenta `#E646D2` | a narrow crack (edges almost touch) | crack outlines; the Narrow cracks header |
| `LOOSE_PIECE` | amber `#FFC828` | a piece not connected to the model | loose-piece tint; the Loose pieces header |
| `NON_MANIFOLD` | red `#EB3434` | a tangled (non-manifold) edge | tangled-edge lines; the Non-manifold edges header |
| `RECENT_REPAIR` | green `#50CD78` | faces made by the last 3 commands on the active object | a tint, toggled by View ▸ Highlight recent repairs (on by default) |
| inactive object | desaturated (saturation 0, brightness 0.55) | not the active object | the whole object, outlines included |

Neutral tokens carry no meaning beyond "the model" and "the room":

| Token | Colour | Use |
|---|---|---|
| `SURFACE` | `#C8C2B8` | front faces |
| `BACK_FACE` | `#4E5464` | back faces, seen through holes or flipped (dark slate, so it can't be confused with a defect colour) |
| `VIEWPORT_BACKGROUND` | `#26292E` | the 3D view's background |
| `TEXT_MUTED` | grey 150 | secondary text, hints, steps Redo would take |
| `ERROR_FILL` | `#6E203A` | the error banner (a dark rose fill for failures, never used in the 3D view) |

Line widths: defect outlines are 2 pt; overlay lines (selection, hover, cracks, tangled edges) are 3 pt. They are drawn as screen-space quads, so they keep that width at any zoom and on Retina displays.

Draw order, bottom to top:

1. the model surface, then hole outlines
2. loose-piece tint, recent-repair tint, selection tint, hover tint
3. crack lines, tangled-edge lines, selection outline, hover outline

## Plain-language defects

One function, `defects::defect_copy`, words every defect for the tree, Properties and tooltips. It is unit-tested against this table (`defect_copy_matches_the_table`).

| Kind | Title | Detail | Next action |
|---|---|---|---|
| hole | "Hole N: small opening" (or "large opening" above 200 edges) | "42 boundary edges · 12.4 mm perimeter" | "Fill hole" |
| slit | "Narrow crack" | "Edges almost touch" | "Needs edge stitching (not yet available)" |
| island | "Loose piece" | "N triangles, not connected to the model" | "Remove piece" |
| non-manifold | "Tangled edge" | "More than two faces share this edge" | "Select the faces involved" |
| over cap | "Very large opening" | (as hole) | "Needs the large-hole method (not yet available)" |

- **Measurements** are in the document's units: mm when a file has none (STL, OBJ), or µm, cm, in, ft or m from a 3MF. They are given to three significant figures (12.4, 3.41, 0.500), and as whole numbers from 100 up. A perimeter that overflowed reads "perimeter too large to measure".
- **Classification** runs on the analysis worker, never the UI thread (MF-RT-001):
  - A boundary loop is a **narrow crack** when its vector area is below the degenerate-area threshold `inspect` and fill_holes use (1e-12 × bounding-box diagonal²).
  - A loop with more edges than fill_holes can take (the spec's `max_loop_edges` maximum, 1000) is **over cap**.
  - Every component after the largest is a **loose piece**.
  - Each non-manifold edge is a **tangled edge**, listed with the faces that share it.
- **Numbering:** "Hole N" counts holes in the tree's order.
- **Operation names** come from `defects::operation_label` and use the same words in Properties and the history breadcrumb: Fill hole / Fill holes (n) / Fill all holes, and Remove piece / Remove pieces (n) / Remove loose pieces.

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
| Home, or Cmd/Ctrl+0 | reset the view: the default angle, everything in view (Home needs Fn+← on Mac laptops) |
| Esc | clear the selection |

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
- **F** frames the target at 2.5× its size, so its surroundings show. If the target faces away from the camera (for example, a hole on the far side), the view first turns to look at it from outside. The move is instant; a later task animates it.

## Constraints learned

- egui's bundled fonts have no "▸" or "→". UI strings use ">" and "›" instead (this document may use ▸).
- egui reports a selectable button's state to accessibility as *toggled*. GUI tests read the current history step that way.
- Interactive widgets need accessible labels: the kittest accessibility check runs whenever the UI settles.
