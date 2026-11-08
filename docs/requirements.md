Below is a requirements baseline I’d use for **MeshFixer**. I’ve written it so it can serve as the starting point for a PRD, engineering backlog, or architecture discussion rather than just a feature wishlist.

# MeshFixer — Product & System Requirements

## 1. Product objective

MeshFixer shall provide a fast, cross-platform desktop application for inspecting, repairing, simplifying, remeshing, modifying, and preparing 3D models for downstream use.

The product should sit between a lightweight mesh repair utility and a full CAD/modeling application. It should make common mesh operations significantly easier than traditional CAD tooling while retaining enough precision, reversibility, automation, and extensibility for serious workflows.

The core product principle should be:

> **Select an object, surface, region, or defect; describe what you want changed; preview the result; apply it non-destructively; and remain able to revise the operation later.**

MeshFixer should support both direct human manipulation and programmatic/LLM-driven workflows.

---

# 2. Design principles

MeshFixer shall be designed around the following principles.

| Principle | Requirement |
|---|---|
| Cross-platform | Windows, macOS, and Linux should be first-class supported platforms. |
| Performance-first | Large models must remain interactable without exhausting system memory or GPU resources. |
| Non-destructive | Operations should be represented as editable history wherever practical. |
| Local-first | Core modeling operations should function without requiring cloud services. |
| Human + AI | Anything reasonably invokable by a user should eventually be invokable through CLI/MCP/API. |
| Progressive complexity | Simple repair actions should remain simple while advanced controls remain available. |
| Preview before commit | Expensive or destructive operations should provide previews wherever possible. |
| Interoperable | MeshFixer should use common 3D formats rather than trapping models in a proprietary ecosystem. |
| Recoverable | Crashes, failed operations, and bad edits should not destroy the user's model or session. |
| Deterministic | The same operation with the same settings and geometry should normally produce the same result. |

---

# 3. Supported file formats

## 3.1 Import

The application shall support:

**P0 — required**
- STL, binary and ASCII.
- OBJ.
- 3MF.

**P1 — strongly desired**
- STEP/STP.
- PLY.
- GLTF/GLB.

**P2**
- OFF.
- FBX.
- AMF.
- IGES where technically practical.

STEP support requires special consideration because STEP represents BREP/NURBS geometry rather than simply a triangle mesh.

When importing STEP, MeshFixer shall either:

1. preserve the original analytic/BREP representation where the geometry engine supports it, or
2. tessellate the model using user-configurable tolerances while retaining links to the originating bodies/faces where possible.

The system shall not silently imply that a tessellated STEP file remains exact CAD geometry.

## 3.2 Export

Users shall be able to export at minimum:

- STL.
- OBJ.
- 3MF.

Where supported by the underlying geometry representation:

- STEP.
- GLTF/GLB.
- PLY.

Export shall allow configuration of relevant properties including:

- units;
- resolution/tolerance;
- binary vs ASCII where applicable;
- object selection;
- object merging;
- coordinate orientation;
- normals;
- colors/materials when the format supports them.

---

# 4. Scene and object model

MeshFixer shall support projects containing multiple objects.

Each object shall have:

- unique ID;
- human-readable name;
- geometry;
- transforms;
- visibility state;
- lock state;
- metadata;
- tags;
- operation history;
- selection state;
- calculated geometry statistics.

The application shall support:

- object selection;
- face selection;
- edge selection;
- vertex selection;
- connected-region selection;
- surface/feature selection.

Users shall be able to hide, isolate, lock, rename, duplicate, and delete objects.

---

# 5. Inspection and diagnostics

MeshFixer shall automatically detect common model problems.

At minimum, diagnostics should include:

- holes/open boundaries;
- non-manifold edges;
- non-manifold vertices;
- self-intersections;
- duplicate vertices;
- duplicate faces;
- degenerate triangles;
- zero-area faces;
- inverted normals;
- inconsistent normals;
- disconnected components;
- extremely thin geometry;
- intersecting shells;
- isolated fragments;
- invalid topology.

The interface shall provide a model health summary such as:

> 4 holes  
> 27 non-manifold edges  
> 12 intersecting faces  
> 3 disconnected fragments

Users shall be able to select detected defects directly from the diagnostics panel.

Diagnostics should run incrementally rather than requiring complete rescans after every edit whenever technically practical.

---

# 6. Automatic repair

MeshFixer shall provide a **Repair Model** workflow capable of automatically correcting common problems.

Automatic repair should support:

- merge duplicate vertices;
- remove degenerate faces;
- remove duplicate faces;
- repair normals;
- close holes;
- resolve non-manifold topology where possible;
- remove isolated fragments;
- reconnect near-coincident geometry;
- repair small self-intersections where practical.

Repair must not operate as an opaque one-click black box.

Before execution, the system should show:

- detected problems;
- proposed actions;
- likely impact.

After execution, it should report:

- repairs performed;
- remaining unresolved defects;
- geometry changes.

Each repair should become an operation in history.

---

# 7. Hole repair

Users shall be able to select a boundary or detected hole and invoke **Fill Hole**.

The operation should offer methods such as:

- flat/simple fill;
- curvature-aware fill;
- smooth patch;
- minimal-surface patch.

Users should be able to preview the repair before applying it.

For large holes, MeshFixer should attempt to preserve surrounding curvature.

---

# 8. Remeshing

Remeshing is a core requirement.

MeshFixer shall support:

- uniform remeshing;
- adaptive remeshing;
- target edge length;
- target triangle count;
- density-based remeshing;
- local remeshing of selected surfaces.

Users should be able to constrain remeshing to:

- entire object;
- selected region;
- detected feature;
- tagged surface.

Important geometric features and sharp edges should optionally be preserved.

---

# 9. Mesh simplification

MeshFixer shall support reduction of mesh complexity while retaining shape.

Users shall be able to simplify based on:

- target triangle count;
- percentage reduction;
- geometric tolerance;
- maximum deviation.

The system should support preservation of:

- boundaries;
- sharp features;
- normals;
- topology where requested.

Simplification should provide a live or rapidly generated preview where practical.

The UI should show before/after values such as:

> Triangles: 4,200,000 → 420,000  
> Maximum deviation: 0.08 mm

---

# 10. Smoothing

Users shall be able to smooth:

- entire objects;
- selected surfaces;
- selected regions.

The operation shall expose at minimum:

- smoothing strength;
- iterations;
- boundary preservation;
- feature preservation.

Where appropriate, smoothing should attempt to avoid shrinking the object.

This operation must be callable through the automation interface, enabling commands such as:

> Smooth this selected surface but preserve the sharp edges.

---

# 11. Surface and feature recognition

Feature recognition is a major product requirement.

MeshFixer should identify geometric patterns including:

- planar surfaces;
- cylindrical surfaces;
- spherical surfaces;
- cones;
- holes;
- slots;
- pockets;
- bosses;
- fillets;
- chamfers;
- repeated geometry;
- symmetry;
- circular patterns;
- linear patterns.

Recognized features shall become selectable entities.

For example:

> Cylinder — radius 5.0 mm  
> Plane — 1820 faces  
> Hole — diameter 8 mm  
> Repeating pattern — 12 instances

Feature detection should operate with configurable tolerance because imported meshes will rarely contain mathematically exact surfaces.

---

# 12. Pattern recognition

Pattern recognition shall identify repeated or approximately repeated geometry.

The system should detect:

- repeated holes;
- radial patterns;
- linear patterns;
- repeated ribs;
- repeated slots;
- repeated surface features.

MeshFixer should expose:

- number of repetitions;
- spacing;
- orientation;
- pattern center/axis where applicable.

Future operations should allow users to modify an entire recognized pattern.

For example:

> Increase all 12 holes in this pattern from 4 mm to 5 mm.

---

# 13. Chamfers and fillets

MeshFixer shall eventually support geometric edge modification.

Users should be able to select an edge or edge loop and apply:

- chamfer;
- rounded edge / fillet.

Operations should expose:

- radius or distance;
- number of segments where mesh-based;
- feature preservation;
- affected edge selection.

Preview must be provided.

These operations should also be callable by automation.

---

# 14. Shell / hull operations

MeshFixer should support:

- convex hull creation;
- shell creation;
- offset surface;
- object thickening where possible.

Potential future support should include more sophisticated wrapping/shrink-wrap operations.

---

# 15. Measurement tools

Users shall be able to measure:

- point-to-point distance;
- edge length;
- surface area;
- angle;
- radius;
- diameter;
- bounding dimensions;
- object volume;
- wall thickness.

Measurements should support snapping to detected geometric features.

---

# 16. Units and precision

MeshFixer shall support at minimum:

- millimeters;
- centimeters;
- meters;
- inches.

The system should detect probable unit mismatches during import.

Internal geometry calculations should use sufficient floating-point precision for engineering-scale models.

Unit conversion shall not unnecessarily modify model geometry.

---

# 17. Transform operations

Users shall be able to:

- translate;
- rotate;
- scale;
- mirror;
- align;
- center;
- orient;
- snap.

Transforms should support numeric entry.

Users should also be able to align models to:

- world axes;
- selected faces;
- detected planes;
- other objects.

---

# 18. Selection system

Selection is a foundational component of MeshFixer.

Users shall be able to select:

- object;
- face;
- edge;
- vertex;
- connected region;
- surface;
- feature.

Selection tools should include:

- click;
- box select;
- lasso;
- paint selection;
- grow;
- shrink;
- invert;
- select connected;
- select similar.

Selections should be persistable as named groups or tags.

Example:

> Exhaust Port Surface  
> Outer Shell  
> Mounting Holes

These named groups should be addressable through automation and LLM tooling.

---

# 19. Tags and semantic geometry

MeshFixer shall allow geometry to carry semantic tags.

Tags should be assignable to:

- objects;
- surfaces;
- faces;
- features;
- selection groups.

Example tags:

- `outer-shell`
- `mounting-hole`
- `do-not-modify`
- `smooth-area`
- `critical-dimension`

Tags shall be available through CLI/MCP/API operations.

This becomes one of the principal bridges between human interaction and AI automation.

---

# 20. Operation history

MeshFixer shall use an editable operation history similar in principle to a CAD timeline.

Example:

```text
Import
  ↓
Repair Mesh
  ↓
Remove Fragments
  ↓
Remesh
  ↓
Smooth Surface #3
  ↓
Chamfer Edges
  ↓
Simplify
  ↓
Export
```

Users shall be able to:

- undo;
- redo;
- inspect previous operations;
- disable an operation;
- modify operation parameters;
- delete an operation;
- insert a new operation earlier in the sequence where feasible.

Changing an earlier operation should cause subsequent dependent operations to recompute.

The architecture should distinguish between:

- reversible operations;
- recomputable operations;
- destructive operations.

Destructive operations should generate checkpoints.

---

# 21. Version/checkpoint system

In addition to undo/redo, users should be able to create named checkpoints.

Example:

> Original Import  
> Repaired  
> High Resolution  
> Print Version

Autosave should periodically capture recoverable project state.

After an application crash, MeshFixer should offer session recovery.

---

# 22. LLM interaction

MeshFixer shall be designed so an LLM can inspect and manipulate a model without relying on mouse-coordinate automation.

The LLM interface should expose semantic operations such as:

```text
list_objects()
inspect_model()
find_defects()
find_surfaces()
find_features()
select_surface()
select_feature()
smooth_selection()
repair_holes()
simplify()
remesh()
add_chamfer()
export()
```

Example interaction:

> User selects a surface.

> User: "Smooth this area but leave the rim alone."

The LLM should be able to determine:

- selected entity;
- neighboring geometry;
- applicable operation;
- safe parameters.

It should then invoke the corresponding modeling command.

The LLM itself should not directly rewrite mesh vertex arrays unless explicitly operating through a low-level geometry API.

---

# 23. MCP support

MeshFixer should expose an MCP server enabling AI systems to inspect and manipulate projects.

Potential MCP resources:

```text
meshfixer://project
meshfixer://objects
meshfixer://objects/{id}
meshfixer://objects/{id}/surfaces
meshfixer://objects/{id}/features
meshfixer://selection
meshfixer://history
meshfixer://diagnostics
```

Potential MCP tools:

```text
inspect_model
inspect_selection
repair_model
repair_hole
smooth_surface
simplify_mesh
remesh
detect_patterns
apply_chamfer
apply_fillet
export_model
```

Every mutation should return:

- affected object;
- operation ID;
- parameters;
- warnings;
- before/after geometry metrics.

---

# 24. CLI

MeshFixer shall provide a command-line interface suitable for scripting and CI workflows.

Example:

```bash
meshfixer inspect part.stl

meshfixer repair part.stl \
  --output repaired.stl

meshfixer simplify part.stl \
  --target-faces 250000

meshfixer convert model.obj \
  --output model.3mf
```

Complex operation sequences should eventually support a script/config format.

Example:

```yaml
input: part.stl

operations:
  - repair
  - remove_islands:
      min_volume: 1mm3
  - remesh:
      edge_length: 0.2mm
  - simplify:
      target_faces: 500000

output: repaired.3mf
```

---

# 25. Automation architecture

The graphical interface, CLI, MCP server, and eventually scripting API should invoke the **same underlying command layer**.

The architecture should look conceptually like:

```text
             ┌──────────── GUI
             │
User ────────┼──────────── CLI
             │
             ├──────────── MCP / LLM
             │
             └──────────── Script API
                         │
                         ▼
                MeshFixer Command API
                         │
                         ▼
                  Geometry Engine
```

This is an important architectural requirement.

Business logic should not exist exclusively inside UI event handlers.

---

# 26. Performance requirements

Performance shall be treated as a first-class design constraint.

MeshFixer should avoid assuming the complete highest-resolution model must remain:

- fully expanded;
- fully indexed;
- fully resident in GPU memory;
- fully recalculated after every operation.

## Target interaction performance

For supported hardware, normal camera navigation should target approximately:

- 60 FPS for moderate models;
- at least 30 FPS for large workable models.

UI interaction should remain responsive while expensive geometry processing is occurring.

Long operations shall execute outside the UI thread.

Users should be able to cancel long-running operations.

---

# 27. Large-model architecture

MeshFixer should use several techniques common to professional visualization and CAD tools.

### Level of detail

Lower-resolution representations should be used during navigation when necessary.

Full geometry should be restored when the camera stabilizes or detailed geometry is required.

### Spatial partitioning

Models should use structures such as:

- BVH;
- octrees;
- KD trees;
- spatial hashing.

These should accelerate:

- selection;
- raycasting;
- collision tests;
- neighborhood searches;
- visibility calculations.

### Lazy computation

Expensive properties should only be calculated when needed.

Example:

Do not calculate wall thickness for every surface simply because a model was opened.

### Incremental computation

Changing one region should not force global recalculation where dependency information allows local recomputation.

### Geometry chunking

Very large meshes should be segmented into spatial chunks.

Only required chunks should need to be:

- processed;
- rendered;
- copied;
- uploaded to the GPU.

---

# 28. Memory management

MeshFixer shall aggressively manage memory.

Requirements include:

- avoid unnecessary mesh duplication;
- support shared/immutable geometry buffers where appropriate;
- stream large files where technically feasible;
- release intermediate geometry promptly;
- avoid keeping full snapshots for every undo operation;
- represent history primarily through operations/deltas where practical.

The system should expose memory diagnostics to developers.

Potential future UI:

> Mesh: 2.1 GB  
> Render cache: 480 MB  
> Undo/history: 350 MB  
> GPU buffers: 710 MB

---

# 29. GPU strategy

The GPU should primarily accelerate:

- rendering;
- picking where applicable;
- selected compute-heavy operations where meaningful.

GPU acceleration should be optional.

Core editing functionality must continue working where:

- integrated graphics are used;
- dedicated GPUs are absent;
- GPU drivers lack advanced compute capabilities.

CPU implementations should therefore remain available for essential operations.

---

# 30. Background processing

Long-running jobs should execute asynchronously from the interface.

Examples:

- remeshing;
- self-intersection detection;
- simplification;
- feature detection;
- pattern recognition;
- STEP tessellation.

The interface shall display:

- operation name;
- progress where measurable;
- cancellation control.

Where feasible, the user should remain able to inspect unrelated objects while processing continues.

---

# 31. Cross-platform requirements

MeshFixer shall support:

- Windows x64;
- macOS Apple Silicon;
- macOS Intel only if the dependency/support burden remains reasonable;
- Linux x64.

The architecture should avoid platform-specific implementations unless necessary.

Geometry algorithms, file parsing, command infrastructure, and project formats should be platform-independent.

Platform-specific layers should primarily concern:

- graphics;
- windowing;
- file dialogs;
- OS integration;
- GPU interfaces.

Build and automated testing should run across all supported platforms.

---

# 32. Project file

MeshFixer should have a native project format capable of storing:

- source assets;
- object hierarchy;
- operations;
- selections;
- tags;
- checkpoints;
- user settings;
- measurements;
- diagnostics state.

The project format should be versioned.

Where practical it should use transparent/open representations rather than an opaque proprietary binary format.

---

# 33. Reliability requirements

Geometry operations shall fail safely.

If an operation cannot complete, the previous model state must remain intact.

The application shall guard against:

- malformed files;
- invalid topology;
- extremely large triangle counts;
- out-of-memory conditions;
- GPU allocation failures.

Expensive operations should estimate required resources where feasible.

Example:

> This remesh may require approximately 5 GB additional memory.

---

# 34. Headless execution

Core MeshFixer functionality shall not depend on the graphical interface.

A headless engine should support:

- conversion;
- analysis;
- repair;
- simplification;
- remeshing;
- export.

This enables:

- CLI execution;
- server processing;
- testing;
- MCP;
- future cloud workflows.

---

# 35. Observability

Development builds should expose metrics including:

- triangle count;
- vertex count;
- render time;
- frame time;
- operation execution time;
- CPU memory;
- GPU memory;
- mesh cache size.

Geometry operations should produce structured logs useful for diagnosing failures and performance regressions.

---

# 36. Extensibility

The architecture should allow additional geometry operations to be added without substantially modifying the application core.

An operation should ideally follow an interface conceptually similar to:

```text
Operation
 ├─ metadata
 ├─ parameters
 ├─ validate()
 ├─ preview()
 ├─ execute()
 ├─ serialize()
 └─ dependency information
```

That same operation should then automatically become usable by:

- GUI;
- CLI;
- project history;
- scripting;
- MCP.

That would dramatically reduce duplication across the product.

---

# 37. Security

Because automation can modify and write files, MeshFixer shall distinguish between:

- read operations;
- modeling operations;
- filesystem operations.

External AI agents should not receive unrestricted filesystem access through MeshFixer.

Export paths and destructive operations should respect explicit user/application permissions.

---

# 38. MVP scope

I would deliberately **not** attempt the complete vision in V1.

A credible first release should contain:

**P0**

- STL / OBJ / 3MF import.
- STL / OBJ / 3MF export.
- Fast 3D viewport.
- Multi-object scene.
- Face/edge/object selection.
- Model diagnostics.
- Hole detection.
- Basic hole repair.
- Normal repair.
- Non-manifold detection.
- Duplicate/degenerate cleanup.
- Automatic repair.
- Simplification.
- Remeshing.
- Basic smoothing.
- Measurements.
- Transform operations.
- Undo/redo.
- Operation history foundation.
- Autosave/recovery.
- CLI architecture.
- Cross-platform Windows/macOS/Linux architecture.
- Performance telemetry.

**P1**

- STEP import.
- Advanced surface recognition.
- Named selections.
- Tags.
- Adaptive/local remeshing.
- Surface-specific smoothing.
- Pattern detection.
- MCP server.
- LLM-driven operations.
- Chamfers.
- Fillets.
- Hull/shell operations.
- Editable history parameters.
- Full project files.

**P2**

- STEP/BREP editing.
- Complex feature reconstruction.
- CAD-like parametric edits.
- Advanced pattern modification.
- Plugin system.
- Python or scripting SDK.
- Distributed/cloud geometry processing.

---

# 39. Key architectural decision

There is one requirement I would put above almost everything else:

> **MeshFixer must be built around a headless geometry engine and command model, with the desktop interface acting as a client of that engine.**

If we instead build a traditional GUI application where repair, simplify, remesh, selection, and export logic becomes intertwined with UI code, the LLM/MCP/CLI vision becomes extremely expensive to retrofit.

If we start with:

```text
Geometry Engine
      +
Command/Operation Layer
      +
History/Dependency Graph
```

then:

```text
Desktop UI
CLI
MCP
LLM
Scripts
Tests
```

all become different ways of driving exactly the same system.

That also naturally enables the **Fusion-style history idea** you raised: an LLM operation, manual operation, or CLI operation can all appear in the exact same history and can subsequently be edited, disabled, reordered where possible, or recomputed.

And I think that combination—**mesh repair + feature understanding + non-destructive history + human/LLM co-editing**—is the part that gives MeshFixer a much more interesting identity than simply becoming another MeshLab/Meshmixer-style repair utility.