# MeshFixer

A modern, open-source desktop workbench for turning messy 3D data (scans, AI-generated meshes, broken STLs) into usable, manufacturable objects.

Status: **Phase 0 spike.** See `docs/spec/phase0-spec.md` and `docs/plan/phase0-milestones.md`.

```bash
cargo run -p mf-desktop -- model.stl     # desktop app
cargo run -p mf-cli -- inspect model.stl # CLI
scripts/check.sh                         # same checks as CI's push job; fast (skips timing/5M tests). Heavy tests run on demand: Actions → "Heavy tests", or scripts/check.sh --full
scripts/run-tests.sh                     # all tests; timing tests run alone afterwards (--fast to skip them)
```

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.99.1) so local clippy matches CI. Install Rust with rustup. Homebrew's `rust` package ignores the pin.

Licensed under either of MIT or Apache-2.0 at your option.
