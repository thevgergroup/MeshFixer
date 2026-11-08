I’d architect MeshFixer as a **desktop-first, cross-platform CAD/mesh application with a native geometry core, a GPU-accelerated viewport, a non-destructive operation graph, and an automation layer that treats humans and LLMs as equal clients of the same command system**.

The central design principle would be: **everything the UI can do, the CLI/MCP layer can also do, and everything modifies the model through the same deterministic operation engine**.

A useful high-level shape is:

```text
┌──────────────────────────────────────────────────────────────┐
│                        MeshFixer App                         │
├──────────────────────────────────────────────────────────────┤
│ UI / Interaction Layer                                      │
│  Viewport | Outliner | Properties | History | Selection     │
│  Tool Panels | Diagnostics | Command Palette                │
├──────────────────────────────────────────────────────────────┤
│ Command / Automation Layer                                  │
│  Commands | CLI | MCP | Scripting | Undo/Redo | Macros      │
├──────────────────────────────────────────────────────────────┤
│ Document + Feature Graph                                    │
│  Objects | Selections | Tags | Features | Dependencies      │
│  Parametric History | Transactions | Checkpoints            │
├──────────────────────────────────────────────────────────────┤
│ Geometry Services                                           │
│  Repair | Remesh | Simplify | Smooth | Boolean | Fillet     │
│  Chamfer | Hull | Pattern Detection | Analysis              │
├──────────────────────────────────────────────────────────────┤
│ Geometry Kernels                                            │
│  Triangle Mesh Kernel         B-Rep / CAD Kernel            │
│  BVH / Spatial Index          Tessellation                  │
├──────────────────────────────────────────────────────────────┤
│ Rendering / Compute                                         │
│  GPU Renderer | Picking | LOD | Compute Jobs | Cache        │
├──────────────────────────────────────────────────────────────┤
│ IO                                                           │
│  STL | OBJ | 3MF | STEP | Native Project | Exporters        │
├──────────────────────────────────────────────────────────────┤
│ Platform Abstraction                                        │
│ Windows                  macOS                  Linux         │
└──────────────────────────────────────────────────────────────┘
```

## 1. Native core, thin application shell

I would **not** build the geometry engine in JavaScript, Python, or inside a UI framework.

The geometry core should be **C++ or Rust**.

My preference would probably be:

**Rust for the application architecture + C++ interoperability for established geometry libraries.**

Something like:

```text
MeshFixer
├── app/
│   ├── desktop UI
│   ├── viewport
│   └── interaction
│
├── core/
│   ├── document
│   ├── feature_graph
│   ├── commands
│   ├── selection
│   └── serialization
│
├── geometry/
│   ├── mesh
│   ├── repair
│   ├── remesh
│   ├── simplify
│   ├── analysis
│   └── patterns
│
├── cad/
│   ├── brep
│   ├── step
│   └── tessellation
│
├── render/
│   ├── gpu
│   ├── picking
│   └── lod
│
├── automation/
│   ├── cli
│   ├── mcp
│   └── scripting
│
└── io/
    ├── stl
    ├── obj
    ├── 3mf
    └── step
```

That separation is important.

You don't want STEP support contaminating your mesh representation, or rendering logic leaking into the repair algorithms.

---

# 2. Two geometry representations

This is one of the biggest architectural decisions.

I would **not try to pretend meshes and STEP solids are the same thing**.

They aren't.

MeshFixer should internally support two first-class geometry types:

```text
MeshObject
    vertices
    edges
    triangles
    normals
    attributes
    topology
    spatial_index

BRepObject
    solids
    shells
    faces
    loops
    edges
    vertices
    surfaces
    curves
```

Then support conversion:

```text
STEP / BRep
     │
     ▼
 Tessellation
     │
     ▼
 Triangle Mesh
```

But importantly:

```text
Mesh → STEP
```

should **not** simply mean "save triangles in STEP."

If we eventually support true mesh-to-CAD reconstruction, that should be an explicit operation:

```text
Detect surfaces
      ↓
Classify geometry
      ↓
Plane / Cylinder / Cone / Sphere / NURBS fitting
      ↓
Detect intersections
      ↓
Construct B-Rep
      ↓
Validate solid
```

That becomes one of MeshFixer's more advanced capabilities.

---

# 3. The document should be a graph, not a pile of triangles

This is where I would borrow heavily from Fusion, CAD systems, Blender Geometry Nodes, and modern DCC systems.

A MeshFixer project should look conceptually like:

```text
Document
 ├── Object A
 │    ├── Import
 │    ├── Repair
 │    ├── Remesh
 │    ├── Smooth
 │    ├── Chamfer
 │    └── Export configuration
 │
 └── Object B
      ├── Import
      ├── Pattern Detection
      └── Simplify
```

Each operation becomes a node.

For example:

```text
ImportedMesh
     ↓
RemoveDegenerateTriangles
     ↓
CloseHoles(max_size=3mm)
     ↓
Remesh(target_edge_length=0.8mm)
     ↓
Smooth(selection=surface_17, iterations=4)
     ↓
Output
```

That gives you the Fusion-style behavior you described.

Change:

```text
Remesh 0.8mm
```

to:

```text
Remesh 0.5mm
```

and downstream operations recalculate.

But critically, **not everything upstream should recompute**.

That leads to the next design decision.

---

# 4. Immutable geometry snapshots + operation graph

I would make geometry effectively immutable.

Instead of:

```cpp
mesh.smooth()
mesh.remesh()
mesh.fix()
```

you do conceptually:

```text
MeshSnapshot 123
      ↓ SmoothOp
MeshSnapshot 124
      ↓ RemeshOp
MeshSnapshot 125
```

Internally, you can optimize this with copy-on-write and shared buffers.

This gives you:

- undo/redo
- branching
- crash recovery
- replay
- deterministic CLI execution
- operation history
- LLM-generated modifications
- caching
- reproducibility

This is particularly valuable for AI.

If an LLM says:

> Smooth this area and reduce the polygon count by 30%.

The application records:

```json
{
  "command": "smooth",
  "target": "selection://surface/17",
  "iterations": 4
}
```

followed by:

```json
{
  "command": "simplify",
  "target": "object://body1",
  "reduction": 0.30,
  "preserve_boundaries": true
}
```

The LLM is **not manipulating vertex buffers**.

It's issuing semantic commands.

That distinction is extremely important.

---

# 5. Commands should be the heart of the application

I would build MeshFixer around a command bus.

Something like:

```text
Command
 ├── name
 ├── arguments
 ├── selection
 ├── validation
 ├── execute()
 ├── preview()
 └── undo metadata
```

Examples:

```text
repair.non_manifold
repair.holes

mesh.remesh
mesh.simplify
mesh.smooth

surface.detect
surface.flatten

feature.fillet
feature.chamfer

pattern.detect
pattern.remove
pattern.replicate
```

Every command should be callable from:

```text
GUI
CLI
MCP
Scripting
Tests
Macros
```

So the architecture becomes:

```text
GUI ─────┐
CLI ─────┤
MCP ─────┤
Script ──┼──> Command Bus ──> Geometry Engine
Tests ───┘
```

This will save an enormous amount of complexity later.

---

# 6. Selection needs to be a real subsystem

This becomes surprisingly important given what you want AI to do.

Selections shouldn't merely be:

```text
triangle IDs = [17, 18, 19]
```

because topology changes invalidate them.

Instead, I'd support several forms of selection:

```text
Explicit topology selection
Semantic surface selection
Geometric query selection
Persistent tagged selection
```

For example:

```text
selection://face/123
```

or:

```text
query:
  curvature < 0.02
  connected = true
  normal.z > 0.8
```

or:

```text
tag://handle_surface
```

This lets the user select something visually and say:

> Smooth this.

The UI can translate the selection into a stable region descriptor.

Then MCP sees:

```json
{
  "selection": "region://df845c",
  "command": "smooth"
}
```

You can also let the AI find things:

> Find all six mounting holes.

Which might produce:

```text
Pattern
 ├── cylinder_1
 ├── cylinder_2
 ├── cylinder_3
 ├── cylinder_4
 ├── cylinder_5
 └── cylinder_6
```

---

# 7. Pattern recognition deserves its own subsystem

I agree with your earlier point that pattern recognition could become one of MeshFixer's differentiators.

I'd structure it separately from basic mesh repair.

```text
Geometry Analysis
       │
       ├── Surface segmentation
       │
       ├── Primitive fitting
       │
       │    Plane
       │    Cylinder
       │    Sphere
       │    Cone
       │    Torus
       │
       ├── Symmetry detection
       │
       ├── Repetition detection
       │
       └── Feature recognition
            Holes
            Slots
            Bosses
            Fillets
            Chamfers
```

Then pattern detection operates over features:

```text
Cylinder detection
        ↓
Hole detection
        ↓
Cluster by radius/depth
        ↓
Find repeated transforms
        ↓
Pattern candidate
```

You can detect:

```text
Linear pattern
Circular pattern
Mirror symmetry
Repeated feature
Grid
```

That can ultimately enable commands like:

> Remove the six ventilation holes.

or:

> Make all of these holes 8 mm.

That starts moving MeshFixer from "mesh repair program" toward **geometry understanding**.

---

# 8. Performance architecture

For something handling multi-million-triangle meshes, I would explicitly design for data locality.

Avoid an object-heavy representation like:

```text
Triangle {
    Vertex*
    Vertex*
    Vertex*
}
```

Instead use contiguous buffers:

```text
VertexBuffer
    x[]
    y[]
    z[]

IndexBuffer
    a[]
    b[]
    c[]
```

or packed structures where appropriate.

Then maintain derived structures independently:

```text
GeometryBuffers
Topology
BVH
Normals
Curvature
Segmentation
RenderBuffers
```

Each has a dirty state.

Example:

```text
Move vertices
   ↓
Geometry dirty

Normals        dirty
BVH            partial rebuild
Topology       unchanged
Segmentation   dirty
Index buffer   unchanged
```

That avoids recomputing everything.

---

# 9. Massive models need multiple representations

For something like a 50-million triangle scan, the viewport should **not necessarily display the working mesh directly**.

You want:

```text
Canonical Geometry
       │
       ├── Processing representation
       │
       └── Display representation
```

For example:

```text
50M triangle source mesh
       ↓
10M processing mesh
       ↓
2M viewport mesh
       ↓
300K navigation LOD
```

While rotating:

```text
300K
```

When motion stops:

```text
2M
```

When zoomed into an area:

```text
full-resolution local region
```

This is similar conceptually to what GIS, game engines, DCC tools, and scan-processing software do.

---

# 10. Background jobs

Heavy geometry work should go through a job scheduler:

```text
Job Scheduler
 ├── CPU thread pool
 ├── GPU compute queue
 ├── IO workers
 └── cancellation
```

Operations should expose progress:

```text
Remeshing
██████████░░░░░░
63%
```

and cancellation.

More importantly, they should work on **immutable snapshots**.

The user should still be able to rotate the model while remeshing is occurring.

```text
UI Thread
    │
Renderer
    │
    ├──────── Geometry Snapshot A

Worker Thread
    │
    └──────── produces Snapshot B
```

Once complete:

```text
Atomic swap A → B
```

That will make the app feel dramatically better.

---

# 11. GPU strategy

I'd keep GPU responsibilities initially narrow.

GPU:

```text
Rendering
Picking
Selection masks
LOD
Mesh shading
Possibly curvature calculations
Possibly nearest-neighbor queries
```

CPU:

```text
Topology mutation
Repair
Remeshing
Booleans
Feature graph
BRep operations
```

Later you could move some workloads to compute shaders, but I would avoid building core algorithms around CUDA because you immediately lose clean GPU portability.

For cross-platform GPU support I'd seriously evaluate:

```text
wgpu / WebGPU abstraction
```

which maps to platforms such as Vulkan, Metal, and DirectX.

Another reasonable choice is Vulkan with abstraction around it, but it's more engineering.

---

# 12. Native project format

I'd create a MeshFixer format, perhaps:

```text
.mfx
```

Conceptually:

```text
project.mfx
  manifest.json
  geometry/
      object1.mesh
      object2.step
  history/
      operations.bin
  thumbnails/
  selections/
  tags/
  metadata/
```

Prefer a container format so projects remain portable.

The important part is:

**Do not save every full mesh revision.**

Save something closer to:

```text
original geometry
+
operation graph
+
periodic cached checkpoints
```

For example:

```text
Import
 ↓
Repair
 ↓
Remesh
 ↓      ← checkpoint
Smooth
 ↓
Fillet
 ↓      ← checkpoint
Simplify
```

This gives fast undo without enormous project files.

---

# 13. Crash recovery becomes easy

Because commands are transactional:

```text
Begin transaction
    ↓
Execute operation
    ↓
Validate result
    ↓
Commit snapshot
```

You can journal operations.

On crash:

```text
Load last checkpoint
      ↓
Replay command journal
```

This is something I would put into the architecture from day one rather than bolt on later.

---

# 14. STEP should use a mature kernel

I wouldn't write a B-Rep kernel.

That's effectively another company's entire product.

I'd put an abstraction in front of something like:

```text
CADKernel
    importSTEP()
    exportSTEP()
    boolean()
    fillet()
    chamfer()
    tessellate()
```

Then initially implement it against an established CAD kernel.

That also keeps you from contaminating the rest of the architecture with kernel-specific types.

---

# 15. LLM architecture

The LLM should sit **outside the geometry engine**.

```text
             ┌──────────────┐
             │     LLM      │
             └──────┬───────┘
                    │ MCP
                    ▼
           ┌─────────────────┐
           │ MeshFixer API   │
           └───────┬─────────┘
                   │
               Commands
                   │
                   ▼
             Geometry Core
```

MCP might expose tools such as:

```text
meshfixer.get_scene()

meshfixer.get_selection()

meshfixer.analyze_object()

meshfixer.find_features()

meshfixer.select()

meshfixer.smooth()

meshfixer.remesh()

meshfixer.simplify()

meshfixer.chamfer()

meshfixer.fillet()

meshfixer.undo()

meshfixer.export()
```

Then an LLM can reason:

```text
User:
"Clean this scan up but don't change the mounting surfaces."

LLM:
1. analyze geometry
2. identify planar mounting surfaces
3. tag mounting surfaces
4. repair holes outside protected surfaces
5. remesh noisy areas
6. smooth selected areas
7. inspect metrics
```

The LLM remains a **planner**.

MeshFixer remains the authoritative execution engine.

---

# 16. Human + AI collaboration

The really interesting workflow isn't:

> AI, fix my mesh.

It's:

```text
Human selects region
        ↓
"Make this smoother but retain the ridge."
        ↓
LLM builds operation
        ↓
MeshFixer previews
        ↓
Human accepts
        ↓
Operation enters feature history
```

The result appears as a normal feature:

```text
History
─────────────
Import
Repair
Remesh
Smooth Region   ← AI-created
Fillet
```

You should not have a separate mystical "AI state."

AI actions should be ordinary MeshFixer actions.

That makes them editable and reversible.

---

# 17. UI process versus geometry process

For stability, I would eventually consider separating geometry processing from the UI process.

```text
MeshFixer UI Process
      │
      │ IPC
      ▼
MeshFixer Geometry Worker
```

If a pathological mesh causes a geometry library to explode:

```text
geometry worker crashes
```

rather than:

```text
entire application crashes
```

Then restart the worker and recover from the operation journal.

You don't necessarily need this in version 0.1, but I'd avoid architectural decisions that prevent it.

---

# 18. Cross-platform

I'd keep this principle very strict:

```text
Core must compile without UI.

Geometry must compile without renderer.

Renderer must not expose platform APIs upward.

UI must not contain geometry algorithms.
```

So:

```text
meshfixer-core
meshfixer-geometry
meshfixer-render
meshfixer-cli
meshfixer-desktop
```

You should be able to run:

```bash
meshfixer repair broken.stl \
  --fix-holes \
  --remove-nonmanifold \
  --output repaired.3mf
```

on a Linux server with no GUI installed.

That's also what makes MCP clean.

---

# 19. Technology stack I would seriously consider

Something close to:

```text
Core / orchestration
    Rust

Heavy geometry / CAD
    Rust + selected C++ libraries

CAD/BRep
    OpenCascade behind abstraction

Mesh algorithms
    custom + proven libraries where appropriate

Rendering
    wgpu

Desktop UI
    Qt / Slint / native Rust UI depending maturity

Concurrency
    Rust async + CPU worker pool

Serialization
    FlatBuffers / Cap'n Proto / custom binary buffers

Scripting
    Python later

Automation
    CLI + MCP

Build
    CMake where C++ required
    Cargo for Rust

Platforms
    Windows
    macOS
    Linux
```

I wouldn't lock all those choices yet, but that's the family of architecture I'd prototype.

---

# 20. The most important architectural boundary

If I had to reduce the entire architecture to one rule, it would be this:

```text
                    COMMAND
                       │
                       ▼
                 FEATURE GRAPH
                       │
                       ▼
                GEOMETRY ENGINE
                       │
                       ▼
                 NEW SNAPSHOT
```

Everything goes through this.

The GUI doesn't modify meshes.

The LLM doesn't modify meshes.

Python doesn't modify meshes.

MCP doesn't modify meshes.

They issue **commands**.

Commands create **features**.

Features transform **geometry snapshots**.

That gives us history, reproducibility, CLI support, AI control, undo/redo, testing, crash recovery, and cross-platform behavior almost for free.

---

## And I would split development into four architectural stages

**Stage 1 — Mesh kernel**

```text
STL / OBJ / 3MF
Viewport
Selection
Repair
Remesh
Simplify
Smooth
Undo/redo
Export
```

**Stage 2 — Geometry understanding**

```text
Surface segmentation
Primitive detection
Feature detection
Pattern recognition
Semantic selections
```

**Stage 3 — CAD operations**

```text
STEP
BRep
Chamfer
Fillet
Boolean
Surface reconstruction
```

**Stage 4 — intelligent control**

```text
CLI
MCP
LLM workflows
Procedural actions
AI-assisted repair
```

Although I'd build the **command architecture and MCP-friendly API in Stage 1**, even if we don't expose the LLM interface immediately.

Because retrofitting that later is exactly the sort of architectural mistake that would hurt us.

One other thing I'd make a first-class subsystem from day one is **observability**: every geometry operation should emit timings, peak memory, triangle-count changes, cache hits, and validation results. For MeshFixer, performance isn't just an optimization problem—it should be something the architecture can measure continuously.

---
MeshFixer — Technology Stack, Libraries, Frameworks & Algorithms
I think we can build MeshFixer using a surprisingly large amount of mature, open-source geometry technology. We should be writing our own application architecture, geometry intelligence, history engine, and human/AI interaction layer, not reinventing decades of computational geometry research.

There are several important technical decisions, though, particularly around the geometry kernel, renderer, user interface, and persistent feature selection.

My proposed direction is a Rust-based application architecture with specialized C++ geometry libraries, a native GPU renderer, and a kernel-neutral geometry API.
1. My proposed technology stack
MeshFixer reference stack
Recommended direction

DESKTOP APPLICATION

Rust + Slint or Qt 6
Cross-platform UI, panels, history, tool interactions
3D VIEWPORT & GPU

wgpu + meshoptimizer
Rendering, picking, meshlets, LOD, GPU resource management
APPLICATION ENGINE

Rust + Rayon + Tokio
Command execution, document graph, scheduler, transactions
MESH KERNEL

PMP + custom structures
Repair, remesh, smoothing, topology

CAD KERNEL

Open CASCADE
STEP, B-Rep, solids, fillets, chamfers

GEOMETRY RESEARCH

libigl + Open3D
Fitting, spectral methods, segmentation

BOOLEAN OPERATIONS

Manifold
Robust watertight mesh Booleans


AUTOMATION & PERSISTENCE

CLI + MCP + SQLite + FlatBuffers
External control, project history, cached geometry

These are proposed selections, not dependencies we should lock without testing.

In particular, the UI framework, the editable mesh representation, and how we integrate Open CASCADE need early prototypes.
2. The actual libraries I'd evaluate
Component	Primary candidate	Alternative
Application language	Rust	Modern C++
Desktop UI	Slint	Qt 6
Rendering	wgpu	Qt RHI / Vulkan
Editable mesh data	Custom + PMP	OpenMesh
Mesh algorithms	PMP	libigl
Advanced geometric operations	libigl	CGAL
CAD kernel	Open CASCADE	Commercial CAD kernel
Mesh Booleans	Manifold	CGAL
Mesh reduction & LOD	meshoptimizer	Custom QEM
Spatial acceleration	Embree / custom BVH	parry3d
Scan processing	Open3D	PCL
Linear algebra	Eigen	nalgebra
Parallel processing	Rayon	oneTBB
Background tasks	Tokio + dedicated workers	Custom task scheduler
Serialization	FlatBuffers	Cap'n Proto
Project metadata	SQLite	JSON manifests
Compression	Zstandard	LZ4
MCP integration	Rust MCP SDK	Python MCP adapter
Testing	cargo test + Catch2	GoogleTest

Four libraries stand out









Open CASCADE Technology

The foundation for STEP, native B-Rep solids, precise mechanical modeling, and analytic fillets and chamfers. I would not attempt to recreate these capabilities.

CAD foundation — essential if STEP is a serious requirement.












Polygon Mesh Processing Library (PMP)

MIT-licensed C++ mesh algorithms including smoothing, remeshing, subdivision and decimation. A compelling baseline for an editable mesh engine. 

GitHub+1

Core mesh toolkit — strong first prototype candidate.












meshoptimizer

Provides mesh simplification and GPU-oriented optimization. Particularly relevant to our goal of opening large models without making the application sluggish. 

GitHub+1

Rendering optimization — adopt early.












Manifold

Designed for robust manifold mesh operations. Very attractive for mesh Boolean and constructive geometry workflows, though malformed scan meshes must be repaired before they can reliably enter its Boolean pipeline. 

GitHub+1

Mesh Boolean backend — adopt with input validation.



One licensing warning up front: CGAL is technically excellent, but much of its high-level geometry processing functionality is GPL-licensed or commercially licensed. That matters if MeshFixer could become proprietary. We should not assume that every open-source geometry library is freely usable in a closed-source application. 

cgal.org+1

3. The editable mesh engine — probably our most important technical choice
I'd separate three things that are often combined in mesh applications:
1. Canonical mesh storage — the authoritative vertices, faces, attributes and identities.
2. Editable topology — connectivity structures needed for repair, remeshing, extrusion, cutting and smoothing.
3. Rendering representation — GPU-friendly buffers, spatial clusters and lower-detail approximations.
These representations have fundamentally different performance requirements.
Mesh representation
For editing, I'd investigate a half-edge data structure.
Conceptual half-edge mesh: each directed edge references its opposite edge, adjacent face, next edge and origin vertex. This enables efficient local topology traversal.
Why this matters:
- Find all faces adjacent to a selected face.
- Identify boundary loops.
- Collapse or split edges.
- Detect non-manifold connectivity.
- Perform local remeshing.
- Grow a selection across a surface.
PMP provides a good starting point for the editable topology representation. I would benchmark it against OpenMesh before committing.

However, I wouldn't keep a massive mesh in an editable half-edge structure at all times. For large models, adjacency and metadata can consume substantially more memory than simple indexed triangle buffers.

I'd use something conceptually like:
struct MeshSnapshot {
    positions: Arc<[Vec3d]>,
    triangles: Arc<[[u32; 3]]>,
    attributes: AttributeStore,
    topology: Option<Arc<Topology>>,
    spatial_index: Option<Arc<Bvh>>,
}


This is illustrative; a production implementation needs paged storage, versioned caches, stable object identities, and concurrency rules.

My proposed optimization: build editable topology lazily, potentially only for the affected region, and let the renderer consume separate compact buffers.

A second critical decision is numeric precision. I'd use double precision for authoritative geometry computations, float precision for most GPU buffers, and camera-relative positioning for scenes with large coordinates.
4. Which algorithms should actually power the tools?
This is where we should be particularly deliberate. A tool called "Smooth" or "Repair" may hide several very different algorithms.

Here is the algorithm shortlist I'd use.
Tool	Recommended algorithm	Library direction
Detect defects	Edge incidence, connectivity, orientation, intersection checks	PMP + custom
Fill holes	Boundary loop triangulation, constrained Delaunay, curvature-aware fairing	PMP / CGAL evaluation
Smooth	Cotangent Laplacian, Taubin, bilateral normal filtering	PMP / libigl
Simplify	Quadric Error Metrics (QEM), constraints	meshoptimizer + custom
Remesh	Isotropic split/collapse/flip, surface reprojection	PMP
Adaptive remesh	Curvature-dependent target edge lengths	Custom / CGAL
Mesh Booleans	Robust triangle intersection and re-triangulation	Manifold
CAD Booleans	B-Rep intersection and topology construction	Open CASCADE
Chamfer / fillet	Offset surfaces, blends, intersection trimming	Open CASCADE for B-Rep
Sharp-edge detection	Dihedral angles, curvature discontinuities	Custom
Plane detection	Region growing + RANSAC + least-squares fitting	Open3D / custom
Cylinder detection	RANSAC + nonlinear least-squares refinement	Custom
Pattern detection	Feature descriptor clustering, rigid transforms	Custom
Symmetry	Principal axes, reflection and registration testing	Custom
Scan reconstruction	Poisson reconstruction, TSDF, ball pivoting	Open3D

Let's examine the important ones.
A. Simplification: Quadric Error Metrics
This would be my default simplification algorithm.

For each vertex, accumulate a quadric describing squared distances to nearby face planes. When evaluating an edge collapse, estimate how much geometric error it introduces.

Mathematically:
\[
E(\mathbf v)=\mathbf v^T Q\mathbf v
\]

where \(Q\) is an accumulated symmetric quadric matrix.




















9










We would need two simplification paths:

Fast simplify: use meshoptimizer for quick reduction, preview meshes, and LOD generation.

Engineering simplify: a feature-aware QEM implementation that considers sharp boundaries, surface tags, mounting faces, material boundaries, and geometric error tolerances.

That second path is important. Simply reducing a 3D-printed engineering part by 80% without preserving cylindrical holes can destroy its usefulness.

I would expose quality constraints like maximum surface deviation in millimeters, not just percentage reduction.
B. Remeshing: isotropic and adaptive
For a standard isotropic remesher:
Split long edges
Create vertices where triangles are too large


Collapse short edges
Remove unnecessary local detail


Flip edges
Improve local triangle connectivity


Relax and reproject
Improve triangle quality while remaining close to the original surface


That established approach is supported by CGAL's remeshing library. Its current documentation also covers adaptive sizing fields and protected feature constraints. 

cgal.geometryfactory.com+1

Our distinctive feature would be to preserve important areas using semantic tags.

For example, we might remesh the curved exterior aggressively but constrain the original geometry around mechanical mating surfaces and bolt holes.
C. Smoothing: three different algorithms, not one









Laplacian
Fast smoothing; tends to shrink geometry









Taubin
Low-shrinkage smoothing using two-pass filtering









Bilateral
Preserves sharper geometric features while denoising

I'd offer them in the engine, but not necessarily expose all the algorithm names in the default UI.

The user might simply see a strength setting, a preserve-features toggle, and a preview. Advanced controls can expose the precise algorithm.

We should also support smoothing limited to a tagged selection with a falloff boundary, preventing visible seams where the modified area meets the untouched mesh.
D. Fillets and chamfers are not one problem
This is a particularly important distinction for MeshFixer.




















8










For a STEP/B-Rep solid, fillets and chamfers should use a proper CAD kernel, specifically Open CASCADE's blending and modeling operations. 

OpenCascade+1

For an STL triangular mesh, the same operation is much harder.

A mesh fillet requires identifying an edge chain, constructing a geometrically meaningful blend, replacing local topology, and validating manifoldness.

I'd propose two paths:
- Mesh bevel: approximately round or bevel a selected edge chain, with topology refinement and local remeshing. This is useful but not guaranteed dimensionally exact.
- CAD fillet: work on a B-Rep representation for accurate radius-based construction.
The long-term opportunity is to recognize the analytic surfaces around a mesh edge, reconstruct the necessary local B-Rep geometry, perform the fillet, and tessellate the result back into the mesh.

That's substantially more ambitious than a conventional mesh bevel.
5. Pattern recognition — where MeshFixer could become genuinely different
This is the subsystem I'd be most interested in developing ourselves, using existing numerical and geometric libraries underneath.

I don't think we should begin with a neural network.

Most of the first wave of mechanical feature recognition can be accomplished with classical computational geometry, and those results will be deterministic, measurable, and easier to explain.
Proposed recognition pipeline
Mesh / B-Rep input
Triangles, faces, adjacency, normals, curvature

Surface segmentation
Region growing, curvature boundaries

Primitive fitting
RANSAC, least-squares, analytic shapes



Feature graph
Planes, holes, cylinders, slots, bosses, candidate relationships

Repetition and symmetry inference
Transform matching, clustering, spacing and axis detection

Semantic results
Six-hole circular pattern · three repeated ribs · symmetric mounting faces

The key algorithms
Surface segmentation: region growing using face normals, principal curvature, and adjacency. For more complex regions, investigate graph-based optimization or spectral clustering.

Primitive fitting: use RANSAC to propose a plane, cylinder, sphere or cone, then refine parameters with nonlinear least squares. Eigen or nalgebra can handle the numerical operations.

Feature identification: recognize relationships between surfaces. For example, a cylindrical sidewall bounded by circular edges, combined with surrounding planar geometry, is evidence for a hole.

Pattern inference: cluster similar features and search for repeated rigid transforms, regular spacing, circular arrangements, and reflections.




















12










The first version should attach confidence and geometric tolerance to every candidate.

For example:
{
  "feature": "circular_pattern",
  "instances": 6,
  "axis": [0, 0, 1],
  "pitch_radius_mm": 24.0,
  "mean_spacing_deviation_mm": 0.03,
  "confidence": 0.96
}


The numbers above are illustrative, not results from an actual model.

The important thing is the intermediate feature graph. We should retain recognized features as actual data that tools, the UI, and LLMs can query, rather than only drawing colored overlays.
Why I wouldn't start with machine learning
An ML model may eventually be valuable for recognizing complex, organic, or ambiguous features. But it introduces training data, deployment size, inference performance, and reproducibility concerns.

I'd start with geometric recognition, build a labeled corpus of difficult cases, and then evaluate ONNX Runtime for local ML inference once we know exactly which feature classes classical algorithms struggle with.

That is a more sustainable route to reliable pattern understanding.

6. Rendering — wgpu versus Qt versus a game engine
My preference is wgpu, but with an important qualification: it gives us a graphics abstraction, not a finished CAD viewport.

wgpu currently supports native backends including Vulkan, Metal and Direct3D 12, which aligns well with our cross-platform requirement. 

Rust

We'll still need to build the CAD-specific viewport behavior.

That includes selection, construction planes, section cuts, clipping, object highlighting, face/edge/vertex modes, gizmos, depth-aware outlines, and diagnostic overlays.
Proposed renderer structure
Scene Graph
   │
   ├── Geometry cache
   ├── Material cache
   ├── Camera + clipping
   └── Selection state
             │
             ▼
      Render preparation
             │
     ┌───────┴─────────┐
     │                 │
  LOD/meshlets      Visibility
     │                 │
     └───────┬─────────┘
             ▼
       GPU render passes
         ├── Depth
         ├── Shaded mesh
         ├── Wireframe
         ├── Selection IDs
         ├── Highlights
         └── Overlays


Selection should be hybrid
For fast interaction:
- GPU ID-buffer picking for visible object and triangle candidates.
- CPU spatial queries for accurate face, edge and vertex identification.
- Topology-aware region growth for larger selections.
- Geometric queries for precise measurements and surface interpretation.
Embree is worth benchmarking for CPU ray intersection and picking workloads. It is optimized for spatial acceleration and ray intersections, though it might be more machinery than we need for a modest first release. 

GitHub

Performance techniques I'd adopt early
Technique	What it solves	Priority
Compact indexed GPU buffers	Memory and bandwidth	Day 1
BVH acceleration	Ray picking and spatial queries	Day 1
Frustum culling	Offscreen geometry	Day 1
LOD hierarchy	High-triangle-count navigation	Day 1
Background rendering uploads	Frame-time interruptions	Early
Chunk-based editing	Memory pressure from huge meshes	Early
Meshlets	Efficient local batches and culling	Benchmark
Occlusion culling	Hidden geometry in complex assemblies	Later
GPU compute operations	CPU-bound analysis	Selectively
Out-of-core storage	Models exceeding comfortable RAM	Later, design for now

I'd target a 60 FPS interactive viewport on our benchmark hardware, but I'd also provide a reduced-quality navigation mode for expensive scenes.

One nuance: a single, continuous mesh that fills the screen doesn't automatically benefit much from frustum culling unless it is subdivided into spatial chunks. That's why I'd want a chunked display representation from the beginning.
7. Desktop UI — Slint versus Qt
This is less obvious than it first appears.
Slint

A very attractive Rust-native option for a lightweight, modern desktop interface. Good fit for a custom UI, and its royalty-free license supports proprietary desktop applications subject to attribution and other terms. 

Slint+1


Lean


Qt 6

More mature desktop UI ecosystem, particularly for complex docking, property editors, trees, dialogs, and established desktop application workflows. The trade-offs are integration complexity, deployment footprint, and licensing constraints. 

Qt 6.12


Established


My initial instinct is to prototype Slint with a wgpu viewport, but I would make Qt the fallback if docking, drag-and-drop panel layouts, accessibility, and native desktop interactions become too expensive to implement ourselves.

I wouldn't use a game engine like Unreal or Unity as the default shell. They're excellent in their own domains, but MeshFixer's principal challenge is topology, precise geometry, persistent selection, and CAD-like editing—not game-world rendering.

A game-engine architecture can introduce unnecessary scene, asset, and lifecycle assumptions.

8. CAD kernel integration and STEP support
Here I would commit more strongly.

Open CASCADE Technology (OCCT) should be our initial CAD kernel candidate.

It provides B-Rep topology, modeling operations, tessellation, curve/surface mathematics, and STEP import/export capabilities. Its license is LGPL 2.1 with an additional exception, so a commercial deployment is possible subject to compliance obligations. 

OCCT3D+2

I'd encapsulate it in a C++ adapter:
Rust Document / Command Engine
             │
       CAD Kernel API
             │
          C++ FFI
             │
       Open CASCADE
             │
       B-Rep geometry
             │
       Tessellation
             ▼
       Viewport mesh


The key is preserving the B-Rep representation after importing STEP.

We should not tessellate a STEP file and discard its precise model.

Instead, a STEP object should hold its original analytic geometry, with one or more tessellated representations used for display.

This lets users perform precise CAD operations when possible and switch to mesh operations where appropriate.

It also means an STL-to-STEP export capability must declare whether it is writing a faceted representation or performing actual analytic surface reconstruction. Those are very different outputs.

9. History, storage and undo/redo
This is an area where I'd build our own framework rather than try to adopt a full CAD application's document architecture.

I'd use:
- SQLite for project metadata, object relationships, command history, stable tags and transaction records.
- FlatBuffers or a versioned binary format for geometry metadata and structured data.
- Zstandard for compressing mesh snapshots and caches.
- Content-addressed storage for reusable immutable geometry.
- Periodic checkpoints to avoid replaying very long operation chains.
The key architectural concept is a dependency graph plus a cache.
Recalculation uses dependency-aware invalidation. Cached intermediate results prevent repeatedly running expensive geometry operations.
One subtle issue is persistent topology identity.

When a remesher changes all the triangles, existing face IDs may disappear. This is a longstanding CAD problem often associated with the topological naming problem.

I'd support three layers of identity:
1. Stable IDs for objects and high-level features.
2. Topology correspondence produced by operations where possible.
3. Geometric re-identification for selections after destructive topology changes.
We should also record when a previously tagged region can no longer be identified with sufficient confidence rather than silently applying an operation to the wrong surface.

That will be especially important when LLMs control the feature graph.
10. MCP, CLI and AI implementation
I'd implement a typed Rust command schema as the canonical API, then generate or adapt the CLI and MCP interfaces from that schema.

The CLI is straightforward to test and integrate into CI pipelines.

MCP should expose higher-level, safe operations instead of raw memory access or arbitrary geometry-kernel calls.

For example, a typical LLM workflow might be:
meshfixer.analyze("object-1")
       ↓
meshfixer.find_features(type="mounting_hole")
       ↓
meshfixer.tag(features, "protected")
       ↓
meshfixer.preview({
  operation: "remesh",
  target: "object-1",
  preserve_tags: ["protected"]
})
       ↓
meshfixer.commit(preview_id)


Every mutation needs validation, transaction semantics, and undoability.

I'd also expose compute budgets, timeouts, cancellation, and progress events so an AI client cannot accidentally monopolize the machine.

For headless automation, the geometry engine should operate with no desktop window, no GPU, and no network requirement.
11. The licensing and integration matrix
This table matters because we should avoid building a business around a library whose license later becomes a blocker.
Library	License position	Recommendation
Rust, Rayon, Tokio	Permissive ecosystem components	Adopt
wgpu	Permissive	Adopt
meshoptimizer	MIT	Adopt
PMP	MIT	Strong candidate
Manifold	Apache 2.0	Strong candidate
Open3D	MIT	Selectively adopt
lib3mf	BSD-2-Clause	Adopt for 3MF  GitHub
Open CASCADE	LGPL 2.1 + exception	Adopt with compliance review
libigl	MPL 2.0 core; some GPL components	Review by module
CGAL	Mixed LGPL, GPL, commercial	Research/benchmark or license appropriately
Slint	GPL, royalty-free, commercial alternatives	Review terms
Qt	LGPL/GPL/commercial, module-dependent	Review terms

The library licenses above are based on currently published upstream information; we should still perform a dependency-level license review before distributing MeshFixer. 

cgal.org+3

12. What I would build versus what I'd buy or reuse
I would draw the boundary this way:
Capability	Build or reuse?
STL / OBJ / 3MF import	Reuse, with strict parsing and validation
STEP import/export	Reuse OCCT
Half-edge mesh operations	Adapt PMP
Basic mesh repair	Reuse and extend
Simplification	Reuse meshoptimizer; customize quality constraints
Rendering backend	Reuse wgpu
CAD Boolean/fillet/chamfer	Reuse OCCT
Manifold mesh Booleans	Reuse Manifold
Feature history and dependencies	Build
Persistent semantic selections	Build
Primitive and pattern recognition	Build on existing math libraries
Human/AI shared command model	Build
Memory-aware job scheduling	Build
Mesh-to-CAD reconstruction	Build incrementally

This gives MeshFixer a coherent intellectual property and engineering focus without trying to implement everything from scratch.
13. The prototypes I would run before finalizing the architecture
Rather than spend months debating Qt versus Slint, or Rust versus C++, I would run five focused technical spikes.

Spike 1 — Large mesh viewer

Load 1M, 10M and 50M triangle models using Rust, wgpu and meshoptimizer. Measure frame time, CPU memory, GPU memory, import time and picking latency.
Decision: validate rendering architecture and chunking strategy.



Spike 2 — Mesh repair and remesh

Integrate PMP and test real-world broken STL files against metrics for manifoldness, geometric fidelity, memory, and runtime.
Decision: whether PMP meets our baseline quality requirements.



Spike 3 — STEP and analytic CAD

Import a mechanical STEP model, select an edge, apply a chamfer or fillet, tessellate and export. Prove Rust/C++ lifetime and error boundaries.
Decision: OCCT integration and native CAD representation.



Spike 4 — Feature detection

Detect planes, cylinders, mounting holes and a circular hole pattern on a known test model, then perturb the mesh and measure recognition stability.
Decision: feasibility and precision of semantic geometry.



Spike 5 — History and AI-controlled editing

Select a surface in the UI, run a command through MCP, alter an upstream remesh parameter, recompute dependencies and verify that the intended surface remains selected.
Decision: viability of the central MeshFixer interaction model.


We should maintain a standard benchmark corpus containing clean CAD models, broken STL files, noisy scans, thin walls, self-intersections, large meshes, and deliberately pathological geometry. That corpus will become one of our most valuable engineering assets.

14. My final technology choices
If I were beginning implementation now, this is the stack I'd start with:
Layer	Selection	Confidence
Core language	Rust	High
Geometry interoperability	C++ through a narrow FFI	High
GPU renderer	wgpu	High
Desktop UI	Slint prototype, Qt benchmark	Medium
Editable mesh processing	PMP	Medium-high
CAD kernel	Open CASCADE	High
Mesh Boolean kernel	Manifold	High
LOD and rendering mesh optimization	meshoptimizer	High
Geometry research and fitting	Eigen, libigl, Open3D	Medium-high
Job execution	Rayon + dedicated worker processes	High
Project metadata	SQLite	High
Snapshot compression	Zstandard	High
External automation	CLI and MCP	High

The biggest unresolved technical decision is actually not Rust versus C++.

It's whether the authoritative mesh representation should be a custom, chunked data model or an existing library's native mesh type.

That decision affects memory consumption, topology operations, persistent selections, partial recalculation, undo/redo, and large-model scalability.

My current leaning is:

Own the canonical mesh format and object identity model. Use third-party geometry libraries as algorithm backends, converting into their working representations only where necessary.

That avoids coupling the entire application's lifetime, history and storage model to a third-party topology implementation. The trade-off is conversion cost, which is precisely what the second technical spike should measure.

I'd treat three things as MeshFixer's long-term differentiators: semantic geometry understanding, non-destructive feature history, and an AI-accessible deterministic command system. The geometry algorithms underneath can largely be assembled from strong existing libraries. The work of integrating those capabilities into one fast, predictable and editable application is where the engineering investment should go.