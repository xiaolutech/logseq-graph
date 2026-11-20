# Handoff

Current implementation state and the next slices, not an architecture decision or a running
changelog. Replace stale summaries; keep contracts in the ADRs and measurements in
[PERFORMANCE.md](PERFORMANCE.md).

Last updated: 2026-11-20, after the implementation round through `149e2fb`, the served-request
benchmark, and the bearer-token / official-wire-shape port: `bench.sh` gained a served phase that
starts the built `logseq-graph serve` on the sandbox copy, and
`crates/server/examples/measure_serve.rs` times representative `POST /api` calls over raw HTTP
(poll off for clean adapter rows, one one-second-poll run for watcher interference), giving the
adapter's per-request latency its first baseline in PERFORMANCE.md's "The served tier" section.
That interference run also settled the polling-cadence question of its round — the fixed
one-second poll moved no single-call row; the watcher has since been replaced by debounced
filesystem notifications with polling kept as fallback.

## Current state

Two crates: `crates/graph` is the embeddable engine; `crates/server` builds the CLI and loopback
HTTP server (`POST /api`). Markdown owns product facts; SQLite is a disposable read model.
The current index schema is **18**, parser version **7**; incompatible indexes rebuild from files.

| Landed area | State and references |
| --- | --- |
| Page identity | File-backed and derived no-file page entities share the folded page-name key. Tags, page refs, aliases and namespace parents claim entities; pre-block properties/refs/tags contribute facts. Alias-linked references merge, while aliases do not borrow another page's file/tree. Listings report `has_file`; edits and file snapshots require a file. [ADR-0015](docs/adr/0015-page-identity-includes-derived-entities.md). |
| Content search | Folded FTS5 trigrams handle needles of at least three characters; a second FTS5 index of CJK unigrams/bigrams handles all-CJK 1–2-character queries. A bounded 101-match probe selects full BM25 for at most 100 matches or unranked rowid order above that. Other short needles, including mixed scripts, still scan. Literal substring and snippet contracts remain. [ADR-0016](docs/adr/0016-content-search-uses-a-folded-trigram-fts5-index.md). |
| Parser parity | Bare `-` continuations, empty boundary lines, unclosed fences, backtick info strings and dash-only block openings follow the pinned official parser. No page-name, normalized-block-text or content-block-count differences remain on the pinned 274-page / 5,189-block corpus; this is not universal parser equivalence. [Corpus parity](docs/corpus-parity.md). |
| Query DSL | Case-insensitive task/priority operands, nested boolean sets, multi-operand `not` as complement of `and`, property value matching, and inclusive journal ranges are implemented. Range endpoints are sorted; relative days/weeks/calendar months/years and validated journal-day integers work. Property matching runs in SQLite through a deterministic function, not a Rust owner-ID collection. See [query semantics](docs/engine-reference.md#simple-query-semantics) for upstream references and remaining differences. |
| Writes | Sixteen library writes are served as own `logseq.Graph.*` methods. `ensureBlockId` explicitly grants durable identity and is a no-op for an existing ID. Cross-page moves accept id-less moved blocks and anchors, recovering through durable IDs or child-index paths bound to pre-write hashes. `removeBlockRange` deletes an inclusive sibling run and its subtrees in one revision; it rejects reversed, cross-parent and cross-page ranges. [Write contracts](docs/logseq-api-parity.md#served-writes-own-api-not-official-parity). |
| Concurrent reads | `SharedGraph::read(&self, ...)` leases one of four read-only SQLite connections by default. The exclusive `Graph` API remains available. RPC releases its startup-state mutex before read/write execution; writes and strict freshness scans use the serialized writer. Indexed values and revision metadata share a snapshot; generation changes reopen pooled connections and register all SQL functions, including `search_cjk_tokens`. In-memory indexes use the serialized fallback. [ADR-0017](docs/adr/0017-concurrent-reads-use-a-bounded-connection-pool.md). |
| Served wire and auth | `serve` requires a bearer token under the official rule — `--api-token` (repeatable) or `LOGSEQ_API_TOKEN`, checked before the body is read, refusal is HTTP 401 in Fastify's `{statusCode, error, message}` plus `code -32001` / `kind "unauthorized"`, and an `OPTIONS` preflight is answered 204 with the official CORS headers before the token check. Success answers HTTP 200 with the bare result value (no JSON-RPC envelope or id echo); failures pair the transport status with the Fastify shape plus `code`/`data`. `crates/server/tests/rpc/official_shapes.rs` replays every captured official answer's *shape* against a sandbox server, so a missing key breaks the build rather than a client. [ADR-0018](docs/adr/0018-the-served-api-requires-a-bearer-token.md). |
| Service and publication | Filesystem notifications refresh paths, with polling fallback and shared scan-failure observations. Socket deadlines and bounded connection/handler slots prevent stalled clients from exhausting request workers. Full reprojections build and catch up unlocked, then publish only after zero-delta verification; eight unsuccessful rounds retain the previous generation and return a retryable failure. The final lock duration is not yet bounded independently of graph size. |

The CLI owns the per-graph platform-cache policy; the library receives an index path. The CLI also
exposes journal creation, versioned page commits, explicit identity grants and sibling-range removal.
See [README.md](README.md) for setup and [the engine reference](docs/engine-reference.md) for the
full read/write/RPC surface rather than duplicating argument tables here.

## Contracts to preserve

- `document` is pure; `source` owns graph file I/O; `index` owns SQL; `engine` orchestrates.
  [ADR-0003](docs/adr/0003-code-is-organized-around-five-boundaries.md) and [AGENTS.md](AGENTS.md)
  map the boundaries and paths. Parser facts and edits now live under `document/facts/` and
  `document/edit/`; the performance hook covers both directories recursively.
- Writes preserve untouched bytes, commit files before replacing whole-page index rows, and retain
  move-intent recovery. Configuration mismatches can require unlocked reprojection before editing;
  matching configurations do not require a graph scan for each write.
- `PageVersion` identifies one file's source bytes, not the SQLite revision. Ephemeral selectors
  are projection-scoped; writes validate their indexed fingerprint under the graph lock before
  trusting their child paths. Neither stale paths nor ephemeral IDs are durable identity.
- Freshness remains explicit: `LastScan` is fail-closed after a recorded scan failure, `AllowStale`
  marks that failure, and `CheckNow`/`VerifyContent` scan before answering. Same-size edits with a
  restored mtime can evade the fast path; content verification is the remedy. `page_versions`
  compares index and disk fingerprints without running any freshness scan.
- Read metadata reports the graph instance, revision, staleness and last successful scan time.
  Source snapshots describe current file bytes; their version is not a promise that the index has
  projected those same bytes. Pool callbacks must not recursively acquire the pool/writer.
- Use `crates/graph/tests/support` for new integration-test graph fixtures. Do not modify official
  captures to match local output or mistake an own RPC method for official API equivalence.

## Verification and CI

Measured on this checkout at `149e2fb` with the pinned toolchain, using
`mise exec -- cargo test --workspace`: **1,283 passed, 0 failed, 0 ignored**. This comprises **445
library unit tests, 767 integration tests and 71 CLI/server unit tests**; rustdoc reports zero
tests. Optional real-graph and private-capture comparisons return early without their required
inputs, so this total is not evidence that those external comparisons ran.

`mise exec -- just check` runs spelling, workspace tests, format checking, Clippy for all targets
with `-D warnings`, the performance-hook self-test, library rustdoc with `-D warnings`, and
commit-subject validation against `origin/main...HEAD`. `prek.toml` also enforces staged hot-path
performance documentation and commit-message shape; its pre-push hook runs workspace tests.

**CI exists:** [.github/workflows/ci.yml](.github/workflows/ci.yml) runs on pull requests and pushes
to `main` or `devin/**`, with an `ubuntu-latest` / `macos-latest` matrix. Each job checks out full
history at the authored PR head (not the synthetic merge commit), installs mise and Rust lint
components, restores the Rust cache, then runs `just check` and **`just deny`**. macOS uses a
canonical `TMPDIR`. Supply-chain checking is separate from local `just check` because it fetches
advisories; CI runs it even when the lockfile did not change. CI does not provision a private graph
or running official Logseq service. `just msrv` (`cargo +1.89 check --workspace`) remains manual.

Useful commands:

```sh
mise exec -- just check
mise exec -- just deny
mise exec -- just msrv
mise exec -- python3 scripts/check_perf_doc.py --self-test
scripts/verify-real-graph.sh /path/to/graph
scripts/bench.sh /path/to/graph
```

The last two operate on temporary copies; never edit the user's graph to benchmark. Report the
graph shape, toolchain/profile, machine load and before/after numbers under PERFORMANCE.md's
rules. Concurrent-read measurements include a cheap indexed-read regression, not just the
scan-heavy throughput improvement.

## Next slices and remaining limits

1. **Bound rebuild publication lock time at roughly one million pages.** The unlocked shadow
   builder and iterative zero-delta catch-up are already implemented; do not redo them. The final
   `engine/reprojection.rs::publish_shadow` still calls `shadow_delta` under the graph/scan locks,
   and that rediscovery/stat sweep grows with graph size. OS sync/publication also has no hard
   latency bound. Design and measure a bounded final verification/publication protocol without
   allowing stale scan bytes, changed configuration or writer revisions to slip through. Retain
   atomic generations, catch-up exhaustion behavior, identity reconciliation and move intent.
2. **Expand official evidence, not local guesses.** `fixtures/expected/db_q.json` captures only
   `(page-property :type [[ADR]])`; the new task/priority, date and nested-property forms have local
   tests plus pinned upstream source references, not fresh official HTTP captures. Capture them
   against a running official instance before making equivalence claims. Sibling reads (#18/#19 in
   the API matrix) also lack fresh official calls. The existing capture has no complete no-file
   name list, so exact derived-entity population parity is not established.
3. **Keep explicit compatibility boundaries visible.** General Datalog, query presentation/Clojure
   forms, sorting/sampling, scheduled/deadline predicates and configurable comma-separated property
   keys remain unsupported. Journal-title endpoints resolve through indexed pages rather than the
   upstream configured title parser; raw property text loses upstream value-kind information, so
   the page-name fallback can also fold quoted/config-excluded strings. Page properties come from
   the pre-block, not the first dash block. No synthetic pre-block selector is invented. Namespace
   and several block-property/sibling reads remain engine-only (see the API matrix). Arbitrary
   partial-line/cross-parent deletion is not defined by the implemented sibling-range API.

Reference material: [ADR index](docs/adr/README.md), [API parity](docs/logseq-api-parity.md),
[corpus parity](docs/corpus-parity.md), [lint policy](docs/lint-policy.md),
[LESSONS.md](LESSONS.md), and [PERFORMANCE.md](PERFORMANCE.md). Historical performance and lint
experiments are evidence at their stated revision, not current-state claims.
