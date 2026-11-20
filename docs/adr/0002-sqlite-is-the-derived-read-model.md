# ADR-0002: SQLite is the sole derived read model

Status: Accepted
Date: 2026-10-29

## Context

A full in-memory graph couples memory use and index-construction cost to graph size. A queryable
materialized index permits bounded reads and page-scoped updates without taking ownership of
content.

## Decision

- Normal reads are served by one concrete `Index` backed by SQLite, including an in-memory SQLite
  fallback when the cache directory is not writable.
- The database lives outside the graph in the cache directory and is always deletable as a cache
  family. The logical index path's `.current` sidecar atomically selects a generation-specific
  SQLite file; direct SQLite inspection uses `Index::storage_path()`. WAL/SHM filenames are never
  reused across generations. Graph handles check publication before each read and under the write
  lock before writing. Full scan publication follows ADR-0004.
- Recovery intents are not derived state: they live in the durable graph journal specified by
  ADR-0004. A legacy intent must be exported before the index can be discarded or rebuilt.
- Raw source lines and persistent source maps are not stored in SQLite; they are reconstructed by
  parsing files when writing.
- Schema or parser-version incompatibility causes a rebuild, not a cache migration.
- Schema v14 stores the deterministic, versioned configuration signature of a completed
  projection. It encodes the supported name format, both journal patterns and ordered favorites
  with length-prefixed strings, rather than a collision-prone hash or process-local generation.
  A missing or different signature forces full reprojection, including on reopen with startup
  scanning disabled. Clearing derived rows invalidates the signature in the same transaction;
  only a successful complete scan publishes it, so interrupted reprojections cannot bless a
  partial generation. An older schema is rebuilt under the existing incompatibility rule.
- The `index` module is the only module that knows SQL or exposes SQLite transaction boundaries.
- There is no storage-backend trait, plugin registry, bincode cache, full-graph read model, or second
  production read path.
- [ADR-0015](0015-page-identity-includes-derived-entities.md) supersedes the one-file-one-page
  identity rule: the derived SQLite model also serves page entities without files.
- Amended by ADR-0011: the page snapshot a commit compares its base version against is read from the
  file, not from the projection, because a token that describes the index cannot refuse a write to
  bytes the index has not seen. That read is not a second read model: it answers one page's current
  source state, the read that opens a page at one block cuts its subtree out of the same parse, and
  every ordinary read still goes through the index.

## Consequences

Query memory can scale with results rather than graph size, and unchanged pages need not be
deserialized on warm starts. The project accepts a native SQLite dependency, schema rebuilds, WAL
operation, and the need to test file/index divergence. Cache deletion remains a valid recovery
operation.
