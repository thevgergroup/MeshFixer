# MeshFixer

A modern, open-source desktop workbench for turning messy 3D data (scans, AI-generated meshes, broken STLs) into usable, manufacturable objects.

Status: **Phase 0 spike.** See `docs/spec/phase0-spec.md` and `docs/plan/phase0-milestones.md`.

```bash
cargo run -p mf-desktop -- model.stl     # desktop app
cargo run -p mf-cli -- inspect model.stl # CLI
scripts/check.sh                         # same checks as CI's push job; fast (skips timing/5M tests)
scripts/check.sh --full                  # also the heavy/timing/5M tests, locally
scripts/run-tests.sh                     # all tests; timing tests run alone afterwards (--fast to skip them)
```

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.99.1) so local clippy matches CI. Install Rust with rustup. Homebrew's `rust` package ignores the pin.

CI is a two-tier gate. Every push and PR runs the fast gate above (`scripts/check.sh`'s checks, `ci.yml`). The heavy tests (timing, cancellation, frame-gate, 5M-triangle and the milestone evidence bundle, `heavy.yml`) and the fuzzing session (`fuzz.yml`) are on demand only, never on push or an ordinary PR event, started by a person: from the Actions tab (`workflow_dispatch`), or by adding the `run-heavy` / `run-fuzz` label to a PR. The label trigger exists because GitHub can only `workflow_dispatch` a workflow once it's on the default branch; until `heavy.yml`/`fuzz.yml` are merged, the label is how to start them from a PR branch. Each job checks that the label just added is the right one, so adding any other label is a no-op.

Licensed under either of MIT or Apache-2.0 at your option.
