# Performance

Measurements of this library on real graphs, and what each change did to them.

## How to use this file

Read this before optimising anything, and write to it after a change lands.

- **A number is only valid inside its own environment.** The machine, the build profile, and the
  graph are part of the measurement. Two numbers from different environments are not comparable,
  and this file says which environment produced which table for exactly that reason.
- **Every change that touches a hot path adds a row under [What each change did](#what-each-change-did).**
  If it was not measured, write `not measured` and say why. A guessed number is worse than a blank,
  because the next person will compare against it.
- **A pre-commit hook enforces that for the hot paths.** `scripts/check_perf_doc.py` refuses a commit
  that stages a measured hot-path file without staging this file. It is a local git setting
  (`git config core.hooksPath .githooks`), so a fresh clone has it only after running that line, and
  `--no-verify` bypasses it. It catches forgetting, not refusal.
- **Reproduce with `scripts/bench.sh <graph-root>`.** It copies the graph to a temporary directory,
  runs `crates/graph/examples/measure.rs` against the copy, and removes the copy on exit. The real
  graph is never written to.
- **Record the graph's shape with the numbers.** A graph grows: the tables below name page and block
  counts because a later run on the same graph will not match unless those counts match too.
- **Check the machine is idle before believing a number.** On a loaded machine every row reads 5–10%
  high, including the ones that only touch the index. Two runs minutes apart on the same build,
  one at `load average 5.2` and one at idle, differed by exactly that much; the loaded one looked
  like a regression and was not. If a run disagrees with the baseline, run it again before
  investigating, and write down `uptime` next to anything surprising.

## Environment

| | |
| --- | --- |
| CPU | Apple M1 Pro |
| OS | macOS 27.2 |
| Memory | 32 GB |
| Toolchain | rustc 1.99.1 |
| Profile | `--release`, no custom profile settings |

## Baseline: the large graph

A personal graph in daily use: **14138 pages, 112890 blocks, 16.1 MB of Markdown**, index 38.9 MB.
Measured 2026-11-01, at `3bf22ab`; re-measured at `33a5da4` after the two write slices merged, and the
index-bound rows are unchanged within noise. Re-measured again 2026-11-03 at `3f9a9da` on the same
graph, now **14143 pages and 112951 blocks**: every row above still agrees within noise, and the six
rows for the editor contract were appended from that run, which is why they name a 6192-block page.

| Operation | Time |
| --- | ---: |
| Cold start: project every file | **9512 ms** |
| Warm open: stat every file, project none | 103 ms |
| `CheckNow`: stat every file | 99 ms |
| `VerifyContent`: read and hash every file | 1154 ms |
| `search` with a hit, limit 20 | 0.93 ms |
| `search` with no hit (scans every block's text) | 32.6 ms |
| `pages`, all 14138 | 2.71 ms |
| `tags`, all 1329 | 3.33 ms |
| `page_blocks` of the largest page (6186 blocks) | 2.18 ms |
| `page_tree` of the largest page (6186 blocks) | 1.13 ms |
| `page_snapshot` of the largest page (6192 blocks) | 9.41 ms |
| `page_snapshot` of the median non-empty page (6 blocks) | 0.24 ms |
| `block_snapshot` rooted at a mid-tree block of the largest page | 10.27 ms |
| `commit_page` with one `EditText` on the largest page | 155.57 ms |
| `commit_page` with ten `EditText` on the largest page | 271.98 ms |
| `commit_page` refused as stale on the largest page | 9.44 ms |
| `refresh_paths` of one file (the 6186-block page) | 131 ms |
| `append_block` to the largest page (6186 blocks) | 150 ms |
| `insert_batch` of 10 blocks (the 6186-block page) | 241 ms |
| `insert_block` × 10 in a loop, for comparison | 1516 ms |
| `prepend_block_in_page` (the 6186-block page) | 145 ms |
| `set_block_collapsed` (the 6186-block page) | 162 ms |
| `append_block` to the median page (6 blocks) | 14.07 ms |
| `commit_page` with one `EditText` on the median page (6 blocks) | 12.19 ms |

### Re-measured at `a647df0`, after schema v11 and v12

The table above was taken at `3f9a9da`, before blocks had ephemeral ids (v11) or a task marker and a
priority (v12). Re-measured 2026-11-06 on the same graph, then grew to **14148 pages, 113285 blocks,
53.5 MB index** (from 38.9 MB: **+14.6 MB, +129 bytes per block**), at `load average 6.0-16.4` on ten
cores with four commits measured back to back so the load confounder is visible in the rows rather
than hidden in a footnote:

| Operation | `3f9a9da` (v10) | `ca53033` (v11) | `385c081` (still v11) | `a647df0` (v11+v12) |
| --- | ---: | ---: | ---: | ---: |
| Cold start: project every file | 10511 ms | **15168 ms** | 14518 ms | **17673 ms** |
| `refresh_paths` of the 6197-block page | 136 ms | 289 ms | 306 ms | **363 ms** |
| `append_block` to the 6197-block page | 154 ms | 305 ms | 308 ms | **337 ms** |
| `CheckNow` | 106 ms | — | — | 102 ms |
| `page_tree` of the 6197-block page | 1.24 ms | — | — | 1.40 ms |
| `search` with a hit | 0.93 ms | — | — | 0.91 ms |
| `search` with no hit | 32.6 ms | — | — | 34.0 ms |
| index size | 38957056 B | 49156096 B | 49086464 B | 53477376 B |

Reads did not move: every row that touches no write path is within the spread this file already
records. **The two schema slices cost the write path, and almost all of it is v11.** Minting an
ephemeral id for every block and indexing it uniquely is +44% cold start, 2.1× `refresh_paths`, 2.0×
`append_block`, and +10.2 MB of index; v12's two columns and five indexes add the remaining +4.3 MB
and about +15%. That is the shape to expect from a random 36-character key into a 113k-entry unique
b-tree: a page replacement deletes and re-inserts every block row, so each block now writes a
randomly placed index entry as well as its row, and the randomness is the point (ADR-0012) rather
than an implementation detail that could be sorted away.

Two things worth saying about what this does and does not mean. A page of 6197 blocks is the worst
case on this graph and the median page has six, where `append_block` is dominated by parsing the page
rather than by the index; and the numbers here are absolute milliseconds, not a budget — 337 ms to
append one block to a 6197-block page is slow in isolation and unchanged in practice, because no
person appends to that page in a loop. What would matter is a slice that makes the *median* page
slower, and this one did not.

### Considered and rejected: an integer surrogate for ephemeral identity

The remaining large index is `blocks_by_ephemeral_id`: 111942 of 113285 blocks carry an id, each a
36-character string inserted at a random position in a 113k-entry unique b-tree. Replacing it with an
integer the index allocates in insertion order is the obvious next candidate, and the cost of the
slice that introduced it is known from the table above (v11: +44% cold start, 2.1× `refresh_paths`,
2.0× `append_block`).

**It was rejected on a measurement, not a taste.** Measured 2026-11-06 at `a647df0` plus the median
rows in the table above, `load average 8.1-13.3`: appending a block costs **350 ms on the 6197-block
page and 14.1 ms on the median page of 6 blocks**; one `EditText` costs 350 ms and 12.2 ms on the
same two pages. A page replacement writes one index entry per block, so the median page writes seven
entries into trees of six and of 113k — the ephemeral index cannot be a measurable share of its 14 ms.
The surrogate would buy a fraction of the worst case and nothing where a person actually works.

What those 12-14 ms *are* spent on is now measured, in the section below: two durability flushes and
not the index. That is why the surrogate stays rejected — it would have shaved a six-percent share.

### Where a per-edit write's time goes

`append_block` and `commit_page` each replace one file and one page's rows, so a per-edit number is
mostly durability and partly projection. Separated by driving each step of the engine's own write path
through the public API, on the median page of 6 blocks, three runs at `load average 4.1-6.4`:

| Step | Run 1 | Run 2 | Run 3 |
| --- | ---: | ---: | ---: |
| **`append_block`, whole** | 15.58 ms | 16.65 ms | 17.41 ms |
| take the write lock (open + flock) | 0.15 ms | 0.13 ms | 0.20 ms |
| read the page's bytes | 0.03 ms | 0.05 ms | — |
| parse the page | 0.00 ms | 0.01 ms | — |
| **replace one file durably (two flushes, one rename)** | **11.08 ms** | **10.60 ms** | **9.91 ms** |
| — of which: write the bytes, no flush | — | 0.18 ms | 0.44 ms |
| — of which: flush the file | — | 5.11 ms | 4.57 ms |
| — of which: flush the directory | — | 5.02 ms | 0.04 ms |
| **project a changed page (read, parse, hash, one transaction)** | **1.01 ms** | **1.22 ms** | **1.15 ms** |
| project nothing: stat an unchanged page | 0.43 ms | 0.37 ms | 0.39 ms |
| `commit_page` with one `EditText`, whole | 11.15 ms | 10.28 ms | — |

Three things follow, and the first is the one that matters for every schema argument above.

**The index is 6-17% of an edit; durability is 60-70%.** One changed-page projection — read, parse,
hash, and one transaction replacing the page's rows — is 1.01-1.22 ms on this page. The file
replacement is 9.9-11.1 ms, and `sync_all` is `F_FULLFSYNC` on macOS: it asks the device to flush its
own cache, which is a hardware round trip and not a code path. No column, key, or index this file
records can move 10 of 17 ms.

**Both flushes are the guarantee, not an oversight.** The file is flushed before the rename so a crash
cannot leave a renamed file with unsynced content, and the directory is flushed after the rename so
the rename itself survives one. Dropping either weakens exactly the property this repository is built
on — that the Markdown file is the only durable fact — so the trade is available only as an explicit
opt-in a caller asks for, never as a default.

**Batching is where an editor buys its latency, and it is already the API.** One commit replaces the
file and the page's rows once however many operations it carries, so the flushes are paid per commit
and the per-operation cost becomes document transformation: on the 6197-block page, one `EditText` is
350 ms and ten are 460, i.e. about 12 ms for each additional edit rather than 350. A caller that
appends ten times pays ten file replacements; the same caller sending one `commit_page` pays one.

The directory flush's spread (5.02 ms against 0.04 ms) is the honest noise in this table: it is
whatever the filesystem had already flushed, so the "two flush" total is stable at 9.9-11.1 ms while
its two halves are not. Nothing here is compared against the other rows of this file — the point is
the split, not the absolute.

An earlier run of the same graph through the test harness reported 3.9 s for the cold start. The two
runs differ in where the files were read from — the benchmark copies the graph into a fresh
temporary directory first — and the rows that read file contents moved together (cold start, and
`VerifyContent` from 629 ms to 1154 ms) while the stat-only rows did not (`CheckNow` stayed at 99 ms,
`refresh_paths` at 131 ms). That pattern points at the disk rather than at the code, and it is
recorded rather than explained away: **the I/O-bound rows in this table are not comparable across
disks, only the index-bound rows are.**

## Baseline: the small graph

The 314-file Logseq documentation graph, used by `scripts/verify-real-graph.sh`:
**314 pages, 6353 blocks, 547 KB of Markdown**, index 1.35 MB. Measured 2026-11-01, at `3bf22ab`;
the six rows for the editor contract were appended 2026-11-03 at `3f9a9da`.

| Operation | Time |
| --- | ---: |
| Cold start: project every file | 200 ms |
| Warm open | 2.96 ms |
| `CheckNow` | 2.00 ms |
| `VerifyContent` | 14.0 ms |
| `search` with a hit, limit 20 | 0.25 ms |
| `search` with no hit | 1.64 ms |
| `pages`, all 314 | 0.08 ms |
| `tags`, all 23 | 0.07 ms |
| `page_blocks` of the largest page (2686 blocks) | 0.91 ms |
| `page_tree` of the largest page (2686 blocks) | 0.50 ms |
| `page_snapshot` of the largest page (2686 blocks) | 3.30 ms |
| `page_snapshot` of the median non-empty page (5 blocks) | 0.09 ms |
| `block_snapshot` rooted at a mid-tree block of the largest page | 3.84 ms |
| `commit_page` with one `EditText` on the largest page | 54.36 ms |
| `commit_page` with ten `EditText` on the largest page | 83.63 ms |
| `commit_page` refused as stale on the largest page | 3.39 ms |
| `refresh_paths` of one file (the 2686-block page) | 30 ms |
| `append_block` to the largest page (2686 blocks) | 47 ms |
| `move_block` across pages (2686-block source, 582-block destination, 5-block subtree) | 93.94 ms |

The cross-page row was added and measured 2026-11-12 on M1 Pro, macOS 27.2.1, rustc 1.99.1, with
`scripts/bench.sh`; this run held **314 pages and 6343 blocks**, so it is not a re-measurement of
the 6353-block rows above. The harness picks the two largest pages that already have a resolvable
durable block, moves the source's first durable subtree after the destination anchor, and does not
mint an `id::`. Selector discovery is setup; the timed call includes the write intent, both durable
file replacements, and the one two-page index commit. This run overlapped compilation and is an
upper bound rather than an idle baseline.

On a larger private graph (**14180 pages, 113705 blocks**, about 62 MB of Markdown), the
same row measured **103.98 ms** and **134.38 ms** for a **666-block source, 270-block destination,
25-block subtree**. The runs started at `load average 9.46` and 11.19 and ended at 10.68 and 76.74;
the second run overlapped heavy compilation. These are the available large-graph measurements, not
a comparison with the older 14k-page reference graph or evidence of a regression.

## The served tier: `logseq-graph serve`

`bench.sh` measures the adapter as well as the engine. After the `examples/measure.rs` run it builds
the CLI, starts `logseq-graph serve` on the same sandbox copy, and runs
`crates/server/examples/measure_serve.rs`, which speaks raw HTTP/1.1 to the child over a fresh
`TcpStream` per request, `Connection: close` — the same framing `crates/server/tests/rpc.rs` uses,
so the timed path is the tested path. Every single-call row answers 21 requests, drops the first
(cold) answer, and reports the median of the remaining 20, the protocol the query rows above use;
the burst row keeps requesting for a wall window and reports its own sample count. The child runs
with `--poll-interval 86400000` (one day), so the watcher never fires inside a row and these are
the adapter's own numbers; the one-second-poll run is below.

These are new baselines, not effects: before 2026-11-14 no run behind this file started `serve`, so
no served row could have moved. What a row costs over the same engine read is the adapter's JSON
parse, the request lock, and the serialization: `getAllPages` of 14182 pages answers in 15.60 ms
against the engine's 4.07 ms `pages` listing, a 6207-block `getPageBlocksTree` in 15.04 ms against
`page_blocks`' 3.68 ms, and the small answers (a 6-block page, `getPageVersion`) in 0.19–0.29 ms,
where the added cost is not separable from the TCP connect it sits beside.

Measured 2026-11-14 on M1 Pro, macOS 27.2.1, rustc 1.99.1, `--release`, one run per column, at
`load average 2.72 → 3.06` for the small-graph run and `3.31 → 2.71` for the large-graph one (the
machine's ambient load; nothing else was running). The graph shapes are the ones the runs
printed — **314 pages, 6345 blocks** on `logseq-docs` and **14182 pages, 113816 blocks** on
a larger private graph, as they stand *after* the engine tier's own writes, which is why the largest
page holds 2682 and 6207 blocks rather than the 2686 and 6206 the cold starts counted. The median
page is the median non-empty page by block count, the page the engine write rows measure; page
sizes travel in the row labels. The 314-page column was taken before the example's chunk decoder
moved from `&str` to bytes (the large-graph run found a chunk boundary inside a multi-byte page
name); the request bytes, the served binary and the decoded ASCII bodies are identical, so this is
provenance, not an effect. Both columns are new baselines: re-run a column at the time it is first
used as a comparison.

| Request | `logseq-docs` | larger private graph |
| --- | ---: | ---: |
| serve: getCurrentGraph | 0.42 ms | 3.70 ms |
| serve: getAllPages (314 / 14182 pages) | 0.76 ms | 15.60 ms |
| serve: getPageBlocksTree of the median page (5 / 6 blocks) | 0.26 ms | 0.19 ms |
| serve: getPageBlocksTree of the largest page (2682 / 6207 blocks) | 6.84 ms | 15.04 ms |
| serve: search with a hit, limit 20 (needle `logseq`, 20 returned) | 0.54 ms | 5.17 ms |
| serve: query `(task DONE)` at limit 20 (5 / 20 results, truncated on the large graph) | 0.27 ms | 2.28 ms |
| serve: query `(task DONE)` at limit 1000 (5 / 1000 results, truncated on the large graph) | 0.23 ms | 6.80 ms |
| serve: getPageVersion of the median page (5 / 6 blocks) | 0.25 ms | 0.28 ms |
| serve: getPageSnapshot of the median page (5 / 6 blocks) | 0.26 ms | 0.25 ms |
| serve: getPageSnapshot of the largest page (2682 / 6207 blocks) | 7.53 ms | 18.80 ms |
| serve: burst getPageBlocksTree of the median page, 5 s window | 34434 requests, median 0.12 ms, p95 0.18 ms, max 47.36 ms | 34710 requests, median 0.12 ms, p95 0.19 ms, max 21.19 ms |
| serve: appendBlock to the median page (5 / 6 blocks) | 10.87 ms | 11.53 ms |

### The watcher's interference

The same served tier on the larger private graph with the default one-second poll
(`BENCH_POLL_MS=1000 scripts/bench.sh`), same machine, same build, at `load average 3.04 → 3.09`. Every
single-call row is unchanged: `getCurrentGraph` 3.70 → 3.76 ms, `getAllPages` 15.60 → 15.58, the
6207-block `getPageBlocksTree` 15.04 → 15.13, `search` 5.17 → 5.21, `(task DONE)` at limit 1000
6.80 → 6.84, the largest `getPageSnapshot` 18.80 → 18.85, `appendBlock` 11.53 → 11.55 — every
difference inside 0.1 ms, i.e. not separable from run-to-run noise.

The burst row is where the passes show, because a 5 s window at one request at a time overlaps
roughly five of them, and each pass is a ~100 ms stat walk of the 14k files on the watcher's own
handle (the `CheckNow` row above):

| 5 s window of `getPageBlocksTree` of the median page | poll off (86400000 ms) | poll on (1000 ms) |
| --- | ---: | ---: |
| requests answered | 34710 | 33656 |
| median | 0.12 ms | 0.12 ms |
| p95 | 0.19 ms | 0.19 ms |
| max | 21.19 ms | 46.65 ms |

One run per column and one max sample per run, so the max is a tail observation rather than a
rate; the shape is what matters — the watcher holds no request lock, so the median and p95 do not
move, and the poll costs about 3% of the window's throughput plus a doubled tail.

**Decision: keep the fixed one-second poll, with no coalescing or adaptive scheduling.** One pass
is a `CheckNow`-shaped stat walk of about 100 ms over 14182 files, so the duty cycle is roughly a
tenth of one core on this machine; no single-call row moves by a measurable amount, and the burst's
median and p95 are identical. Adaptive backoff would buy back a share of that background work while
weakening ADR-0008's promise that an external edit is visible on the next poll. Path-scoped
coalescing needs a filesystem change-notification source this repository does not have:
`refresh_paths` refreshes paths a caller already names, and discovering which paths changed
externally is exactly the stat walk. The watcher is one `sleep → rescan` thread, so a pass that
runs long already delays the next rather than overlapping it; `--poll-interval` stays the operator's
lever. Revisit when a graph's `CheckNow` pass approaches its interval, when a request-latency budget
exists, or when a graph far larger than 14182 files is measured — the served-tier rows are the
baseline for any such claim. No code or ADR changed: the service contract is the one ADR-0008
already states.

## What each change did

Newest first. `not measured` means exactly that.

The rows dated 2026-11-03 were measured on `main` at `3f9a9da`, on the graph below, with
`scripts/bench.sh` and the rows it prints: **14143 pages, 112951 blocks, 38.9 MB index**, `load
average 8.65` for the first run and `7.10` for the second. The index-bound rows in the large-graph
baseline agree with it to within noise (`CheckNow` 103 ms against 99, `page_tree` 1.18 against 1.13,
`append_block` 149.07 against 150), which is what makes the numbers comparable at all. Where a row's
first run and re-run differ by more than 5% both are given. The 314-page graph was measured once, at
`load average 7.46`. The `snapshot` row was measured on the reference graph after it grew to
**14147 pages and 113001 blocks**, at `load average 11.3-13.5`; its numbers are three rounds each,
and the `CheckNow` it cites was measured in the same session.

| Date | Commit | Change | Effect |
| --- | --- | --- | --- |
| 2026-11-20 | working tree | rusqlite 0.41 -> 0.43; the `search_key` SQL function takes `&mut Context`, the signature 0.43 requires | **Not measured.** The function body is unchanged; only the closure's argument type changed, and the bundled SQLite moved with rusqlite. |
| 2026-11-20 | working tree | `[profile.test] opt-level = 1` in `Cargo.toml`, so `cargo test` builds the workspace and its dependencies at opt-level 1 | **Measured locally (8 vCPU, cold target dir):** test build 15 s -> 35 s, test run 60 s -> 15 s, 54 test binaries pass either way. The run was dominated by five 10,000-level nesting tests at about 15 s each unoptimized. Release builds and `cargo build` are unchanged. |
| 2026-11-19 | working tree | Method names resolve the official way (snake leaf for `logseq.` names, as-is otherwise; writes keep exact names); one status→reason table serves the status line and the Fastify `error` member | **Not measured.** Resolution was already one linear scan of 19 spec leaves; it now builds two short strings per request (`to_lowercase` over a dozen bytes) beside the scan. The reason-phrase table replaces one `match` with an identical `match`. No graph read or write changes. |
| 2026-11-19 | working tree | `getUserConfigs` reads `:journal/page-title-format` from `config.edn` per call instead of a start-up cache; `DB.q` page entries state `journalDay` as the number | **Not measured.** `getUserConfigs` adds one small `config.edn` parse on a rare call — no index query, scan, or graph read — and `journalDay` is three integer multiplies in an answer that already exists. No served-tier row separates either from run-to-run noise. |
| 2026-11-19 | working tree | `bench.sh` resolves the release binary via `cargo metadata` and the served example takes a `notify|poll` watcher mode (the poll run passes `--no-watch`); `source::write::tests` canonicalizes its temp root | **Not measured.** Test and harness-only edits: `move_to_recycle` and the served request path are unchanged, so nothing a row could name moved. |
| 2026-11-19 | working tree | `serve` requires a bearer token (ADR-0018): one `Authorization` header lookup and one case-insensitive comparison before the body is read, and startup refuses without `--api-token`/`LOGSEQ_API_TOKEN` | **Not measured.** No graph read, scan, write, projection, or serialization changes. The gate is one linear header search and one string comparison on a request that already parses JSON and takes the state mutex, so a served-tier row cannot separate it from run-to-run noise. The new tests pin the 401 shape, the unauthorized write that never reaches a file, and the startup refusal. |
| 2026-11-19 | working tree | `POST /api` answers the official wire shape: a success is the bare result value (the JSON-RPC envelope and `id` echo are gone), a failure is its HTTP status with Fastify's `{statusCode, error, message}` plus this repository's own `code` and structured `data`; the official CORS policy is answered (preflight 204 before authentication, origin headers on every answer) | **Not measured.** The success body is slightly smaller (no envelope) and the error body slightly larger (three more members); both are re-serializations of JSON that was already being serialized. CORS is a few static headers on a response that already serializes JSON. No query, scan, or file read was added, so no served-tier row can separate these from run-to-run noise. The tests pin the wire shape and the CORS headers. |
| 2026-11-19 | working tree | The served shapes now match the captured official answers (wire-shape differential): `App.search` states `has-more?`, caps `blocks` at 10 and answers a bare `block/uuid`; `getBlock` reads a bare id as durable then ephemeral; `getPage`/`DB.q` fold `name` and keep `originalName`; `getCurrentGraph` adds `url`; `getUserConfigs` reads the graph's `:journal/page-title-format` from `config.edn` on each call and `getCurrentPage` answers `null`; `logseq.DB.q`, `showMsg`, `getAppInfo` and `checkCurrentIsDBGraph` are added as constant-level reads | **Not measured.** The search read gains one fetched hit (`max(limit, 10) + 1` instead of `limit`) on a query it already runs — at most one extra row from the same indexed scan — and its SQL, limit and row order are otherwise unchanged. `getBlock` runs a second indexed lookup only when the first finds nothing (the not-found path, off the hot path). The folding is one `to_lowercase` per answered name; `url` is one `format!`; `getUserConfigs` adds one small `config.edn` read on a rare call; the other constant methods read no index. No index query, scan or file read is added on any answering path, so no served-tier row can separate these from run-to-run noise. `crates/server/tests/rpc/official_shapes.rs` pins the shapes. |
| 2026-11-19 | working tree | `Editor.getBlock` and `DB.q` block entries state `page` as `{name, uuid, id}` with a projection-derived numeric page id, and `Editor.getPage({id})` resolves it | **Not measured.** The id is one 64-bit FNV-1a hash of the folded name per stated page (a string of a few dozen bytes), computed while serializing an answer that already exists; `getPage({id})` is the only new cost — it reads the page listing (`pages()`) to hash names and find the id, and only a client's recall path issues such a read. No scan, file read or added SQL on the ordinary name path. No served-tier row separates the hash from run-to-run noise; a dedicated `getPage({id})` row would. |
| 2026-11-19 | working tree | The `Origin`-must-equal-`Host` refusal is removed from `transport_guard`; the bearer token is the cross-origin gate (ADR-0018). A head carrying more than one `Origin` is still refused, and the loopback `Host` guard is unchanged | **Not measured.** The guard now counts `Origin` headers instead of comparing one against `Host`, so the head path does strictly less work — one fewer string comparison per request on a request that already parses JSON. No graph read or write changes. The tests pin the foreign-origin-with-token round trip. |
| 2026-11-19 | short CJK search | Schema v18 adds a contentless unicode61 index of folded CJK unigrams and adjacent bigrams; 1–2 all-CJK queries use it, mixed/non-CJK short queries retain the scan; BM25 cutoff stays 100 | **Twice on this Linux VM, main `6bba53f` → B.** Enriched synthetic 3000-page/60000-block graph: cold projection **3534 / 3525 → 5557 / 5278 ms**, index **24.703 / 24.682 → 30.904 / 30.913 MB**. Sparse `书` **2.1663 / 2.1288 → 0.1993 / 0.1973 ms**, `中文` **2.2076 / 2.1123 → 0.1971 / 0.1981 ms**; one-hit `龘` **70.6871 / 69.8555 → 0.1268 / 0.1269 ms**, miss `齉齾` **71.5695 / 70.3109 → 0.0980 / 0.0975 ms**. Common early-hit `的` is slower: **0.1193 / 0.1177 → 0.1907 / 0.1897 ms**. Pinned docs: cold **352.72 / 372.26 → 410.67 / 438.54 ms**, index **3.449 / 3.453 → 3.543 / 3.551 MB**, `我们` miss **4.5028 / 4.5326 → 0.0745 / 0.0739 ms**. Measured alternative U saves about 2 MB on the synthetic graph, but `中文` takes **0.3270 / 0.3279 ms** versus B's **0.1971 / 0.1981 ms**. Full BM25 on common B terms still costs **38.61–39.57 ms**; probe plus unranked hits **0.138–0.170 ms**, so retain cutoff 100. Latin unchanged within noise. [Full rows, U comparison, and enrichment recipe](#short-cjk-search-linux-vm). |
| 2026-11-19 | read-pool | Four read-only connections behind `SharedGraph`; RPC reads release the startup-state mutex | Release, 1000 pages / 60000 blocks, four workers × 40 `LastScan` searches: mixed short miss throughput **15.95 / 15.23 → 62.15 / 49.47 reads/s** versus main `93a1809` (3.90× / 3.25×). Indexed miss throughput regresses **9731 / 9647 → 8058 / 8360 reads/s**. See the concurrent-read probe below for timings, variance, and reproduction. |
| 2026-11-19 | query-dsl property case | Restore page-name comparison for unbracketed property values such as `Book` inside the SQLite scalar function | not measured: restores the previous per-candidate page-name fold; no schema, SQL access-path, or owner-materialization change. |
| 2026-11-19 | query-dsl | Preserve nested query set boundaries; filter property values within SQLite; sort journal endpoints and accept journal-day integers | not measured: no representative graph is provisioned in this clone. Property reads no longer materialize candidate owner IDs in Rust; SQLite still interprets each candidate property's text through a deterministic scalar function. Nested sets may require SQLite temporary sets; task/priority/date indexes and schema are unchanged. |
| 2026-11-19 | working tree | Derive no-file page entities and resolve page titles and alias backlinks | `scripts/bench.sh`, release, Intel Xeon VM (8 CPUs), Linux 6.8, Rust 1.99.1, official docs corpus v0.9.2 (274 files, 5,189 blocks), one run per build: cold start **97.70 → 107.37 ms**, warm open **6.97 → 6.93 ms**, changed-page projection **0.48 → 0.57 ms**, index **1,572,864 → 1,716,224 bytes**. Listing now returns 700 rather than 274 pages (**0.14 → 0.80 ms**) and 17 rather than 14 tags (**0.09 → 0.16 ms**). Largest-page snapshot **1.90 → 2.70 ms** and version read **0.23 → 0.51 ms** with title verification; median-page version read **0.0343 → 0.0374 ms** (100 reads averaged). Backlink timing not measured by this harness; the query plan tests require seeks on the persisted reference keys. |
| 2026-11-19 | working tree | Match official bare-bullet text, boundary blank lines, and unclosed code fences | Not measured: the reference performance graph is unavailable on this VM. Fence matching adds one linear reverse pass and one optional-fence entry per source line; projection still reads each block's own lines once. |
| 2026-11-19 | sibling range delete | Serve `remove_block_range` via RPC and CLI | Not measured: one explicit range operation resolves two selectors, checks ephemeral fingerprints, and cuts a contiguous source-line run before the existing single-page replacement and projection. RPC adds only typed argument/result plumbing. Ordinary single-block removal retains its pure edit and single commit path. |
| 2026-11-19 | cross-page positional intent | `source/intent.rs` optionally parses and serializes child-index paths; RPC drops obsolete move refusals | Not measured: no measurable cost expected from two small optional path arrays in the existing journal. Single-page writes and the multi-page hash protocol are unchanged; ephemeral endpoints pay the existing freshness hash and validate their paths against the parsed pre documents. RPC argument and result shapes are unchanged. |
| 2026-11-19 | on-demand durable id | Expose `ensure_block_id` through RPC and CLI | Not measured: a new explicit operation uses the existing one-page property-write pipeline; an already durable target reads and parses its page but skips replacement and projection. Existing writes retain their commit path. |
| 2026-11-19 | folded trigram search, bounded ranking | Schema v17 indexes `search_key(blocks.text)` with contentless FTS5 trigrams; literal phrase searches probe at most 101 matches, use full BM25 for at most 100 matches, otherwise return unranked FTS rowid order; shorter searches retain the scan | **Measured twice before/after on this Linux VM, not the M1 baseline.** Pinned `logseq/docs`: cold projection **104.43 / 117.73 → 403.76 / 354.15 ms**, index **1.56 → 3.32 MB**, Latin hit **0.1244 / 0.1271 → 0.4457 / 0.4380 ms**. Synthetic mixed CJK/Latin (3000 pages, 60000 blocks): cold projection **862.01 / 877.86 → 3465.36 / 3476.67 ms**, index **13.65 → 22.32 MB**; Chinese hit **1.9195 / 1.9842 → 0.2571 / 0.2722 ms**, Chinese miss **60.7760 / 62.1800 → 0.0987 / 0.0981 ms**. Synthetic common Latin `logseq` **0.0626 / 0.0606 → 0.3284 / 0.3433 ms**, down from the initial always-ranked **78.0949 / 79.4386 ms**, but still about 5× the scan, not within 2×. Threshold 100: probe plus exact ranking at 100 matches **0.6164 / 0.6144 ms**. Threshold is not a result cap or best-of-first-100 ranking. Build/size numbers reused (unchanged schema); search rows rerun. Full queries, all rows, environment, generation recipe, and caveats are [below](#folded-trigram-search-linux-vm). |
| 2026-11-19 | working tree | Catch up shadow differences and finalize identities/revisions outside the graph lock; publish only a zero-delta generation | Ubuntu Linux VM, 8-vCPU Xeon, 31 GiB RAM, ext4 virtual disk, release, warm cache, 100,000 pages / initially 1,743,864 blocks; log-normal blocks/page (median 9, mean 17.44, max 350), 30% journals, nested blocks/properties/refs/tags, seed 20261119. External changed files **0 / 1,000 / 5,000 / 20,000**: longest 1-ms-sampled lock hold **0.560 / 0.584 / 0.548 / 0.609 s**; publication-window RPC **HTTP 200 in 0.570 / 0.664 / 0.614 / 0.694 s**; config-trigger-to-publication **122.447 / 124.074 / 130.673 / 146.345 s**. Previous `1315866` measurements for 0 / 1,000 / 5,000 changes held the lock **0.707 / 2.290 / 6.972 s**, with writes **200 in 0.729 s / 200 in 2.353 s / 503 after 5.001 s**; 20,000 before not measured. Each round also includes one successful mid-build RPC file, so actual differences are M+1. All 26,000 external edits survived; final 1,769,872 blocks. Additional unlocked stat passes and sync increase total work; retry allowance is eight rounds, final locked repair allowance zero files/bytes, exhaustion leaves the old generation. Metadata sweeps/OS IO still have no real-time bound. After measurements used the catch-up implementation on `1315866`; later unrelated telemetry/module changes were rebased before gating. Host load varied about 1–3.5; one run per case, not an SLA. Probe/results: `/home/ubuntu/graph-plan-probes/catchup-100k/{measure.py,run.log,100000/results.json}`; before: `scaling-1315866/100000/results.json` in the same parent directory. |
| 2026-11-19 | RPC writes split | Group RPC write parsing and engine calls by domain, with shared parameters, answers and errors | None. Existing functions and match arms moved into domain modules; no code path changed. |
| 2026-11-19 | working tree | Count watcher need-rescan events, watcher errors and full rescans by cause; report them in `getCurrentGraph` | Not measured: one relaxed atomic increment per notify event and per full rescan, and a few relaxed loads per `getCurrentGraph`; no lock, allocation or I/O added to the watcher loop beyond a cause list per full rescan. |
| 2026-11-19 | working tree | `getCurrentGraph` and `info` report the live generation's physical bytes, plus shadow and retired generation bytes | Not measured: one pointer read, one build-lock probe, one listing of the index directory and a few `stat` calls per `getCurrentGraph`; no index query and no graph scan. |
| 2026-11-19 | document facts split | Split document facts into block, property, and link feature modules | None. Functions and types moved into feature modules; no code path changed. |
| 2026-11-19 | query module split | Split index reads and simple-query execution into feature modules | None. Existing functions, types, and tests moved between files; no code path changed. |
| 2026-11-19 | engine module split | Split the graph facade into options, errors, startup, freshness, refresh, projection, and feature-focused reads | None. Existing definitions and methods moved into submodules; no code path changed. |
| 2026-11-19 | index split | Split index operations, schema SQL, and tests into responsibility modules | None. Items moved between files; no code path changed. |
| 2026-11-19 | working tree | Split the engine write module by operation | None. Functions and types moved into operation modules; no code path changed. |
| 2026-11-19 | working tree | Build full reprojections in generation-specific shadow databases; atomically select the verified generation | Release, same warm-cache 10,000-page × 40-block graph, forced journal-format configuration change: longest observed write-lock hold **14.215 s → 0.052 s** (1-ms flock sampling). RPC issued while the late publication lock was held succeeded **HTTP 200 in 0.068 s**; file and index both held 41 blocks afterward. Read-start to publication: 22.362 s (copy/build is outside the lock). Shadow copying, unchanged-byte identity restoration, FULL synchronization, a generation pointer and leases increase disk/IO overhead; ordinary Graph reads add a pointer check. Concurrent mass changes can still make locked fix-up expensive. Windows not measured; directory fsync is Unix-only. Probe/log: `/home/ubuntu/graph-plan-probes/shadow_probe.py`, `shadow-measurement.log`. |
| 2026-11-19 | config split | Split source config into responsibility modules | None. Items moved between files; no code path changed. |
| 2026-11-19 | RPC split | Split the RPC adapter into parsing, errors, state, transport orchestration, shared answers and read-domain modules | None. Items moved between files; no code path changed. |
| 2026-11-19 | working tree | Allow index work on read-only graphs without bypassing an available graph lock | Not measured: writable graphs use the same lock operations; only a permission failure adds a fallback lock lookup. Scan preparation and SQLite publication are unchanged. |
| 2026-11-19 | working tree | Replace served polling with debounced filesystem notifications and path refreshes | Release, Ubuntu Linux VM, origin/main `3b9d149` versus this change: idle serve CPU over 60 s **12.01 s → 0.00 s** (20.0% → below 0.01-s tick resolution); change-to-default-read median over 20 successive external appends **1235.30 ms → 58.14 ms**. No reference graph was available: generated 14,136 flat Markdown pages, initially 94,236 blocks / 15,893,716 bytes, 53.3% empty, median non-empty 6 blocks, two large pages of 1234 / 6187 blocks. Separate warm indexes, default 1000-ms polling period; CPU is /proc/PID/stat utime+stime delta, latency is monotonic append-to-getPageBlocksTree visibility via POST /api, queried every 10 ms with a unique token, on an initially six-block page. The before/after trials add 20 blocks each. Host load was about 2 during baseline (local gate overlap); idle means no graph edits or read requests during the 60-s window. Probe and raw samples: `/home/ubuntu/measure-watcher.py`, `watcher-before.json`, `watcher-after.json`; synthetic graph: `/home/ubuntu/watcher-bench-realistic`. |
| 2026-11-19 | working tree | Retry accept failures, grow request bodies only as bytes arrive, and drain early rejections after closing the write half | Not measured: non-interrupted accept errors back off 25 ms and report once per streak instead of exiting. Declared Content-Length and chunk sizes no longer allocate their full body before bytes arrive. Rejection cleanup discards at most 1 MiB for at most 250 ms (nonblocking for capacity refusals); successful calls retain their existing close path. Unit tests cover truncated bodies and full JSON rejection responses with 128 KiB of unread input. |
| 2026-11-19 | working tree | Isolate socket I/O from RPC handler slots and bound connection lifetimes | Ubuntu Linux 6.8 x86_64, 8-vCPU Intel Xeon, debug builds, one-page / one-block scratch graph, loopback `getCurrentGraph`, fresh connection per call, 20 warmups then 200 sequential calls: median **0.263 → 0.350 ms**, p95 **0.338 → 0.423 ms**, mean **0.264 → 0.359 ms** (one run each; load averages 0.12 / 0.11). The per-connection thread and explicit framing add about 0.09 ms here; these are transport microbenchmarks, not large-graph read measurements. Sixteen stalled bodies previously made a normal request exceed 3 s; the same regression now has a free handler slot. Reproduction script: `/home/ubuntu/measure-http-transport.py`, baseline binary copied before replacing tiny_http, same Python socket loop against both binaries. |
| 2026-11-19 | working tree | Prepare full scans outside the graph lock; verify metadata and publish prepared facts in one SQLite transaction | Release, same warm-cache 10,000-page × 40-block scratch graph, forced reprojection by journal-format config change: longest observed lock hold **26.982 s → 14.215 s** (1-ms flock probe). A served append after inotify observed the first preparation read succeeded **HTTP 200 in 0.006 s**, versus the previous **503 in 5.001 s**; its 41 file blocks matched 41 indexed blocks. Read-start to publication: 15.174 s. Batch SQL still holds the lock for 14.215 s, so writes arriving during commit can still exceed the default 5-s budget. Preparation retains graph-sized facts and a full transaction grows WAL/temporary storage; incremental unchanged files still avoid parsing. The already-locked durable-id fallback remains the deliberate exception. Probe/log: `/home/ubuntu/graph-plan-probes/prepared_scan_probe.py`, `prepared-scan-measurement.log`. |
| 2026-11-19 | working tree | Move recovery intent from disposable SQLite to a graph-wide durable journal beside `write.lock` | Not measured: cross-page moves add one atomic journal replacement with file/directory fsync before page writes, and one removal/directory fsync after the index commit. Opening checks for a legacy row under the graph lock before an index rebuild; ordinary single-page writes create no journal. |
| 2026-11-19 | working tree | Persist projection configuration signatures and reconcile mismatched write handles; share sources through `Arc` | Not measured: a write adds one singleton signature comparison in SQLite and an Arc clone rather than cloning the configuration. A missing or changed signature pays for a locked full reprojection on open or before name resolution; ordinary matching writes do not scan. Schema v14 causes one cold rebuild of older indexes. |
| 2026-11-19 | working tree | Contain recycle, creation cleanup and lock-file mutations at resolved parents | Not measured: recycle validates both parents before mkdir/rename; create, replacement and cleanup share ancestor resolution; lock acquisition checks the lock entry before opening it. No parse or projection work changes. |
| 2026-11-19 | working tree | Exclude full scans and path refreshes with the graph-wide write lock through projection | Not measured: each scan or refresh adds one filesystem lock acquisition; writes can wait for a whole scan rather than overlap it. Last-scan reads are unchanged. |
| 2026-11-19 | working tree | Resolve the non-Git write lock from the canonical graph root, not the index parent | Not measured: lock-location resolution remains once per open; lock acquisition and write paths add no work. |
| 2026-11-19 | working tree | Validate the resolved creation parent stays inside the root, including missing-parent ancestors | Not measured: creation adds ancestor metadata checks and one canonicalization before creating bytes; replacement and reads are unchanged. |
| 2026-11-19 | working tree | Publish a successful scan's configuration for writing handles to adopt under the write lock | Not measured: writes clone the small source configuration through the shared observation mutex; no extra filesystem scan or index query is introduced. |
| 2026-11-19 | working tree | Bound HTTP bodies to 16 MiB and read them on sixteen fixed workers | Not measured: the real-graph benchmark does not start HTTP; graph dispatch remains serialized and no library operation changes. |
| 2026-11-19 | working tree | RPC Host, Origin and JSON media-type guards before dispatch | Not measured: the adapter adds header validation but changes no library read or write; the graph benchmark does not exercise HTTP. |
||||||| parent of 8bad1b0 (perf(server): measure the served request surface)
| 2026-11-14 | working tree | `examples/measure_serve` in `crates/server`, wired into `scripts/bench.sh` as a second phase: the built `logseq-graph serve` starts on the bench copy and representative JSON-RPC calls are timed over raw HTTP, with `BENCH_POLL_MS` naming the child's poll interval (default 86400000, one day, so the rows are clean of watcher passes) and `BENCH_BURST_SECS` fixing the burst row's wall window (default 5) | **Measured on sandbox copies only; new baselines, not an effect.** The served tier had no rows before — `bench.sh` never started `serve` — so nothing could move; the numbers live in "The served tier" section above: `logseq-docs` (314 pages, 6345 blocks) from `getCurrentGraph` 0.42 ms to `getPageSnapshot` of the 2682-block page 7.53 ms, the larger private graph (14182 pages, 113816 blocks) from 0.19 ms for a 6-block tree to 18.80 ms for the largest snapshot, at `load average 2.72 → 3.06` and `3.31 → 2.71`. The one-second-poll interference run beside them shows the burst row alone moving: 33656 requests at max 46.65 ms against 34710 at 21.19 ms with the poll off, every single-call row unchanged within 0.1 ms. No hot path changed: the example is a client of the adapter, and `bench.sh` only gained a phase. |
||||||| parent of df0d214 (docs: the served-tier row names the commit it describes)
| 2026-11-14 | working tree | `examples/measure_serve` in `crates/server`, wired into `scripts/bench.sh` as a second phase: the built `logseq-graph serve` starts on the bench copy and representative JSON-RPC calls are timed over raw HTTP, with `BENCH_POLL_MS` naming the child's poll interval (default 86400000, one day, so the rows are clean of watcher passes) and `BENCH_BURST_SECS` fixing the burst row's wall window (default 5) | **Measured on sandbox copies only; new baselines, not an effect.** The served tier had no rows before — `bench.sh` never started `serve` — so nothing could move; the numbers live in "The served tier" section above: `logseq-docs` (314 pages, 6345 blocks) from `getCurrentGraph` 0.42 ms to `getPageSnapshot` of the 2682-block page 7.53 ms, the larger private graph (14182 pages, 113816 blocks) from 0.19 ms for a 6-block tree to 18.80 ms for the largest snapshot, at `load average 2.72 → 3.06` and `3.31 → 2.71`. The one-second-poll interference run beside them shows the burst row alone moving: 33656 requests at max 46.65 ms against 34710 at 21.19 ms with the poll off, every single-call row unchanged within 0.1 ms. No hot path changed: the example is a client of the adapter, and `bench.sh` only gained a phase. |
| 2026-11-14 | `c57d387` | `examples/measure_serve` in `crates/server`, wired into `scripts/bench.sh` as a second phase: the built `logseq-graph serve` starts on the bench copy and representative JSON-RPC calls are timed over raw HTTP, with `BENCH_POLL_MS` naming the child's poll interval (default 86400000, one day, so the rows are clean of watcher passes) and `BENCH_BURST_SECS` fixing the burst row's wall window (default 5) | **Measured on sandbox copies only; new baselines, not an effect.** The served tier had no rows before — `bench.sh` never started `serve` — so nothing could move; the numbers live in "The served tier" section above: `logseq-docs` (314 pages, 6345 blocks) from `getCurrentGraph` 0.42 ms to `getPageSnapshot` of the 2682-block page 7.53 ms, the larger private graph (14182 pages, 113816 blocks) from 0.19 ms for a 6-block tree to 18.80 ms for the largest snapshot, at `load average 2.72 → 3.06` and `3.31 → 2.71`. The one-second-poll interference run beside them shows the burst row alone moving: 33656 requests at max 46.65 ms against 34710 at 21.19 ms with the poll off, every single-call row unchanged within 0.1 ms. No hot path changed: the example is a client of the adapter, and `bench.sh` only gained a phase. |
| 2026-11-13 | working tree | Write failures carry one library-owned state: a source durability failure after the target became visible reports `Committed`, post-commit answer reads map to `IndexAfterWrite`, and CLI exit codes and RPC `write_state` derive from `WriteError::write_state()` | **Not measured.** The normal write still performs the same write, fsyncs, rename, re-observation, one query, and one transaction; the only new work on success is a formatting-free `match` over the existing error value, which runs only after a write has already failed. The changed code touches the durability and error branches, not a successful read, parse, projection, or serialization path, so no baseline row can move. The new source and engine fault tests exercise only failure paths. |
| 2026-11-13 | working tree | `http/rpc/methods` owns method names, leaf resolution, arity, read options, and limit ceilings for reads and writes | **Not measured.** A request resolves through one linear scan of the same 27 method specs that canonical names and error discovery read, replacing separate match statements and integer-indexed name arrays. This changes no graph read, file operation, SQL statement, argument shape, or serialization; no request-latency claim is made. |
| 2026-11-13 | working tree | Every single-page edit uses `commit_page_edit` for its atomic file write, fresh observation and parse, and whole-page index replacement; marker decoding has one index owner | **Not measured.** The append, update, property-write, remove, and same-page move paths call the helper that insert and prepend already used, with the same operations in the same order and the caller's write lock held throughout. No new read, parse, projection, or index statement is added; the metadata and block answers still come from the committed rows. This removes duplicated orchestration, not a measured cost. |
| 2026-11-13 | working tree | Block views carry the structural marker already stored in the index; the RPC tree answer reads `page_blocks` directly instead of reading and joining a second `PageTree` | **Not measured.** Correctness first: `getBlock` and block-valued writes now report headings rather than a fabricated bullet. The tree path removes one block-row read and tree reconstruction; other block reads decode the two existing marker columns beside the rows they already fetch. `bench.sh` has no RPC request benchmark, and its existing `page_blocks` row now bounds the changed read, but no new timing is claimed. |
| 2026-11-12 | `e8a9b7f` | JSON-RPC adapter: expose the fourteen library writes under own `logseq.Graph.*` methods, translate their arguments, results and structured errors, and hold the server state lock for each complete mutation (ADR-0014) | **Not measured as a server benchmark.** `bench.sh` exercises `examples/measure.rs` and never starts `serve`, so there is no request-latency or lock-contention number to report. The library's existing write, batch and cross-page rows remain the measurements of the underlying mutations; this row does not reuse them as adapter timings or claim that serialization and request locking are free. |
| 2026-11-12 | working tree | The served graph gains a polling watcher on a separate engine handle and SQLite connection; related handles share scan exclusion and scan observations, and `last` stays fail-closed until a failed poll is repaired | **Not measured as a server benchmark.** A default read still takes no scan or filesystem walk; it now locks a short-lived shared observation before its ordinary index query, while the watcher pays one `CheckNow` per configured interval outside the request lock. The existing large-graph `CheckNow` rows bound that background work at roughly 100 ms per pass, but this change does not claim a request-latency measurement. |
| 2026-11-12 | working tree | `examples/measure`: a reproducible cross-page `move_block` row, using two real durable-id pages and stating both page sizes and the moved subtree size | **Measured on sandbox copies only.** `logseq-docs`: **93.94 ms** for 2686 → 582 blocks, 5-block subtree. Larger private graph: **103.98 / 134.38 ms** for 666 → 270 blocks, 25-block subtree, under the graph shapes and load recorded above. The write path itself is unchanged; these are new baselines, not an optimisation. |
| 2026-11-12 | `d106b58` | `crates/server/src/http.rs` → `crates/server/src/http/rpc/mod.rs`: the read-only JSON-RPC adapter moves into the `http/rpc/` directory the 2026-11-10 amendment to ADR-0003 names as the one HTTP boundary, as a pure `git mv` behind a declaration-only `http` module that re-exports `serve`, and `HOT_PATHS` in `scripts/check_perf_doc.py` follows it in the same commit | **Not measured, and there is nothing to measure: no behaviour changed.** The file moved without one byte of its content changing — `git mv` renames it, `main.rs` still reaches the same `serve` through the same `http::serve` path, and the module tree gained one declaration and one re-export, which emit nothing on any read, write, scan, or serialize path. The adapter's own cost per call is still unmeasured, for the reason the `249a3bb` row states: `bench.sh` never starts `serve`, so there is no real-graph row to move. The `HOT_PATHS` change is the reason this row exists: the old `crates/server/src/http.rs` prefix would have stopped matching the moved file, and the prek check would have stopped asking for a row on adapter changes at all. |
| 2026-11-12 | `eec4acc` | `domain::search_key` folds block text for Unicode case-insensitive substring matching; `index` registers it as a `search_key` SQL scalar function on every connection, and both `query::search` and the simple-query content leaf use the folded statement only for a non-ASCII needle | **Not measured on a real graph, and no baseline row can move.** An ASCII needle still runs the byte-identical built-in `lower` statement — for an ASCII needle the two folds agree on every position — so `search` with a hit, `search` with no hit, and the query content rows have no added step. The new function is called once per scanned block text for a non-ASCII needle only (plus once per hit for the snippet position), and returns its argument without a copy when the text is already folded; that path has no pre-existing row in this file. No schema version or projection column changed, so cold start, index size, and every write row are untouched. Quantifying a full scan with a non-ASCII needle needs a `bench.sh` row that prints one; until then there is no number to compare against. |
| 2026-11-12 | `51324eb` | `document::facts`: a `#` inside the label of a Markdown inline link or image is no longer a tag, measured against the pinned official graph parser on a one-file probe (see `docs/corpus-parity.md`). Tag extraction alone sees a label-masked copy of each visible line; page refs and block refs keep the unmasked line, and `PARSER_VERSION` moves to 5 because the bytes did not move and the tag rows a page projects to did, so an existing index needs the rebuild that refuses to serve the stale tag `2`. Tests: `a_hash_inside_a_markdown_link_label_is_not_a_tag`, `a_hash_inside_brackets_without_a_link_still_counts` | **Not measured.** The real graph this file measures is not on this machine (`LOGSEQ_GRAPH_REAL` unset, no 14k-page checkout present), and the dispatch explicitly stopped the search for one, so no `scripts/bench.sh` number can be produced here. What can be said without a number: the added scan has a fast path that returns the line unchanged when it holds no `](` — one substring search per visible line — and only lines that hold a link take the bracket-stack pass and one allocation, `PageFacts::extract` of 6192 blocks is 5.06 ms against a cold start of 16.5 s. A guessed cold-start delta would be smaller than this table's own spread and would be invented. Re-run `scripts/bench.sh <graph-root>` on the reference graph before citing a number. |
| 2026-11-12 | `3a67224` | `document::parser`: only `-` opens a block. `*` and `+` are valid CommonMark bullets and are text to Logseq, so a logbook line, a `curl -v` line, or a code fragment becomes continuation text instead of an outliner entry; `index::schema` moves `PARSER_VERSION` to 4, and `BlockMarker::Bullet` no longer claims to cover three spellings | **Measured on the reference graph; the block count is the effect, not the speed.** Two `scripts/bench.sh` runs on M1 Pro, macOS 27.2.1, rustc 1.99.1, `--release`, `uptime` load 10.66 and 10.15. **14,178 pages, 113,688 blocks, index 50,356,224 and 50,364,416 bytes**, against 114,089 blocks before the change on the same checkout: **401 fewer blocks**, because 429 `*` or `+` lines in 19 files stop being entries and become continuation text (the rest of the 429 were already continuation lines). Cold start **17,874.12 / 16,855.24 ms** against the previous row's 16,510.98 on a quieter machine — the two runs differ from each other by 5.7%, which is the machine, so read the direction, not the number. Warm open 103.97 / 102.40 ms against 103, `CheckNow` 100.73 / 100.05 against 102.20, `search` with no hit 35.94 / 34.19 against 35.86, `refresh_paths` of the largest page 318.18 / 319.42 ms against 429.57 on the busier run before it, `append_block` to the largest page 341.04 / 323.38 ms against 316.84. The rule itself is two byte comparisons removed from `parse_marker` (`*` and `+` used to be accepted there), so there is no added work to measure; what changed is which rows a page projects to. `PARSER_VERSION` moving to 4 is one full rebuild per existing index, the same cold start this row measured. |
| 2026-11-12 | `e477c38` | `document::parser`: a backtick fence whose info string holds a backtick no longer opens a fence (CommonMark 4.5, measured against the official graph parser), so a line such as `- ```:custom-css-url "…"``` ` is inline code and the lines after it stay blocks; `index::schema` moves `PARSER_VERSION` to 3 because the same bytes now project to different rows | **Measured on the reference graph; nothing moved, and nothing can move at this size.** One `scripts/bench.sh` run on M1 Pro, macOS 27.2.1, rustc 1.99.1, `--release`, **14,178 pages / 114,098 blocks / index 50,442,240 bytes**, `uptime` load 10.48 (a loaded machine reads high, so compare within stated ranges rather than against an idle run). `cold start` **16,510.98 ms** against the v13 baseline 16,588 / 16,959 ms and the `249a3bb` row's 16,052 / 16,605 ms; `warm open` 102.59 ms against 103; `CheckNow` 102.20 ms against 99 to 107.66; `search` with no hit 35.86 ms against 32.6 to 35.77; the query medians unchanged (`(task DONE)` at limit 1000 3.58 ms against 3.42 to 3.70, `(not (task TODO))` 34.81 ms against 34.22 to 36.01); `append_block` to the largest page 316.84 ms against 315.00 to 324.99. The cost the change adds is one `contains` over the remainder of a line, reached only when the line already starts with three or more backticks or tildes, inside a scan that is 16.5 seconds of file reads and SQLite transactions; separating it from this table's spread is not possible and a number claiming to would be invented. `PARSER_VERSION` moving to 3 is a one-time rebuild for every existing index, which is the same 16.5 seconds this row measured. |
| 2026-11-11 | `e0b438e` | `crates/graph/src/lib.rs`: the P1 lints (`clippy::expect_used`, `clippy::unwrap_used`, `clippy::panic`, `unreachable_pub`) and eight zero-noise P2 lints (`print_stdout`, `print_stderr`, `wildcard_imports`, `unused_lifetimes`, `rust_2018_idioms`, `allow_attributes`, `allow_attributes_without_reason`, `let_underscore_must_use`) become crate-root warnings under one `cfg_attr(not(test))`, following `crates/server`; the twenty-five `expect` sites are propagated or marked `#[expect]` with a stated reason, three `pub` constants become `pub(crate)`, and the one `#[allow]` becomes an `#[expect]` | **Not measured, and there is nothing to measure: no hot path changed.** An `#[expect]` and an `#[allow]` are attributes, so neither emits code at run time; the `let _ = f()` sites became `drop(f())`, which is the same call with the same discard; and the `domain/block.rs` character scan now walks `char_indices` instead of bytes, which visits strictly fewer positions on a malformed id and is not on a read, write, scan, or serialize path. No path gained or lost a step, so there is no number to move. |
| 2026-11-11 | `b189813` | `crates/graph/src`: thirty-eight rustdoc warnings — 25 links from public docs to private items, 7 redundant explicit link targets, 6 unresolved links — are made resolvable in the doc comments alone, and `just check` gains `RUSTDOCFLAGS="-D warnings" cargo doc -p logseq-graph --no-deps` | **Not measured, and there is nothing to measure: no code changed.** Every edit is inside a `///` or `//!` line: links to internal helpers (`Self::indent_unit`, `Self::paste_point`, `ScanMode::TrustStat`, `query::search`, `page_version`) became code font, redundant explicit link targets were dropped to their bare label form, and the `Freshness` variants gained the `super::` path the module did not import. Doc comments are not code, so no read, write, scan, or serialize path gained or lost a step and there is no number to move. The gate is the change: a broken intra-doc link now fails `just check` instead of scrolling past in a build log. |
| 2026-11-11 | `2fa3243` | `crates/server/src`: the P2 lints (`clippy::wildcard_imports`, `unused_lifetimes`, `rust_2018_idioms`, `clippy::allow_attributes`, `clippy::allow_attributes_without_reason`, `clippy::let_underscore_must_use`) join the P1 four at the binary's crate root, the one `#[allow(unreachable_patterns)]` in `main.rs` is deleted because the lint never fires on this toolchain, and `http.rs`'s `let _ = request.respond(response)` becomes `drop(request.respond(response))` | **Not measured, and there is nothing to measure: no read, write, scan, or serialize path gained or lost a step.** The rewrite in `http.rs` is the only line of code that moved and it moves nothing: `let _ = x` and `drop(x)` both drop the value at the same point, one is a non-binding binding and the other a call to a function whose body is a no-op. The `respond` it discards fails only when the peer closed the connection before the answer was complete, which is a request that ended early, not an error path, so no branch was added or removed. Everything else is attributes, and attributes emit no code. |
| 2026-11-11 | `06aa001` | `crates/server/src/main.rs`: the P1 lints (`clippy::expect_used`, `clippy::unwrap_used`, `clippy::panic`, `unreachable_pub`) become crate-root warnings for the binary, its fifteen `expect` sites are either propagated or marked `#[expect]` with a stated reason, and thirty `pub` items it never exported become `pub(crate)` | **Not measured, and there is nothing to measure: the normal path does not change.** No read, write, scan, or serialize path gained or lost a step — an `#[expect]` is an attribute, so it emits no code at run time. The one site whose behavior moved at all is `one_argument` in `http.rs`, which replaced nine `expect`s in `handle` with a refusal: the branch it reports is the one `parse_call` already makes unreachable by refusing a call whose positional count is not the method's, and that branch used to panic and now answers the internal-error object, so the only case with a different cost is a case the adapter cannot reach. |
| 2026-11-11 | `2357769` | P0 Rust lints move into `[workspace.lints]` in `Cargo.toml` (`unsafe_code`, `missing_docs`, `missing_debug_implementations`, `dbg_macro`, `todo`, `unimplemented`), with doc comments and `Debug` derives on the public items this exposed in `document/edit.rs`, `document/line.rs`, `domain/block.rs`, `source/lock.rs`, `engine/`, and `index/` | **Not measured.** Only lint configuration, doc comments, and derives changed; no behavior changed, so there is no cost to measure. |
| 2026-11-11 | `249a3bb` | `find` on the CLI and the `pages` field of `logseq.App.search`: a name lookup answers file pages through their own name or their `alias::` values, page-name hits first | **Measured on the reference graph; it adds two new baselines and moves no old one.** `scripts/bench.sh` ran twice at `249a3bb`/`8f488c9` on M1 Pro, macOS 27.2.1, rustc 1.99.1, `--release`, 14,174 pages / 113,601 blocks / index 50,135,040 bytes, `uptime` load 6.41 and 6.48 (a loaded machine reads 5–10% high, so compare within these runs, not against an idle one). The new read costs **`page_candidates("zmi", 20)` 3.93 ms** averaged over 20 calls, and the list-valued property path it shares with `query_exec` costs **`(page-property :alias zmi)` 2.96 ms** median at limit 20 (2.95 ms at limit 1000, 2 results). Two rows were added to `crates/graph/examples/measure.rs` so `bench.sh` covers both from here on; they are new baselines, not effects, because neither row existed before. Every pre-existing row stayed within noise: cold start 16052 / 16605 ms against the v13 baseline of 16588 / 16959 ms (the 9512 ms figure belongs to the older v12 schema), warm open 100.70 / 102.54 against 103 ms, `search` with no hit 33.36 / 35.77 against 32.6 ms, `(task DONE)` at limit 1000 3.43 / 3.70 against 3.42 ms, `(not (task TODO))` at limit 1000 35.64 / 36.42 against 34.22 ms. |
| 2026-11-10 | `249a3bb` | `crates/server/src/http.rs`: the REST GET dispatch becomes one JSON-RPC `POST /api` surface — thirteen methods, structured error objects, and one freshness policy per call | **No library-side regression; the adapter's own cost is still not measured.** The two `scripts/bench.sh` runs in the row above exercise the library through `examples/measure.rs`, which this change does not touch: every engine read still runs one `served.read` under one policy, and all baseline and query rows are unchanged within noise. What has no number is the adapter's JSON parse and serialize cost per call — `bench.sh` never starts `serve`, so there is no real-graph row to move, and a number invented here would be exactly the guessed number this file forbids. |
| 2026-11-06 | `46702df` | `examples/measure`: the write path is decomposed step by step on the median page | **Measured, and it settles the per-edit question.** Of a 15.6-17.4 ms `append_block` on a 6-block page, **9.9-11.1 ms is one durable file replacement** (two `F_FULLFSYNC` round trips and a rename) and **1.01-1.22 ms is the whole index side** — read, parse, hash, and one transaction. The lock, the read, and the parse together are under 0.2 ms. So a per-edit latency is bound by durability, which is the property the library exists for, and every schema argument in this file is arguing about 6-17% of it. The lever an editor actually has is batching, which the API already offers: one commit replaces the file once regardless of how many operations it carries. |
| 2026-11-06 | `1c970fb` | `examples/measure`: the write rows are measured on the median non-empty page as well as the largest one | **Measured, and it changed a decision.** The benchmark reported only the worst case, which makes every write look index-bound and argues for schema work nobody needs: on the reference graph `append_block` is 350 ms on the 6197-block page and **14.07 ms on the median page of 6 blocks** (`commit_page` 350.66 / 12.19 ms). That is what rejected the integer surrogate for ephemeral identity — see the section above, which also says plainly that what those 12-14 ms are spent on has not been measured. |
| 2026-11-06 | `0b2d838` | `index::schema` v13: the four indexes on sparse columns — `blocks_by_task_marker`, `blocks_by_priority`, `blocks_by_durable_id`, `pages_by_journal_date` — become partial on `<column> IS NOT NULL`, and the schema version moves so an existing full-tree index is actually rebuilt | **Measured on the reference graph** (14148 pages, 113285 blocks), two interleaved rounds old → new → old → new, one machine, `scripts/bench.sh`: old = `897ba5f` (main, schema v12) at `load average 4.70/6.80/7.75` and `5.82/6.80/7.66`; new = this change at `7.19/7.16/7.84` and `8.16/7.23/7.77`. The load-independent number is the index: **53.4 MB (53448704 and 53506048 bytes) → 50.0 MB (50040832 and 50065408)**, i.e. **−3.42 MB, −6.4%, −30.2 bytes per block** — 113285 task-marker entries become 2954, 113285 priority entries become 179, 113285 durable-id entries become 1342, and 14148 journal-date entries become the journals only. The writes follow, with non-overlapping ranges in both rounds: `cold start` 18010 / 19968 → **16588 / 16959 ms** (−11.7%), `refresh_paths` of one file (6197 blocks) 315.83 / 353.75 → **304.27 / 310.37 ms** (−8.6%), `append_block` 373.68 / 367.74 → **315.00 / 324.99 ms** (−13.7%), `commit_page` with 1 `EditText` 348.31 / 369.85 → **334.09 / 336.73 ms** (−6.6%), `commit_page` with 10 `EditText` 441.09 / 466.17 → 437.42 / 441.16 (within noise). Read the direction, not the percentage: the two old runs differ from each other by 11% on `cold start`, which is more than this change's own variance. Unchanged, as expected — they touch no sparse index: `CheckNow` 106.79 / 115.13 → 107.66 / 98.13 ms, `page_tree` of the largest page 1.17 / 1.27 → 1.20 / 1.21 ms, `search` with no hit 34.05 / 35.03 → 33.51 / 34.22 ms. Query timings are unchanged too, which is the point of the `IN (…)` leaves implying `IS NOT NULL`: `(task TODO)` 0.04 / 0.04 → 0.04 / 0.04 ms, `(task DONE)` at `limit` 20 1.90 / 1.79 → 1.81 / 1.96 ms, at `limit` 1000 3.42 / 3.47 → 3.74 / 3.52 ms (the only row that moved the wrong way, by 5%, i.e. the noise rule applies), `(priority A)` 0.10 / 0.11 → 0.11 / 0.11 ms at `limit` 20 and 0.25 / 0.26 → 0.27 / 0.26 ms at `limit` 1000, `(not (task TODO))` 31.74 / 34.26 → 32.24 / 33.73 ms. The plan tests in `index::query_exec` are what keep it: they assert each leaf still seeks its partial index and that the index is still partial. |
| 2026-11-06 | `a647df0` | Re-measured the whole baseline on the reference graph after schema v11 and v12, four commits back to back on one machine | **Measured, and it revises the baseline table.** Reads are unmoved; writes are not: +44% cold start, 2.1× `refresh_paths`, 2.0× `append_block`, and the index went 38.9 → 53.5 MB (+129 bytes per block). Attributed by measuring `ca53033` (v11, ephemeral ids) and `385c081` against `3f9a9da` (v10): nearly all of it is v11's random unique key, with v12 adding the last +4.3 MB and ~15%. The table above now carries all four runs side by side. |
| 2026-11-06 | `e206a36` | `search` hits state their selector: `SearchHit` carries a `BlockSelector`, the search SELECT reads `blocks.durable_id` and `blocks.ephemeral_id` from the same row it already reads, and the CLI prints it as the first column while `GET /search` states `selector: {kind, id}`; a tree, block, or snapshot entry gains the same field | **Not measured.** It adds no seek, no join, and no index: the two identity columns come from the `blocks` row the search already reads and joins to `pages`, and the extra work is decoding one of the two into an enum plus one more serialized field — formatting and a two-column read, both below the run-to-run spread every row above already records. `search` was itself never given a number on a real graph (the content-search read has no measured row), so this change cannot be separated from a baseline that does not exist; measuring it belongs to a row that first measures `search`. The hit's `text` field keeps its published name — the value is a snippet, the field name is not — so nothing on the wire changed shape except an added field; a rename would have been a protocol change with no cost on this table, which is exactly why it belongs in the JSON, not here. |
| 2026-11-06 | `79bd2c2` | `server`: `query` on the command line and `GET /query` on the wire, plus the selector spelling `block`/`update`/`remove`/`move` and `/block` now accept | **Not measured, and it needs no row of its own.** The change adds no read: one invocation runs exactly the query read ADR-0013 measured nothing about either, and the adapter does what every other adapter read does — parse, one engine call, one serialization. What is worth naming is the one cost the CLI adds over the library: `query` prints a selector per match, which is formatting, not a read. The engine-side cost of a query is recorded in the `index::query_exec` row below; this row exists because `server` is a hot path by the pre-commit rule, and it should say what it did to a measured number, which is nothing. |
| 2026-11-06 | `2f3d6da` | `index::query_exec`: a simple query becomes SQL — leaves are sub-selects of row keys, `and`/`or`/`not` become `INTERSECT`/`UNION`/`EXCEPT`, and the answer is one ordered, limited read (ADR-0013) | **Measured on the reference graph** (14147 pages, 113007 blocks, 53.2 MB index at schema v12) with `scripts/bench.sh` and the query rows the example now prints: **20 samples per row, first (cold) answer dropped**, two runs, `load average 57.38/84.36/63.13` for the first and `23.91/69.88/59.16` for the second, so every timing is an upper bound and the two runs agreeing is what makes them usable. Harness at `b7153cb`; the read is this row's `2f3d6da`, unchanged since. Median at `limit` 20 → 1000: `(task TODO)` **0.04 → 0.04 ms** (the graph holds 15 `TODO` blocks, so both limits answer the same 15 rows, best 0.04, worst 0.12); `(task DONE)`, the most common marker with 2937 blocks, **1.95-1.98 → 3.92-4.03 ms** (best 1.71, worst 4.42) — that is the `limit`-sensitive shape; `(priority A)` **0.11 → 0.27-0.30 ms** (179 blocks, best 0.10, worst 0.41); `[[github.com]]`, the page the most `page_refs` rows point at (6546) and therefore the widest recursive subtree in the graph, **10.16-10.37 → 11.48-12.15 ms** (best 9.40, worst 13.03); `(property collapsed true)`, the most used property key with its most used value (2136 rows), **4.51-4.77 → 6.19-8.37 ms** (best 3.95, one worst of 18.81 — the machine, not the read); `(and (task TODO) (priority A))`, two leaves over 15 and 179 rows, **0.05 → 0.05 ms** (4 blocks, best 0.05, worst 0.20). **`(not (task TODO))` over all 113007 blocks: 34.79-35.21 ms at `limit` 20 and 35.45-36.01 ms at `limit` 1000** (best 33.48, worst 44.22 and 50.47). It did **not** fail, stall, or time out at `limit` 1000 — the complement is 112992 rows and the answer is truncated, not refused. **The prediction this row made about that cost was wrong about where the work goes.** `EXPLAIN QUERY PLAN` on the reference graph (pristine index of the same 113007 blocks, no writes) gives `MERGE (EXCEPT)` — `SCAN blocks` on the left, `SEARCH blocks USING COVERING INDEX blocks_by_task_marker (task_marker=?)` on the right — so the set difference is **not** a temp b-tree: both sides arrive in rowid order and SQLite merges them in one pass, which measured 16.6 ms on its own through `sqlite3`. The only temporary b-tree in the plan is `USE TEMP B-TREE FOR ORDER BY`, and it holds the **whole** complemented set — 112992 block rows joined to their pages, sorted by `(name_key, preorder)` before `LIMIT` can cut anything. That is why the row is ~35 ms and why `limit` barely moves it: 20 rows 34.79 ms against 1000 rows 35.45 ms is 1.9%, i.e. about 0.7 µs per returned row against ~35 ms of graph-wide work, the same per-row figure `(task DONE)` shows. **So `limit` should stay mandatory, but not for the reason this row gave**: it bounds what leaves the engine and forces the caller to state an answer size — it does not bound a `(not …)`, whose cost is the size of the graph. The leaves still each seek one index, as pinned by the `EXPLAIN QUERY PLAN` tests; what no test pinned, and what the measurement adds, is the plan the *combination* gets. |
| 2026-11-04 | `7e3d03d` | `document::edit`: the `merge_block_text` doc states where a first child's children land | None. A doc comment changed; no code path changed. |
| 2026-11-04 | `1aaf38d` | `blocks.task_marker` and `blocks.priority`: the two facts a block's text leads with (`domain::TaskMarker`, `domain::Priority`) become projection columns, plus the five indexes a task, property-by-name, or journal-date query seeks on (schema v12, parser v2) | **On the 314-page graph** (314 pages, 6353 blocks), measured with `scripts/bench.sh` twice per build at `load average 5.5-8.2`, before = `HEAD` without this change and after = with it. Index size is the load-independent number: **2084864 and 2080768 bytes** after against **1900544 and 1892352** before, i.e. **+180224..192512 bytes, +28.4..30.3 bytes per block** — the two short values in the row, one index entry each in `blocks_by_task_marker` and `blocks_by_priority`, one more entry per property row in `page_properties_by_name` and `block_properties_by_name`, and one per page in `pages_by_journal_date`. Cold start went **289.88 and 293.10 ms** before to **354.40 and 341.13** after, about **+50-60 ms over 6353 blocks (~+9 µs per block)**: two `leading` reads of the block's own text plus two more bound columns and two more index inserts per block. Unchanged within the load's noise: `page_blocks` of the 2686-block page 1.26-1.29 ms against 1.31-1.45, `refresh_paths` of the same file 56.12 against 54.65-56.12, `append_block` 68.12-74.10 against 55.86-64.46 (that spread is the machine, not the change). The five new indexes are one equality seek each, pinned by `EXPLAIN QUERY PLAN` tests rather than by a timing. Not re-measured on the 14143-page graph. |
| 2026-11-05 | `f92a7ac` | `blocks.ephemeral_id`: every block that states no `id::` is given a random id the projection mints, so a block without durable identity has an address a caller can edit through (ADR-0012) | **On the 314-page graph** (314 pages, 6353 blocks, 547 KB of Markdown), measured once with `scripts/bench.sh` at `load average 8.97/12.77/42.95` — far busier than the `7.46` the 2026-11-01 baseline was taken at — so treat every timing below as an upper bound rather than a comparison. The one load-independent number is the index: **1896448 bytes**, up from **1351680**, i.e. **+544768 bytes, +85.8 bytes per block**, which is the 36-character value plus one unique index entry per block. The mechanism is one more column on the block `INSERT` and one more index entry per block in a page replacement; a write also pays one FNV-1a hash of the page text before the edit (the fingerprint gate) and reads pay one more short column per block (`page_blocks` of the 2686-block page 1.26 ms against 0.91, inside the load penalty). The new lookups are one seek each: an ephemeral id is one equality on `blocks_by_ephemeral_id`, and a write's answer is one seek on the existing `(page_id, preorder)` unique key. Cold start read 259.98 ms against 200 ms and `append_block` 57.40 ms against 47 ms, both on the loaded run; the extra work is real but its size is not separable from that load, so both rows owe a re-measure at idle. Unchanged within noise: warm open 2.80 ms (2.96), `CheckNow` 1.90 ms (2.00), `VerifyContent` 12.48 ms (14.0), `page_tree` 0.51 ms (0.50), `page_snapshot` 2.93 ms (3.30), `block_snapshot` 3.08 ms (3.84), `commit_page` one and ten `EditText` 57.22 and 84.95 ms (54.36, 83.63), a batch refused as stale 3.12 ms (3.39). The three property writes (`upsert_block_property`, `remove_block_property`, `set_block_collapsed`) take the graph lock after review, because the fingerprint gate they carry means nothing if another writer can replace the bytes between the check and the write; that is one `flock` per write, the same cost every other write already pays, and it is **not measured separately** — the smallest write row here is tens of milliseconds against a lock acquisition the `append_block` row already contains. |
| 2026-11-04 | `fb251f8` | `domain::query`: the simple-query grammar, its parser, its result and error types, and `EngineError::Query(QueryError)` — the contract only, with no SQL and no read path | **Not measured, and there is nothing to measure.** The engine change is one enum variant, one `From` impl, one `Display` arm, and one `source` arm; no read, write, or scan path can construct it yet, because the executor is the next slice. The parser is pure text over one `&str`: it allocates the tree it returns and touches no index, no file, and no clock, so it is not on any path this file records. The row exists because the pre-commit hook asks for one, and `not measured` with a reason is what it asks for. |
| 2026-11-04 | `d17ff7f` | `GET /snapshot/{name}` answers one page's current file content — its properties and its blocks in the `/tree` shape — under the file's own version, without a scan | **Measured on the reference graph** (14147 pages, 113001 blocks, 38.9 MB index, `load average 11.3-13.5`, three rounds). Over HTTP, 200 requests per round: **0.66-1.01 ms** for the median non-empty page (6 blocks), against 0.65-1.07 ms for `/tree/{name}?freshness=last` and 0.95-1.23 ms for `/page-version/{name}` on that page — one file read and one six-block parse are invisible beside the request. The tier follows the page, not the graph: `/snapshot` of the 6195-block page is **12.8-13.5 ms** (50 requests per round) against 6.6-6.9 ms for `/tree?freshness=last`, because a snapshot parses the whole file where a tree read queries rows (`page_snapshot` 9.71 ms against `page_tree` 1.27 ms at the engine tier). The read this endpoint exists to replace is `/tree?freshness=check` on the same median page: **198.0 ms** (5 requests), which stats all 14147 files to answer about one; `CheckNow` alone measured **102.7 ms** on this run. The endpoint adds no read of its own — it is the `page_snapshot` the engine already had, plus one flattening walk and one serialization (engine 0.21 ms and 9.71 ms, HTTP 0.66-1.01 ms and 12.8-13.5 ms) — and like `/page-version` it names no freshness policy and answers a name it cannot resolve with `exists: false`. |
| 2026-11-04 | `b84002d` | `GET /page-version/{name}` answers a page's indexed version beside its disk version, without a scan | **Measured on the reference graph** (14146 pages, 112999 blocks, 38.9 MB index, `load average 4.4-4.9`, two runs). Engine read: **0.033-0.041 ms** for the median non-empty page (6 blocks, 100 reads averaged) and **1.07-1.08 ms** for the 6195-block page. The tier is the page's size, not the graph's: one seek on `pages.name_key`, one baseline row, one stat, one read of the file, and one hash of its bytes. The same question costs `CheckNow` **100.6-102.1 ms** on this run, because it stats all 14146 files to answer about one page, and `page_snapshot` 0.16-0.27 ms / 9.21-9.74 ms, which parses the page and extracts its facts as well. Over HTTP the request is indistinguishable from any `?freshness=last` read: 0.86-1.24 ms against 0.72-1.15 ms for `/page` on the same page, three rounds of 200 requests each, the spread wider than the difference; `/page?freshness=check` on that same page measured **222 ms** on this build. That is the whole point of the endpoint — the same question at the `last` tier instead of the `check` tier — and the extra cost over a plain index read is the one `file_baseline` row that supplies `index_version`. |
| 2026-11-04 | `a14393f` | `pages` states a journal's calendar day | **On the 314-page graph** (239 pages and 75 journals, 6353 blocks). Not measured, and the reason is the noise: `pages, all 314` came back 0.10–0.11 ms at `load average 7.46`, 0.12–0.13 ms at 26.4–32.4, and 0.14 ms at 12.59 — three runs whose load does not move the number in one direction. At a tenth of a millisecond the run-to-run spread is larger than anything this change could add, so no timing here is an effect. The index stays **1351680 bytes**: the column is NULL for every non-journal row, and SQLite stores a NULL without a payload, so only the 75 journal rows carry the ten-byte day; every row's record header gains one serial-type byte either way. The listing reads one more short column per row and decodes one `Option<String>`, with no extra seek and no extra table. |
| 2026-11-04 | `b416040` | `:favorites` is read from `logseq/config.edn` and served at `/favorites` with an `exists` flag per entry | **Measured on the reference graph** (14146 pages, 112970 blocks, 38.9 MB index, `load average 11.20`): `/favorites?freshness=last` took **0.69–0.91 ms** over five requests, on top of the HTTP and JSON cost every endpoint pays. The list is configuration rather than a projection, so a request pays one indexed seek on `pages.name_key` per entry — 16 on this graph — and no scan. The config scanner now keeps the elements of an ordered vector when every one is a string; that runs once per scan over `logseq/config.edn` alone and allocates a handful of `String`s, which is below the run-to-run noise of every row above. The cold start is unchanged: `:favorites` names no file discovery reads. |
| 2026-11-04 | scan progress | The startup scan reports `scanned`/`total` through two atomics, and `serve` binds its port before the projection runs | **Not measured, and the reason is arithmetic rather than omission.** The scan path gains one release store per scan and one acquire-release add per projected file: 14146 of them on the reference graph, where the cold start is 9512 ms, so the counter is at most a few microseconds of 9.5 s and far below the ±5 ms run-to-run spread this file already records. Serving gains one atomic load pair per `/info` poll during the scan, and no request after it: the adapter reads the pair only while the graph state is `Scanning`. What this change does move is when a caller can first reach the process at all — the port answers from the moment the socket is bound instead of 9.5 s later on a cold index — and that is a latency of the first answer, not a cost of any operation below. |
| 2026-11-04 | `769e385` | `pages` states its kind, and `file_baseline` loses `page_kind` | **Measured on the 314-page graph** (the 14143-page graph these rows were taken on is not on this machine). `pages`, all 314: 0.08–0.09 ms at `06e8b30` and 0.10–0.11 ms here, three runs each; the index grows from 1347584 to 1351680 bytes. The difference is one more short TEXT column on the same row — one `String` per row in the loop, no extra seek and no extra table — and the write path is unchanged (`append_block` 43.78 ms against 45.90 ms, within run-to-run noise). On the large graph the same column would be bounded by 0.1 µs per row, about 1.4 ms on 14138 pages, which does not move the `/pages` row above: serialising that listing already costs 1.8 ms. That extrapolation is arithmetic, not a measurement. |
| 2026-11-03 | `af58961` | Audit round on the batch commit: one source for the folded-state bytes, all four move targets covered, a post-write read failure reported as written | **No change measured.** Correctness only: the collapsed key and its two values now come from one `collapsed_value`, the post-write read failure is classified as `ReReadAfterWrite` instead of `Source` (which changes the error a script gets, not what a write costs), and `splice_in` gained an empty-document guard. No code path added to a write. |
| 2026-11-03 | `609496a` | A property line can end a page: `splice_in` appends instead of indexing past the last line | **No change measured.** One extra branch for the append-at-end case, which used to be an out-of-bounds panic on a page whose last block ends the file; the `at < lines.len()` path is byte-identical, so every existing property write renders the same bytes and does the same work. |
| 2026-11-03 | `d9a272c` | Versioned batch commit with page-scoped CAS | **Measured on the 6192-block page.** One `EditText` **155.57 ms**, ten `EditText` **271.98 ms** (re-run: 163.41 and 287.01), a batch refused as stale **9.44 ms** (re-run 10.29). On the 314-page graph, whose largest page holds 2686 blocks: 54.36 ms, 83.63 ms, 3.39 ms. One operation costs `append_block`'s 149.07 ms plus the snapshot read that produces the base version (9.41 ms). The 9 operations between 1 and 10 add **12.9 ms each**, which is one re-parse of the page per operation — the cost ADR-0011 accepts, measured rather than assumed. A refused batch writes nothing and pays only the read and the parse. |
| 2026-11-03 | `dba2264` | Audit round on the rooted snapshot: one read instead of two, a handle that cannot alias a restructured file | **No change measured.** It removed a defect rather than adding a cost: `block_snapshot` used to call `read_with` twice, and each call scans under `CheckNow` (103 ms on this graph) and re-reads every file under `VerifyContent` (1033 ms). The page path now comes from the same closure, so the row below is one scan. The alias guard adds one `u32` comparison per block visited. |
| 2026-11-03 | `9efe19b` | Rooted snapshot with ancestors, block-reference resolution | **Measured on the 6192-block page: 10.27 ms** (re-run 10.49), against `page_snapshot`'s 9.41 ms for the same page; on the 2686-block page of the small graph, 3.84 ms. The difference is the ancestor walk; the read itself is the page parse, because one subtree is cut out of the whole-page snapshot rather than assembled from rows. |
| 2026-11-03 | `e5147d3` | Audit round on the property-blind edit: content lines stay together, one shared indent cut | **No change measured.** Same page replacement, same block and line counts: the extra content lines are spliced after the last content line instead of the block's last own line. The only byte change is that `merge` no longer produces a property line between two content lines, and a `split` that used to be refused (fewer content lines, property after the content) now succeeds. |
| 2026-11-03 | `fa715e7` | Property-blind text edit (`set_block_text_text`) and explicit marker write (`set_block_marker_text`) | not measured separately. Both are pure document edits on one block of a page the write path has already read; the batch rows above are what they cost once, ~12.9 ms of page re-parse each on the largest page. |
| 2026-11-03 | `da84ab0` | Audit round on the page snapshot: `child_path` by its own name, one `durable_id` rule, and a page-name check | **No change measured.** Lifting `durable_id` to one function moves an expression, the rename moves a name; the new guard costs one `page_name_key` fold and one string comparison per snapshot read, which is invisible beside the file read and the whole-page parse it sits next to. |
| 2026-11-03 | `f564736` | Page snapshot and page version token | **Measured.** 6192-block page **9.41 ms**, 6-block median non-empty page **0.24 ms**; on the 314-page graph, 2686 blocks **3.30 ms** and 5 blocks **0.09 ms**. It is one stat, one read of the whole file, one parse, and one facts extraction, and it is the first read that answers from the file rather than the projection (ADR-0002's amendment) — the price of a version token that can refuse a stale write. |
| 2026-11-03 | `8be6571` | Structure-edit self-check: one lift column per lifted child, and a parent claim per placed block | **No change measured.** Correctness only: the five structure edits re-parse their own output and now also claim each placed block's parent, so a block that lands one level off is refused instead of written. A correct edit produces the same bytes it did before. |
| 2026-11-03 | `21fa495` | Split, merge, unwrap, indent, outdent as pure document edits | not measured separately. Nothing in the write path calls them yet; the batch rows above bound them (one page re-parse each), and their own cost is a slice of source bytes plus the re-parse they perform to check themselves. |
| 2026-11-03 | `6f76e3a` | Audit fix on journal creation: reject a year outside `0..=9999` in `JournalDate::new` | not measured, and the reason is arithmetic rather than omission: one `i64` range check per journal file name parsed during discovery, once per journal file per scan, which is far below run-to-run noise on any graph here. |
| 2026-11-03 | `046aa45` | Date-based journal creation | not measured. Same pipeline as `create_page`: one create, one read, one projection of an empty page. The `append_block` rows already bound a one-page projection; this one has no blocks to project. |
| 2026-11-01 | `15c91b9` | `prepend_block_in_page`, `set_block_collapsed`, `insert_batch` | **Measured on the 6186-block page.** Ten blocks through `insert_batch` cost **241 ms**; ten `insert_block` calls in a loop cost **1516 ms** — **6.3×**, because the batch is one page replacement and the loop is ten. One `prepend_block_in_page` is 145 ms and one `set_block_collapsed` is 162 ms, both the price of any single write on a page that size. |
| 2026-11-01 | `3c6522b` | `delete_page`: move a page file into `logseq/.recycle/` and drop its rows | not measured. The file is one `fs::rename` inside the file system, there is no scan and no projection; the index work is deleting rows that are already keyed, so the cost is a page lookup plus two index deletes and does not grow with the graph. |
| 2026-11-01 | `2c86199` | `upsert_block_property` / `remove_block_property`: the two property writes | **No change measured.** Both ride `update_block`'s single-page pipeline with the edit swapped out; one page parse and one page replacement, no new scan. Covered by the same `33a5da4` re-measurement above. |
| 2026-11-01 | `650634e` | `insert_block`: a new write that mints one block at a `MoveTarget` position | **No change measured.** A new API on the existing single-page pipeline: one document edit plus one page replacement, no new scan. Re-measured at `33a5da4` with both write slices merged: every row is within run-to-run noise of the `3bf22ab` baseline, on the same graph and the same 38899712-byte index. |
| 2026-11-01 | `e917743` | Move graph file IO out of `engine` into `source` | None. Two functions changed layers; no code path changed. |
| 2026-11-01 | `9b60db5` | Move `BlockMarker` into `domain` | None. A type definition moved; no runtime code changed. |
| 2026-11-01 | `2ca2855` | `move_block` accepts `BlockSelector` | not measured. Same-page moves resolve an ephemeral handle with one extra index query before the edit; the edit itself is unchanged. |
| 2026-11-01 | `81edfff` / `23ed87d` | `update_block` / `remove_block` accept `BlockSelector` | not measured. A durable write resolves exactly as before; an ephemeral write pays one metadata read plus `handle.matches`, which is the same check a read already pays. |
| 2026-11-01 | `e839d9c` | Block properties, siblings, namespace reads | not measured. Each is one index query on an existing index; the sibling read is two seeks on `(page_id, preorder)` and `(page_id, parent_id, sibling_index)`. |
| 2026-10-31 | `ff98fe7` | Read-only REST adapter | HTTP and JSON add under 1 ms on every endpoint except `/pages`, where serialising 14138 entries costs about 1.8 ms on top of the 2.55 ms query. `?freshness=check` measures the same as the engine's own `CheckNow`. |
| 2026-10-31 | `e92a665` | `refresh_paths` | A one-file refresh costs the page's projection only, not a scan: 1.10 ms for a 1-block page, 14.1 ms for 1229 blocks, 121 ms for 6186 blocks, against 96–103 ms for a full `CheckNow` regardless of page size. |
| 2026-10-31 | `b65879c` | Fix the real-graph measurement itself | Not a performance change, but it changed the numbers: the harness moved to `--release` with `--test-threads=1`, and picked the median **non-empty** page. Earlier debug-mode numbers are 2–3× higher and were taken with two tests competing for the CPU; they are not comparable and are not repeated here. |

### Folded trigram search: Linux VM

Measured 2026-11-19 on Ubuntu 22.04.5 LTS, Linux 6.8.0-1060-aws, x86_64 KVM VM,
8 vCPU Intel Xeon, 31 GiB RAM, ext4 virtual disk, rustc 1.99.1, **release profile**.
These are not comparable to the M1 Pro/macOS tables above. Before = `origin/main` at
`fa33ba5924c404d827fac39e4b60c736e8e68ed4` in a separate worktree; after = the folded-trigram
implementation with the 100-match ranking threshold on that same base. Both used the identical
extended `examples/measure.rs` harness. Build/size values are reused from the initial FTS change;
only the search rows were rerun after adding the bounded ranking decision.
These measurements precede the rebase onto the page-identity and parser slices (schema 17 in
the combined branch); they do not remeasure those slices' additional projection or storage work.
Release compilation finished before measurement; cold means a fresh database, **not** a flushed
OS file cache or Rust compile time. `bench.sh` copied the graph for every run. Index size was read
after closing the only connection to checkpoint WAL, before the benchmark writes; small size
differences between rounds include random ephemeral UUID/index packing.

Runs were serial and interleaved: docs before → after, synthetic before → after, then repeat.
`uptime` one-minute load at each start (before/after, round 1 then round 2): docs
**1.75 / 1.69; 1.70 / 1.56**; synthetic **2.15 / 1.90; 1.52 / 1.48**.
The live one-second `vmstat` sample before every run showed **100% idle**, no runnable competitor
and 0% I/O wait; the load average still reflected the preceding compilation/benchmark. No other
build ran during measurement. Cold-start variance is visible; do not infer precise percentages
from it. Search rows are warm **medians of 20 calls**, first call discarded, limit 20, `LastScan`.
The bounded-ranking reruns used docs → synthetic, then repeat; starting one-minute loads were
**0.49 / 0.61 / 0.70 / 0.64**, with **100% idle** in each live pre-run sample.

Public corpus: `https://github.com/logseq/docs`, tag `v0.9.2`, commit
`8f9f0c4a73cb4a75f64e3ddd50b098e6e62769b9`, **274 projected pages / 5189 blocks**,
the same pin as `docs/corpus-parity.md`. The Chinese phrase found in its source is in
`Changelog_2020.org`, which this projection does not index: all three Chinese needles below
return **zero** on this corpus, before and after. They are not mislabeled as Chinese hits.

Synthetic graph: **3000 pages / 60000 blocks**, deterministic, no randomness. For page `p` in
`0..3000`, write `pages/Page {p:04}.md` with 20 top-level blocks, `b` in `0..20`. Each block is
`- 项目记录 {p:04} 条目 {b:02}: 阅读文档和测试 Rust logseq example` followed by a newline;
when `(p * 20 + b) % 97 == 0`, append ` 今天学习了 Rust 编程 中文书 你好世界` before the newline.
This gives **619 Chinese hit blocks**, while `logseq` occurs in all 60000 blocks: intentionally a
common-term ranking stress case, not a representative frequency distribution for every graph.

| Graph | Operation (milliseconds unless bytes) | Before 1 | Before 2 | After 1 | After 2 |
| --- | --- | ---: | ---: | ---: | ---: |
| Public docs | Cold projection | 104.43 | 117.73 | 403.76 | 354.15 |
| Public docs | Checkpointed index bytes | 1560576 | 1564672 | 3317760 | 3317760 |
| Public docs | Latin hit `logseq` (20 returned) | 0.1244 | 0.1271 | 0.4457 | 0.4380 |
| Public docs | Latin miss `no-such-token-8f3c21d7` | 1.0964 | 1.1013 | 0.4159 | 0.4174 |
| Public docs | CJK `你好世界` (no hit in this corpus) | 4.1229 | 4.2291 | 0.0829 | 0.0863 |
| Public docs | CJK miss `不存在量子独角兽` | 4.1097 | 4.1282 | 0.0835 | 0.0829 |
| Public docs | 1-character CJK `你` (no hit) | 4.2473 | 4.2741 | 4.1936 | 4.2618 |
| Public docs | 2-character CJK `你好` (no hit) | 4.1690 | 4.1880 | 4.1753 | 4.2247 |
| Synthetic CJK | Cold projection | 862.01 | 877.86 | 3465.36 | 3476.67 |
| Synthetic CJK | Checkpointed index bytes | 13660160 | 13647872 | 22315008 | 22331392 |
| Synthetic CJK | Latin hit `logseq` (20 returned) | 0.0626 | 0.0606 | 0.3284 | 0.3433 |
| Synthetic CJK | Latin miss `no-such-token-8f3c21d7` | 11.6617 | 11.5276 | 0.1018 | 0.1016 |
| Synthetic CJK | CJK hit `你好世界` (20 returned) | 1.9195 | 1.9842 | 0.2571 | 0.2722 |
| Synthetic CJK | CJK miss `不存在量子独角兽` | 60.7760 | 62.1800 | 0.0987 | 0.0981 |
| Synthetic CJK | 1-character CJK hit `你` (20 returned) | 1.9168 | 1.9837 | 1.9554 | 1.9455 |
| Synthetic CJK | 2-character CJK hit `你好` (20 returned) | 1.9096 | 1.9371 | 1.9694 | 1.9381 |

Reproduce after cloning the pinned corpus and generating the synthetic graph as above:

```sh
export MEASURE_SEARCH_QUERIES=$'logseq\nno-such-token-8f3c21d7\n你好世界\n不存在量子独角兽\n你\n你好'
mise exec -- scripts/bench.sh /path/to/graph
```

Copy the same measurement example into the before worktree, prebuild both release examples, and
run each twice on the same machine. Raw logs for this run are in
`/home/ubuntu/cjk-search-measurements/{docs,cjk}-{before,after}-{1,2}.log` for the original runs;
bounded-ranking search reruns are in
`/home/ubuntu/cjk-search-bounded-100-measurements/{docs,cjk}-after-{1,2}.log` on the measuring VM.

The measured tradeoff is **about 2.1× / 1.6× index storage** (public / synthetic), roughly
**3–4× cold projection**, far cheaper negative searches, and a roughly **7× faster Chinese hit**
on the synthetic graph (619 matches now take the unranked overflow path). Short needles still scan.
Common Latin hits are still about **3.5× / 5× slower** than the old bounded scan, but no longer
pay the initial 79-ms full-ranking cost. The built-in trigram tokenizer buys exact
substring semantics and ranking without custom tokenizer code; these numbers do **not** compare
it against an implemented custom bigram/character tokenizer. ADR-0016 accepts the extra index and
indexing work, not a universal search-speed improvement.

#### Why the ranking threshold is 100

Controlled SQL probes on the same synthetic graph used one connection, release mode, two rounds,
20 warm samples per median, statement preparation and hit decoding included:

| Form for `logseq` | Round 1 ms | Round 2 ms |
| --- | ---: | ---: |
| Full BM25 | 36.5379 | 37.0144 |
| Unordered MATCH, no rank evaluation | 0.1358 | 0.1439 |
| Rank first 1000 candidates | 5.7898 | 5.7997 |
| Rank first 2000 candidates | 6.2995 | 6.4241 |
| Rank first 5000 candidates | 8.2493 | 7.7265 |
| Rank first 1 candidate | 5.2169 | 5.5667 |
| Constant score, first 20 candidates | 0.1442 | 0.1423 |

The capped subquery uses an unordered FTS scan and the outer query sorts; replacing rank with a
constant removes the roughly 5-ms floor. Bundled SQLite's `fts5Bm25GetData` calls `xQueryPhrase`
over the whole phrase to initialize IDF; the outer candidate limit cannot bound that work.
The controlled persistent-index BM25 result differs from the fresh-index 79-ms result above:
FTS segment layout matters, so only variants on the same index are directly comparable.

We therefore probe at most threshold+1 rowids without rank, use full BM25 only at or below the
threshold, and return ascending FTS rowid order unranked on overflow. This is **not**
best-of-first-threshold ranking, and the caller's limit is independent of the threshold.
Starting at 1000, a controlled threshold sweep added `thresholdterm` to exactly threshold block
texts through the production triggers, kept 60000 total blocks, and measured probe plus the
selected hit query (20 hits decoded, all containing the term):

| Threshold | Common `logseq` overflow ms, rounds 1 / 2 | Exactly threshold matches, full BM25 ms, rounds 1 / 2 |
| --- | ---: | ---: |
| 1000 | 0.4317 / 0.4298 | 1.6656 / 1.6656 |
| 256 | 0.2616 / 0.2571 | 0.8016 / 0.7947 |
| **100 (chosen)** | **0.2313 / 0.2307** | **0.6164 / 0.6144** |
| 32 | 0.2301 / 0.2277 | 0.6270 / 0.6203 |

100 keeps ranking below a millisecond at the boundary and cuts common-term probe work versus
1000; 32 buys essentially nothing further on this controlled index. The complete API benchmark
on a fresh index costs 0.33–0.34 ms for synthetic `logseq`, still above the old scan's 0.06 ms
and the requested approximate 2× target. A 1000-match threshold cost 1.0100 / 1.0191 ms in that
fresh-index benchmark. Probe logs/source: `/home/ubuntu/cjk-query-probe/{bounded.log,src/bin/bounded.rs}`.

### Short CJK search: Linux VM

Measured on the same Ubuntu 22.04.5 / Linux 6.8.0-1060-aws x86_64 VM, 8-vCPU Xeon,
31 GiB RAM, ext4, rustc 1.99.1, **release profile**, not the M1 baseline.
Main = `6bba53fe36e509f38ba31160a85da4be98549172` (schema 17); B = schema 18
with the second FTS index maintained by production insert/delete/text-update triggers.
U was also a full trigger-maintained prototype, not a bulk-load approximation.
Each variant ran twice, docs then synthetic in each round, serially with no concurrent build.
Every live pre-run `vmstat` sample showed 100% idle; B's starting one-minute loads were
0.32 / 0.29 / 0.52 / 0.56. Cold means a fresh projected database, not flushed OS caches or
Rust compilation. Sizes are checkpointed bytes, shown below as decimal MB. Search rows are
20-call warm medians, first call discarded, `LastScan`, caller limit 20. Each cell gives
**round 1 / round 2**; cold-build variance is visible, not evidence that B builds faster than U.

Public corpus is the unchanged `logseq/docs` v0.9.2 pin
`8f9f0c4a73cb4a75f64e3ddd50b098e6e62769b9`: 274 files, 700 page entities, 5189 blocks.
All the CJK needles below return zero in the projected docs corpus; these are miss timings,
not mislabeled CJK hits.

Synthetic enrichment: start with the deterministic 3000-page/60000-block recipe in
[the trigram measurements](#folded-trigram-search-linux-vm). Append ` 的 是 我们` to
every block line. Append ` 龘靐 a中` to the first block of `pages/Page 0000.md` only.
Thus 的/是/我们 each match 60000 blocks, 书/中文 match 619, 龘/龘靐 and a中 match one,
and 齉/齉齾 match none. Both source corpora stay untouched; each run measures a fresh copy.
This enriched graph is not numerically interchangeable with the original trigram-only graph.

| Synthetic operation (ms unless MB) | Main | B |
| --- | ---: | ---: |
| Cold projection | 3534.49 / 3525.28 | 5557.37 / 5277.58 |
| Checkpointed index MB | 24.702976 / 24.682496 | 30.904320 / 30.912512 |
| Common 1-char 的 (20 returned) | 0.1193 / 0.1177 | 0.1907 / 0.1897 |
| Common 1-char 是 (20 returned) | 0.1192 / 0.1180 | 0.1877 / 0.1870 |
| Common 2-char 我们 (20 returned) | 0.1249 / 0.1187 | 0.1898 / 0.1958 |
| Sparse 1-char 书 (20 returned) | 2.1663 / 2.1288 | 0.1993 / 0.1973 |
| Sparse 2-char 中文 (20 returned) | 2.2076 / 2.1123 | 0.1971 / 0.1981 |
| One-hit 1-char 龘 | 70.6871 / 69.8555 | 0.1268 / 0.1269 |
| One-hit 2-char 龘靐 | 70.7771 / 69.4857 | 0.1275 / 0.1275 |
| 1-char miss 齉 | 71.3391 / 69.7337 | 0.0978 / 0.0973 |
| 2-char miss 齉齾 | 71.5695 / 70.3109 | 0.0980 / 0.0975 |
| Latin `logseq` (20 returned) | 0.6097 / 0.6171 | 0.6116 / 0.6079 |
| Mixed `a中` (one hit, scan) | 71.1321 / 70.4421 | 68.2268 / 69.1428 |

| Public docs operation (ms unless MB) | Main | B |
| --- | ---: | ---: |
| Cold projection | 352.72 / 372.26 | 410.67 / 438.54 |
| Checkpointed index MB | 3.448832 / 3.452928 | 3.543040 / 3.551232 |
| 1-char 的 (miss) | 4.4793 / 4.5636 | 0.0758 / 0.0743 |
| 1-char 是 (miss) | 4.5110 / 4.5534 | 0.0747 / 0.0739 |
| 2-char 我们 (miss) | 4.5028 / 4.5326 | 0.0745 / 0.0739 |
| 1-char 书 (miss) | 4.5486 / 4.5509 | 0.0740 / 0.0736 |
| 2-char 中文 (miss) | 4.5586 / 4.5318 | 0.0740 / 0.0749 |
| 1-char 龘 (miss) | 4.5247 / 4.5499 | 0.0741 / 0.0736 |
| 2-char 龘靐 (miss) | 4.5978 / 4.5355 | 0.0740 / 0.0743 |
| 1-char miss 齉 | 4.6496 / 4.5391 | 0.0740 / 0.0736 |
| 2-char miss 齉齾 | 4.4312 / 4.5311 | 0.0741 / 0.0734 |
| Latin `logseq` (20 returned) | 0.4453 / 0.4410 | 0.4354 / 0.4468 |
| Mixed `a中` (miss, scan) | 4.6350 / 4.7667 | 4.4134 / 4.4520 |

#### Measured alternative U and retained cutoff

U emits one folded CJK scalar per token and one filler per maximal non-CJK run; a two-character
query is an adjacent-unigram phrase. B emits prefixed unigrams plus adjacent bigrams, making
each one- or two-character query a single-token lookup. Both use built-in unicode61 with
diacritic removal disabled; no custom tokenizer FFI. See ADR-0016 for the CJK predicate.

| Operation | U, rounds 1 / 2 | B, rounds 1 / 2 |
| --- | ---: | ---: |
| Synthetic cold projection ms | 4828.23 / 5746.25 | 5557.37 / 5277.58 |
| Synthetic total index MB | 28.848128 / 28.844032 | 30.904320 / 30.912512 |
| Synthetic 我们 ms | 0.2579 / 0.2550 | 0.1898 / 0.1958 |
| Synthetic 中文 ms | 0.3270 / 0.3279 | 0.1971 / 0.1981 |
| Synthetic 龘靐 ms | 0.1774 / 0.1750 | 0.1275 / 0.1275 |
| Public docs cold projection ms | 428.30 / 433.36 | 410.67 / 438.54 |
| Public docs total index MB | 3.551232 / 3.547136 | 3.543040 / 3.551232 |
| Public docs 我们 miss ms | 0.0743 / 0.0771 | 0.0745 / 0.0739 |

B trades roughly 7% more total synthetic DB storage than U for about 25–40% faster two-character
lookups. Neither variant makes common early-hit searches faster than the original bounded scan.
Sparse hits and misses avoid that scan, while mixed short queries intentionally retain it.

Controlled SQL measurements on the synthetic indexes, all 60000 matches verified, prepare and
hit decoding included, two 20-sample warm medians:

| B query | Full BM25 ms | Probe only ms | Probe + unranked hits ms |
| --- | ---: | ---: | ---: |
| 的 | 38.6140 / 38.6210 | 0.0390 / 0.0402 | 0.1434 / 0.1491 |
| 是 | 38.7767 / 39.5747 | 0.0388 / 0.0421 | 0.1464 / 0.1496 |
| 我们 | 39.1521 / 39.1884 | 0.0384 / 0.0400 | 0.1379 / 0.1701 |

U's full-BM25 medians were 35.6274 / 36.1563 ms for 的, 35.9683 / 35.7498 ms for 是,
and 49.2018 / 48.3863 ms for 我们. Built-in BM25 is still match-count-sensitive.
Both indexes keep the existing 101-match probe: at most 100 matches get full BM25; overflow
returns unranked ascending rowids. This is not best-of-first-100 ranking or a result-count cap.
SQL probes use a different post-harness index layout from the full API rows, so compare forms
on the same index rather than treating their absolute numbers as interchangeable.

Reproduce with the enriched graph and the existing example's newline-separated query override:

```sh
export MEASURE_SEARCH_QUERIES=$'的\n是\n我们\n书\n中文\n龘\n龘靐\n齉\n齉齾\nlogseq\na中'
mise exec -- scripts/bench.sh /path/to/graph
```

Prebuild release examples before timing. The recorded runs used saved release binaries of that
same example and fresh graph copies. Raw logs/binaries and U source are in
`/home/ubuntu/cjk-short-search-measurements/`: `main-{docs,cjk}-{1,2}.log`,
`U-{docs,cjk}-{1,2}.log`, `B-final-{docs,cjk}-{1,2}.log`, `{U,B}-cutoff.log`, and
`unigram.rs`. Enrichment/runner: `/home/ubuntu/short-cjk-bench.py`; SQL probe:
`/home/ubuntu/cjk-query-probe/src/bin/short_cutoff.rs`.

### Concurrent-read pool: Linux VM

Measured 2026-11-19 on the same 8-vCPU Intel Xeon KVM Linux VM described above (31 GiB RAM,
rustc 1.99.1), release profile. The deterministic graph has **1000 pages / 60000 blocks**:
for `p` in `0..1000`, `pages/Page {p:04}.md` contains 60 top-level lines, `b` in `0..60`, each
`- 项目记录 {p:04} 条目 {b:02}: 阅读文档和测试 Rust logseq example` plus a newline.
Both queries below miss. Mixed-script `x无` exercises the existing short-needle scan; `不存在`
exercises the folded trigram index. An all-CJK short query now uses main's second FTS index,
not a scan. This deliberately contrasts expensive and very cheap reads, not typical
query frequencies. There are no graph writes during timing.

Before is main **`93a1809`** (schema 18), built in `/home/ubuntu/read-pool-baseline`, with the new
`examples/concurrent_reads.rs` harness copied there as `concurrent_reads_baseline.rs` and only its
`SharedGraph` import and pooled half removed. It uses four threads competing for the single
`Mutex<Graph>`, matching the old
request serialization. After is this branch's four-slot `SharedGraph` using the same graph and
already projected SQLite generation. The harness also prints an after-build serialized comparator;
that is **not** substituted for main in this table. The original and pooled runs use identical
search, result decoding, thread/barrier timing, 20-result limit, and `LastScan` policy.

Each timed run performs **4 workers × 40 calls = 160 calls**, after an untimed warm call. Latencies
include time waiting for the mutex or connection lease. Builds finished before timing. The index
and OS caches are warm, and pool creation/startup scanning are excluded. Runs were serial:
mixed before → after, indexed before → after, then repeat. One-minute load before each pair was
**1.35 / 1.23 / 1.13 / 1.33**; live one-second `vmstat` samples showed **99–100% idle** and no
runnable competitor or I/O wait. An earlier round contaminated by another worktree's test run
was discarded; these runs followed that process's exit. The mixed-query spread
is large enough that a single precise speedup would be misleading, so both rounds are retained.

| Query | Metric | Before 1 | Before 2 | Pooled 1 | Pooled 2 |
| --- | --- | ---: | ---: | ---: | ---: |
| Mixed short miss `x无` | Total wall ms | 10031.41 | 10504.76 | 2574.30 | 3234.41 |
| Mixed short miss `x无` | Reads/s | 15.95 | 15.23 | 62.15 | 49.47 |
| Mixed short miss `x无` | p50 latency ms | 62.649 | 64.194 | 63.536 | 66.528 |
| Mixed short miss `x无` | p95 latency ms | 64.342 | 83.619 | 67.926 | 121.404 |
| Indexed miss `不存在` | Total wall ms | 16.44 | 16.59 | 19.86 | 19.14 |
| Indexed miss `不存在` | Reads/s | 9730.81 | 9646.51 | 8058.39 | 8360.37 |
| Indexed miss `不存在` | p50 latency ms | 0.092 | 0.093 | 0.454 | 0.421 |
| Indexed miss `不存在` | p95 latency ms | 0.127 | 0.131 | 0.798 | 0.748 |

The pool improves aggregate throughput for expensive reads, **not** individual query CPU cost.
The mutex is not fair: serialized workers can run many consecutive calls while a few other calls
wait much longer, so p50/p95 do not summarize total queueing or imply a tail-latency improvement.
The very cheap indexed miss is **13–17% lower throughput**, with higher latency, after adding
pool synchronization, per-read snapshots, and publication coordination. Four connections also
retain separate SQLite caches and generation leases; this is a bounded concurrency tradeoff,
not a universal read-speed improvement. Writes and strict freshness scans remain serialized.

Generate the deterministic graph above, prebuild the release example, then run each command twice:

```sh
mise exec -- cargo build --release --example concurrent_reads
target/release/examples/concurrent_reads /path/to/graph /path/to/cache/index.db x无
target/release/examples/concurrent_reads /path/to/graph /path/to/cache/index.db 不存在
```

For the original baseline, copy the example to a `93a1809` worktree and remove the pooled half as
described above; prebuild before measuring and interleave original/pooled runs on the same index.
Raw logs are `/home/ubuntu/read-pool-measurements/{base,final}-{mixed,indexed}-{1,2}.log` on the
measuring VM. Earlier pre-rebase and contended probes are not the table's measurements.

## Structural costs

These are not regressions to fix. They are the shape of the design, and a change that moves one of
them needs an ADR, not a micro-optimisation.

- **A page is replaced as a whole** (ADR-0004). Writing one block re-inserts every block of its page,
  so every single-block write grows linearly with the page: `append_block` 47 ms at 2686 blocks and
  150 ms at 6186, `prepend_block_in_page` 145 ms and `set_block_collapsed` 162 ms at 6186. Block-level
  upserts would fix it and would break the model that makes a page replacement atomic.
- **Batching is the only mitigation that does not break that model.** `insert_batch` puts N blocks
  into one replacement: 241 ms for ten blocks against 1516 ms for ten calls, 6.3×. The saving is
  bounded by the page, not by N — the batch still re-projects every block of the page — so it helps
  a paste and does nothing for a single keystroke.
- **What a batch commit's per-operation cost is made of.** On the 6192-block page, one extra
  operation in a batch costs **12.9 ms**, and it is not the parse: `PageDocument::parse` of the whole
  page is **0.69 ms**, while `PageFacts::extract` is **5.06 ms** and every operation performs two of
  them, one in the edit and one in the edit's own re-parse check. Rendering the block's lines and
  walking `block_shapes` make up the rest. An attempt to hand the check's parse back to the next
  operation (`perf/batch-parse-reuse`, 2026-11-04) removed 0.69 ms per operation and moved a ten
  operation commit from 268.68 ms to 269.25 ms: nothing, inside the ±5 ms the same run produced.
  Sharing the facts extraction instead would take an operation to about 7.8 ms and a ten-operation
  commit to about 225 ms; it was not done, because the page replacement above is 149 ms of that
  commit and a page with the median block count answers the whole batch in under a millisecond.
- **A scan is one stat per file.** `CheckNow` is 99 ms on 14138 files and does not depend on page
  sizes; that is the floor for any change to freshness.
- **`VerifyContent` reads every file.** It exists for the one case a stat cannot see — a sync tool
  restoring a modification time — and costs a full read and hash.
- **Short non-CJK or mixed searches still scan every block's text on a miss** (ADR-0016). The historical scan-only
  baseline was 32.6 ms on 112890 blocks. Queries of at least three characters now use the folded
  trigram index, and short all-CJK queries use the unigram/bigram index; the Linux measurements
  above include their storage, projection, and ranking costs.
- **Writes do not scan.** A write resolves its target through the index and re-projects only the page
  or pages it wrote, so an append never pays for a page it did not touch.
