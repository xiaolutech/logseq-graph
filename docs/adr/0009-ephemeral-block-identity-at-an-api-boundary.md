# ADR-0009: Ephemeral block identity at an API boundary

Status: Accepted
Date: 2026-11-01

## Context

ADR-0005 defines two block identities. A **durable** id is the `id::` UUID stored in Markdown: it
survives a restart, a reorder, and an edit. An **ephemeral** handle is a tuple of graph instance,
revision, page, and preorder path: it names one block in one observation and nothing after the next
write.

That split matches the files. On the reference graph of 14137 pages and 112882 blocks, only 1342
blocks state an `id::` — **1.19%**. The other 98.8% have no durable identity, because Logseq itself
only writes `id::` for a block that something references.

The official Logseq plugin API does not have this distinction. Every block object it returns carries
a `uuid`, and `getBlock`, `updateBlock`, `removeBlock`, and `insertBlock` all address blocks by it.
An API that wants Logseq clients to point at this library therefore has to answer a question
ADR-0005 left open: what does a compatibility surface state in that field for a block whose file
holds no `id::`?

Three answers were considered.

**Derive a positional UUID.** Hash the page and path into a v5-shaped UUID. The earlier
implementation of this library did exactly that, and its own compatibility notes record the
consequence: after a reorder, a stale UUID can "resolve to nothing **or to another block**". A
client that holds a UUID across one edit silently edits a block it never named. That is data
corruption, one order of magnitude worse than an error.

**Write `id::` back.** Correct and permanent, but it makes a read request write the user's Markdown,
which ADR-0001 forbids, and a `getPageBlocksTree` would add one property line per block.

**State nothing.** Return null for a block that has no durable id.

A fourth variant was considered and rejected: derive the UUID from the existing `synthetic_id`
(`domain/block.rs:143`), which embeds the revision and therefore fails loudly instead of pointing at
a neighbour. It is safe, but its granularity is the whole graph: every write advances the revision,
so one edit invalidates every handle a client holds for that page. Unusable for an editor.

## Decision

- **The library mints no *durable* identity.** A durable id exists only as a line in Markdown. A
  projection may mint an ephemeral id for a block that states none; it is index-scoped, random, and
  every boundary states it as ephemeral.
- **A boundary states a durable id where the file states one, and the projection's ephemeral id
  where it does not**, in a field that names which kind it is. An ephemeral id is never positional:
  it does not resolve by layout, so a stale one names nothing.
- **A compatibility surface must offer an explicit path for callers to address an ephemeral block**,
  because the alternative is a surface on which 98.8% of blocks are read-only. That path must make
  staleness visible: an ephemeral id whose row no longer exists answers "no such block" (or the
  surface's stale answer), never a neighbouring block. The spelling is the surface's decision, not
  this one's; what this decision fixes is that it is explicit, self-describing, and fail-closed.
- **A durable id is created only by an explicit write.** A tool or a write option may assign one,
  and that is a write: it edits the file, it goes through the write lock, and it is the caller's
  named action. A read never assigns an identity as a side effect.
- Amended by ADR-0011: a versioned batch of operations addresses blocks by positional path within
  one page version. That is the explicit path an editor uses for blocks that state no durable id;
  it is still not an identity, and it does not change what a read states.
- Amended by ADR-0012: the library now mints a random, projection-scoped ephemeral id for a block
  that states none, so the "state nothing" answer above is superseded. The position-derived UUID is
  still rejected, and for the same reason: it is a function of the layout, so a stale one points at
  whatever block now sits there. A random value is not a function of anything, and a stale one names
  nothing.

## Consequences

Positive:

- A caller can always tell which kind of identity it holds, because the two are distinguishable by
  shape and by what a boundary states for each. Nothing has to be guessed from context.
- The failure mode of a stale handle is an error, not an edit to a different block.
- The library keeps ADR-0001 intact: reads do not write, and the file remains the only place a
  durable identity lives.
- A compatibility layer's behaviour is specified before it is written, so the surface cannot quietly
  choose the convenient answer later.

A fourth variant was adopted later: **mint a random, projection-scoped id** (ADR-0012). It is not a
function of the layout, so a stale value names nothing instead of a neighbour, and its granularity is
the page rather than the graph, which is what makes it usable for an editor. The position-derived
variant stays rejected.

Negative:

- A compatibility surface is not a bit-for-bit drop-in: an ephemeral id is not a Logseq UUID, so a
  client that only speaks Logseq's must use the surface's explicit ephemeral field.
- Every surface that adds the ephemeral path re-states a spelling that Logseq does not have, so a
  client written against this library is not portable back to Logseq unchanged.
- An ephemeral id expires with its page's bytes and with the index itself, which is one edit — or one
  rebuild — away. It is a value to use now, not a reference to store.
- Assigning durable ids to a whole graph stays a separate, explicit operation; nothing does it
  automatically, so a caller that wants stable ids everywhere must ask for them.
