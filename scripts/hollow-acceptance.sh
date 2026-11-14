#!/usr/bin/env bash
# The four canonical Hollow acceptance tests (owner 2026-11-14): a single
# parametric Box and Cylinder, Box ∪ Cylinder, Make solid of it, and the
# Stanford bunny (Repair, then Hollow), each hollowed plain, with Auto
# drain holes, and opened through a flat cut face; through the command
# layer (crates/mf-command/tests/hollow_acceptance.rs) and clicked through
# the app (crates/mf-desktop/tests/core_workflows.rs, `hollow_acceptance::`,
# kept out of scripts/core-workflows.sh so that one stays fast).
#
# The bunny is fetched by scripts/fetch-fixtures.sh (fixtures/real/,
# git-ignored); without it its cases are skipped by policy, or fail with
# MF_REQUIRE_FIXTURES=1. Its Repair plus Hollow is timed (2 s or less)
# in the optimised build, so the command-layer half runs --release.
set -euo pipefail
cd "$(dirname "$0")/.."

toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"

time cargo test -j 4 --release -p mf-command --test hollow_acceptance --no-fail-fast -- --nocapture "$@"
time cargo test -j 4 -p mf-desktop --test core_workflows --no-fail-fast -- --test-threads=1 hollow_acceptance:: "$@"
