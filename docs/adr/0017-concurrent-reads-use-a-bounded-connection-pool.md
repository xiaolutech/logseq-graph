# ADR-0017: Concurrent reads use a bounded connection pool

Status: Accepted
Date: 2026-11-19

## Context

A SQLite connection is not shareable across simultaneous queries. An exclusive engine handle is
useful to writers but serializes otherwise independent RPC reads. Freshness checks can write the
projection, and shadow publication changes which database file a reader must open.

## Decision

- `SharedGraph` is the shared-caller engine facade. It retains the original `Graph` under a writer
  mutex and leases a bounded number of independent read-only SQLite connections (four by default).
  The pool uses the standard library; there is no new storage backend or pool dependency.
- Read callbacks receive a restricted `ReadHandle`, never mutable access to a writable `Graph`.
  Its policy builder exposes the same indexed reads. A lease is returned on success, error, or
  unwind. When every slot is occupied, additional readers wait; in-memory SQLite is serialized on
  its original connection rather than copied into a second read model.
- Default, `LastScan`, and `AllowStale` queries do not hold the writer mutex. `CheckNow` and
  `VerifyContent` perform their scan through the serialized writer, then query a read-only
  connection. All handles share scan observations and the existing scan/publication exclusion.
- SQL connection initialization, functions, snapshots, and generation leases remain in `index`.
  Read-only connections register the same functions as writer connections, and retain their
  read-only mode when reopening after publication. Indexed read values and revision metadata are
  read inside one SQLite snapshot. The short scan-operation lock is released before query work.
- An in-flight read can finish against its leased, fully published generation. Subsequent reads
  check the generation pointer and reopen before querying. Adopting a publication on a pooled
  reader never clears a scan failure recorded after that publication.
- RPC startup state is locked only to obtain the shared facade, not while serving a read or write.
  Writes retain the original graph/file locks and file-before-index commit ordering.

## Consequences

Up to the configured number of read queries execute in parallel with each other and with writes.
Long reads can retain old generations and WAL pages until they finish. Each connection has its own
SQLite caches; four readers trade additional memory and descriptors for bounded concurrency.
Freshness scans still serialize with writes on the shared writer, but ordinary pooled reads do not.
The exclusive `Graph` API remains available to single-threaded callers; shared callers use
`SharedGraph::read(&self, ...)` and `SharedGraph::write(&self, ...)`.
