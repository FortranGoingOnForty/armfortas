#!/usr/bin/env bash
# Generate code coverage report for armfortas.
#
# Prerequisites (install one):
#   cargo install cargo-llvm-cov      # recommended — uses LLVM instrumentation
#   cargo install cargo-tarpaulin     # alternative — uses ptrace (Linux only)
#
# Usage:
#   ./scripts/coverage.sh             # HTML report → target/llvm-cov/html/
#   ./scripts/coverage.sh --summary   # terminal summary only
#   ./scripts/coverage.sh --lcov      # LCOV format → target/llvm-cov/lcov.info

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Ensure cargo bin dir is in PATH
export PATH="$HOME/.cargo/bin:$PATH"

MODE="${1:-}"

if command -v cargo-llvm-cov &>/dev/null; then
    echo "Using cargo-llvm-cov"

    case "$MODE" in
        --summary)
            cargo llvm-cov --workspace --lib
            ;;
        --lcov)
            cargo llvm-cov --workspace --lib --lcov --output-path target/llvm-cov/lcov.info
            echo "LCOV report: target/llvm-cov/lcov.info"
            ;;
        *)
            cargo llvm-cov --workspace --lib --html
            echo "HTML report: target/llvm-cov/html/index.html"
            ;;
    esac

elif command -v cargo-tarpaulin &>/dev/null; then
    echo "Using cargo-tarpaulin"
    cargo tarpaulin --workspace --lib --out Html --output-dir target/tarpaulin
    echo "HTML report: target/tarpaulin/tarpaulin-report.html"

else
    echo "Error: no coverage tool found."
    echo "Install one of:"
    echo "  cargo install cargo-llvm-cov"
    echo "  cargo install cargo-tarpaulin"
    exit 1
fi
