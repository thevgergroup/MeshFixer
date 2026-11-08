Recently we've seen the rise of open source tooling in 3d printing, replacing older licensed software that was horribly slow and over priced. SolidCraft is an alternative built by the artcraft team as an example and then solidcore is the open source engine they're building in rust [https://solidcraft.io/solidcore/](https://solidcraft.io/solidcore/)

In that vein something that's been a frustration for many is the shutdown of MeshMixer - it was acquired and then shutdown years ago with only ghost versions available out there. Obviously nobody can trust those, who knows where they came from or how to verify them.

I'd like to take the approach the artcraft team did, and build a new desktop client to fill the hold - excuse the pun left by meshmixers demise.&#x20;

Can you research into it, identify what it used do, what's changed since then - obviously AI systems are big, llm controling, but also 3d phone scanning with messy outcomes, point clouds can easily be generated and more people designing on mobile devices.&#x20;

Texturing, text embossing and raise on curved surfaces, a whole world of things.&#x20;
What would a MeshMixer (or as I'd call it MeshFixer) look like today?

Yes. There is a real product here, but I would frame it slightly differently from “rebuild Meshmixer.”

**MeshFixer should be the modern desktop workbench between “I have some ugly 3D data” and “I have a usable, manufacturable 3D object.”**

That means meshes, scans, point clouds, mobile captures and increasingly AI-generated geometry—not just STL repair.



## What Meshmixer actually was

Meshmixer’s magic was not that it was a CAD package. It was that it let you do **local, direct surgery on geometry without caring very much about how the geometry had been created**.

Autodesk still describes its feature set as drag-and-drop mesh mixing, sculpting, convert-to-solid, lattices, hollowing, tree supports, print-bed orientation, selection brushes, remeshing, simplification, deformation, hole filling, boundary stitching, booleans, plane cuts, offsets, projections, tubes/channels, alignment, measurements, thickness and stability analysis. [Autodesk Marketplace](https://marketplace.autodesk.com/apps/2e40e347-0369-4603-b83f-ecab324aed0f?pubDate=20260711\&utm_source=chatgpt.com)

That combination is the important part.

Traditional CAD says:

> Here is the history of the object. Edit its underlying design.

Meshmixer effectively said:

> Here is a surface. Point at the bit that's wrong and do something to it.

That made it extraordinarily useful for scans, downloaded STL files, sculpted models, anatomical geometry, cosplay, reverse engineering, model kitbashing and 3D printing.

The product is now frozen at 3.5.0. Autodesk says it is no longer in development or supported, even though a signed installer remains available through its marketplace. [apps.autodesk.com](https://apps.autodesk.com/ACD/en/Detail/HelpDoc?appId=4108920185261935100\&appLang=en\&os=Win64\&utm_source=chatgpt.com)

## The opportunity has actually become bigger

The input side has changed enormously.

In the Meshmixer era, someone normally arrived with an STL or OBJ. Today they may arrive with a photogrammetry model, iPhone LiDAR scan, RGB-D scan, PLY point cloud, Gaussian splat, an AI-generated model, a sculpt from an iPad, or a multi-part 3MF produced by another manufacturing workflow.

Phone capture in particular is becoming mundane. Polycam now supports object capture on relatively ordinary iPhones and Android devices, and in 2026 expanded spatial capture to non-LiDAR iPhones. Other tools produce photogrammetry, meshes, point clouds and Gaussian splats directly from phones. [Polycam Help Center](https://learn.poly.cam/hc/en-us/articles/34419168797972-Which-Devices-Are-Supported-by-Polycam?utm_source=chatgpt.com)

Open3D 0.20 now even renders Gaussian splats alongside conventional meshes and continues to add point-cloud registration, reconstruction and smoothing capabilities. [Open3D](https://www.open3d.org/blog/?utm_source=chatgpt.com)

So the fundamental workflow is becoming:

**capture → inspect → understand → clean → reconstruct → modify → make printable/useful → export**

Meshmixer mostly owned the middle of that chain.

MeshFixer could own much more of it.

---

# What MeshFixer should be

I'd give it five conceptual modes, but keep them inside **one workspace** rather than making users learn five applications.

| Mode | What the user thinks they are doing | What MeshFixer is actually doing |
|---|---|---|
| **Inspect** | “What's wrong with this?” | topology analysis, curvature, thickness, components, intersections, scale, normals |
| **Repair** | “Make this usable.” | manifold repair, hole filling, remeshing, scan cleanup, reconstruction |
| **Modify** | “Change this bit.” | sculpting, cutting, booleans, offsets, deformation, text, surface operations |
| **Fit** | “Make something that fits this.” | offsets, contact surfaces, clearance, shells, alignment, negative volumes |
| **Prepare** | “Make it ready for printing/export.” | orientation, hollowing, splitting, supports interfaces, simplification, 3MF/export |

That avoids turning it into Blender.

Blender already exists. MeshLab already exists. CAD already exists.

The opportunity is **purposeful geometry manipulation**.

## The opening screen should answer one question

When the user drops something in:

**“What do you want to do with this?”**

Not:

**“Here are 94 tools.”**

The viewport can immediately run a geometry diagnostic and show something like:

> 2.4M triangles  
> 17 disconnected components  
> 6 open boundaries  
> 3 self-intersection regions  
> likely scan pedestal detected  
> dimensions: 126 × 94 × 181 mm  
> minimum wall thickness: 0.7 mm  
> printability: needs attention

The model itself should visualize the problems.

Click “6 open boundaries,” and they glow.

Click “thin walls,” and you get a thickness heat map.

Click “loose geometry,” and those islands highlight.

CGAL already exposes repair primitives for degenerate faces, polygon soups, orientation, holes and self-intersections, so this isn't speculative computer science. [doc.cgal.org](https://doc.cgal.org/latest/PMP_Mesh_repair/index.html?utm_source=chatgpt.com)

---

# The killer feature: intent-based repair

Meshmixer made users understand operations.

MeshFixer should understand **intent**.

Imagine importing a horrible phone scan and typing:

> Clean this up. Remove the table underneath, close the bottom naturally and make it suitable for a 100 mm tall resin print.

MeshFixer does not ask an LLM to hallucinate geometry.

The LLM creates an **operation plan**:

```text
1. Detect main connected component
2. Detect probable ground plane
3. Remove geometry below plane
4. Remove floating components under 2 mm
5. Close boundary using curvature-aware patch
6. Remesh scan noise at 0.35 mm target
7. Preserve high-curvature features
8. Make watertight
9. Scale to 100 mm height
10. Run minimum-wall analysis
```

The geometry engine executes deterministic operations.

The user sees the plan before accepting it.

That distinction is essential.

**AI should orchestrate geometry operations; it should not be the geometry kernel.**

Interestingly, current research is heading in exactly this direction. CVPR 2026's Vinedresser3D uses a multimodal LLM to interpret editing intent and decompose structural/appearance edits, while MIT's InstructMesh research specifically found value in combining natural-language commands with conventional sliders and controls for fabrication-related corrections. [Open Access CVF](https://openaccess.thecvf.com/content/CVPR2026/html/Chi_Vinedresser3D_Towards_Agentic_Text-guided_3D_Editing_CVPR_2026_paper.html?utm_source=chatgpt.com)

So I would put a command bar at the bottom of MeshFixer:

> `Ask MeshFixer…`

And almost every conventional tool becomes callable both manually and semantically.

---

# But don't make it “AI-first”

The biggest mistake would be making the whole interface a chatbot.

3D work is spatial.

Users need to point, paint, drag, rotate, compare and inspect.

A better interaction model is:

**select + command + preview**

Paint an area on a model and say:

> Flatten this but blend the edge into the surrounding surface.

Or:

> Rebuild this damaged corner using symmetry.

Or:

> Make this hole exactly 8 mm diameter.

Or:

> Put “ELLIOT” around this curved surface, raised 0.8 mm.

The LLM resolves intent and parameters. The geometry engine provides precision.

---

# Surface text could be a surprisingly important differentiator

This sounds like a small feature but it is an enormous annoyance in existing workflows.

Text should be treated as a **surface-aware object**, not immediately baked into polygons.

You should be able to type text and attach it to a mesh surface.

Then choose:

| Operation | Result |
|---|---|
| Emboss | raise from surface |
| Deboss | cut into surface |
| Engrave | V/U/channel cut |
| Inlay | produce matching body + cavity |
| Wrap | maintain text proportions around curved surface |
| Project | normal-project onto arbitrary topology |
| Conform | deform glyphs to follow compound curvature |

Then let the user change font, size, spacing, depth and placement later.

Internally, I'd keep text parametric until export and only perform the boolean/remesh when required.

The same architecture can support logos, SVGs, textures-as-relief, braille, serial numbers and manufacturing labels.

---

# Scan repair should be a first-class workflow

This might be the strongest wedge.

MeshFixer should assume scans are bad.

Not occasionally bad.

**Bad by default.**

Phone scans routinely contain some combination of overscan, ground/table geometry, floating islands, uneven triangle density, blurry surfaces, holes, occlusion gaps, noise, duplicate surfaces and broken topology.

So a dedicated **Scan → Object** workflow could be extremely compelling:

```text
raw scan
   ↓
isolate subject
   ↓
remove floor / table
   ↓
denoise
   ↓
align multiple scans
   ↓
fill small gaps
   ↓
reconstruct missing surface
   ↓
remesh
   ↓
preserve/transfer colour
   ↓
scale
   ↓
make printable
```

MeshLab demonstrates that mature open tooling already exists for ICP alignment, global registration, point-cloud processing and Screened Poisson reconstruction. [meshlab.net](https://www.meshlab.net/?utm_source=chatgpt.com)

Open3D gives you another modern, MIT-licensed foundation for point clouds, RGB-D, registration, reconstruction and now splats. [GitHub](https://github.com/isl-org/Open3D/blob/main/LICENSE?utm_source=chatgpt.com)

---

# Point clouds should be native objects

Don't immediately force everything through triangles.

I would have MeshFixer understand at least:

**Mesh → Point Cloud → Voxel/SDF → B-rep-ish/analytic primitive**

as distinct representations.

Different repairs are better in different domains.

A tiny crack in a mesh may be best repaired topologically.

A catastrophically tangled scan might be better converted to a signed distance field and reconstructed.

A noisy flat surface may be better fitted to a plane.

A roughly cylindrical scan could be replaced by a true cylinder locally.

That leads to something much more interesting than a mesh editor:

**representation-aware geometry repair.**

The user doesn't need to know any of this.

They just see:

> Reconstruct this region  
> Preserve detail: High  
> Surface tolerance: ±0.15 mm

---

# A “Make Solid 2.0”

Meshmixer's Make Solid was one of its most useful ideas.

A modern version should expose the tradeoff visually:

**Detail ↔ Robustness**

At one end:

> Preserve every scan bump.

At the other:

> Give me a mechanically sane solid.

The implementation could choose between topology repair, voxelization, SDF reconstruction, Poisson reconstruction or local remeshing depending on the input.

That would solve an enormous number of real-world models.

---

# The geometry stack is much better now

You do not need one enormous kernel.

I'd actually recommend a deliberately layered engine.

| Layer | Candidate technology | Role |
|---|---|---|
| App | Rust + Tauri/native shell | desktop lifecycle, filesystem, updater |
| UI | WebGPU-based viewport | interactive rendering |
| Geometry core | Rust | mesh topology, selections, transforms, orchestration |
| Robust booleans | Manifold / native Rust port | union, intersection, difference |
| Scan/point processing | Open3D | ICP, clouds, reconstruction |
| Advanced mesh ops | selected libigl/CGAL algorithms | remesh, parameterization, repair |
| Volumetric/SDF | OpenVDB-equivalent / custom sparse SDF | brutal repairs, offsets, reconstruction |
| AI layer | local or remote LLM | tool planning and explanations |
| File layer | 3MF/glTF/OBJ/PLY/STL | interoperability |

There is already a **native Rust port of Manifold**, and the Manifold project is specifically focused on robust, guaranteed-manifold mesh operations. Its Python package identifies the project as Apache-licensed. [GitHub](https://github.com/elalish/manifold?utm_source=chatgpt.com)

Rust also already has CAD-kernel experiments such as Truck, which implements B-rep/NURBS concepts under Apache 2.0. [GitHub](https://github.com/ricosjp/truck?utm_source=chatgpt.com)

And SolidCore is explicitly pursuing this same broader architectural thesis: a Rust geometry kernel containing NURBS, curves, surfaces, topology and intersections beneath SolidCraft. [SolidCraft](https://solidcraft.io/solidcore/?utm_source=chatgpt.com)

### One licensing warning

I would **not** architect MeshFixer around MeshLib unless you're happy with its commercial licensing model.

It is technically extremely relevant—repair, booleans, remeshing, offsets, point clouds, ICP, voxels—but its current repository describes it as source-available for non-commercial/educational use, with commercial use requiring a licence. [GitHub](https://github.com/meshinspector/meshlib?utm_source=chatgpt.com)

For something following the SolidCraft philosophy, I'd rather build around genuinely permissive components and contribute missing primitives upstream.

---

# 3MF should become the default, not STL

This is another place where a modern replacement should break with history.

STL should be supported forever.

But it should not be the preferred internal or interchange format.

Manifold's own documentation makes the point well: STL loses topology, whereas formats like 3MF can preserve solid/manifold structure and additional properties. [GitHub](https://github.com/elalish/manifold?utm_source=chatgpt.com)

A MeshFixer project might retain:

```text
geometry
source capture
units
materials
texture
repair history
selections
text objects
surface constraints
manufacturing metadata
original source
```

Then export a flattened STL only when somebody explicitly needs one.

---

# Where “MeshFixer” becomes much more than Meshmixer

There are several modern workflows Meshmixer never really had.

| Workflow | MeshFixer interpretation |
|---|---|
| Broken phone scan | “Recover the object” |
| AI-generated model | “Make this physically manufacturable” |
| Reverse engineering | “Recognize planes/cylinders/holes and restore intent” |
| Mobile sculpt | “Clean topology and dimension important features” |
| Orthotics/cosplay | “Build something fitting this surface with clearance” |
| Multi-part model | “Find contacts, align, key and join” |
| Damaged physical component | “Mirror/reconstruct missing region from surviving geometry” |
| Full-colour scan | “Repair geometry without throwing away colour” |
| Gaussian splat | “Use visual capture to help reconstruct usable geometry” |

That last one is fascinating.

Gaussian splats are great visual representations but generally terrible manufacturing representations.

So imagine loading a scan containing:

**images + camera poses + splat + rough mesh**

MeshFixer can use the high-quality appearance data as context while rebuilding the actual surface.

That is a genuinely 2026-native workflow.

---

# Printability should become a constraint system

Meshmixer had useful thickness/stability analysis.

MeshFixer should go much further.

Instead of merely reporting:

> wall = 0.42 mm

the user can set:

> **Manufacturing target:** FDM, 0.4 mm nozzle  
> Minimum structural wall: 1.2 mm  
> Minimum embossed feature: 0.6 mm  
> Minimum hole diameter: 2 mm

Then visual warnings become contextual.

Even better:

> **Fix thin walls automatically**

with a deviation preview showing what geometry will change.

Similarly:

> Resin hollowing  
> minimum shell 2 mm  
> two drain holes  
> don't alter external surface

That is much closer to what people actually want.

---

# Selection remains absolutely crucial

One Meshmixer lesson I would preserve almost verbatim is **paint selection**.

You want:

brush selection  
surface lasso  
connected component  
similar curvature  
same orientation  
geodesic grow/shrink  
paint-through toggle  
feature-boundary stopping

Because the universal MeshFixer gesture becomes:

> **select the bad bit → tell MeshFixer what you want**

That is a very powerful abstraction.

---

# Non-destructive history changes everything

Meshmixer was largely destructive.

MeshFixer should maintain an operation graph/history:

```text
Import
 └─ Remove Scan Base
     └─ Remesh
         └─ Smooth selected region
             └─ Emboss Text
                 └─ Hollow
```

Click “Remesh” later and adjust it.

Everything downstream recomputes.

Not full parametric CAD.

Just enough **procedural memory** to make experimentation safe.

This is also what makes LLM control sane: an AI agent can manipulate explicit operations rather than irreversibly mutating triangle soup.

---

# I would expose the engine as a CLI/API from day one

This is another place to copy the philosophy behind SolidCore.

For example:

```bash
meshfix inspect scan.glb
meshfix repair scan.glb --preset phone-scan
meshfix remesh head.obj --target-edge 0.4mm
meshfix emboss part.3mf --text "MK4" --depth 0.8mm
meshfix make-solid broken.stl
meshfix compare original.ply repaired.ply
```

And internally:

```rust
let mesh = Mesh::load("scan.glb")?;

let repaired = RepairPipeline::new(mesh)
    .remove_small_components(2.0)
    .close_small_holes()
    .remesh(RemeshTarget::EdgeLength(0.4))
    .make_manifold()
    .run()?;
```

Then desktop MeshFixer is simply the best client for the same engine.

That gives you automation, plugins, scripting, MCP/agent control, server usage, testing and eventually integration into slicers.

---

# Someone has started moving into this space already

This is worth knowing.

A very new project called **MeshRight** appeared in September/October 2026. It explicitly describes itself as a free open-source Meshmixer alternative aimed at scan and print repair. Its current early-alpha feature set includes diagnostics, scan cleanup, hole filling, hollowing, fitting parts to scans, colour-aware 3MF export, an AI interface and MCP integration. [GitHub](https://github.com/Huzy85/MeshRight?utm_source=chatgpt.com)

It currently has a tiny project footprint—GitHub showed only a couple of stars and a few dozen commits when I checked—so I wouldn't interpret it as “the opportunity is gone.” [GitHub](https://github.com/Huzy85/MeshRight?utm_source=chatgpt.com)

I would interpret it as **independent validation that people are arriving at the same conclusion**.

But it also means “Meshmixer except maintained” isn't enough of a thesis.

Your differentiation should be deeper:

**native high-performance desktop app + serious geometry engine + scan-native workflow + non-destructive operations + excellent direct manipulation + AI orchestration.**

---

# The product identity I would pursue

Not:

> Open-source Meshmixer clone.

And not:

> Blender, but easier.

And definitely not:

> ChatGPT for STL files.

I'd position it as:

> **MeshFixer — turn imperfect 3D data into usable geometry.**

Drop in almost anything.

A model downloaded from the internet.

A broken scan.

An AI-generated object.

A point cloud.

A sculpt.

A badly exported STL.

Then inspect it, repair it, modify it and make something from it.

That is a much larger and more durable category.

---

# What I would actually build first

For an initial release, I'd resist sculpt brushes, lattices, full texturing and sophisticated AI reconstruction. The first version has to earn trust as a **geometry utility**.

1. **Fast native viewport + import/export.** STL, OBJ, PLY, GLB and 3MF; million-triangle files must feel ordinary.
2. **Mesh Inspector.** Boundaries, self intersections, non-manifold edges, components, normals, dimensions, thickness and triangle density.
3. **Selection system.** Brush, lasso, connected region, grow/shrink and feature-aware selection.
4. **Repair core.** Orient, weld, remove degenerates, remove islands, fill holes, manifold repair and Make Solid.
5. **Basic direct edits.** Plane cut, erase/fill, smooth, flatten, offset, boolean and remesh.
6. **Scan workflow.** Remove pedestal/table, denoise, isolate subject, reduce, reconstruct and scale.
7. **Surface text.** Emboss/deboss/wrap.
8. **Print preparation.** Hollow, drainage holes, orientation, split and 3MF.
9. **Operation history.**
10. **AI command layer.** Only once all preceding operations have deterministic tool APIs.

That is enough to become useful very quickly.

And critically, every single operation in that sequence becomes both a UI command and an agent-callable primitive.

---

## The architectural principle I'd make non-negotiable

**The AI never edits the mesh directly.**

It can inspect derived properties.

It can look at rendered views.

It can select tools.

It can calculate parameters.

It can construct an operation graph.

But the resulting model always comes from deterministic geometry code.

That gives you reproducibility:

```text
"Repair my scan."
        ↓
LLM interpretation
        ↓
validated operation plan
        ↓
deterministic geometry engine
        ↓
visual diff
        ↓
user accepts
```

And every AI action can be reproduced without the AI later.

That is the model I think serious creative/engineering AI software will converge upon.

---

## Where I'd take MeshFixer long term

The genuinely ambitious destination is **not a mesh editor at all**.

It is a universal geometric translation layer between captured/generated geometry and designed/manufacturable geometry:

**real world → scan → point representation → reconstructed surface → repaired geometry → semantically meaningful geometry → printable/manufacturable object**

SolidCraft attacks the problem from the CAD/B-rep side.

MeshFixer could attack it from the opposite end:

**“I don't have pristine CAD. I have this mess. Help me turn it into something.”**

That feels like an unusually good open-source project thesis right now.

And the pun is good enough to keep the name.


---
Ok that's a decent thesis - a couple of items 3FM is good but we should support STL / OBJ / STEP files if possible
Tooling we do need to support tooling, agree not the laundry list that existed before, like who needs bunnies, but certainly I know remeshing and simplyfiying was a big thing as well as many others.

A couple of items I would like to see if a history ability to step back, change, carry forward, like fusion does. 
Exporting obviously is important
Pattern identification is huge
For LLM parts I do want us to support LLM controlling things either MCP or CLI, perhaps by surface or object tagging maybe combing human and LLM - say human selects a surface and asks LLM to smoothen this out and it does. 
Adding champers and rounded corners or hulls etc.. would be next level. Connect the multiple edges selected .. 

Also we should design it to be extendable - everything should be a plugin - lets borrow from the deepseek harness or pi coder school of design 

As for desktop, how sure are you we want browser based vs compiled?

----

I think these changes make MeshFixer substantially more interesting. And you've convinced me to reconsider my original desktop recommendation.

I would now lean toward a compiled, Rust-native desktop application, with a plugin-first engine and a shared command system used by the UI, CLI, MCP and AI agents.

The key architectural decisions I'd make are:

1. Native first, not browser first. Use Rust and GPU-native rendering; preserve the option to build a web client later.
2. History is foundational. Every modification becomes an editable operation that can be revisited and recomputed, rather than just an Undo action.
3. Everything is a plugin. Remeshing, simplification, booleans, text, pattern recognition, import/export and AI should use the same extension system.
4. Selection is a first-class object. Humans and agents can identify, tag and act on faces, edges, regions and entire objects.
5. Mesh and CAD geometry coexist. STL, OBJ, 3MF and STEP are essential, but STEP requires a genuine B-rep strategy rather than treating everything as triangles.

Let me unpack how I'd build this, including where the technical difficulty actually lies.

# 1. Native desktop versus browser-based

This is probably the first decision to settle, because it influences the viewport, plugin system and UI architecture.

Native Rust

Rust + wgpu + egui/Slint

My preferred direction

Excellent control over GPU resources, large geometry buffers, native plugins, background computation and operating-system integration.

Web-based desktop

Tauri + React + WebGPU

Faster iteration for sophisticated panels, plugin interfaces and AI workflows, but with browser-engine differences and additional UI/backend coordination.

### How I'd compare them for MeshFixer

| Requirement                    | Native Rust              | Tauri/web UI                            |
| ------------------------------ | ------------------------ | --------------------------------------- |
| Large mesh viewport            | Excellent control        | Good, with WebGPU caveats               |
| Multi-million triangle files   | Excellent potential      | Excellent potential with native backend |
| GPU compute                    | Direct through wgpu      | Possible, more integration work         |
| Native dynamic plugins         | Straightforward via host | Possible via Rust backend               |
| Dynamic plugin UI              | More work                | Easier                                  |
| Cross-platform desktop         | Good                     | Good                                    |
| UI development velocity        | Moderate                 | High                                    |
| Browser/mobile client later    | Separate work            | Easier reuse                            |
| Native look and feel           | Depends on toolkit       | Web-styled                              |
| Access to C++ geometry kernels | Direct FFI               | Direct via Rust backend                 |

A critical point: Tauri is not intrinsically slow for geometry. If the mesh processing happens in Rust and the viewport uses GPU rendering, the overhead of a web UI may be largely irrelevant for many operations.

The bigger concern is consistency and control over the graphics subsystem.

Tauri uses platform webviews: Chromium-based WebView2 on Windows and WebKit on macOS and Linux. That introduces different graphics and browser capabilities across platforms.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://tauri.ubitools.com\&sz=32)

Tauri



For a professional geometry application, I would rather own the render loop.

### My proposed native stack

MeshFixer Desktop

Rust executable · Windows / macOS / Linux

egui + docking

Tool panels, tree, inspector, history

wgpu renderer

Meshes, clouds, picking, overlays

Command bus

Unified commands and transactions

Plugin host

Features, imports, tools, AI

Rust Geometry Core

Document model · History · Selections · Meshes · B-reps · Algorithms

I'd seriously evaluate two UI choices:

- egui + wgpu: pure Rust, relatively easy custom viewports, attractive for a tool-heavy editor and fast prototyping. `egui-wgpu` is maintained and explicitly supports desktop and web rendering backends.&#x20;

  [image](https://www.google.com/s2/favicons?domain=https://docs.rs\&sz=32)

  Rust+1

- Slint + wgpu: potentially more polished declarative desktop UI, with a native toolkit and several renderer options. Its licensing needs review before selecting it for a permissively licensed project.&#x20;

  [image](https://www.google.com/s2/favicons?domain=https://docs.slint.dev\&sz=32)

  Slint Docs+1



I would prototype the viewport with egui + wgpu first.

Its immediate-mode interface model is not glamorous, but for a CAD-like utility with continuously changing diagnostics, tool handles and selection properties, it's a sensible fit.

The important caveat is that the renderer must be independent of egui. That keeps it usable for headless previews, automated tests and future alternative frontends.

# 2. File formats: absolutely support STEP

I'd distinguish between three different promises: importing a format, editing it without losing useful information, and exporting it correctly.

| Format            | Import    | Export    | Important considerations                                        |
| ----------------- | --------- | --------- | --------------------------------------------------------------- |
| STL               | Yes       | Yes       | Units, solid connectivity and metadata are not reliably encoded |
| OBJ + MTL         | Yes       | Yes       | UV coordinates, materials, texture references                   |
| 3MF               | Yes       | Yes       | Units, materials, objects and print metadata                    |
| STEP (.step/.stp) | Yes       | Yes\*     | B-rep geometry, assemblies, analytic surfaces                   |
| PLY               | Yes       | Yes       | Point clouds, normals, colours                                  |
| GLB/glTF          | Yes       | Yes       | Textures, materials, scenes                                     |
| OFF/XYZ/PTS       | Desirable | Desirable | Geometry and point-cloud exchange                               |

STEP export needs a qualification.

A STEP file typically contains geometrically meaningful faces and surfaces, not just a triangle soup.

If we import STEP and immediately tessellate it, we can display it and modify its mesh, but we lose the original analytic surfaces.

Exporting the result to STEP doesn't magically restore proper CAD geometry.

So we should keep the original B-rep when possible, alongside a tessellated display representation.

STEP / B-rep source

Faces · edges · curves · NURBS · analytic surfaces

B-rep model

Precision editing

Mesh representation

Rendering and mesh tools

Representation-aware command engine

Chooses mesh, B-rep or volume operations

For initial STEP interoperability, I would strongly consider Open CASCADE Technology (OCCT), which already has mature STEP facilities and B-rep modeling operations. It's LGPL 2.1 with an additional exception, so we'd need to comply with its license rather than pretend it's equivalent to MIT or Apache.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.occt3d.com\&sz=32)

OCCT3D



SolidCore could become an alternative in the longer term, but its own site describes the kernel as still under development. I wouldn't make a working STEP pipeline depend on functionality it hasn't delivered yet.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://solidcraft.io\&sz=32)

A geometry kernel written in Rust



# 3. Fusion-style history, not just Undo

This is a major architectural commitment, and one I would make from day one.

I think MeshFixer needs both:

- Undo/Redo: reverse a recent action.
- Editable history: go back to an earlier operation, change its parameters and automatically rebuild everything downstream.

Here's how that might look inside the app.

MeshFixer · History prototype

Interactive example

Import scanSource

Remove pedestalRepair

RemeshGeometry

Smooth selected regionModify

Emboss labelSurface

Make solidRepair

Remesh

Target edge: 0.45 mm

Target edge length

## 0.45 mm

Recompute downstream

Conceptual UI only: this changes a simulated operation graph, not real geometry.

When someone changes an earlier remesh from 0.45 mm to 0.25 mm, the engine recomputes the operations that depend on it.

The difficult bit is that mesh topology changes destroy naïve face and edge references.

Suppose someone:

1. Imports an OBJ.
2. Selects 200 faces.
3. Remeshes the object.
4. Embosses text onto that selected region.
5. Changes the remesh settings.

Those original 200 faces may no longer exist.

Fusion-style history becomes genuinely hard here.

### The solution: persistent semantic selections

Rather than storing only raw triangle IDs, MeshFixer should support several selection identities:

| Selection type    | How it survives changes                        |
| ----------------- | ---------------------------------------------- |
| Object            | Persistent object UUID                         |
| B-rep face        | Feature/face reference with geometric matching |
| Mesh region       | Anchors, surface coordinates, spatial matching |
| Curvature feature | Re-evaluate geometric predicate                |
| Cylinder          | Fit primitive and track axis/radius            |
| Named selection   | Re-evaluate or reproject saved region          |
| Painted region    | Preserve painted surface field and transfer it |

Imagine a user tags a region:

`outer_rounded_corner`

The tag refers to more than a set of triangle IDs. It may include an approximate normal, location, curvature fingerprint, boundary and region anchors.

When the mesh changes, MeshFixer attempts to recover the selection.

If the result is ambiguous, it should flag the downstream operation instead of silently applying it somewhere else.

This is where MeshFixer can build a meaningful advantage over traditional mesh editors.

### History should also support branching

You might want two alternative repairs from the same original scan.

I would separate the model into a dependency graph and a linear-looking history interface. The graph supports branches, while the interface presents the active sequence simply.

That means it's possible to compare different remeshing algorithms without losing all your downstream work.

# 4. The tools we actually need

I agree completely about avoiding the old laundry list. But that doesn't mean a minimalist geometry editor.

It means a small set of powerful, composable tools.

I'd organize them into six families.

[Localized Remesh - Developers Help - Mmg forum](https://images.openai.com/static-rsc-4/dUe7XVqI_VmsUJqGI-ZcC5FqIqKEXoLQbsqPxwXH4pICShdVaoXfDzFXtCVo9m4T1O1UaSBclj6t_uGgOZpGIQEdtqKpcP1Pmw1xtbPQgk05Grsnjk6JDnUeSQ8K54_tzGy4y8Lx5vqvslZvAP42p9sAwNeYiA2axmm1hSg4Zj0?purpose=inline)

Mesh processing

Remesh (uniform, adaptive, isotropic), simplify/decimate, subdivide, smooth, denoise, weld, fill holes, recalculate normals, remove self-intersections, retopologize regions and make solid.

[New Custom Feature: Selection Fillet — Onshape](https://images.openai.com/static-rsc-4/-Uqybht7OQvYKcy8Dvk4wYMdHgGgIrGp2WjSQm0wokh-aaWKG88gdrU1JO0iY2FS7a0t2jbnOpqSEDPELfMb69onzLKL4I0jPraU36i-3LeavFhgwKa6Pq-3Kyl6ivtbMzU8DKotwDi4JtGxC5LrZ_MoPDZCJqHxygeSL98przQ?purpose=inline)

Edge and surface construction

Chamfer, fillet, bevel, bridge edge loops, loft between boundaries, connect selected edges, offset, shell, thicken, extrude face, sweep, cut, boolean and blend surfaces.

[Artec Studio 20 : Rétro-ingénierie | Solutions de scan professionnelles | Artec3D](https://images.openai.com/static-rsc-4/fTZJoD0MsrZWk4HsDHEZ6jEwfqcKwH8E4RL2IlzHVEHpAPbpOb72W5DVyO15kMxlF26TpyBXqY2yUyDWzK7ZLgVmnqtOLQCmgotvNlalQn1Y109_Txw0Psa62PFBfWmJyXGqruysDLF-wdg_IaHFam-C8EkhwNgKVf2wezHpKYs?purpose=inline)

Pattern and feature recognition

Detect planes, holes, cylinders, circles, symmetry, repeating features, regular spacing, mirrored parts, edge loops, boundary patterns and probable manufactured geometry.

[Gelöst: Projecting text to a cylinder. - Autodesk Community](https://images.openai.com/static-rsc-4/CjPjt3mltkl-CZ6fCY8vtcQGDJ5MRpWuWI4WCy_JJB7dO7OsMg0eFt48yRWZQz6heSaQLkxbXqTzPXm58TXbIj9Ve_2VPwsXCjQOa2fAvylDowR7hKPY_TwV0HZilyZCMJyREvvMk7yIJKdljGdAmxahpkleqVuRLZorqTkapJE?purpose=inline)

Surface detailing

Emboss/deboss text, conform SVG/logos, project patterns, texture displacement, surface wrapping, localized sculpting and colour/material painting.

[Redesign and fix 3d models, stl scan cleanup for 3d printing by Johnson101\_ | Fiverr](https://images.openai.com/static-rsc-4/ZSUL3KXVLElnQcBYq4GwhySJCnUK8zxYTkOrhIdrENCRH5uAhvvN8pH5KkwjVjhJ5omzQG9Nxdv9nlG4JzgIbvm2rdcUKKRm4S-TvToEQg1SsdO-Y_GLZf_YDJ742K7ma2vEwccvIxk_iT0wXF7-cI6Qys9GhiGR2fublDJbJa4?purpose=inline)

Scan and repair

Remove background geometry, isolate components, align scans, fit primitives, reconstruct missing areas, transfer colour and clean point clouds.

[Customize Parts and Increase Efficiency With New PreForm Features & Shareable Print Settings | Formlabs](https://images.openai.com/static-rsc-4/IfSeyW6iGG56DIYxo1A_7VNS6SOuZcm5k_5BiTD06ynMK1J9UoWzxxS95uP0JxKfxhtZcmTpXLxcq9_E3-Hm4_wChdDcC91kIWjDSX6KynJczJqpvbiIEviW8IftOwva97GIXzoJRzT3zC1ju2-PeCwHwmS7qFe-BJOqxKBEYpU?purpose=inline)

Manufacturing

Orient, split, add alignment keys, hollow, create drainage holes, analyze wall thickness, check clearances, measure and export print-ready geometry.

### Chamfers, fillets and edge connections are especially important

These should be distinct operations.

A chamfer cuts a flat bevel across an edge. A fillet creates a rounded transition. A bridge connects two boundary loops. A loft constructs a surface through profiles. A shell creates a wall-thickness structure.

They are different mathematically and their behavior depends on whether the source is an analytic CAD solid or an arbitrary triangulated surface.

For a STEP solid, a true 2 mm fillet is a B-rep modeling operation.

For a scanned OBJ, it may require detecting an edge feature, defining a transition region, reconstructing local geometry and remeshing.

We should expose these as common user commands but allow distinct implementations.

One tool, multiple backends:

```
Fillet selection
     |
     +-- B-rep edge → analytic fillet
     |
     +-- Mesh edge loop → local bevel/remesh
     |
     +-- Open boundary → surface blend
     |
     +-- Unrecognized region → offer reconstruction
```

This avoids a UI full of obscure, nearly duplicate tools.

# 5. Pattern recognition deserves its own subsystem

I agree this is huge. In fact, I'd elevate it from a tool to a shared service that other tools can depend upon.

For example, MeshFixer should identify patterns such as:

- Six holes arranged around a bolt circle.
- A repeated hexagonal lattice.
- Four identical mounting bosses.
- A mesh that's approximately symmetric about a plane.
- Several concentric cylindrical features.
- A circular feature repeated at regular intervals.

A recognition engine should expose these as candidate features, each with a confidence score and measured geometric error.

Pattern Recognition

Illustrative result

Circular hole pattern

6 features · 60° spacing

High confidence

Recognized as a repeated feature, rather than six unrelated holes.

Once identified, the pattern can become an editable object:

> Change this six-hole pattern to eight holes.

Or:

> Make all mounting holes 4 mm.

Or:

> Restore the two damaged features using the remaining four.

CGAL already has Efficient RANSAC and region-growing implementations for recognizing primitive shapes including planes, spheres, cylinders, cones and tori. That's a useful foundation for basic feature recognition.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://doc.cgal.org\&sz=32)

Shape Detection: User Manual



But recognizing a repeated mechanical design feature is a higher level of abstraction than recognizing mathematical primitives.

I would create a `feature-recognition` service that can aggregate primitive detections, symmetry information and dimensional relationships into patterns.

The output should be accessible to both the user and the LLM.

# 6. Everything is a plugin — borrow from DeepSeek Harness and Pi

This is probably the most important design idea you've introduced.

I've checked both architectures, and there's plenty worth borrowing.

DeepSeek Harness uses Cordis: plugins contribute services, typed events and reversible registrations. Its model adapter, session log, tools and even agent loop are plugins.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub+1



Pi coding agent lets TypeScript extensions register LLM-callable tools, commands, event handlers and UI components; its architecture makes extensions exceptionally easy to write and reload.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://pi.dev\&sz=32)

Pi+1



My interpretation for MeshFixer would be:

The core owns the rules. Plugins provide capabilities.

The irreducible core would include document identity, geometry representation contracts, selection references, history transactions, scheduling, plugin lifecycle and data serialization.

Almost everything else becomes a plugin.

MeshFixer Application

Native UI · CLI · MCP · Scripting

Command Registry

Discover · Validate · Execute · Preview · Undo

Mesh Tools

Scan Tools

CAD Tools

Pattern Recognition

Import / Export

AI / MCP

Minimal Rust Core

Document · Geometry contracts · History DAG · Selections · Plugin runtime

### A plugin registers capabilities, not just buttons

Imagine a remeshing plugin:

```
pub fn register(ctx: &mut PluginContext) {
    ctx.register_command(RemeshCommand::new());
    ctx.register_panel(RemeshPanel::new());
    ctx.register_analyzer(MeshDensityAnalyzer::new());
}
```

Its command definition could declare:

```
Command: mesh.remesh

Inputs:
  target: MeshObject | SurfaceSelection
  algorithm: isotropic | adaptive
  target_edge_length: Distance
  preserve_boundaries: Boolean
  preserve_features: Boolean

Outputs:
  GeometryRevision
  TopologyMapping
  QualityReport

Capabilities:
  preview
  history
  headless
  cancellation
```

That last part is crucial. The plugin must describe its capabilities in a machine-readable way.

The command can automatically become available through the desktop UI, CLI, agent tool registry and MCP.

We shouldn't have to implement four versions of the same capability.

### But I would not use Rust dynamic-library plugins as the only extension model

Rust does not offer a stable general-purpose ABI between independently compiled dynamic libraries.

Instead, I'd support three classes of plugin:

| Plugin type      | Best for                                          | Isolation                     |
| ---------------- | ------------------------------------------------- | ----------------------------- |
| Built-in Rust    | Performance-critical geometry algorithms          | In-process                    |
| WASM component   | Portable third-party tools and automation         | Sandboxed, capability-limited |
| External process | Python tools, AI, OCCT adapters, heavy algorithms | Process isolation             |

Eventually, there could be native binary plugins through an explicitly versioned C-compatible ABI, but these would be trusted plugins requiring careful compatibility management.

TypeScript plugins could also run in a restricted script runtime for orchestration and UI extensions.

The guiding idea is that a contributor should be able to create a useful operation without rebuilding the whole app.

For example:

```
meshfixer-plugins/
  remesh/
  simplify/
  hole-repair/
  fillet/
  edge-bridge/
  feature-recognition/
  step-import/
  text-emboss/
  agent-control/
```

We should also support plugin-defined UI panels, file handlers, context-menu actions, keyboard shortcuts and diagnostics.

One extra requirement: plugins must be versioned and reproducible. A saved history operation needs its plugin ID, version, parameters, representation requirements and a stable output contract. If the plugin is subsequently missing, the document must still load, preserve the operation and explain what cannot be recomputed.

# 7. Human and LLM collaboration should use the same selection model

This could be MeshFixer's defining interaction model.

Let's say you import a scanned object and select one face, a collection of polygons, or a region using a paint brush.

The selection is stored in the current document session.

You type:

> Smooth this area, but preserve the sharp edge along the top.

The agent receives structured context, not merely a screenshot:

```
{
  "document": "scan-001",
  "selection": {
    "id": "sel-034",
    "type": "surface_region",
    "object": "body-001",
    "area_mm2": 1850.0,
    "boundary": "feature_aware"
  },
  "instruction": "Smooth without altering sharp upper edge"
}
```

It can inspect the region, choose the appropriate smoothing algorithm and propose parameters.

Then it invokes the same `mesh.smooth_region` command a human would use.

Human + Agent editing flow

Interactive concept

[Autodesk fusion 360 3d printing](https://images.openai.com/static-rsc-4/_XBvycMTRRVsRXESQ-K38aOLIBXPbSVU5c8dq-1J_GBOPBFlzwVFAgYI1JQzjfo0JYAeD8n3qlr-l0l1Aai3GUQArszzZ0GpifGWRSzUGCCh8kto_19D0GSHVQtAKL47xSb5jdK869A0X6wQLNMpsuNs-Dm6F0aD3GJpQ_rpmUo?purpose=inline)

[3dcentroamerica.com](https://3dcentroamerica.com/misc/autodesk-fusion-360-3d-printing.html?utm_source=chatgpt.com)

Selected region: upper_shell

Surface selection · 1 object · simulated UI

Smooth the selected region, preserving sharp edges

Preview agent operation

The AI should also understand selection tags in natural language:

> Round the upper housing edges.

> Identify the four mounting holes.

> Select everything with similar curvature to this face.

> Find the two broken cylinders and repair them using the intact cylinder.

That implies first-class APIs for discovery, selection and inspection, not merely geometry modification.

### The essential agent API

I'd want operations like:

```
document.list_objects
geometry.inspect
selection.from_screen_point
selection.get_active
selection.save_as
selection.find_similar
features.detect
features.list
history.list
history.modify
geometry.preview
geometry.apply
document.export
```

And yes, both CLI and MCP should be built on the same command bus.

CLI is especially valuable because it allows scripting and testing without running the full application.

MCP is valuable because compatible LLM applications can interact with an open MeshFixer document.

A typical command could look like:

```
meshfix select --object body \
  --feature cylindrical_holes

meshfix fillet \
  --selection current \
  --radius 2mm \
  --preview

meshfix export \
  --format step \
  --output repaired.step
```

One important design rule: an agent should use document-revision preconditions. If a user edits the model while an agent is calculating, the agent must not silently apply changes to the wrong revision.

And AI-controlled destructive operations, exports and external-script execution should be permissioned appropriately.

# 8. What the actual desktop might look like

I'd keep a professional, compact interface that resembles a geometry editor rather than a consumer AI application.

MeshFixer

housing-repair.mfx

Select

Repair

Remesh

Simplify

Fillet

Bridge

Pattern

Export

Objects

▾ Housing

　Body

　Top edge

　Holes × 4

Selections

upper_shell

mounts

[How do I round the corners of filleted edges? — Onshape](https://images.openai.com/static-rsc-4/_giUsuz8fFocai5JjunFHHDzfun1YyS2u9XmtgzubRAhoO55MBHRjWvcWgoxqRwmV1I4askVp0Qpn2PbIusZYF8RApu9REnb9HG2YTcybSoW1Fmgkd7Bu9gQgB9p_9Gsl-C7is48Jm2sj9pulCPMUfzUgsobj3AfqcpGD0S2zIc?purpose=inline)

Inspector

Triangles

486,204

Components

1

Boundaries

0

Fillet

Radius

2.0 mm

Preview

History

Import

Repair

Remesh

Fillet

Ask MeshFixer to modify the current selection…

Early interface direction, not a functional mockup. The specific CAD imagery is illustrative.

The UI deliberately puts the command toolbar, viewport, selection tree, inspector and history in view simultaneously.

I'd make the conversational interface collapsible rather than central.

Selection, operation preview and history should remain the primary interaction surfaces.

# 9. The architecture I would build

My current preferred stack is:

| Component             | Choice                                          |
| --------------------- | ----------------------------------------------- |
| Language              | Rust                                            |
| Desktop runtime       | Native                                          |
| GUI prototype         | egui + docking                                  |
| GPU renderer          | wgpu                                            |
| Document model        | Versioned scene/geometry graph                  |
| Geometry engine       | Plugin-driven hybrid mesh/B-rep/volume          |
| Mesh booleans         | Manifold                                        |
| STEP and analytic CAD | OCCT adapter initially                          |
| Point clouds          | Native data model, Open3D adapter where useful  |
| Shape recognition     | RANSAC + region growing + feature relationships |
| Plugin orchestration  | Small Cordis-inspired Rust service registry     |
| Portable plugins      | WASM Component Model                            |
| External plugins      | Versioned process RPC                           |
| Scripting             | CLI first, optional TypeScript/Python           |
| Agent integration     | MCP + command schema                            |
| Native project format | `.mfx`                                          |
| Exchange formats      | STL, OBJ, 3MF, STEP, PLY, glTF                  |

I would keep `.mfx` distinct from 3MF. A project format needs to preserve the full operation graph, plugin references, selections, source data and possibly alternate geometric representations.

3MF remains a preferred manufacturing exchange format rather than being forced to serve as an editable-history container.

### Suggested Rust workspace

```
meshfixer/
  crates/
    mf-core/
    mf-document/
    mf-geometry/
    mf-history/
    mf-selection/
    mf-commands/
    mf-plugin-api/
    mf-plugin-host/
    mf-render/
    mf-desktop/
    mf-cli/
    mf-mcp/

  plugins/
    mesh/
      remesh/
      simplify/
      repair/
      smooth/
      bridge/
    cad/
      fillet/
      chamfer/
      shell/
      step/
    recognition/
      primitives/
      symmetry/
      patterns/
    scan/
      cleanup/
      reconstruction/
    surface/
      text/
      emboss/
    io/
      stl/
      obj/
      3mf/
      ply/

  tests/
    geometry-corpus/
    history-replay/
    plugin-compatibility/
    import-export/
```

There is a serious testing opportunity here too. Every geometric operation should have property-based and adversarial tests: watertightness, orientation, finite coordinates, tolerances, volume preservation where appropriate, round-trip export and history replay.

For an open-source geometry product, the test corpus itself could become an important community asset.

# 10. How I'd prioritize the build

I would put more foundational effort into history and extensibility than in my previous proposal.

| Phase                   | Scope                                                                  | Exit criterion                               |
| ----------------------- | ---------------------------------------------------------------------- | -------------------------------------------- |
| Foundation              | Native viewport, document model, command bus, plugin registry, history | Import, select, modify, undo and replay      |
| Mesh toolkit            | Remesh, simplify, smooth, fill, repair, cut, booleans                  | Useful standalone STL/OBJ repair tool        |
| Selection + recognition | Persistent tagging, primitives, symmetry, patterns                     | Edits survive earlier history changes        |
| CAD interoperability    | STEP in/out, edge operations, fillet, chamfer, bridge, shell           | Hybrid workflows demonstrated                |
| Agent interface         | CLI, MCP, inspection, selection, preview, agent-controlled commands    | Agent can safely modify selected geometry    |
| Advanced workflows      | Scan reconstruction, texture transfer, surface text, print constraints | Strong differentiation from legacy Meshmixer |

Some of these can happen in parallel, particularly CLI/MCP protocol work and individual plugins.

The key is that history and selection tracking need to shape the earliest engine APIs. They cannot be cleanly bolted on later.

## The three biggest technical risks

First: topology identity across history operations. Remeshing changes the fundamental structure of a model. Keeping selections and downstream edits meaningful requires provenance mapping, feature references, reprojection and explicit failure states. This is arguably harder than building the initial viewport.

Second: mixing mesh and B-rep modeling. Operations such as accurate fillets, chamfers and STEP export have very different semantics on scanned triangle meshes versus analytic CAD geometry. We need clear representation tracking and honest export capabilities. In particular, a mesh wrapped in a STEP container is not equivalent to recovering its original parametric CAD surfaces.

Third: the plugin ABI and security model. An everything-is-a-plugin architecture becomes fragile if plugins can freely mutate the document, bypass history or crash the process. Each plugin should receive a scoped geometry snapshot, return a validated change set or new revision, and commit through the same transactional command system.

## The design choice I'd lock in

I would adopt this as the core principle:

> Every operation in MeshFixer is a discoverable, versioned, replayable command over a geometry document. Every command can be invoked by a person, a script or an agent.

That connects almost everything you've asked for.

Remeshing is a command. Simplifying is a command. Finding four holes is a recognition command. Tagging a surface is a selection command. Filleting an edge is a modification command. Exporting STEP is a document command.

History records the commands and their dependencies. Plugins provide the implementations. The UI provides excellent manipulation and previews. MCP and CLI provide machine control.

And my preference now is Rust + wgpu + egui for an early native prototype, with one qualification: before committing to the full application, I'd build a small technical spike comparing egui and Slint with the same renderer. Native rendering is the stronger bet, but UI productivity and plugin-panel extensibility still deserve measurement.

I think the next meaningful deliverable is a proper MeshFixer v0.1 architecture specification, defining the document model, operation/history graph, plugin contract, persistent selections and command API. Those five contracts will determine whether this becomes a genuinely extensible geometry platform or merely another collection of mesh tools.


----
We also need to think about performance, these types of applications just suck the memory and CPU / GPU out of desktops and laptops - we need to think through what sort of tips / tricks can we use to avoid that. What do other heavy processing applications with a highly complex view do? 

Also we need to make cross platform a design principle

---
I think performance and cross-platform support should become two non-negotiable architectural principles for MeshFixer, alongside extensibility and editable history.

And I would go further: MeshFixer should be designed to run comfortably on an ordinary laptop, not merely run quickly on a powerful workstation.

That changes how we design the geometry engine, render pipeline, history, plugin system and application lifecycle.

The biggest lesson from researching existing heavy applications is that they don't achieve good performance simply by having a fast renderer. They achieve it by not doing unnecessary work, not keeping unnecessary data in memory, and not recalculating things that haven't changed.

# 1. What can we learn from other applications?

There are five especially relevant examples.

[Unreal Engine 5 is set for an early 2022 release | PC Gamer](https://images.openai.com/static-rsc-4/CXgjoASgUwNfocsgCFGrNA7pZHuI0aIIa4gpp1SUgKlGr7wbJwMZR88p6gAhPswPUO5KmN60J9xnCbA3LbeEqiBF2N520ZjsP8RhV5qmhwnaIoszi_vBeLIjHF4HPX0K7-hmCBt4PWtkhoP-3TYPtRYFYsAO9auPzn4BDtPCzBQ?purpose=inline)

Unreal Engine 5 — Virtualized geometry

Its Nanite system divides meshes into hierarchical clusters, streams detail as needed and renders geometry appropriate to what's visible on screen. The full-resolution model doesn't need to be resident on the GPU at all times.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://dev.epicgames.com\&sz=32)

Epic Developer Community+1



MeshFixer lesson: render only the detail needed for the current view, without changing the actual source geometry.

[Using ParaView on Your Desktop](https://images.openai.com/static-rsc-4/-GWPHeVmjO10FpDQ5mzltg4OX8r2-xpCLn5Po-kaePmrQux8-BDo9fcv8icoR-TaTyPgyE7GmZGB1aPqBugp4Vsoxq50_TKgOjvmv031fZle83hde94s4cYKpnHDLG7paYZusMo_qV5z66y4mVNJU1y1Jat_OckQZSB8u0rW-2w?purpose=inline)

ParaView — Interactive versus final quality

ParaView deliberately uses different rendering quality while interacting and when stationary. It can also process datasets remotely when they exceed local hardware capacity.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.paraview.org\&sz=32)

ParaView Documentation 5.11.0 documentation+1



MeshFixer lesson: prioritize responsiveness while rotating, selecting and editing; refine the display when interaction stops.

[Voronoi 3d geo nodes - Modeling - Blender Artists Community](https://images.openai.com/static-rsc-4/OnptGdDXwgCu_0-B7e-tIYP2EV-M5ue2DGF-4znmDJEcb6MgpnVDJ-_a33jBlVN1MlhQoiYU_USfThZDxUa1Yr_Kkc4CW7GkYBfnAnIKlhMPgiyu8Rj29OvfJWE_DDV4VWxjNl1dNv1gxmnjYQMyzcxzwMhilpmuhb96E474krY?purpose=inline)

Blender — Simplification and instancing

Blender offers separate viewport quality settings, texture limits, GPU subdivision where supported and settings that restrict unnecessary detail. Its documentation also highlights minimizing repeated geometry evaluation.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.blender.org\&sz=32)

Blender 5.2 LTS Manual+1



MeshFixer lesson: previews, instancing, cache reuse and deferred evaluation are fundamental.

[Fusion 360 | Modelado de Superficies y Paramétrico | Autodesk](https://images.openai.com/static-rsc-4/Z-Ye-Lg85nlm9KpgRdMHgjqPHHVw6exi6Caf2VG8zQPnpgP2JmsRLqUPSPvKPJ5oyA9R5dHnEUnlYVevK7x6ZKzFDPJhgnSnvMiZVLnhd2BJxQnGNPB4Yk_5SZZG2QoqSmKaUdwJJvNA230_YneFJzIeUECVI3ILT-52XOFMja8?purpose=inline)

Autodesk Fusion — History can become the bottleneck

Autodesk identifies long timelines, complex imported geometry and unnecessary features among causes of poor performance. Its tools can identify expensive history operations.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.autodesk.com\&sz=32)

Autodesk+1



MeshFixer lesson: the operation graph needs incremental recomputation, cached results and performance diagnostics from the beginning.

[FlippedNormals: Comprehensive ZBrush Workflow](https://images.openai.com/static-rsc-4/oSDeTy8MDifUWi01p5KlMldabyRzW1694CYO8wIiruhn-ncBWNE8xo3sx18bPMYtA2yxW5J9hzF79IKfXt2x1wSUHwyh7IJueW2506bYHfWfq0uhi_s0pmIDDnVhdAXj7DjJUZJeklaDWl6bETk5p_ehYHQhut-srb5LLPx-5yQ?purpose=inline)

ZBrush — Edit at manageable resolution

ZBrush distinguishes interactive work from expensive final operations. Even its Live Boolean documentation warns that very high subdivision settings and display resolution affect responsiveness.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://help.maxon.net\&sz=32)

Maxon Documentation



MeshFixer lesson: do cheap, local previews first and commit high-resolution geometry only when needed.

We don't need to recreate Nanite or build a scientific visualization cluster. But these systems establish several patterns that are particularly applicable to MeshFixer.

# 2. Seven performance principles I'd build into the core

A. Separate source geometry from display geometry

The 25-million-triangle scan someone imports should remain intact, even if only 400,000 representative triangles are currently rendered. View quality and actual model accuracy must be independent.

B. Partition geometry into spatial chunks

Break very large meshes or point clouds into spatial regions. This allows localized operations, independent caching, selective GPU uploads and parallel processing.

C. Recompute only what changed

Editing the fillet at the end of a 50-operation history must not rerun the original scan reconstruction. The dependency graph should identify precisely what is invalidated.

D. Treat RAM and VRAM as managed caches

Memory usage should obey configurable budgets. Clean, reconstructable data can be evicted. Originals and saved operations remain recoverable from disk.

E. Prioritize interaction over background computation

Mouse movement, viewport navigation and selection outrank remeshing, analysis, previews and file indexing.

F. Adapt to the actual hardware

Integrated GPU, laptop battery, thermal limitations, memory pressure and high-DPI screens should influence rendering and worker scheduling.

G. Expensive plugins must not destabilize the editor

A plugin that leaks memory, hangs or performs a very expensive geometry operation should be interruptible or isolated where practical. A failed operation must not corrupt the current document.

# 3. The most important optimization: geometry virtualization

Imagine importing a detailed phone scan of a statue containing 30 million triangles.

A naïve editor may maintain the original mesh, an editable topology structure, a selection structure, a GPU copy, an acceleration structure and several Undo snapshots.

The apparent 30-million-triangle model can therefore consume gigabytes.

Even a basic representation illustrates the problem.

Illustrative memory cost for 30 million triangles

Triangles

## 30 million

| Index buffer (3 × u32)                               | 0.36 GB |
| ---------------------------------------------------- | ------- |
| Expanded positions + normals (3 vertices × 24 bytes) | 2.16 GB |
| Combined illustrative GPU buffers                    | 2.52 GB |

Decimal GB, uncompressed example. A true indexed mesh shares vertices, so its actual usage depends on vertex count and attributes. These numbers exclude history, topology, textures, acceleration structures and CPU copies.

That is why blindly throwing data at the GPU is not the answer.

## Proposed strategy: a multiresolution mesh hierarchy

One source mesh, multiple working resolutions

Source · 30M triangles

Full precision

Detailed viewport · 3M triangles

Interactive viewport · 300K triangles

Distant thumbnail · 30K triangles

Example levels, not prescribed thresholds. The renderer should select detail according to projected screen error, visibility and memory budget.

When the user zooms into a small region, the renderer can load the detailed chunks for that area and show simpler geometry elsewhere.

When the user paints a selection, picking needs to resolve against the authoritative geometry, rather than treating the simplified display mesh as ground truth.

This distinction is essential for precision editing.

There are two implementation levels:

V0.1: Generate several levels of detail per object. Use simplified geometry while interacting and higher detail when stationary.

Later: Introduce hierarchical mesh clusters with streaming and culling, following the broad ideas behind Nanite.

I'd avoid making a complete Nanite-like renderer a prerequisite for the first release. That is a substantial project by itself.

# 4. Localized operations instead of rebuilding the entire object

A major opportunity lies in making geometry operations spatially incremental.

If a user selects a 10 mm region on a 300 mm scan and asks MeshFixer to smooth it, the engine should ideally:

1. Identify the affected triangles.
2. Expand the region to include the algorithm's required boundary neighborhood.
3. Operate on that subset.
4. Reconcile the boundary with the untouched mesh.
5. Update only affected spatial indexes, caches and GPU buffers.
6. Invalidate downstream operations that depend on the changed region.

Not every operation can work this way. A global manifold repair or full Poisson reconstruction may legitimately require processing the entire object.

So each plugin should declare its computational scope.

| Scope          | Example                                                         |
| -------------- | --------------------------------------------------------------- |
| Region-local   | Smoothing, painting, selected-face offset                       |
| Neighborhood   | Fillet, bevel, local remesh                                     |
| Object-global  | Simplification to target triangle count, some repair operations |
| Scene-global   | Collision analysis, assembly alignment                          |
| Reconstruction | Poisson reconstruction, some Make Solid algorithms              |

This scope becomes part of the command metadata and scheduler.

It also gives LLM agents useful information: they can choose between a fast localized repair and an expensive global reconstruction.

# 5. History is potentially our biggest memory problem

The Fusion-style history we discussed can either be a major advantage or a performance disaster.

I would never store a complete mesh copy for every history step.

Instead, MeshFixer should use an immutable, versioned operation graph with selective caching.

History with selective materialization

Original imported geometry

Persistent source

Remove pedestal

Cached result

Remesh

Disk checkpoint

Smooth selected area

Stored command + changed-region data

Fillet edges

Current materialized geometry

When someone edits the earlier Remesh operation, only its descendants need reevaluation. If a downstream selection still maps correctly, its operation can be replayed. If not, MeshFixer reports a selection dependency error.

### Important implementation details

- Copy-on-write geometry: unchanged chunks can be shared between revisions.
- Content-addressed cache: identical inputs, command parameters and plugin versions can reuse a previously computed result.
- Bounded RAM cache: old revisions are evicted instead of accumulating indefinitely.
- Disk checkpoints: expensive stages are periodically materialized to speed up replay.
- Dependency-aware invalidation: only affected branches are recalculated.
- Atomic commit: a failed computation does not replace the last valid geometry.
- Versioned cache keys: changes to a geometry algorithm invalidate incompatible cached results.

There is a subtle problem here: if an intermediate result has been evicted, rebuilding it may require replaying a series of earlier operations. The cache therefore needs a cost model, not just least-recently-used eviction.

A result that took 90 seconds to calculate may be worth retaining longer than a simple transform that takes milliseconds.

I would give every history node a visible computation cost as well.

Autodesk already offers compute-time diagnostics for Fusion features, and Blender's Geometry Nodes tooling also emphasizes identifying expensive evaluations.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.autodesk.com\&sz=32)

Autodesk+1



MeshFixer could make that much more user-friendly.

# 6. CPU and GPU scheduling: never let the UI compete equally with geometry work

A common mistake is to run a computation using every available CPU core because that produces the fastest isolated benchmark.

On a desktop application, that can make the entire computer feel unusable.

MeshFixer should reserve resources for interactivity.

Performance profile

Proposed defaults

EcoBalancedPerformance

Keep interaction responsive while background geometry jobs progress.

## 4

Illustrative worker cap

## 60

Target FPS

## 40%

Example cache budget

Illustrative settings for an eight-core machine, not measured defaults. Actual worker counts, memory budgets and frame targets would be hardware-adaptive.

The scheduler should have priority classes:

| Priority    | Work                                                  |
| ----------- | ----------------------------------------------------- |
| Immediate   | UI input, camera movement, dragging                   |
| Interactive | Selection, picking, small geometry previews           |
| Normal      | User-triggered repairs and command execution          |
| Background  | Thumbnails, analysis, LOD generation                  |
| Idle        | Cache optimization, indexing, optional precomputation |

GPU compute deserves special care. A large compute shader dispatch can interfere with rendering because it shares GPU resources with the viewport.

I would therefore keep the first geometry engine CPU-first, while using the GPU for rendering and selectively accelerating algorithms that demonstrably benefit from it.

Potential GPU candidates include displacement, point transformations, certain smoothing operations, scan visualization and parallel spatial queries.

More complicated exact topology-changing operations and robust booleans may remain CPU-side, at least initially.

For CPU jobs, Rust's Rayon provides data-parallel execution and work scheduling, but we'd still want a MeshFixer-level admission controller to prevent plugins from independently saturating the system.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.rs\&sz=32)

Rust



### Cancellation must be a core capability

If someone starts an expensive reconstruction and then changes their mind, Cancel should work.

Plugins should periodically check a cancellation token and report progress. Computation should produce a provisional result, not mutate the active document in place.

For potentially unstable or untrusted native plugins, a separate process provides stronger termination and memory-isolation options than cooperative cancellation alone.

# 7. Avoid full-frame rendering when nothing changes

This is a surprisingly important laptop optimization.

A desktop editor displaying an idle 3D model does not need to continuously render at 60 or 120 frames per second.

I'd implement an event-driven render loop:

- Camera moving: render continuously at an appropriate target frame rate.
- Geometry preview animating: render as needed.
- Mouse hovering a feature: update only when the hover state changes.
- Static viewport: stop submitting new frames.
- Window minimized: pause viewport rendering and nonessential work.
- Laptop running hot: lower frame rate or visual quality before slowing critical UI actions.

On integrated GPUs, memory bandwidth and power consumption matter as much as raw rendering throughput.

We can also reduce high-DPI costs with dynamic viewport resolution during navigation, restoring full resolution when stationary.

The application should ideally be quiet when idle: no busy-wait loops, unnecessary GPU submissions or continuous background geometry checks.

# 8. Sparse storage for voxels and point clouds

For scan reconstruction, hollowing, offsets and Make Solid, volumetric geometry is valuable.

But a dense voxel grid scales cubically with resolution.

That becomes expensive extremely quickly.

I would use sparse spatial structures for volume operations. OpenVDB was designed for precisely this type of problem: hierarchical storage of sparse volumetric data, minimizing memory requirements and supporting efficient random and sequential access.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.openvdb.org\&sz=32)

OpenVDB+1



We can also partition point clouds spatially and load points at an appropriate screen-space density.

That means a billion-point cloud shouldn't require a billion individually rendered points at all times.

For very large captures, a chunked, disk-backed representation with spatial indexing could allow meaningful inspection on a laptop. That is an architectural target, not a promise that every billion-point dataset will be editable within ordinary RAM.

# 9. Make performance an explicit plugin contract

Our “everything is a plugin” decision creates an additional requirement: plugins must participate in resource management.

Otherwise, one poorly designed extension could consume 20 GB of memory or leave eight CPU cores saturated.

I would extend the plugin contract with resource declarations:

```
pub struct OperationDescriptor {
    pub id: &'static str,
    pub scope: OperationScope,
    pub supports_preview: bool,
    pub supports_cancellation: bool,
    pub supports_incremental: bool,
    pub can_run_headless: bool,
}
```

And each invocation should receive:

```
pub struct ExecutionContext {
    pub memory_budget_bytes: u64,
    pub worker_budget: usize,
    pub cancellation: CancellationToken,
    pub progress: ProgressReporter,
    pub cache: CacheHandle,
}
```

The application scheduler decides when the operation runs, on which resources and with what concurrency limits.

For trusted in-process Rust plugins, these budgets are cooperative; arbitrary allocations cannot be reliably contained just by passing a number.

WASM or out-of-process plugins provide stronger opportunities for enforcing resource limits.

That distinction should be explicit in the design.

# 10. Cross-platform as an engineering principle

I would define three first-class desktop targets from the outset:

Windows

x86-64 · DirectX 12 / Vulkan · Intel, AMD and NVIDIA GPUs

Tier 1

macOS

Apple Silicon · Metal · unified memory architecture

Tier 1

Linux

x86-64 · Vulkan · Wayland and X11

Tier 1

Linux ARM64 and Windows ARM64 are sensible future targets, but I wouldn't commit to shipping them until we have validated the geometry-kernel dependencies and plugin distribution.

Rust and `wgpu` are good choices for this architecture because `wgpu` supports DirectX 12, Metal, Vulkan and some lower-tier OpenGL/GLES paths across platforms.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.rs\&sz=32)

Rust+1



However, supporting multiple graphics APIs is not the same thing as guaranteeing feature parity.

## A platform abstraction boundary

The geometry engine should have no Windows, macOS or Linux UI dependencies.

```
meshfixer-core/
  geometry
  document
  history
  selections
  commands
  plugins
  cache
  scheduler
  formats

meshfixer-platform/
  filesystem
  windows
  dialogs
  GPU adapter discovery
  memory pressure
  clipboard
  drag and drop
  system notifications

meshfixer-desktop/
  UI
  viewport
  docking
  interaction

meshfixer-cli/
  headless execution

meshfixer-mcp/
  agent interface
```

This also means a headless remesh should behave identically on the three operating systems, within defined floating-point tolerances.

### Cross-platform pitfalls to account for

| Concern                     | Design rule                                                          |
| --------------------------- | -------------------------------------------------------------------- |
| GPU capabilities            | Feature detection, not assumptions                                   |
| Integrated vs discrete GPU  | Select based on suitability and power profile                        |
| Unified vs dedicated memory | Track total effective memory pressure, not just reported VRAM        |
| CPU architectures           | Portable algorithms and runtime-dispatched SIMD                      |
| Filesystems                 | Platform-safe paths, atomic save and recovery                        |
| HiDPI                       | Resolution-independent UI and adaptive viewport render scale         |
| C++ kernel dependencies     | CI-tested binary packaging for all supported targets                 |
| Plugin compatibility        | Versioned interfaces, platform-specific implementations where needed |
| Headless support            | Never require a display server for core geometry operations          |
| Graphics failures           | Recoverable GPU-device errors and conservative renderer fallback     |

I would keep GPU-specific optimization behind an abstraction layer so that a future Metal-specific optimization doesn't contaminate the rest of the codebase.

And we should test on actual machines, not just CI runners.

# 11. Define measurable performance budgets

This is where I'd get unusually disciplined.

Rather than saying “MeshFixer should be fast,” create explicit performance acceptance targets.

These are proposed engineering targets, not measured capabilities.

| Scenario                   | Initial target                          |
| -------------------------- | --------------------------------------- |
| Cold launch, no document   | Under 2 seconds on reference SSD laptop |
| Load 1M-triangle STL       | Interactive viewport within 2 seconds   |
| Orbit 5M-triangle mesh     | 60 FPS on reference midrange GPU        |
| Orbit 20M-triangle scan    | At least 30 FPS using adaptive LOD      |
| Select object/region       | Visible response within 100 ms          |
| Drag a modifier handle     | Responsive preview, even if approximate |
| Smooth a small region      | Interactive preview with local geometry |
| Idle CPU usage             | Near zero                               |
| Idle GPU activity          | No continuous rendering                 |
| Undo a cached operation    | Typically under 100 ms                  |
| Recalculate history        | Only dirty dependencies                 |
| Recover from failed plugin | Preserve last committed document        |

The targets require specific reference hardware, datasets and measurement definitions before becoming release gates.

And I would explicitly include constrained hardware in the performance test matrix:

Proposed reference machines

| Entry laptop      | 8 GB RAM, integrated GPU   | Basic mesh work, graceful large-file handling |
| ----------------- | -------------------------- | --------------------------------------------- |
| Mainstream laptop | 16 GB RAM, integrated GPU  | Primary optimization target                   |
| Workstation       | 32–64 GB RAM, discrete GPU | Large scans, reconstruction, complex CAD      |

I'd optimize for the mainstream 16 GB laptop first.

If MeshFixer works beautifully there, it can scale upward. If it only works well on a high-end workstation, we've repeated one of the problems that motivated this project.

## Performance observability inside MeshFixer

I'd also include a developer-oriented performance panel.

MeshFixer · Performance

Illustrative diagnostics

CPU memory

## 2.1 GB

GPU allocation

## 620 MB

Viewport FPS

## 59.8

Disk cache

## 4.6 GB

Costliest history operations

| Reconstruct scan | 8.4 s |   |
| ---------------- | ----- | - |
| Remesh           | 1.9 s |   |
| Fillet edges     | 0.6 s |   |

Mock diagnostic readings, not a real benchmark. Actual telemetry should distinguish CPU, GPU, cache, peak memory and operation time.

This would be extremely useful for testing plugins, investigating memory growth and comparing algorithms.

We should also publish a standardized benchmark corpus: synthetic geometry, real-world scan samples, broken STL files, large point clouds, complicated STEP assemblies and deliberately pathological meshes.

Each release can run performance regressions across Windows, macOS and Linux.

# 12. A revised MeshFixer architecture

Putting everything together, this is the system I'd build.

Native desktop UI

egui / wgpu · Windows / macOS / Linux

Unified command bus

Human · CLI · MCP · LLM agent

Document + history

Immutable revisions, dependency DAG, snapshots

Geometry store

Chunked meshes, B-reps, points, volumes

Resource scheduler

Priority queues, CPU/GPU budgets, cancellation

Cache + streaming

LOD, disk checkpoints, selective eviction

Plugin execution host

Built-in Rust · WASM · External processes

Platform adapters

DX12 / Metal / Vulkan · Filesystem · OS resource monitoring

The scheduler and geometry store should be central parts of the architecture, not implementation details hidden inside individual tool plugins.

# What I would explicitly avoid

A few tempting approaches could make MeshFixer much harder to optimize:

- One giant mutable mesh in RAM. Simple at the beginning, but it makes history, localized edits and large-file handling expensive.
- Recreating full GPU buffers on every edit. Use selective updates and revision tracking.
- Automatically GPU-accelerating every algorithm. Data transfer and synchronization costs can exceed the computational benefit.
- Running everything at maximum available parallelism. Fast benchmarks, terrible laptop experience.
- Storing full geometry at every history step. History memory explodes.
- Using the rendered LOD mesh as the authoritative editable mesh. This risks precision and topology errors.
- Letting each plugin invent its own caches, threads and GPU allocations. Resource management becomes impossible.
- Targeting only one operating system until late development. Platform assumptions become expensive to unwind.

# My recommendation for the project

I would formally add these acceptance principles to MeshFixer:

| Principle                | Requirement                                                 |
| ------------------------ | ----------------------------------------------------------- |
| Cross-platform by design | Windows, macOS and Linux are first-class targets            |
| Laptop-first performance | 16 GB integrated-GPU laptops are a primary reference target |
| Non-blocking interaction | Geometry computations never block the UI thread             |
| Controlled resources     | Shared CPU, GPU, RAM and cache management                   |
| Non-destructive history  | Editable, selectively recomputed operation graph            |
| Scalable geometry        | Spatial chunking, adaptive LOD and streaming                |
| Plugin discipline        | Budgeted, cancellable, versioned operations                 |
| Deterministic core       | Reproducible geometry independent of desktop UI             |
| Observable performance   | Built-in metrics, regression tests and profiling            |

Most importantly, I'd move performance engineering into Phase 0, alongside the document model and plugin system.

Before building dozens of tools, I'd want a technical prototype that loads and navigates 1M, 10M and 30M triangle meshes on all three operating systems, tests adaptive display detail, measures memory usage and demonstrates an expensive background operation without freezing the viewport.

That would validate the choices we've made around Rust, wgpu, geometry storage and scheduling before they become deeply embedded in MeshFixer.

The larger vision is not just a faster Meshmixer. It's an application where users can work with genuinely messy, complex geometry without needing an expensive workstation or thinking constantly about polygon counts, RAM and GPU limitations.

That would be a substantial competitive advantage.