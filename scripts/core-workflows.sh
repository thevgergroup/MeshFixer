#!/usr/bin/env bash
# Fast, end-to-end GUI tests for the core workflows (owner 2026-11-12):
# Split (Keep both/above/below, orientation-named sides), Combine of
# solid+solid+hole with Undo, Delete a split part (Backspace and the
# trash button), Connect (two dowels, Apply), one Sculpt inflate stroke
# with "stay on surface", a Transform move and its Undo, and a
# parametric shape param edit and its Undo, and Hollow (thin parts, a
# placed drain hole, one undo) then a vase by Split with Cap off.
#
# crates/mf-desktop/tests/core_workflows.rs — kept under 3 minutes at
# -j 4; run single-threaded (--test-threads=1), same as every other
# egui_kittest GUI suite here (they drive real frames on a shared GPU
# device and are not safe to run in parallel with each other).
set -euo pipefail
cd "$(dirname "$0")/.."

toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"

# The Hollow acceptance flows (the four canonical shapes, three ways each)
# are their own group, run by scripts/hollow-acceptance.sh.
time cargo test -j 4 -p mf-desktop --test core_workflows --no-fail-fast -- --test-threads=1 --skip hollow_acceptance:: "$@"
