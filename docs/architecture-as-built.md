# MeshFixer architecture: as built

This describes the code as it stands on `main` at `3d4f974`, after PR #10. `docs/architecture.md` is the original vision document; this file is what exists today.

Each diagram marks where the consolidation plan is taking that area:
- solid lines and boxes are real today;
- dashed ones are planned, and the ADR named on them is the decision behind them.

ADRs live in `docs/adr/`. The plan is in `.superpowers/sdd/2026-11-10-phase1-modelling-tools/consolidation-plan.md`.

## 1. Crates and dependencies

These are the 17 workspace crates. Each arrow means "depends on". The leaf crates sit at the bottom.

```mermaid
flowchart TB
  subgraph Frontends
    desktop["mf-desktop<br/>eframe/egui app"]
    cli["mf-cli<br/>meshfixer CLI"]
  end

  subgraph Core["Command core"]
    command["mf-command<br/>Session, revisions, log, replay"]
    spec["mf-spec<br/>YAML op specs, ParamUnit"]
    selection["mf-selection<br/>selections across revisions"]
    document["mf-document<br/>immutable Document, objects"]
  end

  subgraph Engines
    operations["mf-operations<br/>one Operation per spec"]
    parts["mf-parts<br/>Generators: shapes, connectors, fit"]
    boolean["mf-boolean<br/>kernels, plane cut, window"]
    render["mf-render<br/>wgpu renderer, gizmos"]
    io["mf-io<br/>STL / OBJ / 3MF"]
  end

  subgraph Foundation
    geometry["mf-geometry<br/>Mesh, topology, BVH, exact"]
    units["mf-units<br/>Unit, Length, parse, format"]
    runtime["mf-runtime<br/>job pool, cancel, progress"]
    metrics["mf-metrics<br/>alloc and timing"]
    manifold["mf-manifold-sys<br/>C++ Manifold 3.5.3 FFI"]
  end

  fixtures["mf-fixtures<br/>procedural test meshes"]

  desktop --> command & operations & parts & render & io & spec & selection & units & metrics & geometry & runtime
  desktop -. "C23 removes (ADR-0008)" .-> boolean
  cli --> command & operations & parts & render & io & spec & selection & units & metrics & fixtures

  command --> operations & document & spec & selection & io & units & geometry & runtime
  spec --> io & units
  selection --> document & geometry
  document --> geometry & io

  operations --> boolean & parts & document & spec & units & geometry & runtime
  parts --> boolean & spec & io & geometry & runtime
  boolean --> geometry & manifold & runtime
  render --> geometry & units & metrics & runtime
  io --> geometry & units & runtime

  geometry --> runtime
  fixtures --> geometry & runtime
```

**Rules the dependency graph enforces:**
- **Frontends are thin.** Desktop and CLI change a document only through `mf-command::Session` (MF-CMD-001).
- **No GUI types below the frontends.** `mf-render` knows wgpu but not egui.
- **`mf-units` and `mf-runtime` are leaves**, so any crate can use them.
- **Known violation:** `mf-desktop` depends on `mf-boolean`, because some previews re-implement op geometry. ADR-0008 removes that dependency (task C23), and then the compiler enforces the rule.

## 2. The command path: how anything changes a document

Every change, whether a click, a CLI subcommand or a replayed log, goes through the same path.

```mermaid
sequenceDiagram
  autonumber
  participant UI as Tool panel (desktop) / CLI
  participant Q as Command queue (desktop worker.rs)
  participant S as mf-command::Session
  participant Spec as mf-spec (YAML)
  participant Op as mf-operations::Operation
  participant RT as mf-runtime pool
  participant Doc as mf-document

  UI->>Q: Invocation {op, target, params}
  Note over Q: merges two queued commands only if the spec declares<br/>coalesce: absolute (C04 / ADR-0009)
  Q->>S: prepare(inv)
  S->>Spec: validate params, ParamUnit → document units, upgrade_params
  S->>RT: run on a worker (never the UI thread)
  RT->>Op: execute(mesh, target, params, DocContext, RunContext)
  Op-->>RT: OpOutput, or OpError (Refused/Cancelled/Failed)
  RT-->>S: result
  S->>Doc: with_*() → new immutable Document
  S->>S: new RevisionNode, append to command log (history_label)
  S-->>UI: Completed {revision, report} or refusal
```

- **Units have one boundary.** Ops see document units only. Of the ops, only the ones that place mm-internal generators read `DocContext` (C07, C10, C12). C13 moves the desktop-side conversion into one typed `Intent` builder.
- **Replay is Exact.** `replay(dir)` re-runs the command log, and `upgrade_params` maps old param versions onto new ones.
- **Undo and redo** move between `RevisionNode`s. Documents are immutable values, so a revision is a pointer.

## 3. Operations: one spec, one implementation

```mermaid
classDiagram
  class OperationSpec {
    id
    version
    parameters with ParamUnit
    coalesce absolute or never
    history_label
    refusals - C14 ADR-0010
  }
  class Operation {
    <<trait>>
    id() version()
    upgrade_params(recorded, params)
    execute(input, target, params, doc, ctx) OpOutput
    execute_many(inputs, ...)
  }
  OperationSpec "1" -- "1" Operation : same id + version (MF-SPEC-002)

  Operation <|.. SplitPlane : mesh.split_plane / split_box / split_region
  Operation <|.. Combine : mesh.combine / mesh.boolean
  Operation <|.. Hollow : mesh.hollow
  Operation <|.. MakeSolid : mesh.make_solid
  Operation <|.. FillHoles : mesh.fill_holes
  Operation <|.. SculptStroke : mesh.sculpt_stroke
  Operation <|.. PlaceConnectors : split.place_connectors / connector.join
  Operation <|.. AddShape : object.add_shape / add_part / set_shape_params
  Operation <|.. ObjectOps : transform / align / drop_to_plate / arrange / attach / rename / delete
  Operation <|.. Inspect : mesh.inspect / intersections
  Operation <|.. Housekeeping : compact / remove_small_components / delete_faces
  Operation <|.. DocumentUnits : document.reinterpret_units / scale_units
```

The 30 specs are in `specs/operations/*.yaml`. The CLI builds its subcommands from them, and the desktop builds its panels and History labels from them.

## 4. Engines

### 4a. Boolean and cut (`mf-boolean`)

```mermaid
flowchart LR
  subgraph mf-boolean
    K{{"trait Kernel<br/>boolean(a, b, op, ctx)"}}
    MR["ManifoldRobust<br/>(manifold-rust)"]
    MC["ManifoldCpp<br/>(C++ 3.5.3 via mf-manifold-sys)"]
    W["window.rs / clip.rs<br/>local D5 window, refuses window_degenerate"]
    PC["plane_cut.rs<br/>custom fast plane cut<br/>topological cut points"]
    SEC["section.rs / weld.rs / cleanup.rs"]
    V["validate.rs<br/>closed, manifold, no self-intersections"]
  end
  K --> MR & MC
  Combine["mesh.combine (Solid/Hole roles)"] --> W --> K
  Split["mesh.split_*"] --> PC --> SEC
  K --> V
  PC --> V
```

- **Combine uses Manifold** (ADR-0002).
- **Split uses the custom plane cut.** It does 30M triangles in 1.40 s with about 312 MB working memory. Manifold took 33 s and 17 GB and was rejected. See `docs/performance.md`.

### 4b. Generators: shapes and connectors (`mf-parts`, ADR-0005)

```mermaid
classDiagram
  class Generator {
    <<trait>>
    id() name() version()
    spec() OperationSpec
    generate(params, GenContext) ShapePart[]
    role() is_shape() placement_hint()
    checks() GenCheck[]
  }
  class ShapePart {
    name
    role Solid or Hole
    attach_to A, B or Neither
    mesh in mm
  }
  Generator ..> ShapePart
  Generator <|.. BuiltinShapes : box, cylinder, sphere, text...
  Generator <|.. Joint : dowel, square peg, key
  Generator <|.. DovetailSlide
  Generator <|.. Hinge : print-in-place hinge
  Generator <|.. LeafHinge
  Generator <|.. Clasp
  Generator <|.. Flap
  Generator <|.. Thread
  class FitProfile {
    machine tolerance
    fit-class clearance
  }
  Generator ..> FitProfile : clearances
  class Library {
    built-in parts
    saved user parts with manifest unit
  }
  Library ..> Generator
```

Generators work in mm. The op that places them scales the result once, using `DocContext` (D16, C10, C12).

### 4c. Other engines in `mf-operations`

| Module | Engine |
|---|---|
| `hollow.rs` | sparse-band SDF inner offset, plus marching tetrahedra; the opening follows the cavity's cross-section |
| `make_solid.rs` | generalized winding number / SDF, plus dual contouring |
| `fill_holes/` | triangulate → refine → fair |
| `sculpt/` | brush, falloff, a spatial index, local remesh |
| `split/` | plane/box/region split; `joints.rs` (connector placement and fit), `pegs.rs`, `dovetail.rs` |
| `blend.rs` | seam blend within a local window |

### 4d. Geometry foundation (`mf-geometry`)

`Mesh` uses stable-slot chunked storage (ADR-0001). Alongside it the crate provides:
- topology;
- a BVH;
- intersections;
- validation;
- affine transforms;
- `exact`, the only place "on plane", "same point" and "inside" are decided (ADR-0011, C20);
- hashing for revision identity.

## 5. Desktop application (`mf-desktop`)

```mermaid
flowchart TB
  subgraph UI["UI thread (egui frame)"]
    App["MeshFixerApp (app.rs, ~7.3k lines)"]
    Shell["Shell: tool rail, panels, Esc, undo notice,<br/>refusal routing, shown transform/render<br/>shell::Tool + session.rs"]
    Tools["Tools: Transform, Align, Split, Connect, Connectors,<br/>Parts, Hinge, Dovetail, Combine, Hollow, Make solid,<br/>Sculpt, Repair, Inspect"]
    Viewport["viewport.rs: camera, picking, gizmos"]
    Panels["panels / shape_panel / tool_panels<br/>widgets (length_field: C13)"]
    Import["import_dialog: fits-the-plate units"]
  end

  subgraph BG["Background (never the UI thread)"]
    Worker["worker.rs: command queue → Session"]
    Previews["analysis.rs: LatestWorker / AnalysisCache<br/>(previews, topology, defects)"]
    Thumbs["part_thumbs"]
  end

  GPU["gpu.rs → mf-render (wgpu)"]
  Session[("mf-command::Session")]

  App --> Shell --> Tools
  Tools --> Panels
  Tools --> Viewport
  App --> Import
  Tools -- Invocation --> Worker --> Session
  Tools -- preview request --> Previews
  Previews -. "today: some previews re-implement op geometry<br/>target: Plan from the op (ADR-0008, C22/C23)" .-> Session
  Viewport --> GPU
  Session -- Completed / refusal --> App
```

**Where the one-offs still are, and the ADR that removes each one:**

| Today | Target | ADR / task |
|---|---|---|
| Done for Transform, Align, Combine, Make solid, Parts, Connectors and Repair (C21): each is a `ToolSession` holding its own state; the shell (`session.rs`) derives the shown transform and render, the Esc order, undo notice and refusal routing. Split, Connect, Hollow and Sculpt are thin adapters around their old app fields; the hinge swing is read by the shell. `tests/session_contract.rs` checks every tool; `tests/lint_tools.rs` keeps tool modules out of `app.rs`. | Split and Connect sessions (C23), Hollow and Sculpt (C24), hinge and dovetail generator facts (C26) | ADR-0007 / C21, C23, C24, C26 |
| Previews re-run op geometry in the desktop, sometimes on the UI thread | The op exposes `plan()`, and preview and Apply share it | ADR-0008 / C22, C23 |
| `LatestWorker`, ad-hoc threads and `pending_*` fields | Three lanes (command queue, `PreviewSlot`, pick lane); coalescing declared per spec | ADR-0009 / C04 done, C19 |
| Refusals land in the generic error bar | A `RefusalCard` in the panel of the tool that issued the command | ADR-0010 / C14, C16 |
| Per-tool ray picking and region growth | `PickService` and `SelectionKind` | ADR-0012 / C24 |
| Tolerance literals scattered through the code | `mf_geometry::exact`, plus `docs/tolerances.allow` | ADR-0011 / C20 done |

## 6. Units, scale and precision (ADR-0006)

```mermaid
flowchart LR
  Field["length_field<br/>'5 cm + 2 mm'"] -->|"Length"| Intent["Intent builder<br/>(C13)"]
  Intent -->|"'50.2 mm'"| Prepare["Session::prepare<br/>ParamUnit → doc units"]
  Prepare --> Op["Operation<br/>(doc units + DocContext)"]
  Op --> Gen["Generator (mm)"] -->|"× DocContext"| Op
  Op --> Report["report"] --> Fmt["mf_units::format_length<br/>History / status / refusals"]
  Import["STL/OBJ import (unit undeclared)"] --> Dialog["fits-the-plate choice"] --> Prepare
  Op --> Export["STL/OBJ export → mm"]
  Snap["nice_step(world_per_pt, 8 pt)"] --> Gizmo["gizmos / grid / sculpt brush (pt)"]
```

## 7. Tests as architecture

| Tier | What | When |
|---|---|---|
| Targeted | crate and test-file runs | every change |
| `scripts/core-workflows.sh` | 59 end-to-end GUI flows, including the owner's bunny repros | before every demo and every merge |
| `scripts/check.sh` | fmt, clippy, swallow lint, epsilon lint, licences, about 1,250 tests | once per wave, on the merged branch |
| Heavy (`check.sh --full`) | 5M and 30M timing and memory, the messy corpus, idle memory | on demand only |
| Benchmarks | `scripts/bench-*.sh` → `docs/perf/history.jsonl` | on demand; recorded |

**Rule-enforcing tests**, which fail on any new violation rather than on one example:
- every spec has a `history_label`;
- every spec declares `coalesce`;
- every length param declares a unit;
- the epsilon lint;
- the swallowed-defaults lint;
- `snap_step_is_8pt_at_any_zoom`.

Still to come:
- the unit-conversion lint (C13);
- the refusal registry tests (C14);
- the session-contract harness (C21);
- the scale matrix (C29).
