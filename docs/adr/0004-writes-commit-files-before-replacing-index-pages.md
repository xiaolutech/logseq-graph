# ADR-0004: Writes commit files before replacing index pages

Status: Accepted (amended 2026-10-31)
Date: 2026-10-29

## Context

A write crosses two systems that cannot share one transaction: Markdown files and SQLite. Ordering
and recovery must ensure that the derived model never describes content that does not yet exist.

## Decision

Before resolving any write target, the handle compares its index's persisted configuration
signature with the source it will use, including a source published by a shared scan on another
index. A mismatch releases the write lock, prepares a full reprojection, reacquires the lock and
re-verifies before committing, then retries the generation check before resolving names.
Reprojection failure refuses the write before any file edit. A matching
generation still permits ordinary writes without a graph scan. Shared sources use `Arc` so
adoption does not deep-clone configuration on every write. Intent recovery uses the same gate:
unknown or changed generations can require reprojection before recovering by recorded paths.

File creation checks the resolved existing parent or nearest existing ancestor against the graph
root before creating missing directories or bytes. Symlinks pointing outside the root are refused,
just as they are for replacement writes; symlinks pointing inside remain usable.
Deletion validates the resolved source parent and recycle directory before any mkdir or rename;
creation cleanup validates its parent too. Replacement temporaries and their cleanup remain in the
validated target directory. Resolving paths is not a sandbox against another process concurrently
replacing directories without respecting the graph lock.

The graph-wide `write.lock` lives in the Git directory when usable, following a `gitdir:` pointer,
and otherwise in the canonical graph root. Its identity never depends on the index path, including
for non-Git graphs and malformed or dangling Git pointers. The lock file is not removed on release.
A symlink at `.git` is not a usable Git directory or pointer, and `write.lock` must be a regular
file, never a symlink. An explicit `gitdir:` pointer can authorize an external Git lock directory;
that exception does not authorize any graph-content mutation outside the root.

Full scans prepare discovery, text parsing and projection facts without the graph lock. A cold
or empty index, changed configuration, `ReadEveryFile`, or more than 64 observed changed/removed
paths builds a shadow SQLite generation beside the logical index without that lock. Other
`TrustStat` scans keep the in-place transaction. A shadow starts from a consistent SQLite copy;
unchanged fingerprints retain ephemeral identities. Catch-up rounds rediscover additions, removals
and size/mtime changes and project all differences without the graph lock. Concurrent-writer
identities, the configuration signature and a revision above the observed live revision are also
reconciled and synced outside that lock. Publication takes the lock only to verify that the live
instance/revision, configuration and complete file observations still match, then select the
completed generation. The final repair allowance is zero files and zero bytes: any difference
releases the lock for another catch-up, never parsing or bulk SQL under it. At most eight catch-up
rounds are attempted, including restarts for configuration or instance changes. Failure to converge
retains the previous generation and returns a retryable scan failure, not a partial publication.
The metadata sweep remains proportional to graph size; slow metadata IO or fsync cannot have a
hard real-time guarantee. The bound removes change-volume-dependent locked work, not OS latency.
The graph instance stays the same. Page byte-version tokens remain valid across publication;
numeric index revisions are freshness hints, not commit preconditions (ADRs 0011/0014).
An atomic, fsynced `<index>.current` sidecar selects a generation-specific SQLite filename;
generations never share WAL/SHM names. Every Graph read (including `page_versions`) checks this
selection, and writes check under the graph lock, reopening before using another generation.
In-flight readers may finish on the old consistent generation. Direct SQLite tools must resolve
the sidecar or use `Index::storage_path()`; remove the entire cache family to discard an index.
Opening discards abandoned shadows, but build locks and shared per-generation leases protect
active builds and readers. Old unleased generations are collected on subsequent opens; the
original logical-path database and its logical-index lease/build files are retained. Shadows use FULL
synchronization and rollback journals; live connections retain WAL/NORMAL. Publication syncs
the database, pointer and directory on Unix. Generation-specific filenames avoid Windows'
rename-over-open-database problem; directory fsync is Unix-only, so Windows power-loss durability
is weaker and has not been tested. `write-intent.json` remains beside the graph lock, unaffected.
Preparation retains graph-sized facts and needs disk for a full copy plus SQLite temporary data.
Bounded path refreshes retain their locked path. Durable selector misses release the lock for a
full shadow scan, then reacquire it and recheck the lookup and metadata before editing.
Ordinary last-scan reads do not take the graph lock.

Index-only work can lock an existing read-only lock file. If the lock
file is absent and permissions prevent creating it, index work can proceed without it because
cooperating writers with the same access cannot acquire the lock either. Contention, invalid lock
files and other lock errors still fail. Ordinary last-scan reads remain outside this lock.

A mutation follows one orchestration path:

1. Acquire the write lock, verify freshness, and resolve the target.
2. Read and hash the latest source bytes; never derive output bytes from SQLite rows.
3. Parse the affected source into a lossless `PageDocument` and apply a pure local edit.
4. Commit a write intent before touching the file. Required for multi-file writes, unnecessary
   for single-file writes: see below.
5. Write through a same-directory temporary file, fsync, atomic rename, and parent-directory fsync.
6. Re-read and re-parse the written file. This fresh parse, not the candidate AST, is the index input.
7. In one SQLite transaction, replace every affected page as a unit and advance the revision once.
   Only after that commit succeeds, remove the journal and fsync its directory.

Untouched source bytes are preserved. A page replacement includes its blocks, properties, refs, and
tags rather than patching individual block rows. Multi-file operations use explicit intents and
partial-failure outcomes; they never claim filesystem-level atomicity.

### Step 4 is required for multi-file writes, unnecessary for single-file writes

For a write that touches one file, the `file_baseline` row already answers the intent's one
question: may the index disagree with the file because a write was interrupted? Step 5 makes the
file change an atomic rename, so the file is wholly old or wholly new and never half-written; and
step 7 records the observation the projection was built from. After a crash between steps 5 and 7
the stat no longer matches the baseline, so the next scan reads the file, finds a different content
fingerprint, and re-projects the page. The reverse crash leaves both sides untouched. Recovery is
already correct, and a marker nobody clears would add a failure mode of its own: a dead writer's
row would be re-read on every scan until some time-based policy guesses it is stale, which is the
kind of guess this design refuses elsewhere.

As soon as one logical write touches more than one file, the file state alone cannot say what the
write was trying to reach. A crash between the two writes leaves two files whose combination no
baseline describes, with nothing to distinguish the intended result from outside interference. Such
a write therefore commits a scoped intent before the first file is replaced — recording both
files, both of their pre-write and intended post-write content fingerprints, and what the write was
moving — and durably clears the intent only after the transaction replacing both index pages. The intent
is scoped to that one operation, not to every write: a single-file write keeps answering the
question from its baseline alone.

### Multi-file recovery targets

The intent is a versioned `write-intent.json` file beside `write.lock`, not derived SQLite state.
Its same-directory temporary file is fsynced, atomically renamed and directory-synced before page
bytes change. The entry must be a regular file, never a symlink; the lock directory is the authority
boundary, including explicit external Git worktree directories. A different pending intent cannot
be overwritten. Clear is remove followed by directory fsync. Failed cleanup after an index commit
reports a file-changed error; a surviving post/post journal is safe to recover again.

Every open checks for a journal, irrespective of index path or schema. An open without an
acquirable lock refuses any pending journal instead of attempting recovery.
Before opening or discarding a legacy index, the engine exports its `write_intent` row under that
lock, confirms the durable journal, and only then durably clears the legacy row. Conflicting
journal/row pairs or unreadable rows refuse the open. Schema v15 drops the old table; a direct
index open refuses to discard an index with a pending legacy row and requires engine migration.
Clearing derived rows can never clear the graph journal.

A multi-file write with a committed intent targets the following decisions, and every multi-file
operation must hold to them:

- **The destination is written first.** A fixed file order makes every crash state enumerable: a
  state the write's own order cannot produce is outside interference, not an ambiguous crash, and
  recovery can name it as such instead of guessing between two readings.
- **No rollback.** Bytes that landed on disk are never written back over, and a failed write never
  restores original bytes as its recovery. Undoing a write to make the two sides agree would
  discard content the caller asked to keep.
- **Recovery is decided by content fingerprints, not by the index.** Recovery compares each file's
  current fingerprint against the pre-write and post-write fingerprints the intent recorded, and
  commits both pages as they actually are — completing the move when the files say it was reached,
  and projecting both files as they are when they describe neither or both states. When an intent
  names a file that is gone or unreadable, the open fails and the intent stays: recovering from a
  graph this broken is a person's decision, not a heuristic.

## Consequences

A file can become visible before its index transaction commits. During that gap the index is stale,
and the next scan must recover by comparing the file against its baseline rather than by trusting
either side. Successful source writes are not rolled back merely because indexing fails.
Whole-page replacement favors correctness and predictable O(page) indexing over fine-grained SQL
updates.
