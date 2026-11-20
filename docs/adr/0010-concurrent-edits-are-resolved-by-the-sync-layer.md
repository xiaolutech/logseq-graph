# ADR-0010: Concurrent edits are resolved by the sync layer, not the library

Status: Accepted
Date: 2026-11-01

## Context

This library reads and writes one local directory. Every write takes a file lock inside the graph
root and replaces one page file inside one SQLite transaction (ADR-0004). That serialises writers
**inside one process on one machine**, and nothing else. Two machines pointed at the same synced
folder are two independent writers, and the library has no concept of a concurrent edit: a scan
compares size and modification time to decide whether a file changed, not whether two versions
diverged.

That is fine while the graph lives on one machine, and it stops being fine the moment the folder
syncs. Local-first software makes the second machine the normal case, so the gap has to be answered
rather than discovered.

Two devices editing one page produce two complete versions of one file, because a write replaces the
page as a whole. Merging them needs an answer to "which block did this change", and that needs block
identity. On the reference graph of 14138 pages and 112890 blocks, 1342 blocks state an `id::` —
**1.19%**. ADR-0009 forbids the library from minting identity as a side effect, so the other 98.8%
are indistinguishable across two versions of a page.

Three answers were considered.

**Accept the conflict.** The sync layer detects that two versions diverged and preserves both, as a
conflict copy beside the original. A person merges them. This is what Logseq itself does, and what
Syncthing, iCloud Drive, and Git all do.

**Durable ids on every block, then a three-way merge.** Give every block an `id::` once, and a merge
tool can align two versions by identity and merge per block. The cost is measurable: about 112890
blocks gaining one `id:: <36 characters>` line each, roughly 4.5 MB on a 16.1 MB graph, a 28%
increase in the files a user reads and versions. It also needs a merge component that does not exist
here, and it still cannot merge two edits of one block's text without a person.

**CRDT.** Let a replicated document be the source of truth and render Markdown from it. This is the
only option that merges without a person. It also demotes the Markdown file from the product's fact
(ADR-0001) to an export, which is a different product with a different failure mode: a bug in the
document model now loses data that used to be a file on disk.

## Decision

- **The library does not synchronise.** It reads and writes one local directory. Moving bytes between
  machines is the deployment's problem, and every sync tool already solves it.
- **The library does not detect or resolve conflicts.** A scan decides whether a file changed since
  it was projected, not whether two writers disagreed. There is no "conflicted" state, no merge
  function, and no conflict table.
- **A conflict copy is an ordinary page.** Whatever a sync tool names it — `page.sync-conflict-…md`,
  `page 2.md`, a Git conflict marker inside the file — the library reads it as page content and lists
  it like any other page. It does not pattern-match sync-tool naming conventions, because that
  convention is not this repository's to own and would rot.
- **Block identity stays explicit.** No bulk migration to `id::` on every block. A caller that needs
  durable identity asks for it (ADR-0005, ADR-0009); the library never assigns one to make a future
  merge easier.
- **A CRDT document model is rejected**, because it makes Markdown derived rather than authoritative
  and contradicts ADR-0001.

## Consequences

- **Concurrent edits cost a manual merge, and never silent data loss.** A conflict copy is a complete
  Markdown file; any editor, any diff tool, and `git diff` can read it. The person merging sees both
  versions in full, which is the property that makes this safe.
- **Conflict copies appear in `pages()`.** A page named `note.sync-conflict-20240101-120000-ABCDEF`
  is listed, searchable, and deletable like any other. That is the price of not owning a naming
  convention: the library cannot hide what it does not recognise. `delete_page` moves such a copy
  into `logseq/.recycle/` once it has been merged, so the cleanup path already exists.
- **A single write is still atomic.** Nothing here weakens ADR-0004: one page, one replacement, one
  transaction. What is not atomic is the pair of writes two machines made independently, and that is
  a property of the sync layer.
- **Nothing changes in the code today.** The twelve writes, the derived index, and the pure-Markdown
  storage already satisfy this decision. It exists so the boundary is stated before a second machine
  makes it urgent.
- **Revisit when** a sync layer cannot produce a readable conflict copy (for example, a tool that
  silently keeps one side), or when manual merging becomes frequent enough to be a task in its own
  right. At that point the cheapest move is durable ids plus a three-way merge — not CRDT — because
  it keeps ADR-0001 intact at a cost of roughly 28% more bytes on disk.
