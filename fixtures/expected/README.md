# Official API fixtures

Each `*.json` file here holds one answer from one read-only method of the official Logseq HTTP API
(`POST /api`, `{method, args}`). `scripts/capture_official_api.py` writes them. They are regression
baselines for the reads classified in `docs/logseq-api-parity.md`; they are not a runtime dependency.
No fixture was copied from another repository, and no answer was written by hand.

A file has this shape: `schema`, `method`, `args`, `captured_at` (UTC ISO-8601), `graph` (label plus
read-only file counts), `summarized`, and `result`. Small answers are verbatim. An answer larger
than 256 KiB is stored as a reproducible summary instead: `count`, `field_counts` (how many entries
carry each key), `truthy_field_counts` (how many carry a truthy value), and the first three entries
as `sample`; `summarized: true` marks that form; `getAllPages` is stored that way. A full answer is reproducible by running the capture script again. The token is
read only from `LOGSEQ_API_TOKEN` and never enters a file, command output, or log line.

Capture calls only the whitelisted read methods, refuses any write-looking method, never writes into
the graph root, and writes nothing at all if one method fails.

## Capture status

Fifteen fixtures were captured on 2026-11-20 with Logseq 0.10.9 against the public
[`logseq/docs`](https://github.com/logseq/docs) graph at tag `v0.9.2`, checked out at
`/home/ubuntu/work/docs` (graph label `docs`). `getAllPages` reported 756 entries: 298 with a
`file`, 458 without, and 94 carrying an `alias` field. The read-only walk recorded 199 page files,
75 journal files and 442,698 Markdown bytes. All 15 files pass offline validation with 0 problems.

## Numbers and provenance

Three official measurements are in circulation; they are not the same measurement.

- M9 (this capture, 2026-11-20, `logseq/docs` `v0.9.2`): 756 `getAllPages` entries, 298 with a
  `file`, 458 without, 94 carrying an `alias` field. `get_all_pages.json` records the counts and a sample.
  This is the only reproducible official baseline in this repository.
- M1: 34,883 entries, 20,734 without `file`, about 4,862 `alias-bearing entries`. Supplied with the
  parity brief; no capture date, graph identifier, or field definition. `alias-bearing entries` is
  not defined as a DB `alias` field, a distinct alias entity, or an `alias::` spelling.
- M2: 34,881 DB entities, 14,148 with `file`, 20,733 without, 2,521 ghost entities carrying an
  `alias` field; the earlier implementation answered 14,158 file pages plus 2,520 distinct alias
  entities. Read-only `compatibility.md` record, measured 2026-11-10.

M9 was measured on a different graph from M1 and M2: the public `logseq/docs` corpus, not the
private graph M1 and M2 describe. Do not add, subtract, or substitute across M1, M2 and M9: their
counts describe different graphs and, for `alias`, different quantities (`alias-bearing entries`,
`carried an alias field`, distinct alias entities). Matrix rows cite M1/M2 as historical evidence
for the *kind* of difference; M9 is the baseline to re-capture against when a differential test is
written.

## Fixtures

All 15 files are present and were captured in one run. "Shape" is that run's graph: 199 page
files / 75 journal files / 0.44 MB Markdown.

| Fixture | Official method (args) | Why it is a regression baseline |
| --- | --- | --- |
| `get_current_graph.json` | `logseq.Editor.getCurrentGraph()` `[]` | Matrix row 1: pins which graph the run describes, so a fixture from a different graph cannot be mixed into the set. |
| `get_all_pages.json` | `logseq.Editor.getAllPages()` `[]` | Matrix row 2: pins the official page universe and its field counts — the no-file entity population and the `alias` field count. Summarized. |
| `get_user_configs.json` | `logseq.App.getUserConfigs()` `[]` | Matrix row 3: pins the official configuration object, including the favorites key this repository answers as a list with existence flags. |
| `get_page.json` | `logseq.Editor.getPage(name)` `[--page]` | Matrix row 4: pins the official page entity fields (id, uuid, journalDay, properties, alias) for one named file page. |
| `get_page_blocks_tree.json` | `logseq.Editor.getPageBlocksTree(name)` `[--page]` | Matrix row 5: pins block count, nesting, content, marker and properties per block, including the pre-block behaviour this repository does not reproduce. |
| `get_page_linked_references.json` | `logseq.Editor.getPageLinkedReferences(name)` `[--page]` | Matrix row 6: pins the official grouping and its alias-reference merging. |
| `get_pages_from_namespace.json` | `logseq.Editor.getPagesFromNamespace(ns)` `[--namespace]` | Matrix row 14: pins the official direct-child set, including any no-file entities. Summarized. |
| `get_pages_tree_from_namespace.json` | `logseq.Editor.getPagesTreeFromNamespace(ns)` `[--namespace]` | Matrix row 15: pins the official namespace tree with its file-less container nodes. Summarized. |
| `get_block.json` | `logseq.Editor.getBlock(uuid)` `[--block-uuid]` | Matrix row 13: pins one official block entity and its id shape for a block the file states. |
| `get_block_properties.json` | `logseq.Editor.getBlockProperties(uuid)` `[--block-uuid]` | Matrix row 16: pins the official property table, against which this repository's ordered, repeated-key list is compared. |
| `get_block_property.json` | `logseq.Editor.getBlockProperty(uuid, key)` `[--block-uuid, --property-key]` | Matrix row 17: pins the single-value answer the official method gives for one key. |
| `get_previous_sibling_block.json` | `logseq.Editor.getPreviousSiblingBlock(uuid)` `[--block-uuid]` | Matrix row 18: pins the sibling answer against a documented structural difference (selector argument and `BlockView` against official UUID and DB entity). |
| `get_next_sibling_block.json` | `logseq.Editor.getNextSiblingBlock(uuid)` `[--block-uuid]` | Matrix row 19: same documented structural difference as row 18. |
| `app_search.json` | `logseq.App.search(q)` `[--search]` | Matrix row 9: pins the four official result groups, their caps, and the ghost pages this repository's search does not answer. |
| `db_q.json` | `logseq.DB.q(dsl)` `[--query]` | Matrix row 12: pins the official simple-query answer and hit order. |

## Re-capturing

```
LOGSEQ_API_TOKEN=<token> python3 scripts/capture_official_api.py \
  --graph docs --graph-root /home/ubuntu/work/docs \
  --page Tasks --block-uuid <uuid of the first child of "## Usage" on Tasks> --namespace Whiteboard \
  --search Presentation --query '(page-property :type [[Feature]])' --property-key heading
```

Every argument names a real page, block, namespace or query in the graph; the script guesses none
and skips a capture whose argument is missing. Use the same graph the official server has open.

## Offline validation

With no token or an unreachable server, the script reports `OFFLINE (未联网，未采集)` and checks
each existing fixture for: the schema marker, a whitelisted read method, an `args` array, an
ISO-8601 `captured_at`, a `graph` object with integer-or-null file counts, a `result` key, and the
absence of token-bearing keys or values. It writes nothing. A malformed fixture exits 1; an empty
directory is a valid offline state with zero fixtures.

## Differential harness

`crates/server/tests/official_parity.rs` compares seven of these answers with this repository's
file-page reads. Run it with the graph the capture used:

```
LOGSEQ_GRAPH_OFFICIAL=/path/to/graph cargo test --workspace --test official_parity
```

`LOGSEQ_GRAPH_OFFICIAL` must name the graph root recorded as `result.path` in
`get_current_graph.json`. Before any comparison the harness checks that path, the captured graph
name, and that the `pages/*.md` and `journals/*.md` counts are the same order of magnitude as the
fixture's `graph` field. A fixture or a graph from another run therefore fails immediately instead
of silently comparing the wrong data. The graph must be the same graph as at capture time; a
checkout that has drifted since 2026-11-20 is still usable, but the exact delta is printed as a
named finding and accounted for by measured quantities, never by widening a threshold.

The fixtures are read-only. The harness never re-captures, rewrites, or writes into this directory,
and never writes into the graph root: each index is projected into a temporary directory that is
removed when the test ends. With the variable unset or empty every test prints `SKIP=` and passes,
so CI needs neither the graph nor a running Logseq server. A variable that names something other
than a directory fails instead of skipping.

These tests do not claim `equivalent`. The captured answers are DB entities, including ghost and
alias pages with no file; the reads under test are a projection of Markdown page files, so raw JSON
equality never holds. The harness compares written normalization invariants: file-page counts
decomposed into measured quantities, folded name sets, preamble property keys, flattened block
texts, direct backlink texts and one block's text. Every excluded part is named (pre-blocks, alias
merging, no-file entities, DB-minted block UUIDs, `App.search` ghost-page groups); differences that
cannot be normalized away are pinned by exact assertions on their measured shape, not hidden behind
a widened threshold or an ignored test. Row #9, for example, pins four official block hits = three
DB pre-blocks plus one prose hit, exactly one of our hits, and zero prose misses after the DB's
page-name folding. Matrix classifications in
`docs/logseq-api-parity.md` are unchanged by this harness: a row becomes `equivalent` only after a
differential test passes and the read as a whole has no measured deviation.
