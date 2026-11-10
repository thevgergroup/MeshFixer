# MeshFixer UI plan: layout, styling and the pluggable shell

Status: **approved by the owner, 2026-11-11** ("Yes"). The owner added three things: every tool has a visual icon; internationalisation (§10); and multi-material and multi-colour models stand out from the accoutrement (§11). Answers to §9: dark chrome, the Browser on the right, and an icon-only rail at laptop sizes, as proposed.
Supersedes: the panel-by-panel layout that grew during M3–P1. Owner verdict: "a jumble like tags from a tag cloud".
Inputs: the owner's Meshmixer and Fusion 360 screenshots and notes, the M4 final UX review, and the P1-A frames.

---

## 1. What we keep and what we drop from the references

| | Meshmixer | Fusion 360 | MeshFixer decision |
|---|---|---|---|
| **The mesh** | Crisp, clean, the hero of the screen. A matte material, a visible wireframe on demand, a big mid-grey stage | Clean, with a ground shadow | **Keep and improve.** The model is the brightest, most detailed thing on screen. The chrome stays neutral and dim. |
| **Primary tools** | A labelled icon rail on the left (Import, Meshmix, Select, Sculpt, Stamp, Edit, Analysis, Shaders, Export, Print) | A ribbon: workspace tabs, then grouped icon dropdowns | **Keep Meshmixer's labelled rail.** It is visual, always visible and learnable. No ribbon tabs: they hide tools behind a mode switch. |
| **Tool settings** | A panel that docks beside the rail. Good: one place. | A floating dialog with OK and Cancel, beside the model | **One docked tool panel**, beside the rail, with OK/Apply and Cancel. Never floating over the model. |
| **Sub-commands** | **Deep flyouts:** Edit… → Modify… → submenu, then navigate back to change a setting. The owner calls this the main failure. | Dropdown groups (CREATE ▾, MODIFY ▾), one level deep | **At most one level.** A tool's sub-commands are a visible list inside its panel, never cascading menus. Changing a setting never needs navigating back. |
| **Object list** | None (it had an Object Browser window) | A collapsible Browser tree, top left | **A collapsible Browser** that docks on the right, closed by default when a document has one object. |
| **Navigation** | View cube, top right | View cube, top right, plus a compact nav bar at the bottom centre | **View cube top right, and a compact nav bar bottom centre,** dim until hovered. |
| **Status** | Vertex and triangle count, bottom right, small | Minimal | **A small stats readout bottom right;** messages, progress and cancel bottom left. |
| **History** | Undo only | A timeline along the bottom | **Undo and Redo plus a History panel** (already exists), optional and docked. A timeline is not needed now. |

## 2. Principles

1. **The stage belongs to the model.** No widget may sit inside the central 60% of the viewport unless the user is directly manipulating something there: the gizmo, a cut plane, a brush cursor.
   - Panels dock at the edges. Overlays are small, sit in the corners, and stay dim until hovered.
   - **Focus mode** (`Tab`) hides all chrome except the rail and the active tool panel. `Shift+Tab` hides everything.
2. **Every tool can be seen and has a name.** A tool lives on the rail with an icon and a label, exactly as in Meshmixer. Nothing is reachable only through a menu.
3. **One level deep.** Category, then tool, then settings, all in one panel. No cascading menus anywhere. Breadcrumb back is one click, and so is Esc.
4. **One primary action per panel.** It is accent-filled (Apply, Fill, Split). Everything else is secondary (subtle) or tertiary (text). This is the fix for the "tag cloud".
5. **Plain language first.** Every message states the problem in plain words and gives a next action, with technical terms second (an M4 rule that stays).
6. **Easy for the average user, fast for the expert.** These are layered, not separate modes. See §6.
7. **One colour, one meaning** (an existing rule). The UI accent is reserved for "active and primary". Defect, selection and axis colours are never reused for chrome.

## 3. Layout regions

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ File  Edit  View  Tools  Help                     ⌘K Search commands…   ⚙    │ ← menu bar (thin)
├──────┬──────────────────┬───────────────────────────────────────┬────────────┤
│ ▣    │ TRANSFORM      ✕ │                                ┌───┐  │ BROWSER  ⌃ │
│Select│ Move, rotate or  │                                │cube│ │ ▾ scan     │
│      │ scale the object │                                └───┘  │   ○ 4 holes│
│ ✥    │──────────────────│                                       │   ○ 2 loose│
│Trans-│ ○ Move  ○ Rotate │              ( the model )            │ ▸ head     │
│form  │ ○ Scale          │                                       │            │
│      │ Position   mm    │                 ◎ gizmo               │            │
│ ✂    │ [ 12.0][ 0 ][ 0 ]│                                       │            │
│Split │ Rotation   °     │                                       │            │
│      │ [  0  ][ 0 ][ 0 ]│                                       │            │
│ ⧉    │ Snap  [1 mm ▾] ☑ │                                       │            │
│Com-  │──────────────────│                                       │            │
│bine  │ Drop to plate    │                                       │            │
│      │ Reset            │                                       │            │
│ ⬢    │                  │                                       │            │
│Solid │ [   Done   ]     │                                       │            │
│ ⌇    │                  │                                       │            │
│Sculpt│                  │                                       │            │
│ ⚙    │                  │   ⟲ ✋ 🔍 ⛶ ◧ ▦          (nav, dim)  │            │
│Repair│                  │                                       │            │
├──────┴──────────────────┴───────────────────────────────────────┴────────────┤
│ ↶ ↷   Moved scan by 12.0, 0, 0 mm                      12,408 tris · 10 mm³ │ ← status bar
└──────────────────────────────────────────────────────────────────────────────┘
```

| Region | Content | Default | Rules |
|---|---|---|---|
| **Rail** (left, 64 pt) | Tool categories: an icon plus a short label. Groups are separated by thin rules: *Arrange* (Select, Transform), *Shape* (Split, Combine, Make solid, Sculpt), *Add* (Parts, Connectors), *Fix* (Repair, Inspect), *Output* (Export, Print prep). | Always visible | The active tool has a filled tile and an accent bar. The tooltip gives the name, shortcut and one line of help. Plugins add entries (§5). |
| **Tool panel** (left, about 280 pt, resizable) | The active tool only: header (icon, name, one-line help, close), sections, then the primary action. If the category has several tools, a tool list appears first. Choosing one replaces the list with its settings, and a ← link returns. | Opens when a tool is chosen, collapses when it's done | Never more than one level. Settings change live (preview) wherever that is cheap. Apply/Done commits as one undo step. Cancel or Esc reverts. |
| **Viewport** | The model, the plate, overlays, and the gizmo or tool handles | — | The centre is protected (principle 1). The view cube sits top right. Nav sits bottom centre, dim. A first-use hint appears top centre, once. |
| **Browser** (right, about 240 pt, collapsible) | Objects tree, defects grouped under each object (counts as badges), and visibility toggles | Collapsed if there is one object and no defects | No buttons inside rows. Selecting a row selects in the viewport, and double-click frames it (M4 behaviour). Context actions live in the right-click menu and the tool panel. |
| **Docked extras** (right or bottom, tabbed with the Browser) | History, Metrics, Log, and later a Script console | Closed | Opened from the View menu or ⌘K. They tab together, never stack into a wall. |
| **Status bar** | Undo/Redo (icons), message plus inline progress and cancel, stats on the right | Always | One line. Warnings are expandable (the M4 import-warnings fix). |
| **Context HUD** | During a drag: a small numeric readout near the cursor ("+12.0 mm X", "35°") | Only while dragging | Disappears on release. Typing a number during a drag sets the value exactly, as in Blender. |

Laptop size:
- **At 1280×720,** the tool panel and Browser can't both be open with a narrow viewport. Opening the Browser auto-collapses the tool panel to its header.
- **At 1024×640,** the rail drops its labels (icons only, with tooltips), and the Browser becomes an overlay drawer.

## 4. Visual style

- **Chrome:**
  - neutral charcoal: panels #1E2024, rail #17191C, separators #2A2D32;
  - text: primary #E6E8EB, secondary #9AA0A8, disabled #5C6168.
  - The chrome is deliberately dimmer than the model.
- **Stage:**
  - a soft vertical gradient (#3A3F46 to #2B2F35);
  - the build plate grid in subtle grey, with a major line every 50 mm.
- **Model:**
  - a light matte material (#C9CCD1), three-point studio lighting plus a soft ambient term;
  - a soft contact shadow on the plate;
  - **4× MSAA.** Wireframe overlay toggles with `W` in View, in 1 px darker lines, as in Meshmixer.
  - Material presets ("Shaders"): Clay (default), Studio, Wireframe, Normals and X-ray. These are Meshmixer's best "inspect" aid.
- **Accent:** one, a calm blue (#3D8BFD), used only for the active tool, the primary button and focused inputs.
- **Type:** the egui default sans at four sizes: Title 16, Section 13 semibold, Body 13, Caption 11. Numbers are tabular.
- **Spacing:** a 4 pt grid. Panel padding 12, section gap 16, row height 28. One corner radius: 6.
- **Controls:**
  - **Buttons** come in three tiers: primary (filled accent), secondary (subtle fill #2A2D32), tertiary (text).
  - **Segmented controls** for modes (Move | Rotate | Scale), never pill clouds.
  - **Numeric fields** have a unit suffix, support drag-to-scrub on the label, and accept arithmetic (`12+3.5`).
- **Icons:** one consistent outline set (Phosphor, MIT). They are tinted with the text colour, and the accent tint is used only when active.
- **Gizmo** (owner feedback):
  - axis colours only;
  - the move arrows have cone heads;
  - the rotate rings have **curved arrow grab handles** at the point nearest the viewer;
  - the scale axes have box heads;
  - **no view ring by default.** Screen-space rotation is Shift plus a drag on any ring;
  - the hovered handle brightens.

## 5. The pluggable shell (what a plugin system must have)

Every built-in tool registers exactly as a plugin would. If a built-in needs a private hook, the contract is wrong. The core contract lives in `mf-desktop::shell`:

| Contribution | Fields | Used by |
|---|---|---|
| **Tool** | `id`, `name` (a message id, §10), `icon` (**required**: a tool without an icon is rejected at registration, so every tool has a visual icon), `category`, `order`, `shortcut`, `help` (one line), `availability(doc, selection) -> Available / Unavailable(reason)`, `panel(ui, ctx)`, `viewport_input(event) -> Consumed / Pass`, `overlays()`, `hides_defects()`, `on_open`, `on_close(commit or cancel)` | Rail, tool panel, viewport input routing, ⌘K |
| **Command** | `id`, `title`, `keywords`, `shortcut`, `enabled(ctx)`, `run(ctx)`. Every operation spec (MF-SPEC) becomes a command automatically. | Menu bar, ⌘K palette, right-click marking menu, keymap, scripting |
| **Panel** | `id`, `title`, `icon`, `default_dock` (left / right / bottom), `min_size`, `ui(ui, ctx)` | Docked extras (History, Metrics, plugin panels) |
| **Overlay** | `id`, `name`, `z_order`, `legend_entry` (colour token plus label), `draw(renderer, ctx)`, a visibility toggle | The viewport and the legend in View → Overlays |
| **Inspector section** | `id`, `order`, `ui(ui, object)` | The object summary card (no tool active) |
| **Context action** | `id`, `applies_to(target)` (object, face set, hole, …), `title`, `run` | The right-click menu on the model and on Browser rows |
| **Settings page** | `id`, `title`, `ui` | Settings (Print fit, Units, Keymap, Appearance) |

**Layout model:**
- Docking uses fixed regions: rail, left tool dock, right dock, bottom dock and viewport corners. A region holds tabbed stacks. Panels are resizable and collapsible.
- The layout is serialized per user (`layout.json`) with *Reset layout*, plus named layouts ("Repair", "Modelling"). These are layouts, not modes; tools stay where they are.
- Implementation: `egui_dock` (MIT) if its release supports egui 0.37; this is checked in step 1 of the build. Otherwise a small in-house dock covering exactly the regions above, which is about 400 lines.
- The keymap is one table of command id to shortcut, user-editable. Conflicts are shown in Settings ▸ Keymap.

**Tool lifecycle contract:**
- Opening a tool closes the previous one. If the previous tool has uncommitted changes, they are committed or cancelled per its policy, and the user is asked only when work would be lost.
- A tool's live preview never enters history. Apply creates exactly one command.
- Esc, in order: cancel the running job → cancel the drag → cancel the tool → clear the selection.

## 6. Average user vs expert (layered, not modes)

| Need | Average user | Expert |
|---|---|---|
| Find a tool | The labelled rail, hover help, the first-use hint | Single-key shortcuts (T, S, B, …), ⌘K command search, recent tools |
| Run it | Sensible defaults, one primary button, live preview | Type values into the HUD during a drag; arithmetic in fields; Enter = apply |
| Repeat | Undo/Redo | **Repeat last command** (Space), apply the same tool to each selected object |
| Context | The "next action" chip on the summary card ("4 holes: Repair") | **Right-click marking menu** on the model: the 8 most relevant actions for what is under the cursor |
| Precision | Snapping on by default, and the snap step scales with zoom | Numeric entry, a snap-step dropdown, Cmd to invert snapping, pivot options |
| Workspace | The default layout | Saved named layouts, focus mode (Tab), a custom keymap, and later scripting and macros |
| Learning | Inline help line, tooltips, plain-language refusals | Shortcuts printed in every tooltip and menu item, so experts grow out of the average path |

## 7. What changes in the current app

1. The right "Properties & Tools" panel splits apart:
   - tools move to the rail and the tool panel;
   - the object summary moves to the inspector card (no tool active);
   - defect actions move to the Repair tool and the right-click menu.
2. The left "Objects & Defects" panel becomes the right-docked Browser, without buttons in rows. The legend moves to View → Overlays, as a compact popover.
3. The bottom nav pill becomes the dim nav bar plus a view cube.
4. Repair (fill holes, remove loose pieces, compact, mark intentional) becomes one tool with sections: Holes, Cracks, Loose pieces, Tangled edges. Each section has a list and one primary action.
5. Transform loses its pill-row look and becomes a segmented mode control with numeric fields, a snap dropdown, and Drop to plate / Reset as secondary actions. Done is the primary action.
6. The gizmo gets ring arrow handles and loses the view ring.
7. New: the ⌘K palette, the right-click marking menu, the drag HUD, focus mode, material presets, the view cube, 4× MSAA and the soft plate shadow.

## 8. Build order (each step ends with real app frames at 1440×900 and 1280×720 for owner review)

1. **Shell contracts and visual system:**
   - `shell` registries (§5), theme tokens and icons;
   - Check that `egui_dock` and `egui-phosphor` work with egui 0.37.
   - Existing tools re-registered through the contracts, with no visible change yet.
2. **Layout:** the rail, the tool panel, the Browser, the status bar and the nav bar. Transform and Repair are moved into it. **Owner checkpoint (frames).**
3. **Stage quality:** MSAA, materials and presets, the soft shadow, the view cube and the wireframe toggle. **Owner checkpoint.**
4. **Expert layer:** ⌘K, the marking menu, the drag HUD, repeat-last, focus mode and the keymap settings.
5. **New tools plug in through the contracts:** Split, Combine, Make solid, Sculpt and Parts. No tool-specific shell code.

Acceptance criteria for each step:
- No widget inside the central viewport zone except during manipulation.
- One primary button per panel.
- No cascading menus.
- Every tool is reachable from the rail, by its shortcut and from ⌘K.
- Every existing GUI test passes, using the same accessible labels.

## 10. Internationalisation (not in v1, but designed in from the start)

v1 ships in English only. Retrofitting translations later is expensive if strings are scattered literals, so the rules start now:
- **Message ids, not literals.** Every user-facing string goes through `t!("transform.moved", object = name, dx = …)`, backed by an English catalogue in one file per area. Contracts in §5 carry message ids (`name`, `help`, `title`), never display text. Plugins ship their own catalogue.
- **Whole sentences with placeholders.** Never concatenate fragments ("Moved " + name + " by …"), because word order differs between languages. Plurals use catalogue plural rules ("1 hole" / "4 holes"), not `if n == 1`.
- **Format by locale.** Numbers, decimal separators and units go through one formatter. Units stay explicit (mm, °).
- **The layout must tolerate longer text.** Labels may grow by about 40% (German), so no fixed-width text buttons. The rail label truncates with a tooltip, and the tool panel wraps. Right-to-left support is noted but deferred; the layout must not assume left-to-right in ways that are hard to mirror.
- **Catalogue format:** Fluent (`fluent-rs`, Apache-2.0/MIT) is the likely target, chosen when the first translation lands. Until then, a static English table behind the same `t!` macro.
- **Test:** a debug "pseudo-locale" (strings wrapped in ⟦…⟧ and lengthened by 40%) shows any untranslated literal, and any layout that breaks with longer text, in GUI frames.

## 11. Multi-material and multi-colour models: the model stands out, the accoutrement steps back

3MF carries per-object and per-triangle colours and materials (the Materials and Properties extension, plus slicer paint data). Multi-material, multi-head and multi-colour printers make this common. Two consequences follow.

**A. Display: the model's own colours are the hero, and every UI element is subordinate.**
- **View modes** (the "Shaders" preset menu):
  - **Material**, the default when the file carries colours: shows the model's own colours and materials faithfully.
  - **Clay**, the default otherwise: one neutral matte, best for reading shape and defects.
  - **Material IDs:** each extruder or material gets a distinct, labelled swatch, to check assignments before printing.
  - **Wireframe** and **X-ray**.
- **Saturation budget:**
  - The chrome, the plate, the grid and the shadow are desaturated neutral greys, always.
  - The only saturated colours on screen are the model's own and the small set of overlay meanings (selection, defects, axes).
- **Overlays stay readable on any model colour.** Overlays never repaint large areas of a coloured model. They use:
  - **Halos:** lines (hole outlines, selection boundaries, cracks) are drawn with a 1 px dark outline plus a light core, so they read on red, black or white plastic alike;
  - **Outline plus a light tint**, not a fill, on Material view;
  - **Patterns where colour alone could clash:** the existing hatch for frozen faces, and a dashed outline for intentional openings;
  - **Motion for emphasis:** the selection pulse.
- **Defect inspection stays honest.** The Repair and Inspect tools *offer* a one-click switch to Clay ("Show the model in clay to see defects more clearly"), and remember the choice. They never switch silently.
- **The legend shows the model's materials** (swatch, name, extruder) next to the overlay meanings, in two clearly titled groups: "Model" and "Overlays".

**B. Data: colours and materials survive every operation.** This is a geometry and I/O requirement, not just display.
- Faces carry a material or colour attribute (object default plus per-triangle override), read from and written to 3MF.
- Provenance already maps every output face to its sources, so:
  - booleans, split, sculpt and Make solid inherit attributes from the source face;
  - hole fills inherit from the boundary neighbours, choosing the majority, with ties broken deterministically;
  - joins and connectors keep each part's own material, so a kitbashed part can print in another colour.
- Replay hashes include the attributes.
- Tracked as an issue for P1 (before Mixing and Connectors). It is required for any 3MF round trip not to lose colour.

## 9. Open questions for the owner (answered 2026-11-11: as proposed)

1. **Light or dark chrome?** Meshmixer was light grey; the plan proposes dark charcoal so the model is the brightest thing. A light theme can be added later as an Appearance setting.
2. **Browser side.** The plan docks it on the right, as with Fusion's floating panel, so the left is tools only. Would you rather have Fusion's Browser on the left, below the tool panel?
3. **Rail labels.** Labels are always shown at 1440 and above; at laptop sizes the rail shows icons only. OK?
