#!/usr/bin/env bash
# Measure the read and write surface on a temporary copy of one graph.
#
# Usage: scripts/bench.sh <graph-root>
#
# Prints the environment line that makes the numbers comparable, then Markdown table rows meant to
# be pasted into PERFORMANCE.md. The graph itself is never written to: the copy is what the
# measurement edits, and it is removed on exit.
#
# Two phases. The engine tier runs `crates/graph/examples/measure.rs` against the copy; the
# served tier then builds the CLI, starts `logseq-graph serve` on the same copy, and runs
# `crates/server/examples/measure_serve.rs`, which times representative JSON-RPC calls against the
# running child. `BENCH_POLL_MS` is the child's poll interval (default `86400000`, one day, so the
# served rows are clean of watcher passes; a short value such as `1000` is how the watcher's
# interference is measured) and `BENCH_BURST_SECS` is the burst row's wall window (default 5).
set -euo pipefail

graph="${1:?usage: bench.sh <graph-root>}"
[ -d "$graph" ] || { echo "not a directory: $graph" >&2; exit 2; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/logseq-graph-bench.XXXXXX")"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/graph" "$work/cache"
for dir in pages journals logseq; do
    if [ -d "$graph/$dir" ]; then
        cp -R "$graph/$dir" "$work/graph/$dir"
    fi
done

cpu="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'unknown CPU')"
os="$(sw_vers -productVersion 2>/dev/null || uname -sr)"
echo "Environment: ${cpu}, ${os}, $(rustc --version | cut -d' ' -f1-2)"
echo "Graph: ${graph}"
echo

cargo run --release --quiet --manifest-path "$repo/Cargo.toml" -p logseq-graph \
    --example measure -- "$work/graph" "$work/cache/index.db"

# The served tier: the same copy, a running `serve` child, representative JSON-RPC calls timed
# over raw HTTP. The child never sees the named graph, only `$work/graph`.
poll_ms="${BENCH_POLL_MS:-86400000}"
burst_secs="${BENCH_BURST_SECS:-5}"
# The executable path honours CARGO_TARGET_DIR and a configured build.target-dir, so resolve it
# through cargo rather than assuming target/release inside the repository.
target_dir="$(cargo metadata --format-version 1 --no-deps \
    --manifest-path "$repo/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
cargo build --release --quiet --manifest-path "$repo/Cargo.toml" -p logseq-graph-cli
watch_mode="${BENCH_WATCH_MODE:-notify}"
cargo run --release --quiet --manifest-path "$repo/Cargo.toml" -p logseq-graph-cli \
    --example measure_serve -- \
    "$target_dir/release/logseq-graph" "$work/graph" "$work/cache/index.db" \
    "$poll_ms" "$burst_secs" "$watch_mode"
