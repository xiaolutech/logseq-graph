# ADR-0012: Ephemeral block identity is projection-scoped

Status: Accepted
Date: 2026-11-05

## Context

ADR-0005 gave the library two block identities: a durable `id::` UUID read out of Markdown, and an
ephemeral handle that bound the graph instance, the revision, the page, and a preorder path.
ADR-0009 then asked what a compatibility surface should state for the 98.8% of blocks that hold no
`id::`, and refused to synthesise one.

The handle answered the safety question and failed the usability one. Its granularity is the whole
graph: every write advances the revision, so one edit invalidates every handle any client holds, for
every page. ADR-0009 rejected exactly that granularity when it refused to derive a UUID from the
`synthetic_id`, and the handle reintroduced it by another route. A client cannot show a block, keep
its address, and edit it a moment later — the ordinary shape of an editor.

Three properties are needed at once, and only a value the index owns can have all three:

- **Not derivable.** A value computed from a position is a function of that position, so after an
  edit it names whatever block now stands there. That is the aliasing ADR-0009 refused, and it stays
  refused: *position-derived identity is still rejected*. What is adopted here is a random value,
  which no function can recompute.
- **Page granularity.** One edit must expire the addresses of the page it edited and nothing else.
- **Fail closed.** A value whose block is gone must name nothing, and a write through it must
  refuse, never reach a neighbouring block.

## Decision

- **The library mints no *durable* identity.** A durable id exists only as an `id::` line in
  Markdown. A projection may mint an *ephemeral* id for a block that states none: a random UUID,
  stored only in `blocks.ephemeral_id`, never written to a file, never recorded in a write intent,
  and never presented as durable.
  Amendment: `ensure_block_id` is ADR-0009's explicit caller-requested identity grant, not a
  projection side effect; it writes `id::` using Logseq's DataScript squuid format (Unix seconds
  prefix, random v4 remainder), as sourced in the engine reference.
- **A block states exactly one identity.** A row holds either `durable_id` or `ephemeral_id`, and the
  schema's `CHECK` refuses a row with both or with neither. The public selector has the same two
  cases, and the block view carries the one its row states.
- **An ephemeral id names one block while its row exists *and* the page's projected content
  fingerprint equals the fingerprint of the bytes an operation observes.** An absent row names no
  block. A fingerprint mismatch is refused as a stale page, and the refusal writes nothing.
- **The fingerprint gate is an invariant, not an optimisation.** A row's preorder and child path are
  coordinates in the bytes that row was projected from, and in nothing else. What makes an ephemeral
  write safe is not that the id is random; it is that the coordinates are checked against the file
  under the write lock before a byte is computed, so an index that missed an external edit refuses
  instead of editing a neighbour.
- **A re-projection of unchanged bytes reuses the ids it already minted**, matched by preorder. Equal
  fingerprints mean one text and one parser version, so the block at a preorder is the same block.
  Without this, a scan that re-projects without changing anything (`VerifyContent`, a stat-only
  refresh) would expire every ephemeral id in the graph, which is the granularity ADR-0009 rejected.
- **Any change to a page's bytes invalidates every ephemeral id of that page**, because the page's
  rows are replaced as a unit. Ids of other pages are untouched.
- **A write addressed by an ephemeral id never scans and never searches.** The lookup is one
  equality on the unique `blocks.ephemeral_id` index. A miss is reported as a miss; no layer looks
  for a block that "must have been" this one.
- **A write intent and a snapshot never hold an ephemeral id.** A snapshot is a pure function of
  one file's bytes. Amendment (2026-11-19): a cross-page move records each endpoint as a durable id
  or a child-index path resolved under the lock against its file's pre bytes after the fingerprint
  check. Recovery resolves the source path only when the source matches `from_pre_hash` and the
  destination matches its post hash, then verifies the derived removal against `from_post_hash`
  before writing. The anchor is a record, not a recovery locator.
- **Reads do not verify fingerprints.** A read answers what the index says, under the freshness
  policy, and marks staleness there. Only a write can put an edit in the wrong block.

## Consequences

Positive:

- A client can read a page, keep every block's selector, and edit any of them without re-reading.
  A page-scoped expiry makes that usable for an editor.
- A stale id is inert: it names nothing on read and is refused on write. There is no aliasing path,
  because there is nothing to re-derive.
- Duplicate durable ids keep working as they did — files are the authority and `AmbiguousBlockId`
  is unchanged — while an ephemeral id is unique by construction and the schema enforces it.
- The identity a write answers with is the row its own replacement committed, so writes chain.

Negative:

- An edit invalidates every id a client holds for that page, so a client that spans an edit must
  re-read. This is page granularity, not the graph granularity ADR-0009 rejected.
- An ephemeral id is meaningless outside the index that minted it: deleting or rebuilding the index
  mints new ones. A client must treat them as expiring values, not as references to persist.
- The security of a write through an ephemeral id rests on the content fingerprint comparison, which
  costs one hash of the page's bytes per ephemeral write, under the write lock, before the edit.
- Amendment (2026-11-19): cross-page moves need no `id::` at either end, but their positional intent
  records are coordinates valid only in the bytes bound by their pre hashes, not reusable identities.
