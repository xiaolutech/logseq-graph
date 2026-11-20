# ADR-0003: Code is organized around five explicit boundaries

Status: Accepted
Date: 2026-10-29

## Context

Storage, Markdown editing, request policy, and protocol compatibility evolve for different reasons.
Putting them in a large engine or writer module obscures the write contract and lets adapters grow
incompatible graph semantics.

## Decision

The workspace has two crates: `graph` for the embeddable library and `server` for CLI and protocol
adapters. The library exposes `Graph`, public data types, commands, and structured errors. All
adapters use that facade; none access source files, the parser, or SQLite directly.

Within `graph`, code is organized into `domain`, `document`, `source`, `index`, and `engine`:

```text
crates/
  graph/src/
    domain/       page/block views, identity, queries (query/), commands, revisions, errors
    document/     lossless AST, parser, facts, local serialization; pure edits in edit/
    source/       config (config/), discovery, file IO, hashing, atomic writes, locks, write intent
    index/        schema (schema/), projection, queries, page replacement, shadow generations
    engine/       Graph facade, startup, reads (read/), writes and recovery (write/), refresh
  server/src/
    args/         CLI argument parsing, split by command family in parse/
    http/rpc/     HTTP/JSON translations of engine reads (reads/, by domain) and writes
```

- `domain` contains no IO or storage dependencies.
- `document` performs pure parsing and editing and knows neither files nor SQLite.
- `source` is the only layer that operates on graph files.
- `index` is the only layer that operates on SQLite.
- `engine` orchestrates these boundaries and contains no Markdown edit algorithms or SQL.
- The canonical operations are page lookup, page-tree lookup, bounded block queries, backlinks,
  search, and `WriteCommand` execution. Public results carry revision metadata.

## Consequences

The library remains embeddable without HTTP, and compatibility quirks stay outside the core. The
module layout is intentional and not a plugin architecture: new concepts should enter an existing
boundary unless a new ADR changes this decision.

## Amendment (2026-11-01)

Two points the original text left open. Both were settled while reviewing whether the boundaries
still hold, and neither changes a decision above.

- **A graph file is a file inside the graph root**: the Markdown pages, the journals, and
  `logseq/config.edn`. The SQLite index is not one. It lives under `$XDG_CACHE_HOME` (or the platform
equivalent), it is deletable at any time, and losing it costs a re-projection rather than data.
  `index` managing its own database files — creating the parent directory, removing a discarded
  index — therefore does not break "`source` is the only layer that operates on graph files". It is
  managing its own storage, which is what that layer exists for.
- **`document` may depend on `domain`.** "Pure" here means no files and no SQLite, not "depends on
  nothing". `BlockMarker` (`Bullet` / `Heading { level }`) is a structural fact about a block, so it
  belongs in `domain`; the parser produces it, and a parser producing a value type is the correct
  direction. The dependency runs `document → domain`, never the reverse. This is why `BlockMarker`
  moved out of `document/parser.rs`: `domain`'s public `BlockNode` had a `marker` field and a
  `marker()` accessor, so the value type was already on `domain`'s public surface.

## Amendment (2026-11-10): one HTTP boundary, and it is RPC

- The server has exactly one HTTP boundary, `http/rpc/`; no `http/rest/` module exists.
- `http/rpc/` translates one `{method, args}` call into one engine read under one freshness policy
  and serializes that read's result. It holds no query logic: it names no SQL, opens no page
  document, and chooses no freshness policy beyond the one the request declares.
- A method whose answer comes from the index declares exactly one freshness policy; a method whose
  answer comes from a graph file declares none and treats a declared policy as an error.

## Amendment (2026-11-19): features split inside a layer, RPC grouped by domain

The five top-level boundaries above remain the only layers; this amendment records how code is
organized inside them and corrects descriptions the code has outgrown.

- **Inside a layer, code is split by feature into subdirectories** (`engine/read/`,
  `engine/write/`, `document/edit/`, `source/config/`, `index/schema/`, `domain/query/`, the
  server's `args/parse/`). A subdirectory belongs to exactly one layer and obeys that layer's rules;
  it is never a sixth boundary, and code does not move between layers to share a subdirectory.
- **RPC methods are grouped by domain.** `http/rpc/` serves both reads and writes over the one
  `POST /api` surface. Reads live in `http/rpc/reads/`, one module per domain (pages, blocks,
  search, tags, query, graph), and writes translate to `WriteCommand`s in `http/rpc/writes.rs`.
  The 2026-11-10 amendment's rule applies to both: one call maps to one engine read or write, and
  the adapter holds no query logic.
- **Durable write intent is source state, not index state.** A pending move is recorded in
  `write-intent.json` beside the graph's write lock (`source/intent.rs`), so it survives deleting
  the index. `index` keeps only a reader for legacy intent rows, used to export them before a
  schema rebuild.
- **`index` owns its storage generations.** A rebuild projects into a shadow database and
  publishes it atomically (`index/storage.rs`); selecting, leasing and removing generations is
  `index` managing its own storage, as the 2026-11-01 amendment allows.
