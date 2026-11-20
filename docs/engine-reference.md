# Engine and adapter reference

Detailed parsing, identity, write, RPC, and verification contracts. Start with the
[project overview](../README.md) for setup and a runnable example.

The workspace consists of the embeddable `crates/graph` library and the CLI/RPC adapter in
`crates/server`. Markdown remains authoritative and SQLite is a disposable read model, not a
second content store. There is no legacy in-memory read path or migration layer.

## Concurrent callers

`Graph` remains an exclusive handle for single-threaded callers. Wrap it with
`SharedGraph::new(graph)` for shared access: `read(&self, |reader| ...)` leases one of four
read-only SQLite connections; `with_readers` accepts a different nonzero bound. Inside a read
callback, `reader.default_reads()` uses the configured default and `reader.reads(policy)` selects
an explicit policy. Both return the ordinary `Reads` surface and revision/observation metadata.
`page_snapshot`, `page_versions`, and storage diagnostics are also available on the restricted
handle. The callback cannot edit graph files. `write(&self, |graph| ...)` serializes access to the
original writable handle without changing its existing cross-process locks or commit contracts.
Callbacks must not recursively acquire the same pool or writer: the bounded pool and writer mutex
are not reentrant. A lease returns on errors and unwinding; a panic inside a writer poisons it.

The RPC adapter clones the shared facade under the startup-state mutex and releases that mutex
before acquiring a read lease or writer. Independent requests therefore execute queries in
parallel, not just on parallel HTTP workers waiting for one engine mutex. A full pool applies
backpressure by waiting for a lease. No additional worker threads or dependencies are introduced.

All connections register `search_key`, `search_cjk_tokens`, and `query_property_matches`.
Indexed policy reads pin their
metadata and result in one SQLite read transaction. A brief scan-operation guard coordinates
snapshot acquisition with publication and is released before executing the query. Ordinary reads
never hold the writer mutex; strict `CheckNow`/`VerifyContent` policies first scan through the
serialized writable handle, then query on the leased connection. File-tier reads retain their
existing source-byte contracts (`page_versions` still runs no freshness scan).

Every read checks the atomic generation selection. Reopening preserves read-only mode and repeats
function registration. Per-generation leases keep old files alive while in-flight snapshots finish;
later reads adopt the newly published generation. Pooled adoption does not clear a later shared
scan failure, so `LastScan` remains fail-closed and `AllowStale` reports staleness across all slots.
An in-memory index has no independent file-backed readers and falls back to the serialized original
connection. Long callbacks can retain old generations/WAL pages and delay reclamation (ADR-0017).

## Parsing and editing contracts

Projection follows the pinned official parser for bare `-` continuations, boundary blank lines,
and unclosed code fences; [the synthetic probe](corpus-parity.md#continuation-and-fence-probe)
records the exact rules, including whitespace-only lines. These affect block text and structure,
not source preservation: rendering an unedited document still reproduces every byte.
Edits that must retain a leading empty content line keep a separator after the dash; a trailing
empty content line at a block boundary is rendered as whitespace-only so it survives projection.

The library and its command line are usable today. The `document` layer parses and edits Markdown without rewriting a byte it did not touch:
besides appending, rewriting, removing, inserting, and moving blocks, it splits, merges, unwraps,
indents, and outdents them, rewrites the marker bytes that open a block, and replaces the text an
editor shows while leaving every property line where it sits. `source` covers Logseq configuration
with Markdown page and journal discovery. The SQLite `index` is a disposable projection kept current
by incremental scans, and every read states the freshness policy it answered under. The read surface
covers pages, page trees, page properties, block properties, backlinks, tags, siblings, namespace
pages, content search, **name lookups** (`find <name>` on the CLI, the `pages` field of
`logseq.App.search`), block reads by durable id or ephemeral id (ADR-0012), and **Logseq simple
queries** — `(task TODO DOING)`, `(priority a b)`, `[[page]]`, `#tag`, `(property k v)`, `(page-property k v)`,
`(page-tags …)`, `(namespace x)`, `(page x)`, `(journal)`, `(between -7d today)`, quoted text, and
their `and`/`or`/`not` combinations, answered by SQLite over the projection (ADR-0013). A query answer
states a `BlockSelector` per block, and so does every search hit, so either can be written through
directly: the CLI prints that selector in the spelling `block`, `update`, `remove`, and `move` read
back (`durable:<uuid>` or `ephemeral:<uuid>`, with a bare uuid still meaning durable), and
`logseq.Graph.query` and `logseq.App.search` state the same identity as JSON. A query that does not parse, or an advanced Datalog query, is a typed error rather
than an empty answer.

### Simple-query semantics

The file-graph reference is Logseq 0.10.15, commit
[`03bcefbdf87458f87499abb988ac3b9fac2c07d8`](https://github.com/logseq/logseq/blob/03bcefbdf87458f87499abb988ac3b9fac2c07d8/src/main/frontend/db/query_dsl.cljs):
`build-task` and `build-priority` uppercase operands; `build-between-two-arg` sorts its
endpoints and `->journal-day-int` resolves relative days/weeks/calendar months/years and
today/yesterday/tomorrow. `build-property-two-arg`, `build-page-property`, and
`build-and-or-not` define property operands and boolean nesting. The associated
[`query-dsl-rules`](https://github.com/logseq/logseq/blob/03bcefbdf87458f87499abb988ac3b9fac2c07d8/deps/db/src/logseq/db/rules.cljc)
restrict `between` to blocks on journal pages with an inclusive day range and use equality
or collection membership for properties, not substring matching. Page-reference values
use page-name normalization, including a fallback for unbracketed values such as `Book`.
The raw-text projection does not retain Logseq's parsed property value kind: this fallback
also folds quoted/config-excluded plain strings, a compatibility limitation rather than
an assertion that Logseq compares those strings case-insensitively. Upstream
[`text.cljs`, `parse-property`](https://github.com/logseq/logseq/blob/03bcefbdf87458f87499abb988ac3b9fac2c07d8/deps/graph-parser/src/logseq/graph_parser/text.cljs#L165-L187)
preserves excluded and quoted values as strings; equality in the rules is case-sensitive
for those strings. `(not a b)` means
`not (and a b)`, not `not (or a b)`. SQL subqueries preserve every nested set boundary.
Only page predicates (including their boolean combinations) return pages; any block
predicate makes the whole expression return blocks, lifting page filters to their blocks.

`QueryContext` supplies today; no executor clock is read. Journal-day integers `YYYYMMDD`
are also accepted and validated without requiring an endpoint page. This is an explicit
extension: the pinned upstream helper does not correctly handle bare journal-day integers.
Named `[[journal pages]]` still resolve through existing indexed pages rather than the
upstream configured title parser. Invalid dates are typed errors. Existing ADR-0013 limits
remain: no synthetic pre-blocks, custom comma-separated property configuration, or advanced
Datalog. Built-in list keys split commas; references and exact scalars match other keys.
Property lookup and value matching execute in SQLite (a deterministic scalar function
interprets stored property text), with no Rust collection of candidate owner IDs and no
schema change. Task, priority, and journal dates already have indexed columns.

`query_read.rs` pins mixed-case task/priority, journal ranges, and nested property sets.
The captured `fixtures/expected/db_q.json` only covers `(page-property :type [[ADR]])`;
no official running instance was available to capture additional answers, so these new
cases cite the pinned implementation and its `src/test/frontend/db/query_dsl_test.cljs`
instead of relabelling locally generated answers as official captures.

Four reads go further:
`page_snapshot` answers a whole page in one read — its structure, every block's editable text, its
properties, and a page-level version token; `block_snapshot` answers one block as the root of a read
— its subtree plus the chain of blocks above it — located by durable id or by ephemeral id;
`page_versions` answers that version token twice, as the bytes the index projected and as the bytes
on disk now, so a caller can tell whether the index is behind one page without asking for a scan; and
`resolve_block_ref` answers what a `((block ref))` names: `Missing`, `Ambiguous { pages }`, or
`Found`. Sixteen writes exist —
`create_page`, `create_journal`, `append_block`, `insert_block`, `insert_batch`,
`prepend_block_in_page`, `update_block`, `remove_block`, `move_block`, `upsert_block_property`,
`remove_block_property`, `set_block_collapsed`, `delete_page`, `ensure_block_id`, `remove_block_range`, and `commit_page` — and
`move_block` works within a page and across pages, under a write intent that makes an interrupted
cross-page move recoverable independently of its index: `write-intent.json` lives beside the graph
write lock, is atomically fsynced before page writes, and is removed and directory-synced only after
the index commit. A fresh, deleted or different index uses the same journal. Legacy SQLite intents
are exported durably before schema rebuild; a conflicting or symlinked journal fails closed.
`update_block`, `remove_block`, `remove_block_range`, and `move_block` take either identity,
and every read answers the selector of the block it reports, so the 98.8% of blocks on the reference
graph that state no `id::` are editable through the id a read handed out, with no read in between.
That id is minted by the projection, lives only in the index, and names one block while its row
exists and the page's bytes are the ones that row was projected from; a write whose file no longer
matches refuses as a stale page instead of editing a neighbour (ADR-0012). `insert_block`, `insert_batch`, and `prepend_block_in_page` answer the block or blocks they created
instead of the page, so a caller can keep editing what it just inserted without another read. The two property writes set
or remove one `key:: value` line of a block, and treat `id::` as an ordinary key: assigning one is
ADR-0009's explicit way to give a block durable identity. `ensure_block_id(&BlockSelector)` is the
named grant: it returns `Read<EnsuredBlockId>` with `id`, `created`, and a durable `block` view.
An existing valid `id::` is returned without a file replacement or revision advance; otherwise the
write uses the same lock, ephemeral fingerprint gate, property insertion and commit path as upsert.
The CLI spells it `ensure-block-id <selector>`. New ids match Logseq's file-graph UUIDs:
[Logseq 0.10.9 `db/new-block-id`](https://github.com/logseq/logseq/blob/0.10.9/src/main/frontend/db.cljs#L198-L200)
calls `d/squuid`; [DataScript 1.5.3 `util/squuid`](https://github.com/tonsky/datascript/blob/1.5.3/src/datascript/util.cljc#L25-L48)
uses Unix seconds in the first eight hex digits, with the remaining random v4 UUID bits, version 4
and RFC variant, rendered lowercase and hyphenated. Logseq's
[`copy-block-ref!` / `set-blocks-id!`](https://github.com/logseq/logseq/blob/0.10.9/src/main/frontend/handler/editor.cljs#L954-L972)
persists that UUID as `id::`. Its
[`batch-set-block-property!`](https://github.com/logseq/logseq/blob/0.10.9/src/main/frontend/handler/editor/property.cljs#L97-L105)
keeps existing property order and appends new keys; this write likewise puts `id::` after existing
properties, or immediately after the marker line if there are none.
`delete_page` moves the file into
`logseq/.recycle/` instead of unlinking it, so a mistaken deletion is recoverable by hand. `insert_batch` lands a pasted run of blocks in one
page replacement rather than one per block. `commit_page` is the versioned batch: one page, one
`baseVersion`, and an ordered list of semantic operations land as one file replacement and one
whole-page projection, and a base version the page's bytes no longer match answers a typed stale
error having written not one byte. `create_journal` creates the journal of a date, naming it through
the two journal formats the configuration holds. The CLI exposes `journal` for `create_journal` and
`commit` for `commit_page`; the latter takes a page, its base version, and a JSON array of snake_case
operations. The `serve` command answers graph reads and
writes in the official request shape over `POST /api`: one `{method, args}` call with Logseq's official method shape,
`logseq.<Namespace>.<method>`, with a per-request freshness policy (ADR-0008). Names resolve the
official way: a `logseq.`-prefixed name by the snake form of its last dot segment
(`logseq.Editor.get_page` and `logseq.getPage` are both `get_page`), a bare name as written
(`get_block` resolves, `getPage` does not). `logseq.Graph.*` writes keep their exact names. The two file-tier reads,
`logseq.Graph.getPageVersion` and `logseq.Graph.getPageSnapshot`, answer from the file and therefore
declare no policy. Methods in the `logseq.Graph.*` namespace are this repository's own reads and writes and are
not part of the official API contract; the `selector`, `freshness`, `truncated`, `limit` and `today`
fields of the `Editor`/`App` methods that carry them are own semantics too. A filesystem watcher
coalesces external edits for 50 ms (at most 500 ms), then refreshes the affected paths using a separate
graph handle. Configuration changes, directory events, lost events and refresh failures trigger full
scans. Watcher setup failure falls back to polling; `serve --no-watch` forces that mode for unreliable
mounts. `--poll-interval <ms>` sets the polling and failed-scan retry period (default 1000 ms).
A failed scan keeps default reads fail-closed until a later full scan succeeds. Path refreshes do not
advance `last_scan` or clear divergence. A caller can still request `freshness: check` or `verify` — or use
`getPageVersion`, which reports the file's current
version beside the indexed one without scanning, and `getPageSnapshot`, which answers that page's
current content in the shape `getPageBlocksTree` uses. Under ADR-0015, `getPage` and
`getAllPages` include no-file pages named by tags, references, aliases and namespace
parents. Those have `has_file: false`, no blocks, and no file version. Creating a file
for one makes it file-backed. A leading `title::` or front-matter title overrides the
filename without making that filename an alias. Page properties are from the pre-block;
properties inside the first dash block remain block properties. Linked references merge
references to the requested page and its aliases. A link from page properties has
`pre_block: true` and no usable content-block position.

The library is a from-scratch implementation with no shared runtime dependency or wholesale
module ports from its predecessor; fixtures and differential tests establish behavioral compatibility.

## RPC surface

`logseq-graph serve` requires a bearer token, exactly as the official server does: every request
must carry `Authorization: Bearer <token>` (scheme case-insensitive, one or more spaces or tabs),
and the server accepts every `--api-token` plus `LOGSEQ_API_TOKEN`. Without a token `serve` refuses
to start; a request that presents none, or a value the official rule would not match, answers HTTP
401 with the official `{statusCode: 401, error: "Unauthorized", message: "Access Denied!"}` body
(plus this surface's `code -32001` / `data.kind "unauthorized"`) and never echoes the token (ADR-0018).

The surface answers the official CORS policy too (`@fastify/cors`, `origin "*"`): an `OPTIONS`
preflight gets `204` with `Access-Control-Allow-Origin: *` and the methods/headers it asks for,
before the token check (a preflight carries no token), and every answer carries the origin header —
without it a browser client such as the Logseq Copilot extension cannot call the API at all.

The argument shapes an official client sends are accepted too: a page identity object
(`{name: ...}`, `{uuid: ...}`, or `{id: <number>}`) in place of a name string, and a `null` where a
browser omits an optional argument (`JSON.stringify([uuid, undefined])` writes `[uuid, null]`). A
numeric entity id resolves the projection-derived page id this surface states: `id` is a stable
hash of the folded name, stated as `block.page.id`, because a file graph has no DB entity id
(ADR-0001). A block's `page` — in `Editor.getBlock` and in `DB.q` block entries — is a
`{name, uuid, id}` object rather than a name string, because the official client hands `block.page`
straight back to `Editor.getPage`, and its recall path drops a block unless `block.page.id` is a
number.

[API parity](logseq-api-parity.md) classifies every method against the official Logseq API and names
the test that pins it; `fixtures/expected/` holds the official answers those rows compare
against. See [Contributing](../CONTRIBUTING.md) before changing either one.

`logseq-graph serve` answers `POST /api` with one `{method, args}` call. `args` is an array of
positional strings for reads; an optional trailing object carries the read's own options (`freshness`,
`limit`, `today`). That trailing object is **our** shape, not an official argument shape: the
official methods in [API parity](logseq-api-parity.md) take positional arguments only. Keeping a read's
policy beside its arguments makes one call self-contained and keeps ADR-0008's one-policy-per-
request rule visible in the request itself; no freshness member is added to the envelope. Each
call declares at most one freshness policy, and `getPageVersion` / `getPageSnapshot` declare
none. `logseq.App.search` states the official file-edition shape — `blocks` entries under
`block/uuid`, `block/content`, `block/page`, `pages` as names, and empty `pages-content` and
`files` — so an official client can map a hit and read it back through `Editor.getBlock`. This
surface's own richer `hits` (a selector per hit) and `page_candidates` sit beside the official
fields: file pages whose own name or an `alias::` value matched, page-name hits first, each
stating `via` and, for an alias hit, the alias. A shared alias answers one candidate per page;
the caller picks the jump. Block `hits`
are literal substrings under `domain::search_key`: queries of at least three characters use a
folded, case-sensitive trigram FTS5 index. One- and two-character queries consisting entirely of
CJK under ADR-0016's Han/kana/Hangul and ideographic-mark predicate use a second FTS5 index
of folded unigram/adjacent-bigram tokens. Non-CJK characters break adjacency, so `中x文`
and `中，文` do not answer `中文`. Both indexes probe at most 101 matches: at most
100 matches get full BM25 ranking; larger hit sets return in ascending FTS rowid order unranked,
not best-of-first-100 ranking. The threshold does not cap the caller's result count. Other short
queries, including mixed `a中`, retain the bounded scan and unspecified order (ADR-0016). Empty queries
and zero limits return no hits. The snippet, selector, limit, and answer shape are unchanged; no
rank field is exposed. `Editor.getBlock` answers the official top-level fields (`uuid`, `content`,
`page`, `marker`, `format`) beside this surface's own block view, and accepts the official
`{includeChildren}` argument; `Editor.getPage` likewise states `name`, `originalName`, `uuid`,
`journal?` and `id` at the top level. The wire shape is the official server's: a successful call
answers its value alone (the body *is* the result a browser client parses), and a failed call
answers Fastify's `{statusCode, error, message}` beside the transport status, plus this surface's
own `code` and structured `data` as extra members (`data.kind` names the class; 400 to `-32602`,
401 to `-32001`, 404 to `-32004`, 409 to `-32009`, 500 to `-32603`, 503 to `-32003`; protocol
failures use `-32700`, `-32600` and `-32601`).

HTTP requests must carry a `Host` naming `localhost` or a literal loopback IP address at the
actual bound port (including the assigned port when binding port 0); other hosts receive HTTP 403
before dispatch. A head carrying more than one `Origin` is also refused with 403, but the `Origin`
value itself is not checked: the bearer-token gate of ADR-0018, not the page's origin, is what
authenticates a caller — that is what lets an extension page call from a `chrome-extension://`
origin. `POST /api` requires `Content-Type: application/json` (parameters such as
`charset=utf-8` are allowed), otherwise HTTP 415.
Request bodies are limited to 16 MiB (HTTP 413), checked both against the declared length and
while reading decoded bodies. Sixteen request workers handle complete requests; up to four pooled
read callbacks execute concurrently by default, while writes and strict freshness scans serialize.
Bounded connection slots isolate socket I/O, with a 5-second idle timeout and 15-second
read and write deadlines.

### Projection generations and locking

After a successful background scan reloads configuration, writes adopt that scan's configuration
before resolving files; a journal renamed by its title format is writable under the new name.
Each index persists the configuration signature of its completed projection. Opening with a
different or missing signature fully reprojects the graph, even with startup scanning disabled.
Before resolving write targets, a handle checks its own index against the source it will use;
a mismatch prepares and builds a shadow unlocked, then verifies and publishes under the lock
before any page bytes change. A failed reprojection
refuses the write and is retried, not marked as a completed generation. Shared scan sources use
`Arc`, avoiding a deep configuration clone per write. Schema v18 and parser version 7 rebuild
incompatible indexes.
File creation, like replacement, refuses resolved parent directories outside the graph root,
including symlinked `pages` directories; missing directories inside the root are still created.
Deletion validates both the page parent and the recycle directory before creating directories or
moving the page: an outside-root `logseq` or `logseq/.recycle` symlink is refused without changing
the page. Creation cleanup uses the same resolved-parent containment check. Temporary files stay
beside the validated replacement target. These checks do not sandbox concurrent filesystem changes
by programs that do not respect the graph lock.
Writers share one `write.lock` regardless of index location: in the Git directory (following a
valid `gitdir:` pointer), or in the canonical graph root when no usable Git directory exists.
A symlink at `.git` falls back to the graph root, and `write.lock` itself must not be a symlink.
An explicit Git worktree `gitdir:` pointer remains the supported exception for an external lock
directory; page writes and recycle operations never use that exception.
Full scans prepare discovery, text parsing and projection facts without the graph lock. A cold
or empty index, changed configuration, `ReadEveryFile`, or more than 64 observed changed/removed
paths builds a shadow SQLite generation beside the logical index without that lock. Other
`TrustStat` scans keep the in-place transaction. A shadow starts from a consistent SQLite copy;
unchanged fingerprints retain ephemeral identities. After building, up to eight catch-up rounds
rediscover files and apply additions, removals and metadata changes to the shadow without the
graph lock. Identity reconciliation, revision-floor updates, the signature and database sync also
run unlocked. Each round then acquires the lock and checks the live instance/revision,
configuration and full file observations. The final delta allowance is **zero files / zero bytes**:
any change releases the lock for another round; no file parsing or bulk repair SQL runs inside it.
The revision check prevents a concurrent index commit from invalidating unlocked identity or
revision finalization even when file observations appear unchanged. Exhaustion returns
`EngineError::StaleIndex` with a retry reason and keeps the previously published generation;
default reads remain fail-closed until a later successful scan. Configuration changes can restart
preparation but share the same eight-round allowance. Metadata verification still scales with
total files, and OS metadata/fsync delays prevent a universal wall-clock guarantee.
The graph instance stays the same. Page byte-version tokens remain valid across publication;
numeric index revisions are freshness hints, not commit preconditions (ADRs 0011/0014).
`VerifyContent` now publishes a higher revision even when the bytes are unchanged.
An atomic, fsynced `<index>.current` sidecar selects a generation-specific SQLite filename;
generations never share WAL/SHM names. Every Graph read (including `page_versions`) checks this
selection, and writes check under the graph lock, reopening before using another generation.
In-flight readers may finish on the old consistent generation. Direct SQLite tools must resolve
the sidecar or use `Index::storage_path()`. Deleting the logical-path database invalidates the
selection on the next open; remove the entire cache family to reclaim all cache space.
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

A read-only non-Git graph can be opened and
rescanned with an index outside the graph: scans still lock an existing `write.lock` when it can be
opened read-only, and may proceed without a lock only when permissions prevent creating an absent
lock. Writes still need the write lock, and pending move recovery still needs exclusive access.

| Method | Official counterpart | Note |
| --- | --- | --- |
| `logseq.Editor.getCurrentGraph` | `logseq.Editor.getCurrentGraph` | page and block counts, index facts, startup-scan progress (ADR-0008), and the graph root's `name`, `path` and `url` (`logseq_local_<path>`) — the three fields the official method answers |
| `logseq.Editor.getAllPages` | `logseq.Editor.getAllPages` | file and derived no-file pages, distinguished by `has_file` |
| `logseq.Graph.getFavorites` | `logseq.App.getUserConfigs` | the `:favorites` list with an `exists` flag per entry; own method |
| `logseq.Editor.getPage` | `logseq.Editor.getPage` | file or no-file page metadata, properties, revision, and the official top-level `name` (folded) / `originalName` (as written) / `uuid` / `journal?` / `id`; accepts a `{name}` / `{uuid}` / `{id}` object or a bare number |
| `logseq.Editor.getPageBlocksTree` | `logseq.Editor.getPageBlocksTree` | flat preorder with `depth`, `marker`, and `selector` |
| `logseq.Editor.getPageLinkedReferences` | `logseq.Editor.getPageLinkedReferences` | one representative per referencing page, with alias references merged |
| `logseq.Graph.getAllTags` | `logseq.Editor.getAllPages` (tag pages) | tags with a page count; own method |
| `logseq.Graph.getPagesWithTag` | `logseq.DB.q(page-tags)` | file page names; own method |
| `logseq.App.search` | `logseq.App.search` | the official `blocks` (capped at 10) / `pages` / `pages-content` / `files` / `has-more?` shape: `block/uuid` is the bare id a client hands back to `Editor.getBlock`, `block/content` the snippet, `block/page` the page name; plus this surface's own `hits` with a `selector` per hit, `page_candidates`, `limit` and `query` |
| `logseq.App.checkCurrentIsDBGraph` | `logseq.App.checkCurrentIsDBGraph` | the official client's edition probe; answers `false` because this projection is file-first (ADR-0001), with no graph read and no policy — the live official server refuses the probe `MethodNotExist`, so `false` is a deliberate-difference, not a captured answer |
| `logseq.App.getAppInfo` | `logseq.App.getAppInfo` | `{version: <crate version>, supportDb: false}`: the shape the official client reads for its connection message; no graph read and no policy |
| `logseq.App.getUserConfigs` | `logseq.App.getUserConfigs` | the facts the graph files state — `currentGraph`, `me`, `preferredFormat` and `preferredDateFormat` (the graph's own `:journal/page-title-format`); the app-state keys are not stated (ADR-0001) |
| `logseq.Editor.getCurrentPage` | `logseq.Editor.getCurrentPage` | `null`: there is no editor (ADR-0003), and `null` is the official "no current page" answer |
| `logseq.showMsg` | `logseq.showMsg` | accepted and dropped: there is no GUI (ADR-0003), and the answer is the fixed message-id string `notification` |
| `logseq.Graph.getPageVersion` | none | index and disk version tokens; declares no policy; own method |
| `logseq.Graph.getPageSnapshot` | `logseq.Editor.getPageBlocksTree` | the page file as it is now; declares no policy; own method |
| `logseq.Graph.query` | `logseq.DB.q` | the simple-query grammar with `truncated`, `today`, and `selector`; own method |
| `logseq.DB.q` | `logseq.DB.q` | the official spelling of the query read: answers the official bare array; a block entry states `uuid` / `content` / `page`, a page entry states `uuid` / `name` (folded) / `originalName` / `journal?` / `journalDay` as the official number; shares the grammar with `logseq.Graph.query` |
| `logseq.Editor.getBlock` | `logseq.Editor.getBlock` | a block by a bare `<uuid>` (read as durable, then ephemeral), `durable:<uuid>`, or `ephemeral:<uuid>`, with the official top-level `uuid` / `content` / `page` / `marker` / `format`; the official `{includeChildren}` argument is accepted and answered with the block alone |

### Served-write contract

ADR-0014 fixes the contract for serving all sixteen library writes. The adapter translates
arguments, results and errors; the library remains the only implementation of edits, locks,
version checks, write intents and recovery. These methods use the `logseq.Graph.*` namespace and
are **this repository's own API, not official Logseq API parity**, even when their operation
resembles an official write.

A write takes no `freshness` option and, with a matching configuration signature, starts no
graph-wide scan. A positional page commit uses
the page snapshot's opaque `version` as its `baseVersion`; operations run in array order against
the preceding operation's result. A stale version or refused operation rejects the whole batch
before source bytes change, and a stale error carries the current page snapshot for recomputing
paths. The result contains the committed snapshot, `applied` count and ordinary read metadata.

Errors remain structured (the official Fastify body plus this surface's `code` and `data`). In
particular, a refusal before the file commit is distinct
from `IndexAfterWrite`, `ReReadAfterWrite`, `PartialMove` and a source `Committed` durability
failure, which are reported as `file_changed` because source bytes may already have landed: the
adapter neither rolls bytes back nor reports such a failure as success. Cross-page
moves accept either identity at both ends; the intent records durable ids or child-index paths
bound to the corresponding pre hashes, never ephemeral ids. Startup scanning and open
failure refuse writes just as they refuse ordinary reads; the server does not queue or replay
them.

| Own RPC method | `args` | Result value |
| --- | --- | --- |
| `logseq.Graph.createPage` | `[name]` | `page` metadata |
| `logseq.Graph.createJournal` | `[date]`, `YYYY-MM-DD` | `page` metadata |
| `logseq.Graph.appendBlock` | `[page, text]` | `page` metadata |
| `logseq.Graph.insertBlock` | `[target, text]` | inserted `selector` |
| `logseq.Graph.insertBatch` | `[target, texts]` | `selectors` in input order |
| `logseq.Graph.prependBlockInPage` | `[page, text]` | inserted `selector` |
| `logseq.Graph.updateBlock` | `[selector, text]` | `block` view |
| `logseq.Graph.removeBlock` | `[selector]` | `page` metadata |
| `logseq.Graph.removeBlockRange` | `[first, last]` | `page` metadata; delete an inclusive run of siblings and their subtrees in one revision; either identity at either end |
| `logseq.Graph.moveBlock` | `[selector, target]` or `[selector, direction, anchor]` | `page` metadata |
| `logseq.Graph.upsertBlockProperty` | `[selector, key, value]` | `block` view |
| `logseq.Graph.ensureBlockId` | `[selector]` | bare UUID `id`, boolean `created`, durable `block` view |
| `logseq.Graph.removeBlockProperty` | `[selector, key]` | `block` view |
| `logseq.Graph.setBlockCollapsed` | `[selector, collapsed]` | `block` view |
| `logseq.Graph.deletePage` | `[name]` | deleted `page` metadata |
| `logseq.Graph.commitPage` | `[page, baseVersion, operations]` | `snapshot`, `applied` |

`target` is `{"before": selector}`, `{"after": selector}`, `{"firstChildOf": selector}` or
`{"lastChildOf": selector}`. The three-argument move spelling is `direction` =
`before`/`after`/`first_child_of`/`last_child_of` followed by the anchor selector.
`commitPage` operations use the same snake_case grammar as the CLI's `commit` command.
[API parity](logseq-api-parity.md) states the complete write contract and its error classification.

### Measuring, and keeping the measurement honest

[Performance](../PERFORMANCE.md) records what each change did to the numbers. Two things keep it from going stale:

`scripts/bench.sh <graph-root>` reproduces the tables. It copies the graph to a temporary directory,
runs `crates/graph/examples/measure.rs` against the copy (the engine tier), then starts the built
`logseq-graph serve` on the same copy and runs `crates/server/examples/measure_serve.rs` against the
child over raw HTTP (the served tier), prints Markdown rows, and removes the copy on exit, so the
named graph is never written to.

The `perf-doc` hook refuses a commit that stages a file under a measured hot path without staging
[Performance](../PERFORMANCE.md) too. The hot-path list lives in `scripts/check_perf_doc.py`; adding a path there is
a decision, because every later commit touching it then owes a row — including a row that says
`not measured`. `prek.toml` is the only hook configuration and `mise.toml` pins every tool the gates
need (rust, prek, typos, python, just, cargo-deny). Enable the git hook shims once per clone:

```sh
mise install
prek install
```

On a fresh clone with no `core.hooksPath`, `prek install` writes the shims into `.git/hooks/`, which
git does not track. If `core.hooksPath` is set, prek honours it and writes the shims into that
directory instead. Clones that followed the old README and ran `git config core.hooksPath .githooks`
must unset that setting before installing — the repository no longer ships `.githooks/`, so leaving the
setting in place leaves the clone without a working hook:

```sh
git config --unset core.hooksPath
prek install
```

The shim records the prek binary path used at install time and falls back to `prek` on `PATH`; after
upgrading prek or mise, re-run `prek install --force` to refresh it. `prek run --all-files` runs every
pre-commit hook by hand, and `prek run --all-files --stage pre-push` runs the pre-push set. The
supply-chain gate is not in that set: `cargo deny check` (`just deny`) is a `pre-commit` hook
filtered to the dependency graph and to `deny.toml`, which define the local dependency/policy inputs.
An ordinary commit skips it; a dependency or policy commit pays ~1.3s. It
fetches the advisory database on every run and fails when it cannot, so an offline commit that
touches one of those files is red — the cached database is not a fallback; be online and commit again, or run
`just deny` to see the same result and warm the cache. A new CVE published while `Cargo.lock` is
untouched does not trigger the local dependency hook; CI runs `just deny` on every configured
push/PR job, and it remains worth running by hand. Builtin
hooks with no `stages` in `prek.toml` fire at both stages, so the two file-rewriting fixers also run
at pre-push; their `^fixtures/` exclude applies at both stages, and `fixtures/** -text` in
`.gitattributes` keeps line-ending conversion away from the byte-exact captures. `git commit
--no-verify` still bypasses the hooks: the check catches forgetting, not refusal. `python3
scripts/check_perf_doc.py --self-test` exercises the check itself against throwaway repositories; it
runs with every `GIT_*` variable stripped, because a linked worktree's hook exports an absolute
`GIT_DIR` that would otherwise send those throwaway repositories' `git add` into the caller's index.

### Verifying against a real graph

The optional `real_graph` integration tests run only when `LOGSEQ_GRAPH_REAL` names a Logseq graph
root; otherwise they print `SKIP=` and pass, so a plain checkout needs no graph.
`scripts/verify-real-graph.sh
<graph-root>` runs them and reports discovery counts, byte-exact round-trip totals, cold and warm
start timings, the storage cost of the projected block text, one bounded search measurement, and the
incremental chain: one `CheckNow` scan after editing, adding, deleting, and renaming a file, and
after touching one whose bytes stay the same, each showing which page moved and which pages kept
their revision. The named graph is only read: the incremental scans change a temporary copy of its
pages, journals, and configuration, and both that copy and the derived index are removed afterwards.

## Language

Project documentation, source comments, identifiers, and commit messages use English by default.
Graph content remains arbitrary Unicode and must not be normalized for presentation.

## Contributing, conduct, and licence

[Contributing](../CONTRIBUTING.md) lists the gates, the hot-path rule and the ADR workflow.
[Code of conduct](../CODE_OF_CONDUCT.md) states the standard for project spaces. This project is
licensed under the [MIT License](../LICENSE).
