# ADR-0005: Block identity is explicitly durable or ephemeral

Status: Accepted
Date: 2026-10-29

## Context

Position-derived identifiers can change after edits, while database rowids change after rebuilds.
Presenting either as permanently stable makes callers hold identifiers whose lifetime is undefined.

## Decision

- Public block targets use `BlockSelector::Durable(BlockId)` or
  `BlockSelector::Ephemeral(EphemeralId)`; SQLite rowids are never public identity.
- Durable identity is an `id::` UUID stored in Markdown and preserved across edits and moves.
- An ephemeral id is a random UUID minted when a page is projected, stored only in the index, and
  never written to Markdown. It names one block while its row exists **and** the page's projected
  content fingerprint equals the fingerprint of the bytes an operation observes. An absent row names
  no block; a fingerprint mismatch is refused as a stale page and writes nothing. The library never
  re-resolves one to a neighbouring block, never scans to look for one, and never mints a durable id
  as a side effect.
- Public block views carry the selector of the block they report, so a caller can address it again
  without a second read.
- Ordinary block creation does not add `id::`. Callers explicitly request durable identity when
  needed, and operations that require durable identity either receive that permission or fail.
- Every mutation revalidates source freshness even when the target has durable identity.
- Amended by ADR-0011: a commit inside one page version addresses blocks by positional path. That
  path is not a third identity: it means something only against the base version that produced it.
- Amended by ADR-0012: the ephemeral half of this decision is now a random, projection-scoped id
  rather than a handle bound to an instance and a revision. `EphemeralHandle`, and the `StaleHandle`
  error that reported its mismatch, are deleted; an id that no row holds is an ordinary miss.

## Consequences

Ephemeral callers must re-query after any change to that page's bytes instead of risking aliasing to
another block; a page they did not touch keeps its ids. Durable identity adds a visible property line
and is therefore opt-in. Rebuilding SQLite never changes the meaning of a durable identifier, and it
replaces every ephemeral one.
