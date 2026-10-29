#!/bin/sh
# Mutation testing of what a step changed: cargo-mutants breaks the code in
# `src/` that differs from BASE (default `main`, working tree included) and
# lists the changes no test notices. Exits non-zero when one survives.
# Configuration, and what it leaves out, is in .cargo/mutants.toml.
set -e
cd "$(dirname "$0")/.."
base="${1:-main}"
patch="$(mktemp)"
trap 'rm -f "$patch"' EXIT
git diff "$base" -- 'src/*.rs' > "$patch"
if [ ! -s "$patch" ]; then
    echo "no change in src/ against $base"
    exit 0
fi
cargo mutants --in-diff "$patch" --jobs 2
