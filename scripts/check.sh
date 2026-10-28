#!/bin/sh
# The quality gate of AGENTS.md, as one command. It checks formatting
# instead of applying it, so running it never changes the tree.
set -e
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
