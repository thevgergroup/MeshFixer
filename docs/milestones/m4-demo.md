# M4 acceptance demo (owner-run, ~15 minutes)

This is the acceptance gate for M4 (MF-ISSUE-015): **the owner drives**, not
Claude. Each step says what to do and what to look for. Nothing here
requires debug knowledge — ordinary mouse/trackpad controls (drag to orbit,
shift-drag to pan, scroll or pinch to zoom, click to select, F to frame).

## Before you start

Run:

```
scripts/demo.sh
```

This builds the release app and CLI, fetches the real Archaeopteryx scan
(`scripts/fetch-fixtures.sh`, into `fixtures/real/`, verified by SHA-256),
generates a fresh 5M-triangle damaged sphere and the two-object transformed
3MF scene (both regenerated each run — large, so they're never committed),
and opens MeshFixer at a 1440×900 window with nothing loaded. Use **File ▸
Open** (or drop a file on the window) for each step below; the files it
staged are printed when it finishes, under `artifacts/m4/demo/`:

- `demo-5m-damaged.obj` — the 5M-triangle damaged sphere (generated)
- `fixtures/real/archaeopteryx.obj` — the real scan (fetched, used in place)
- `demo-scene.3mf` — the two-object transformed scene (generated)
- `sphere-three-holes.obj` — a sphere with three holes of 55/94/181 edges,
  kept in the repo for the Smooth vs Curvature step

If you'd rather watch the flow first: `artifacts/m4/demo/m4-preview-reel.mp4`
is an **automated preview, not this demo** — real GUI frames from the
headless tests, assembled with ffmpeg, so you see the shape of the session
before running it yourself.

---

## 1. Performance at scale

**Open** `demo-5m-damaged.obj` (File ▸ Open).

- **Orbit** (drag) and **zoom** (scroll/pinch) around the model. It should
  stay smooth — no stepping or lag as you move. (Preliminary bench on the
  dev machine: about 384 fps at 5M triangles, not a CI-gated number; see
  `m4.md`.)
- Select the whole object (click **Model** in the tree) and run **Fill all
  holes** from Properties. While it runs, **keep orbiting** — the view
  should keep responding while the progress bar moves.
- Click **Cancel** partway through. The status line should say the document
  is unchanged, and the History panel should show no new step.
- Run **Fill all holes** again and let it finish this time.
- Open **View ▸ Metrics**: frame times, memory and the job's timing should
  be visible.

## 2. Surgical repair on the real scan

**Open** `fixtures/real/archaeopteryx.obj`.

- A units prompt appears; **metres** should already be selected (the scan's
  size makes that the sensible default). Accept it.
- **Browse the defects** in the left panel (Objects & Defects). Hover a hole
  in the 3D view: a tooltip should show its size. Click it to select it,
  then look at Properties.
- **Fill only that hole.** Nothing else on the model should change — orbit
  around afterward and confirm the rest of the scan looks untouched.
- **Undo**, switch the fill style (Flat / Smooth / Curvature-matched) in
  Properties, and fill the same hole again.
- Find a **narrow crack** in the defect list and try to fill it: it should
  be **refused, with a clear plain-language reason** (not a dead end —
  it should say what it means for printing and offer "Mark as intentional
  opening").
- **Remove an island** (a loose piece) from the defect list and confirm it's
  gone from both the tree and the view.
- **Export to 3MF** (File ▸ Export). Open the exported file in Cura (or your
  slicer of choice) and check its **true physical size** matches the scan
  (the units prompt mattered: this scan is about 0.50 × 0.41 × 0.03 m).

### Smooth vs Curvature on large holes

**Open** `sphere-three-holes.obj` (three holes: 55, 94 and 181 boundary
edges).

- Select the **largest hole** (181 edges). In Properties, fill it with
  **Curvature-matched**. Note the warning that curvature fills are less
  accurate above 60 edges, and look at the result.
- **Undo**, then fill the same hole with **Smooth** instead. This is the
  style you liked on large holes at Checkpoint C — compare the two results
  directly on the same hole.
- Try the two smaller holes (55 and 94 edges) with both styles if you'd
  like a second comparison; the difference should be much less visible
  there.

## 3. Multi-object editing

**Open** `demo-scene.3mf` (two objects, each at its own position/rotation:
a damaged scan and a plate with a hole, a narrow crack and a tangled edge).

- Confirm **both objects sit where they belong** — they shouldn't overlap
  or sit at the origin on top of each other.
- **Click one object**, select a hole on it, and **fill just that hole**.
  Confirm the other object is untouched.
- Click the **other object** and repeat: select one of its defects and act
  on it.

---

## Live session (TODO — fill in after the recording)

- [ ] TODO: link the screen recording of this session here.
- [ ] TODO: close MF-ISSUE-015 with the recording linked above.
- [ ] TODO: record the owner's feedback below, one line per friction point,
      each with a severity (S1/S2/S3). Any S1/S2 is fixed before M4 is
      accepted, or the owner explicitly defers it (recorded here with the
      reason).

| # | Friction point | Severity | Disposition |
|---|---|---|---|
| | TODO | | |
