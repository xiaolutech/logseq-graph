# ADR-0001: Markdown files own all product facts

Status: Accepted
Date: 2026-10-29

## Context

The service must coexist with Logseq, editors, git, and synchronization tools. A database that owns
content would make those tools secondary writers and would make recovery depend on this service.

## Decision

- Markdown files under the graph root are the only source of product facts.
- Every index, cache, revision marker, and write intent is derived operational state.
- Deleting the complete derived state must be safe and sufficient to trigger reconstruction from
  the files alone.
- No product fact may exist only in SQLite or only in process memory.
- When files and derived state disagree, the files win. The disagreement is repaired or reported;
  it is never resolved by changing the files to match derived state.
- [ADR-0015](0015-page-identity-includes-derived-entities.md) supersedes the file-only
  interpretation: Markdown can name page entities for which there is no separate file.

## Consequences

External writers remain supported, and the graph can always be backed up as ordinary files. The
implementation must handle a read model that lags the files and must recover interrupted writes.
Features that require database-only canonical content are out of scope.
