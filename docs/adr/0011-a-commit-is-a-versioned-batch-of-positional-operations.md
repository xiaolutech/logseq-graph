# ADR-0011: A commit is a versioned batch of positional operations

Status: Accepted
Date: 2026-11-03

## Context

ADR-0005 gives a block two identities, durable and ephemeral, and ADR-0009 keeps both explicit at
every boundary. Both are addressing rules for a *single* write: a caller names one block in one
observation, and the library transforms it.

An editor adding two blocks in one keystroke asks a different question. It holds one observation of a
page, applies several intents to it without waiting for file I/O, and the intents have to land
together or not at all — a half-applied structure change is a document whose block count disagrees
with the keystroke that caused it. Addressing those intents with a `BlockSelector` fails on both
counts. A block created by the second intent of the batch has no durable id, because `id::` is
written only when something references a block and 1.19% of the blocks on the reference graph state
one. An ephemeral handle is bound to a graph revision, so an unrelated write to another page
invalidates every handle an editor holds for this page — the granularity ADR-0009 already rejected.

Staleness has to be page-scoped for the same reason. A write to another page changes nothing about
this page's bytes and must not refuse this page's commit.

## Decision

- **A commit is one page, one base version, and an ordered list of operations.** It applies them in
  array order to one candidate document, replaces the source file once, re-reads it once, projects
  the whole page in one index transaction, and advances the revision once. An invalid operation
  rejects the whole batch before the source file changes, and no prefix of a failed batch is written.
- **Inside a batch a block is addressed by a path of child indices from the page root**, resolved
  against the candidate document as the preceding operations of the same batch left it. The empty
  path names no block, and a path that names nothing is a refusal rather than a neighbouring block.
- **A batch path is not an identity.** It states nothing durable, nothing about a graph revision,
  and it means something only against the base version that produced it. It does not extend
  ADR-0005: `BlockSelector` remains the only addressing surface for the standalone writes, and no
  read hands out a path as an identity.
- **The freshness guard is a page-scoped, opaque `version` token over the page's authoritative source
  bytes** — the FNV-1a 64 fingerprint the projection already records for a file baseline, in hex. A
  commit compares `baseVersion` with the bytes it read under the write lock, before writing anything.
  A mismatch writes no source bytes, changes no index rows, and answers a typed stale error carrying
  a fresh snapshot of the page. Comparing an SQLite revision is insufficient: another program can
  change the file without updating the index. The token is compared for equality and never parsed.
- **Offset units are stated at every boundary.** The library splits editable text at a UTF-8 byte
  offset that must sit on a character boundary and refuses an offset that does not; an adapter that
  holds a UTF-16 code-unit offset converts deliberately. Neither layer may split a character.
- **Refusals are typed and name the failing operation.** No operation in a batch merges, rebases, or
  guesses a new target (ADR-0010).

## Consequences

Positive:

- A keystroke that changes structure costs one lock, one read, one file replacement, one re-parse,
  and one projection instead of one each per operation — the mitigation `insert_batch` already
  applies to a paste.
- A page's commit is refused only by a change to that page. An edited or unreadable neighbour is not
  this page's problem.
- Nothing new can be mistaken for identity: a path cannot outlive the batch that stated it, and no
  read mints one.

Negative:

- A path is invalid the moment the page changes, so a caller that keeps a path across commits has to
  re-read. That is the price of positional addressing with a version guard.
- A batch still costs O(page) for the source replacement and the projection: batching bounds the
  number of replacements, not the size of one. No operation here is O(1).
- Two clients editing one page both fail closed and neither merges. Synchronization stays outside this
  library (ADR-0010).
- The library re-parses the candidate between operations, so a batch of n operations costs n parses
  of one page. A single mutable tree would remove that cost and would be a second implementation of
  every edit; the measurement decides whether it ever needs one.
