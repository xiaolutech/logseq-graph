# ADR-0006: Read freshness is an explicit policy

Status: Accepted
Date: 2026-10-29

## Context

Core writes can update the index immediately, but external writers change files independently of
the watcher. A single unqualified "fresh" flag would hide both known divergence and the limits of
stat-based observation.

## Decision

- Every read result carries graph instance, revision, staleness, and last-scan metadata.
- `Freshness::LastScan` is the default: serve the last committed observed revision, but fail with
  `StaleIndex` when divergence is already known.
- `Freshness::AllowStale` explicitly permits known-stale results and marks them stale.
- `Freshness::CheckNow` synchronously checks the relevant source state, applies discovered changes,
  and only then executes the query. A failed check fails the read.
- Successful core writes provide read-your-writes at the returned revision.
- `stale = false` means that the model matched the observed files at the stated scan; it is not a
  claim that no external writer has changed a file since that observation.

## Consequences

Default reads fail closed when the service knows the index is stale, while callers that prefer
availability can opt into stale data. Fresh-on-demand reads pay for bounded file checks. External
changes can remain unobserved until a scan, so the API exposes observation time rather than an
absolute freshness promise.
