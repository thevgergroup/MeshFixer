#!/usr/bin/env bash
# Run the same checks as CI's test job before pushing.
set -euo pipefail
cd "$(dirname "$0")/.."
toolchain="$HOME/.rustup/toolchains/$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)-$(rustc -vV | sed -n 's/^host: //p')/bin"
[ -d "$toolchain" ] && export PATH="$toolchain:$PATH"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
scripts/test-check-swallowed.sh > /dev/null
scripts/check-swallowed-defaults.sh
cargo deny check licenses 2>/dev/null
# Timing tests run on their own after the rest (scripts/run-tests.sh).
tests=0
scripts/run-tests.sh > /tmp/meshfixer-check-tests.txt 2>&1 || tests=$?
grep -E "^test result: FAILED|panicked|worst frame" /tmp/meshfixer-check-tests.txt || true
scripts/ci-test-summary.sh /tmp/meshfixer-check-tests.txt "${MF_SKIP_LOG:-/nonexistent}"
! grep -q "^test result: FAILED" /tmp/meshfixer-check-tests.txt
[ "$tests" -eq 0 ]
