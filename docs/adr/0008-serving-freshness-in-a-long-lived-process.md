# ADR-0008: Serving freshness in a long-lived process

Status: Accepted
Date: 2026-10-31

## Context

The scan is one to two orders of magnitude more expensive than any query it serves. On a measured
graph of 14137 Markdown files, 112882 blocks, and 16.1 MB of Markdown, a cold full projection costs
7.99 s, a warm `CheckNow` stat pass 262 ms (120 ms in an independent release measurement), and a
`VerifyContent` read-and-hash pass 629 ms. Against that, answering queries from the index costs:
a 20-hit search 0.88 ms, a no-hit search that must scan all `blocks.text` 31 ms, listing all 14137
pages 2.55 ms, and reading the largest page's 6186 blocks 1.84 ms. Writes touch only their own page:
10.9 ms to append to a 1-block page, 134 ms to append to the 6186-block page.

The page distribution concentrates change on small pages: 53.3% of pages are empty, the median
non-empty page has 6 blocks, the 99th percentile 85, and only 2 pages exceed 1000 blocks.

The CLI pays freshness per request: a command that wants current answers runs `CheckNow` on its own
terms and exits. A long-lived server cannot copy that. A per-request `CheckNow` would pay 262 ms of
stat work on every read to guard queries that cost about a millisecond, and a per-request
`VerifyContent` would pay 629 ms. A timer-based poll does not fix this: it reads the same
`(size, mtime)` pair that ADR-0006 already records as blind to a sync tool restoring a modification
time, it only moves the scan cost from the request to the background, and its staleness window is
whatever the timer interval happens to be.

## Decision

- The RPC adapter passes the request's freshness policy through to the engine. No adapter
  chooses, caches, or retries freshness (ADR-0003: adapters hold no query logic).
- Every request declares a freshness policy explicitly, and the default is `Freshness::LastScan`.
  The default is stated in the protocol itself, not inferred by an adapter; `CheckNow` and
  `VerifyContent` remain explicit caller upgrades.
- A long-lived process pays the full projection exactly once, at startup. No served read repeats
  the cold-start scan.
- The listening socket is bound before that projection starts, and the projection runs off the
  request path. A caller therefore reaches the process for the whole time it is indexing instead of
  reading a refused connection as a server that never started.
- Until the graph is open, `getCurrentGraph` answers the scan's progress — `{"scanning": true,
  "scanned": N, "total": M}` — and every other method answers a `-32003` structured error carrying
  the same three fields. Once the graph is open, `getCurrentGraph` states `"scanning": false` beside
  its usual fields, so a caller branches on one field that is always present rather than on one that
  appears. A caller decides readiness by that answer, never by the diagnostic line that announces the
  bound socket.
- Progress is the engine's, not the adapter's: the engine reports files projected out of the files
  discovery found, as counters another thread reads, and the adapter only serializes them. A caller
  of the engine that needs the same numbers passes a counter to `Graph::open_with_progress`; nothing
  in the scan calls back into the caller's code.
- A startup scan that fails is answered, not fatal: the process keeps the bound socket and every
  method reports the failure with a `-32603` structured error that states the reason. The process no
  longer exits when the graph cannot be opened, because the port it already owns is the only channel
  the caller has.
- Index refresh is driven by filesystem notifications, coalesced into path-level refreshes.
  Polling remains the fallback when watcher setup fails and the explicit `serve --no-watch` mode
  for mounts such as Docker Desktop's FUSE file sharing, where notifications can be unreliable.
  Its configurable interval defaults to one second.
- The watcher owns a separate graph handle and SQLite connection. It never holds the request state
  lock while scanning, so default reads continue to answer the last committed revision during a
  background scan. Related handles share scan exclusion and the latest scan observation. Full
  scans prepare and build without the graph lock, then catch up changed files in the shadow
  outside it. Under the lock, publication requires zero remaining file differences and unchanged
  live revision/configuration; otherwise it releases the lock and retries catch-up (ADR-0004).
  After eight unsuccessful rounds the scan fails without replacing the live generation; ordinary
  retry policy applies. Small TrustStat scans retain in-place commits. Writes can run
  during preparation and catch-up, including across different indexes; path refreshes remain locked throughout.
  Ordinary last-scan reads do not wait on that lock. Both commits and writes use the configured
  write-lock wait budget. No changed-file parsing or bulk repair SQL runs under the shadow publication
  lock; its metadata sweep and OS synchronization still have no hard wall-clock upper bound.
  Handles reopen a published generation before their next read or write, including independent
  Graph handles sharing the logical index path.
  Adopting a matching completed generation clears failures from the replaced generation without
  moving the handle's observation timestamp forward.
- Under a watcher, `Freshness::LastScan` remains fail-closed: a failed background scan refuses
  default reads until a later scan succeeds. `AllowStale` answers the last committed rows with
  `stale: true`. Both notification refreshes and polling can miss a same-size edit with a restored modification time;
  `VerifyContent` remains the only policy that detects it (ADR-0006).

## Consequences

Notifications avoid graph-wide stat passes while idle. Overflow, watcher errors, configuration
changes and directory events instead demand a full scan, as does a failed path refresh.
Polling mode still pays one graph-wide stat pass per interval, including when nothing changed.

A caller that polls readiness pays one request per poll for the length of the projection, which is
9.5 s on the reference graph (PERFORMANCE.md); the alternative — a caller that connects once and
waits — would need a push channel this surface does not have. The scan's progress counters describe
the scan running now,
so a later `CheckNow` restarts them; the served answer states `scanning: false` between scans, and a
caller that wants a progress bar for a rescan reads the engine's counter directly.

With notifications delivered, the debounce delay plus refresh cost is the observable staleness
bound; polling mode retains the interval plus scan cost bound.

A long-lived server retains a serialized request writer and a separate watcher handle. ADR-0017
adds pooled read-only connections without an exclusive index-file lock. Graph-wide filesystem
locks and SQLite transactions still serialize write/scan commits; WAL permits concurrent reads.

The original service measurements were a 31 ms no-hit text scan, a 134 ms largest-page append and
a 7.99 s cold start paid once per process lifetime. They are historical context, not current
budgets. ADR-0016 now defines indexed FTS5 search and its short-query fallback; this decision
still does not introduce block-level upsert or cold-start parallelization. Current measurements
live in PERFORMANCE.md.

## Amendment (2026-11-04): a read that answers from a page file declares no policy

Two methods have no freshness to declare. `getPageVersion` answers the version of the bytes the index
projected for a page beside the version of that page's file as it is now, so a caller can tell whether
the index is behind one file without asking for a scan. `getPageSnapshot` answers that file's content
— its properties, and its blocks in the shape `getPageBlocksTree` already answers — under the version
of the same bytes, so noticing that one page changed and reading that page are two requests at the
file tier instead of a poll of one and a scan of the graph. Neither runs a scan, and a freshness policy
on either is refused rather than an argument the adapter ignores.

The snapshot has one limitation the version read does not: it locates a page's file through that
page's index row, so a page written since the last scan answers `exists: false` until a scan projects
it. That is the price of not listing the graph to find one file, it is stated in
`Graph::page_snapshot`'s documentation, and it does not affect the caller this read is for: one that
opens a page it already knows. A caller asking about a page it has just written is asking the index a
question only a scan can answer.

The decision above still holds. No adapter chooses, caches, or retries a policy, and every read whose
answer comes from the index still declares one. What this amendment fixes is the boundary the two
kinds of read share: a policy settles *which scan's rows a read answers from*, and a read that answers
from a file has no rows to settle. It therefore declares nothing and states the same `revision`,
`stale`, and `last_scan` fields as every other answer, so a caller reads one envelope rather than two.
Accepting a freshness policy here would be worse than declaring none: it would read as
permission to skip the file read, and skipping the file read is what would stop the answer from being
the current version.

The costs this leaves: the read pays one indexed seek, one baseline row, one stat, one read of the
page file, and one hash of its bytes — 0.033–0.041 ms for the median non-empty page of the reference
graph and 1.07 ms for the page holding 6195 blocks, against `CheckNow`'s 100.6–102.1 ms to answer the
same question about one page (PERFORMANCE.md). It is also the one answer that does not move when a
scan runs, because both of its versions are facts about bytes. A caller that wants the index brought
up to date still needs a scan, which is what `check` and `verify` are for on every other method.

## Amendment — 2026-11-19: Notifications drive path-level freshness

The long-lived process registers a recursive filesystem watcher before a catch-up full scan.
Notifications are hints: direct Markdown children of `pages/` and `journals/` become a
deduplicated path refresh after 50 ms of quiet, bounded at 500 ms from the first relevant event.
Hidden entries, nested files, backups, editor leftovers and index sidecars are not projected.
Configuration changes, directory events, overflow / rescan flags and watcher errors request a
full scan. Any failed path refresh also requests a full scan. A failed scan is reported once per
distinct failure and retried every polling interval until recovery.

Path refresh does not advance `last_scan` or clear divergence: only a successful full scan does.
The notification-mode staleness bound, when events are delivered, is debounce plus refresh cost
instead of interval plus scan cost. An idle watcher blocks without scanning. Notifications can be
lost silently on some mounts; setup failure automatically falls back to polling, and `--no-watch`
explicitly chooses polling, including for Docker Desktop FUSE. `--poll-interval` retains its
polling-period meaning and also controls failed-scan retries.

Both path refresh and ordinary full scans trust stat fingerprints. A same-size edit with a restored
mtime is still only caught by `VerifyContent`, even if a notification was delivered.
