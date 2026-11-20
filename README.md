# logseq-graph

A parsing and indexing layer for file-based Logseq graphs, offered as an embeddable Rust engine,
a CLI, and a loopback API server speaking the official wire shape. Callers can inspect and edit graphs without running Logseq:
**Markdown files are the source of truth; SQLite is a rebuildable index.** Upper layers such as UI,
sync, plugins, and desktop applications live in separate repositories.

## Status

The engine discovers Markdown pages and journals using Logseq configuration, parses block structure
and facts, and incrementally indexes them. Writes include page/journal creation, block edits and
moves, properties, recycling pages, and version-checked batches, preserving untouched source bytes.

Known limits:
- Not a complete Logseq parser or API implementation; see [API parity](docs/logseq-api-parity.md)
  and [corpus parity](docs/corpus-parity.md).
- Advanced Datalog queries are rejected; supported simple-query forms are documented in the engine reference.
- Text search is literal substring matching, not fuzzy/word search. Folded FTS5 indexes cover
  needles of at least three characters and all-CJK 1–2-character needles; other short queries scan.
  Indexed queries use BM25 only for at most 100 matches, then fall back to unranked rowid order.
- File-backed and derived no-file page entities are modelled, including aliases, refs, tags and
  namespace parents, but do not reproduce Logseq's DB IDs or every official entity (ADR-0015).
- Page properties are read from the preamble, not the first block.
- Ephemeral block IDs are projection-scoped, not durable references. Cross-page moves accept
  either identity and recover through durable IDs or pre-hash-bound child paths.
- Fast scans can miss same-size edits with restored modification times; use content verification.

See the [engine reference](docs/engine-reference.md) for detailed contracts and operational limits.

## Install and build

From a checkout, install the tools pinned in `mise.toml` and enable hooks:

```sh
# Only if mise is not installed:
curl -fsSL https://mise.run | sh
export PATH=$HOME/.local/bin:$PATH

mise trust && mise install && mise exec -- prek install
mise exec -- just build
```

The binary is `./target/release/logseq-graph`; this builds from source, not a global installation.
See [hook setup notes](docs/engine-reference.md#measuring-and-keeping-the-measurement-honest)
for clones that previously configured `core.hooksPath`.

## Quick start

### CLI

Run from the repository root. This creates a disposable graph outside the checkout:

```sh
export LOGSEQ_GRAPH="$(mktemp -d)"
./target/release/logseq-graph create Welcome
./target/release/logseq-graph append Welcome 'TODO Read [[Guide]] #demo'
./target/release/logseq-graph pages
./target/release/logseq-graph tree Welcome
./target/release/logseq-graph query '(task TODO)'
```

`pages` lists `Welcome  1`; `tree` shows `TODO Read [[Guide]] #demo` (without the source bullet).
The query prints a writable selector alongside the matching text. Pass selectors to `block`,
`update`, `remove`, or `move`; ephemeral values must not be stored as durable IDs.
Use `--graph <dir>` instead of `LOGSEQ_GRAPH` for an existing graph. The CLI keeps its index in a
per-graph platform cache file; `--index <path>` overrides it.

### Loopback HTTP API

In the same shell, start the server (a bearer token is required, as the official API requires):

```sh
./target/release/logseq-graph serve --bind 127.0.0.1:8765 --api-token local-token
```

In another terminal:

```sh
curl -sS http://127.0.0.1:8765/api \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer local-token' \
  -d '{"method":"logseq.Editor.getAllPages","args":[{"freshness":"check"}]}'
```

The response is the bare result value — a `pages` array with `Welcome` and freshness metadata —
with no JSON-RPC envelope. Stop with Ctrl-C.
The endpoint accepts one `{method, args}` call per request, not a batch; it is not a REST API.
It binds only to loopback, checks Host, and requires the token on every request
(ADR-0018); `LOGSEQ_API_TOKEN` works as well as `--api-token`.
See the [RPC reference](docs/engine-reference.md#rpc-surface) for methods, options, errors, and limits.

## Features

- Lossless Markdown parsing and local edits, including nested blocks and heading markers.
- File-backed and derived pages, journals, properties, refs, tags, backlinks, namespaces and favorites.
- Block search and Logseq simple queries with explicit limits and writable selectors.
- Page/block snapshots, version tokens, and stale-write refusal for semantic edit batches.
- Atomic page replacement, recoverable cross-page moves, and deletion into `logseq/.recycle/`.
- Explicit durable identity grants and inclusive sibling-range deletion in one revision.
- Explicit freshness policies; filesystem notifications with polling fallback for servers.
- Concurrent RPC reads through a bounded read-only connection pool; writes stay serialized.
- Full reprojections build and catch up shadow indexes outside the graph lock, then publish only
  after zero-delta verification. Eight unsuccessful rounds fail safely and leave the old generation intact.

## Interfaces and architecture

### Embeddable Rust library

`crates/graph` (package `logseq-graph`, import `logseq_graph`) exposes the exclusive `engine::Graph`
handle and concurrent `engine::SharedGraph` facade, public domain types, commands, and structured
errors. Callers choose the index path
and read freshness policy; no HTTP runtime is required.

### CLI

`crates/server` (package `logseq-graph-cli`) builds the `logseq-graph` binary.
It translates command arguments into engine operations and renders answers for shell use.

### Loopback HTTP server

The binary's `serve` command exposes engine reads and writes over `POST /api` behind a
bearer-token gate (ADR-0018), answering in the official wire shape.
Logseq-shaped methods and this project's own `logseq.Graph.*` methods are distinguished in the
[API parity table](docs/logseq-api-parity.md). Adapters do not implement graph semantics.

Within the engine, [ADR-0003](docs/adr/0003-code-is-organized-around-five-boundaries.md)
defines five boundaries:

| Boundary | Responsibility |
| --- | --- |
| `domain` | Identity, views, commands, queries, revisions, and errors; no I/O |
| `document` | Pure parsing, facts, source-preserving serialization, and edits |
| `source` | Graph configuration, discovery, file I/O, atomic writes, and locks |
| `index` | SQLite schema, projections, and queries |
| `engine` | Graph facade, synchronization, reads, and writes; no SQL or edit algorithms |

## Development

```sh
mise exec -- just check
```

This runs spelling, workspace tests, formatting, Clippy, the performance-check self-test, rustdoc,
and commit-subject checks. `prek.toml` defines pre-commit and pre-push hooks, installed above.
Hot-path changes require a performance row. Dependency/policy changes also run the network-dependent
supply-chain gate, separate from `just check`.

[CI](.github/workflows/ci.yml) runs `just check` and `just deny` on Linux and macOS for pull requests
and pushes to `main` or `devin/**`. Optional real-graph tests skip without a supplied graph;
see [verification notes](docs/engine-reference.md#verifying-against-a-real-graph).

## Documentation

- [Architecture decisions](docs/adr/README.md) and [repository map](AGENTS.md)
- [Engine and adapter reference](docs/engine-reference.md)
- [Implementation handoff](HANDOFF.md)
- [Performance measurements](PERFORMANCE.md)
- [Lessons](LESSONS.md)
- [Logseq API parity](docs/logseq-api-parity.md)
- [Corpus parity](docs/corpus-parity.md)
- [Lint policy](docs/lint-policy.md)
- [Contributing](CONTRIBUTING.md) and [code of conduct](CODE_OF_CONDUCT.md)

Documentation and code use English; graph content remains arbitrary Unicode.
Licensed under the [MIT License](LICENSE).
||||||| parent of 8bad1b0 (perf(server): measure the served request surface)
### Measuring, and keeping the measurement honest

`PERFORMANCE.md` records what each change did to the numbers. Two things keep it from going stale:

`scripts/bench.sh <graph-root>` reproduces the tables. It copies the graph to a temporary directory,
runs `crates/graph/examples/measure.rs` against the copy, prints Markdown rows, and removes the copy
on exit, so the named graph is never written to.

The `perf-doc` hook refuses a commit that stages a file under a measured hot path without staging
`PERFORMANCE.md` too. The hot-path list lives in `scripts/check_perf_doc.py`; adding a path there is
a decision, because every later commit touching it then owes a row — including a row that says
`not measured`. `prek.toml` is the only hook configuration and `mise.toml` pins every tool the gates
need (rust, prek, typos, python, just, cargo-deny). Enable the git hook shims once per clone:

```sh
mise install
prek install
```

On a fresh clone with no `core.hooksPath`, `prek install` writes the shims into `.git/hooks/`, which
git does not track. If `core.hooksPath` is set, prek honours it and writes the shims into that
directory instead. Clones that followed the old README and ran `git config core.hooksPath .githooks`
must unset that setting before installing — the repository no longer ships `.githooks/`, so leaving the
setting in place leaves the clone without a working hook:

```sh
git config --unset core.hooksPath
prek install
```

The shim records the prek binary path used at install time and falls back to `prek` on `PATH`; after
upgrading prek or mise, re-run `prek install --force` to refresh it. `prek run --all-files` runs every
pre-commit hook by hand, and `prek run --all-files --stage pre-push` runs the pre-push set. The
supply-chain gate is not in that set: `cargo deny check` (`just deny`) is a `pre-commit` hook
filtered to the dependency graph and to `deny.toml`, because RUSTSEC advisories, the licence allow
list, duplicate crate versions, and crate sources can only change when a dependency or a rule does.
An ordinary commit skips it; a dependency or policy commit pays ~1.3s. It
fetches the advisory database on every run and fails when it cannot, so an offline commit that
touches one of those files is red — the cached database is not a fallback; be online and commit again, or run
`just deny` to see the same result and warm the cache. A new CVE published while `Cargo.lock` is
untouched triggers nothing, which is what keeps `just deny` worth running by hand. Builtin
hooks with no `stages` in `prek.toml` fire at both stages, so the two file-rewriting fixers also run
at pre-push; their `^fixtures/` exclude applies at both stages, and `fixtures/** -text` in
`.gitattributes` keeps line-ending conversion away from the byte-exact captures. `git commit
--no-verify` still bypasses the hooks: the check catches forgetting, not refusal. `python3
scripts/check_perf_doc.py --self-test` exercises the check itself against throwaway repositories; it
runs with every `GIT_*` variable stripped, because a linked worktree's hook exports an absolute
`GIT_DIR` that would otherwise send those throwaway repositories' `git add` into the caller's index.

### Verifying against a real graph

The optional `real_graph` integration tests run only when `LOGSEQ_GRAPH_REAL` names a Logseq graph
root; otherwise they print `SKIP=` and pass, so a plain checkout needs no graph.
`scripts/verify-real-graph.sh
<graph-root>` runs them and reports discovery counts, byte-exact round-trip totals, cold and warm
start timings, the storage cost of the projected block text, one bounded search measurement, and the
incremental chain: one `CheckNow` scan after editing, adding, deleting, and renaming a file, and
after touching one whose bytes stay the same, each showing which page moved and which pages kept
their revision. The named graph is only read: the incremental scans change a temporary copy of its
pages, journals, and configuration, and both that copy and the derived index are removed afterwards.

## Language

Project documentation, source comments, identifiers, and commit messages use English by default.
Graph content remains arbitrary Unicode and must not be normalized for presentation.

## Contributing, conduct, and licence

`CONTRIBUTING.md` lists the gates, the hot-path rule and the ADR workflow.
`CODE_OF_CONDUCT.md` states the standard for project spaces. This project is
licensed under the [MIT License](LICENSE).
