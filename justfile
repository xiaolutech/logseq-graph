# Day-to-day entry point for this repository.
#
#   just                     # build the release binary
#   export LOGSEQ_GRAPH=~/path/to/your/graph
#   just info                # or: ./target/release/logseq-graph -g "$LOGSEQ_GRAPH" info
#
# Nothing here installs anything: every recipe runs `target/release/logseq-graph` from this
# directory. If you want the short command while working, an alias is enough and leaves no trace:
#
#   alias logseq-graph="$PWD/target/release/logseq-graph"
#
# The graph root is never hard-coded: this file is committed, and a personal path in it would be one
# person's machine in a shared recipe. Set `LOGSEQ_GRAPH` instead, or pass `--graph` yourself.

# Graph root. Set it in the environment; every graph recipe refuses to run without it.
graph := env("LOGSEQ_GRAPH", "")

# Release build: on a 14136-file graph the debug binary is about twice as slow for every command.
bin := "target/release/logseq-graph"

# Default: build, then show how to run it from here.
default: build
    @echo
    @echo "built: ./{{bin}}"
    @echo
    @echo "Try:"
    @echo "  export LOGSEQ_GRAPH=~/path/to/your/graph"
    @echo "  ./{{bin}} info"
    @echo "  just info"
    @echo "  just --list"

# Build the release binary.
build:
    cargo build --release -p logseq-graph-cli

# --- reading ---------------------------------------------------------------------------------------

# Graph revision, page and block counts, index size, observation time.
info: _guard build
    @./{{bin}} -g "{{graph}}" info

# Every page with its block count.
pages: _guard build
    @./{{bin}} -g "{{graph}}" pages

# One page: block count, revision.
page NAME: _guard build
    @./{{bin}} -g "{{graph}}" page "{{NAME}}"

# One page as a tree, indented by depth.
tree NAME: _guard build
    @./{{bin}} -g "{{graph}}" tree "{{NAME}}"

# Pages that reference this one, with the referencing block.
backlinks NAME: _guard build
    @./{{bin}} -g "{{graph}}" backlinks "{{NAME}}"

# Tags and how many pages use each.
tags: _guard build
    @./{{bin}} -g "{{graph}}" tags

# Pages that use one tag.
tag NAME: _guard build
    @./{{bin}} -g "{{graph}}" tag "{{NAME}}"

# Substring search over block text.
search QUERY LIMIT="10": _guard build
    @./{{bin}} -g "{{graph}}" search "{{QUERY}}" --limit {{LIMIT}}

# One block by its durable `id::` UUID.
block ID: _guard build
    @./{{bin}} -g "{{graph}}" block "{{ID}}"

# --- writing ---------------------------------------------------------------------------------------

# Append one top-level block to an existing page.
append NAME TEXT: _guard build
    @./{{bin}} -g "{{graph}}" append "{{NAME}}" "{{TEXT}}"

# Replace the text of the block that states this `id::`.
update ID TEXT: _guard build
    @./{{bin}} -g "{{graph}}" update "{{ID}}" "{{TEXT}}"

# --- verification ----------------------------------------------------------------------------------

# The five gates this repository requires before a commit, plus the two checks the
# pre-commit and commit-msg hooks add (spelling and subject shape). `typos` comes from
# mise.toml; the commit check is wired through prek, so `just check` re-runs the script
# directly to cover a clone that has not run `prek install` yet.
# The declared MSRV (`rust-version = "1.89"` in both crates), on demand. Deliberately not a hook:
# it needs a second toolchain and a full compile, neither of which a commit should pay. It is also
# `cargo check`, not clippy and not test — clippy 1.89 does not know every lint name this workspace
# enables (an unknown lint under `-D warnings` is a toolchain-compatibility failure, not an MSRV
# failure), and tests may pull in dev-dependencies that need a newer compiler.
msrv:
    cargo +1.89 check --workspace

check:
    typos --force-exclude
    cargo test --workspace
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    python3 scripts/check_perf_doc.py --self-test
    RUSTDOCFLAGS="-D warnings" cargo doc -p logseq-graph --no-deps
    python3 scripts/check_commit_msg.py --range "origin/main...HEAD"

test:
    cargo test --workspace

# Supply-chain gate: RUSTSEC advisories, licence allow list, duplicate versions, crate sources
# (`deny.toml`). Deliberately not part of `check`: cargo-deny fetches the advisory database on every
# run and fails with exit 1 when it cannot, so `just check` stays runnable offline. It is a
# pre-commit hook filtered to the dependency inputs, so it fires exactly when one of them changes
# and a dependency commit made offline is red; `just deny` is the same command, for running it by
# hand before the hook does, and the only thing that catches a new advisory while the lockfile is
# untouched.
deny:
    cargo deny check

# Byte-for-byte round trip and the incremental path, on a temporary copy.
real: _guard
    scripts/verify-real-graph.sh "{{graph}}"

# Anything the recipes above do not cover, for example `just run --verify-content info`.
run *ARGS: _guard build
    @./{{bin}} -g "{{graph}}" {{ARGS}}

# Abort a graph recipe when `LOGSEQ_GRAPH` is unset, before anything else runs.
_guard:
    #!/usr/bin/env sh
    if [ -z "{{graph}}" ]; then
        echo "LOGSEQ_GRAPH is not set" >&2
        echo "  export LOGSEQ_GRAPH=~/path/to/your/graph" >&2
        exit 2
    fi
