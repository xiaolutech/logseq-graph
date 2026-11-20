# ADR-0014: Served writes preserve file-versioned commit contracts

Status: Accepted
Date: 2026-11-12

## Context

The RPC surface currently exposes reads while the library already has sixteen writes. Serving a
write over HTTP must not turn the adapter into a second write engine. The library already owns the
file-first ordering, graph-wide write lock, source-byte validation, write-intent recovery, and
whole-page projection that make a mutation safe (ADR-0004, ADR-0011, ADR-0012).

A served editor needs both standalone writes and positional batches. Both must fail closed when the
page changes underneath their addresses. A stale error is useful only if a client can recover from
it, while a response that merely reports an internal failure does not distinguish a refused write
from one whose file landed but whose projection failed. The server's existing startup states also
apply to writes: an available socket is not an available graph (ADR-0008).

## Decision

- **Writes use the configuration of the last successful shared scan.** The polling handle
  publishes its source configuration with its successful observation, and a writing handle adopts
  it under its write lock before resolving names or rendering journal paths. If its own index's
  persisted configuration signature differs, it fully reprojects under that lock first, including
  when the published source came from a handle with a different index. Renamed journal titles
  cannot resolve through the request handle's old configuration after the watcher has projected
  the new one.

- **HTTP bodies are bounded before dispatch.** Declared lengths over 16 MiB are refused with 413;
  decoded bodies are read through the same limit plus one byte to detect overflow even without
  a trusted length. Sixteen fixed request workers isolate a stalled body from independent clients
  without creating an unbounded number of application threads. Graph calls retain their state
  mutex, so concurrent body reads do not introduce concurrent mutations. Body reads have no
  deadline; exhausting all sixteen workers remains a local denial-of-service limitation.

- **The HTTP boundary rejects foreign authorities and origins before dispatch.** `Host` must name
  `localhost` or a literal loopback address at the bound port. An optional `Origin` must match
  that Host's HTTP origin. Invalid hosts or origins receive 403; `POST /api` without the
  `application/json` media type receives 415 (media-type parameters are allowed). Missing Origin
  remains valid for non-browser clients. These checks apply to reads and writes, including during
  startup, and are browser isolation rather than authentication of local processes.

- **Every served write delegates to the corresponding library write.** The RPC adapter translates
  arguments, results, and errors only. It does not implement edits, resolve blocks, touch files or
  SQL, choose a different write order, or introduce another version, locking, intent, or recovery
  protocol. All sixteen library writes may be exposed through this one contract; their individual
  RPC method names and argument shapes are a separate surface decision.
- **Version tokens remain opaque page-file versions.** A `baseVersion` is the same content
  fingerprint token a page snapshot already states and the same token the library compares with the
  bytes read under the write lock. No RPC method substitutes the graph revision, an index version,
  an HTTP validator, or a second fingerprint for it. An ephemeral-selector write retains the
  library's implicit projected-fingerprint comparison; no request supplies a replacement fingerprint
  for that comparison.
- **The library's write lock covers each whole mutation.** The adapter holds the graph state lock for
  one complete library call, so one RPC mutation has one observation and one response. It never
  releases either lock between a library call's version or fingerprint check and its commit, nor
  combines multiple library writes into an RPC transaction. Lock contention and timeout remain
  structured failures rather than an adapter retry.
- **A batch commit carries `baseVersion` and applies its operations in array order.** The version
  guards the whole batch, including an empty one; positional paths belong only to that page version.
  A version mismatch or any refused operation rejects the entire batch before the source changes.
  The answer reports the committed page snapshot, applied count, and ordinary read metadata from the
  library. No adapter validates a prefix, splits a batch, retries individual operations, or advances
  the version itself.
- **A served write scans only to repair a configuration-generation mismatch.** A write has no
  `freshness` option. `last`, `check`, and
  `verify` select which scan a read answers from; they do not weaken or strengthen a write's
  page-file guard. The adapter does not run `CheckNow` or `VerifyContent` before or after a write.
  With a matching configuration generation, the library performs only its page resolution discovery
  when required, and projects only the page or pages it wrote. A successful write returns its actual
  revision and read metadata;
  read-your-writes for that revision does not claim that unrelated pages were refreshed.
- **The RPC layer never rebases, merges, or repairs a conflict.** A stale batch version, stale
  ephemeral selector, missing selector, ambiguous durable id, or refused operation is returned to the
  client. A stale-version error includes the current page snapshot supplied by the library so the
  client can re-read positions and decide whether to retry. Repeating an unchanged request is not an
  implicit rebase. Conflict resolution remains the sync layer's responsibility (ADR-0010).
- **Multi-file writes keep their write-intent and partial-failure contracts.** The destination is
  written first, source bytes are never rolled back, and recovery remains fingerprint-driven on the
  next graph open. A partial move, re-read failure, or index-after-write failure is an explicit
  error, never converted into success merely because some bytes landed. The RPC layer does not
  clear, reinterpret, or recover an intent, and does not automatically reopen the graph after one.
- **Cross-page moves preserve the library's identity rules.** Amendment (2026-11-19): either end
  accepts an ephemeral selector through the fingerprint gate; the intent records its pre-hash-bound
  child-index path (ADR-0012). The RPC surface does not write an `id::`, substitute an ephemeral id
  in an intent, or downgrade the operation to a same-page move.
- **Every write failure is a structured error in the official Fastify shape.** The body carries
  `{statusCode, error, message}` — the shape the official API returns — plus this repository's own
  `code` and `data`; `data.kind` identifies the library error class and `data` carries its
  actionable facts, including the affected page, operation index and variant, ids, paths, or
  current snapshot where the library supplies them. The HTTP status remains a transport hint, not
  the contract. Two statuses are deliberately stricter than the official API, and this is a
  documented difference: an unknown method answers 400 with `code -32601` where the official API
  answers 200 with `{error: "MethodNotExist: <leaf>"}`, and a missing page or block answers 404
  with `code -32004` where the official API answers 200 `null`. No failure
  becomes an empty result, a plain success, or an unstructured internal-error string, and no error
  mapping claims that no bytes landed after a file-commit failure.
- **Startup and open-failure behavior is the same for reads and writes.** While the startup scan is
  running, a write is refused with the existing scanning error and its progress data; it is not
  queued, replayed, or allowed to race graph opening. After a failed open, a write returns the
  structured open failure and changes nothing. Only a successfully opened graph accepts writes.
  Restart and recovery are explicit server lifecycle actions, not a hidden consequence of a write.

## Consequences

The RPC surface gains writes without gaining a second implementation of their correctness rules.
Clients use one page-version model for snapshots and batches, receive actionable stale and partial
failures, and can rely on successful responses describing the bytes the library committed.

A write still holds the single server state lock for its complete O(page) source replacement and
projection; a cross-page move pays that cost for two pages. Concurrent RPC requests wait rather than
form a second transaction or read-consistency model. Writes do not make unrelated externally changed
pages fresh, and a successful write does not replace an explicit read freshness policy.

Clients must handle stale versions by obtaining a new snapshot and recomputing their operations.
Cross-page moves accept durable or ephemeral selectors at either end; granting durable identity is
not required. After a partial file commit or a failed startup, a client receives an error rather than an
automatic retry, rollback, rebase, or recovery attempt.
