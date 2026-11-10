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

Verification is local (owner ruling, 2026-11-11: no automatic GitHub Actions usage). Run `scripts/check.sh` (the fast gate: format, lint, licences, fast tests) before every commit. Run `scripts/check.sh --full` (adds the timing, cancellation, frame-gate and 5M-triangle tests) on demand, once work is complete, for example at milestone close. The GitHub workflows (`ci.yml`, `heavy.yml`, `fuzz.yml`) still exist, but are manual only: they never run on push or PR, only when started by hand from the Actions tab.

Licensed under either of MIT or Apache-2.0 at your option.
