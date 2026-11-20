# Logseq HTTP API parity

This is the test basis for the served HTTP surface. Each row names one official method or one official
behaviour, the read in this repository that answers the same question, whether the two are
equivalent, and the test that pins the answer. Every test in the RPC parity suite names a row.

Official method names and argument shapes come from the file-edition HTTP API's `POST /api`
`{method, args}` envelope, as recorded in a 17-read method table and a read-only compatibility
record of an earlier implementation of this library. That record supplies method names and measured
deviations only: it is not a dependency, and no code, module, or fixture was copied from it. Target
architecture comes from the ADRs in `docs/adr/`. The "repository read" column names the engine
read and the RPC method that answers it; a row classifies the read, not its transport.
Backticked slash paths in the reason column name the GET endpoint the read answered before the
`POST /api` surface replaced it, and are historical labels only.

Classification:

- `equivalent`: the same question with the same argument shape, and no measured deviation. No row is `equivalent` in this revision: #18 and #19 were downgraded because their argument and answer shapes differ (a `BlockSelector` / `BlockView` against an official UUID / DB entity), and a captured fixture alone does not establish equivalence.
- `deliberate-difference`: a related question with a documented difference. The reason names a
  measured number, an ADR clause, or a recorded known difference.
- `out-of-scope`: no parity claim is made. The reason states why; these are not pending items.

Test names come from `crates/server/tests/rpc.rs` and its topical submodules in
`crates/server/tests/rpc/`, from the engine integration tests under `crates/graph/tests/`, and from
the captured-answer differential suite in `crates/server/tests/official_parity.rs` (test names
starting `row_NN_`).
Rows #14-#19 cite
engine tests only: the 19-method RPC slice implements no method for those official reads.
A few RPC tests label one narrow behaviour `equivalent` inside a row classified
`deliberate-difference`; the row classification is about the read as a whole.

## Measured facts

| ID | Observation | Source |
| --- | --- | --- |
| M1 | Official `getAllPages` held 34,883 entries; 20,734 had no `file` (tag pages, namespace parents, pages referenced without a file, journal ghosts); about 4,862 alias-bearing entries. | Supplied with this specification. No capture date, graph identifier, or field definition; `alias-bearing entries` is the brief's term, not a defined DB field. |
| M2 | Official `getAllPages` held 34,881 DB entities: 14,148 with a `file`, 20,733 without, of which 2,521 carried an `alias` field. The earlier implementation answered 14,158 file pages plus 2,520 distinct alias entities; M5 holds the 14,155 / 34,929 complement figures. | Read-only `compatibility.md` record, measured 2026-11-10. Counts are DB entities and DB fields. |
| M3 | Historical baseline before ADR-0015: only file pages were listed. The projection now derives no-file entities for tags, references, aliases and namespace parents. | ADR-0001, ADR-0002, ADR-0013; `pages_lists_every_page_with_its_block_count_in_folded_name_order` |
| M4 | Measured graph behind ADR-0008: 14,137 Markdown files, 112,882 blocks, 16.1 MB; 53.3% of pages empty, median non-empty page 6 blocks. | ADR-0008 context |
| M5 | `getPageLinkedReferences(alias)` answered 1 direct reference where the official server merged alias references into the declaring page's 10. Page-level `DB.q` complements used the 14,155 file pages where the official complement held 34,929 entities. | Read-only `compatibility.md` record, known differences |
| M6 | Official `App.search` page lists include tag pages and pages referenced without a file; ordering is the DB's internal order. This repository's block hits use full BM25 rank for indexed queries (at least three characters, or 1–2 all-CJK characters) with at most 100 matches, ascending FTS rowid order without ranking above 100 matches (after a bounded 101-match probe), and unspecified scan order for other short queries (ADR-0016); page-list ordering is unchanged. `DB.q` hit order and queries with org semantics (`TODO`, `DONE`, `State`, `CLOCK`, years) differ in hit count because the official search index extracts markers and `LOGBOOK` / `SCHEDULED` structurally. | Read-only `compatibility.md` record, known differences; ADR-0016 for this repository's hit ordering |
| M7 | Parser-level known differences: page-name normalisation for URL-encoded characters, a leading space, or a trailing `___` (about 25 of 13,919 pages); leading whitespace on indented property lines; pre-block content trailing newline; property array order; bare URLs are not page references. | Read-only `compatibility.md` record, known differences |
| M8 | Historical earlier-implementation baseline: `getPage(alias)` returned ids that were not official DB ids; tag/namespace ghost IDs returned `null` and non-alias ghosts were not materialised. ADR-0015 now derives no-file entities by name; official DB-ID lookup remains unavailable. | Read-only `compatibility.md` record, not-implemented and known-differences sections; ADR-0015 for current behavior |
| M9 | Official `getAllPages` held 756 entries: 298 with a `file`, 458 without, and 94 carrying an `alias` field. The other 14 read answers were captured in the same run. | `fixtures/expected/*.json`, captured 2026-11-20 with Logseq 0.10.9 against the public `logseq/docs` graph at `v0.9.2` (199 page files, 75 journal files, 442,698 Markdown bytes). The only reproducible official baseline in this repository. |

### Provenance and field semantics of M1, M2 and M9

M1 and M2 are two different measurements, not two views of one count.

- M1 has no capture date, graph identifier, or field definition. `alias-bearing entries` (≈4,862)
  is the brief's own term; it is not defined as distinct alias entities, as DB entities carrying an
  `alias` field, or as `alias::` spellings.
- M2 counts DB entities from `getAllPages` and DB fields on those entities. `carried an alias field`
  (2,521) is a property of ghost entities, not the same quantity as `alias-bearing entries`
  (≈4,862), and neither is the 2,520 distinct alias entities of the earlier implementation.
- `alias` in this repository is text (M3), not any of the three official quantities above.
- M9 is the current capture, measured on the public `logseq/docs` graph rather than the graph
  M1 and M2 describe. M9's 94 is entries whose `alias` field is present.
- Do not add, subtract, or substitute across M1, M2 and M9: 34,883 − 20,734 = 14,149 is not
  M2's 14,148, 4,862 is neither 2,521 nor 2,520, and M9 counts a different graph. Each matrix row cites one observation by
  ID and names it; rows #2, #7, #14, #15 and #26 use M1, and #8 uses M2, without combining the
  two. Rows cite M1/M2 as historical evidence for the *kind* of difference; M9 is the baseline
  to re-capture against when a differential test is written.

M9 is a fresh official capture (`fixtures/expected/README.md`, Capture status). It pins the
official shapes and counts. `crates/server/tests/official_parity.rs` now compares the answers of
rows #1, #2, #4, #5, #6, #9 and #13 with this repository's reads using written normalization
invariants; the differential harness is described below. The matrix therefore still claims no
`equivalent` row, and the harness changes no classification.

## Differential harness (captured answers)

Run the differential suite with the graph root spelled by `get_current_graph.json`'s `result.path`:

```
LOGSEQ_GRAPH_OFFICIAL=/path/to/graph cargo test --workspace --test official_parity
```

`fixtures/expected/` is resolved from `CARGO_MANIFEST_DIR`, the graph root is read only, and each
index is projected into a temporary directory that the test removes on exit; nothing is written into
the graph root. With the variable unset or empty every test prints `SKIP=` and passes, so a plain
checkout needs no graph. A variable that names something other than a directory fails instead of skipping.

A source guard runs before every comparison: it checks the captured graph name and path, and the
order of magnitude of `pages/*.md` and `journals/*.md` against the fixture's `graph` counts. A
fixture from another graph therefore fails immediately. A drifted checkout is reported as a named
finding with its exact delta, not hidden.

The harness compares written normalization invariants, not raw JSON equality. The captured answers
are DB entities; our reads project both file-backed and derived page entities from Markdown.
What is compared: a count decomposed into measured quantities (the capture's own file walk, the
official file-entity gap and local drift), folded name sets, preamble property keys, flattened
block texts, direct backlink texts, and one block's text. Every excluded part is named: pre-blocks
(ADR-0013), exact no-file counts (the capture has no name list), DB-minted block UUIDs (ADR-0012), and the
ghost-page, page-content and asset groups of `App.search` (M6). Differences that cannot be
normalized away are pinned by exact assertions on their measured shape instead of being hidden by a
widened threshold; no test is ignored. Row #9 is the clearest case: the four official `blocks` hits
are three DB pre-blocks holding page preamble properties plus one prose hit whose `[[...]]` page
name the DB folds to lowercase, so none of the four texts is answered verbatim and the one prose
hit agrees after folding. `row_09_app_search_pins_the_pre_block_and_name_folding_difference`
asserts 4 = 3 + 1, exactly one of our hits, that folded hit, all four verbatim misses and zero
folded prose misses. The row's classification is unchanged.

A second differential harness pins the **wire shape** rather than the values:
`crates/server/tests/rpc/official_shapes.rs` replays each captured method against a sandbox server
and asserts that every key the official answer states is also stated here with a compatible JSON
type, unless the key is listed in the test as a documented difference with its matrix row. Values
are never compared — the captures describe another graph — so this suite runs in every `cargo test`
with no environment variable and no reference graph. It also pins the methods the Logseq Copilot
extension calls, and asserts that every captured method this slice does not serve answers
`MethodNotExist` and is listed with its row, so a captured method cannot go missing silently. That
suite exists because a shape mismatch — a search answer without `blocks`, a `block/uuid` with a
`durable:` prefix where a client hands the id straight back to `Editor.getBlock` — breaks a real
client without breaking any value comparison; it was added after exactly that happened.

Passing a differential test does not establish equivalence. A row is upgraded to `equivalent` only
when the differential test passes and the read as a whole has no measured deviation, which no row
has in this revision.

## Matrix

| # | Area | Official method and behaviour | This repository read (RPC method) | Classification | Reason / evidence | Test |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | RPC reads | `logseq.Editor.getCurrentGraph()`, args `[]`: answers the graph currently open in the app. | `Graph::reads().pages()` plus index stat (`logseq.Editor.getCurrentGraph`) | deliberate-difference | `/info` answers page and block counts, the logical index path, the physical bytes of the live index generation with its SQLite sidecars (`index_size`), an in-progress shadow build (`index_building_size`) and every other generation still on disk (`index_retired_size`), startup scan progress, and a `watch` object (in both the ready and the scanning answer) with the watcher's `mode`, its `need_rescan_events` (notify queue overflows and dropped events), `watcher_errors`, `full_rescans` and `full_rescans_by_cause`. The answer also states the official `name`, `path` and `url` (`logseq_local_<path>`) fields; ADR-0008 requires readiness on the bound socket and 503 for other endpoints, and ADR-0006 requires revision, staleness and last-scan metadata. The official answer has no `scanning` state. | `get_current_graph_matches_logseq_Editor_getCurrentGraph`; `storage_usage_separates_the_live_generation_from_shadows_and_retired_ones`; `get_current_graph_reports_the_startup_scan_progress`; `each_rescan_cause_is_classified_and_counted`; `default_reads_observe_external_changes_without_polling`; `no_watch_keeps_polling_available`; `the_port_is_released_after_the_server_stops`; `row_01_get_current_graph_pins_the_captured_graph` (differential) |
| 2 | RPC reads | `logseq.Editor.getAllPages()`, args `[]`: answers every page entity in the app DB, including entities with no file. | `Graph::reads().pages()` (`logseq.Editor.getAllPages`) | deliberate-difference | M1 is the observation this row is classified against: 34,883 entries, 20,734 with no `file`. M2 is a separate measurement with different field semantics and is not combined with M1 here (Provenance). ADR-0015 derives file and no-file pages from Markdown facts; the response includes `has_file` rather than a DB file entity. ADR-0001 keeps Markdown files the sole source of product facts; ADR-0002 keeps the page rows derived. | `get_all_pages_matches_logseq_Editor_getAllPages`; `an_alias_is_a_no_file_page_and_merges_linked_references`; `freshness_is_one_policy_per_request`; `rest_endpoints_are_gone_and_only_post_api_is_answered`; `pages_lists_every_page_with_its_block_count_in_folded_name_order`; `row_02_get_all_pages_compares_the_file_page_universe` (differential) |
| 3 | RPC reads | `logseq.App.getUserConfigs()`, args `[]`: answers the app's static user configuration object. | `logseq.App.getUserConfigs` (an own answer from the graph files; `Graph::reads().favorites()` stays `logseq.Graph.getFavorites`) | deliberate-difference | The served answer states the facts the graph files hold — `currentGraph`, `me` (the graph url), `preferredFormat` and `preferredDateFormat` from the graph's own `:journal/page-title-format` (`source::journal_page_title_format`) — and omits the app-state keys (theme, language, start of week, workflow), which no file states (ADR-0001). `getFavorites` remains the own read of the `:favorites` key. | `user_configs_answers_the_graph_facts_and_omits_app_state`; `extension_reads_are_all_answered`; `get_favorites_matches_logseq_App_getUserConfigs`; `favorites_are_answered_as_written_and_state_whether_their_page_exists` |
| 4 | RPC reads | `logseq.Editor.getPage(name)`, args `[name]`: answers one page entity (id, uuid, journalDay, properties), also by alias name, entity uuid or numeric entity id. | `page_metadata` + `page_properties` (`logseq.Editor.getPage`) | deliberate-difference | File and no-file pages resolve by folded name, not DB id; a bare numeric argument or `{id}` resolves the stable hash-derived numeric page id this surface states (`block.page.id`, `getPage.id`). The answer states the official top-level `name` (folded), `originalName`, `uuid` (the folded name, ADR-0001), `journal?` and `id`. The pre-block supplies page properties; properties inside the first dash block stay block properties (ADR-0015). | `get_page_matches_logseq_Editor_getPage`; `get_page_accepts_a_unicode_name_as_a_json_string_matches_logseq_Editor_getPage`; `an_alias_is_a_no_file_page_and_merges_linked_references`; `method_names_with_and_without_the_sub_namespace_are_equivalent`; `the_old_http_error_model_is_a_structured_error_object`; `page_properties_keep_written_order_and_repeated_keys`; `row_04_get_page_compares_names_and_property_keys` (differential) |
| 5 | RPC reads | `logseq.Editor.getPageBlocksTree(name)`, args `[name]`: answers the page's nested block tree with DB entity fields (parent, left, refs, pathRefs), including pre-block content. | `page_blocks` (`logseq.Editor.getPageBlocksTree`) | deliberate-difference | The answer is flat preorder/depth entries, each with the block's marker and a durable or projection-scoped ephemeral selector (ADR-0012); ADR-0013 refuses a synthetic pre-block result. M7 records the pre-block newline and indented-property-whitespace differences. | `get_page_blocks_tree_matches_logseq_Editor_getPageBlocksTree`; `get_page_blocks_tree_reports_heading_and_bullet_markers`; `projected_page_tree_preserves_hierarchy_and_order`; `a_snapshot_agrees_with_the_page_tree_block_for_block`; `row_05_get_page_blocks_tree_compares_non_pre_block_content` (differential) |
| 6 | RPC reads | `logseq.Editor.getPageLinkedReferences(name)`, args `[name]`: answers blocks that reference the page, grouped by page; alias references merge into the declaring page. | `Graph::reads().backlinks()` (`logseq.Editor.getPageLinkedReferences`) | deliberate-difference | This read merges aliases and returns one representative reference per source page. Page-property references add `pre_block: true` (ADR-0015). | `get_page_linked_references_matches_logseq_Editor_getPageLinkedReferences`; `backlinks_match_by_folded_key_once_per_page_and_in_name_order`; `a_backlink_reports_the_earliest_referencing_block`; `row_06_get_page_linked_references_compares_direct_backlinks` (differential) |
| 7 | RPC reads | `logseq.Editor.getAllPages()`, args `[]`: tag names appear as page entities with no file; the official read table has no method that enumerates tags. | `Graph::reads().tags()` (`logseq.Graph.getAllTags`) | deliberate-difference | Tag names also appear in `getAllPages` under ADR-0015. This read answers a derived `tags` projection of tag name and distinct page count (ADR-0002), not a list of entities. M2 is not used in this row. | `get_all_tags_matches_getAllPages_tag_pages`; `tags_count_distinct_pages_rather_than_blocks`; `a_tag_is_one_row_whatever_its_spelling` |
| 8 | RPC reads | `logseq.DB.q(dsl)`, args `[dsl]`: `(page-tags [[name]])` answers pages carrying a tag through the simple query DSL. | `pages_with_tag()` (`logseq.Graph.getPagesWithTag`) | deliberate-difference | ADR-0013 executes simple queries in the index with a deterministic row order and a mandatory limit; this endpoint answers file page names from one structured read instead of a DSL query. The compatibility record states structured query is its own contract: equivalent in scenario, not query-language compatible. M2 records a 14,155 file-page universe against a 34,929 official complement; it is a separate measurement from M1 and is not combined with it. | `get_pages_with_tag_matches_logseq_DB_q_page_tags`; `pages_with_tag_reports_each_page_once_in_folded_name_order` |
| 9 | RPC reads | `logseq.App.search(q)`, args `[q]`: answers blocks (cap 10), pages-content (cap 10), pages (cap 100) and files (always empty). | `Graph::reads().search(q, limit)` (`logseq.App.search`) | deliberate-difference | One block-hit list with a caller limit (default 20, ceiling 1000) and a selector per hit; M6 records the official ghost pages and DB ordering, M3 the smaller page universe. ADR-0007 makes block text derived searchable state; ADR-0016 probes at most 101 matches for indexed queries (at least three characters, or 1–2 all-CJK characters via folded unigram/bigram tokens), then uses full BM25 for at most 100 matches or ascending FTS rowid order unranked on overflow (not best-of-first-100 ranking). This threshold does not cap results; other short queries (including mixed `a中`) scan in unspecified order, and rank is not exposed. | `app_search_matches_logseq_App_search`; `app_search_hits_carry_an_actionable_selector`; `app_search_refuses_a_limit_above_the_ceiling`; `row_09_app_search_pins_the_pre_block_and_name_folding_difference` (differential; pins the pre-block / name-folding difference); `ranked_search_returns_the_strongest_match_before_applying_the_limit`; `ranking_at_the_threshold_switches_to_rowid_order_above_it`; `short_cjk_phrases_require_contiguous_characters`; `short_cjk_hits_are_ranked_before_applying_the_limit`; `rescanning_rewrites_and_deletes_short_cjk_hits` |
| 10 | RPC reads | No official method reports the fingerprint of the bytes projected for one page beside the fingerprint of that page's current file. | `Graph::page_versions()` (`logseq.Graph.getPageVersion`) | out-of-scope | ADR-0008's amendment defines this file-tier version read and its refusal of a freshness parameter; ADR-0006 defines the freshness policies this read does not need. The official API has no counterpart, so there is no parity claim to test. | `get_page_version_is_out_of_scope_and_keeps_its_version_tokens`; `get_page_version_refuses_an_extra_freshness_policy` (out-of-scope; no equivalence test) |
| 11 | RPC reads | `logseq.Editor.getPageBlocksTree(name)`, args `[name]`: answers a page's block tree from the app DB. | `Graph::page_snapshot()` (`logseq.Graph.getPageSnapshot`) | deliberate-difference | ADR-0002's amendment makes the snapshot a read of the page file's current bytes, so it answers properties and blocks under the file's own version; ADR-0008's amendment gives it no freshness policy. ADR-0012 states a selector only where the file writes `id::`. The official read answers derived rows. | `get_page_snapshot_matches_getPageBlocksTree`; `get_page_snapshot_states_a_selector_only_for_file_ids`; `get_page_snapshot_answers_the_bytes_edited_since_the_scan`; `get_page_snapshot_answers_an_unknown_page_with_exists_false`; `get_page_snapshot_refuses_an_extra_freshness_policy`; `get_page_snapshot_reports_an_undecodable_file_as_a_structured_error` |
| 12 | RPC reads | `logseq.DB.q(dsl)`, args `[dsl]`: answers a simple query or a full-text string; general Datalog answers `null`. | `Graph::reads().query()` (`logseq.Graph.query`) | deliberate-difference | ADR-0013: own grammar, index executor, a refused query named with its reason instead of `null`, deterministic row order, mandatory limit, and an explicit `today` for relative operands. The same read is also served under the official `logseq.DB.q` name and answers the official bare array: a block entry states `uuid` / `content` / `page` (`{name, uuid, id}`) and a page entry `uuid` / folded `name` / `originalName` / `journal?` / numeric `journalDay`, with no wrapper, `truncated`, or read metadata. M5 records the complement-size difference (14,155 against 34,929) and M6 the ordering and org-semantics differences. | `query_matches_logseq_DB_q`; `db_q_states_the_official_page_entity_fields`; `extension_reads_are_all_answered`; `query_states_that_its_answer_was_truncated`; `query_selector_reads_the_block_it_names`; `a_query_the_grammar_refuses_is_a_structured_error_not_an_empty_result`; `a_page_level_query_answers_pages` |
| 13 | RPC reads | `logseq.Editor.getBlock(uuid)`, args `[uuid]`: answers one block entity by UUID. | `Graph::reads().block()` (`logseq.Editor.getBlock`) | deliberate-difference | Accepts `<uuid>` or `ephemeral:<uuid>` (ADR-0005, ADR-0009, ADR-0012), reports a not-found error for an unknown id and a conflict naming every page for a duplicate `id::`; the official DB mints ids for blocks without `id::`, where this projection mints ephemeral ones. M7 records the non-`id::` uuid algorithm difference. | `get_block_matches_logseq_Editor_getBlock`; `an_ambiguous_block_id_names_every_page`; `the_old_http_error_model_is_a_structured_error_object`; `a_durable_target_answers_the_block_that_states_the_id`; `an_id_stated_on_two_pages_is_refused_instead_of_picking_one`; `row_13_get_block_compares_the_located_blocks_text` (differential) |
| 14 | Official reads without an RPC method | `logseq.Editor.getPagesFromNamespace(ns)`, args `[ns]`: answers the direct child pages of a namespace as page entities. | `Graph::reads().pages_from_namespace(ns)` | deliberate-difference | M1 records 20,734 official entities without a `file`. This read answers the stored spellings of direct child entities under the folded page key, and an empty answer says "nothing under this name". M2 is not combined with M1 here (Provenance). | `a_namespace_answers_its_children_and_its_subtree`; `namespace_reads_fold_case_like_every_page_identity` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 15 | Official reads without an RPC method | `logseq.Editor.getPagesTreeFromNamespace(ns)`, args `[ns]`: answers a namespace tree of child namespaces and leaf pages recursively; container nodes have no file. | `Graph::reads().pages_tree_from_namespace(ns)` | deliberate-difference | This read answers the whole subtree as a name-ordered flat list including no-file namespace parents, not a tree (ADR-0015). M1 counts namespace parents among the official no-file entities. | `a_namespace_answers_its_children_and_its_subtree`; `namespace_reads_fold_case_like_every_page_identity` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 16 | Official reads without an RPC method | `logseq.Editor.getBlockProperties(uuid)`, args `[uuid]`: answers a block's property table. | `Graph::reads().block_properties(selector)` | deliberate-difference | This read answers one element per property line in written order, preserving repeated keys and verbatim values; `block_properties_keep_written_order_and_repeated_keys` pins that and ADR-0001 makes the file lines the product facts. The official read table describes a property table, not an ordered list of lines. | `block_properties_keep_written_order_and_repeated_keys` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 17 | Official reads without an RPC method | `logseq.Editor.getBlockProperty(uuid, key)`, args `[uuid, key]`: answers one block property value. | `Graph::reads().block_properties(selector)` filtered by key | deliberate-difference | There is no engine read that takes a key. Repeated keys are facts this projection keeps, so one key may answer more than one value, while the official method answers one property value (ADR-0001). | `block_properties_keep_written_order_and_repeated_keys` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 18 | Official reads without an RPC method | `logseq.Editor.getPreviousSiblingBlock(uuid)`, args `[uuid]`: answers the block at the previous sibling position. | `Graph::reads().block_siblings(selector).previous` | deliberate-difference | The official method takes `[uuid]` and answers a DB block entity; this read takes a `BlockSelector` that also accepts `ephemeral:<uuid>` (ADR-0012) and answers a `BlockView` with a durable or ephemeral selector. M7 records the non-`id::` block uuid algorithm difference. No official-service call was made this round, so equivalence with the official answer has not been verified; the sibling rule itself is pinned only by local tests. | `block_siblings_report_the_neighbours_on_both_sides`; `two_top_level_blocks_are_siblings_of_each_other` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 19 | Official reads without an RPC method | `logseq.Editor.getNextSiblingBlock(uuid)`, args `[uuid]`: answers the block at the next sibling position. | `Graph::reads().block_siblings(selector).next` | deliberate-difference | Same documented difference as #18: selector argument and `BlockView` answer against the official UUID and DB entity (ADR-0012, M7). No official-service call was made this round, so equivalence with the official answer has not been verified. | `block_siblings_report_the_neighbours_on_both_sides`; `two_top_level_blocks_are_siblings_of_each_other` (engine only; the RPC slice answers `MethodNotExist`, pinned by `unimplemented_captures_answer_method_not_exist`) |
| 20 | Out of scope | `logseq.showMsg(msg, opts?)`, args `[msg, opts?]`: shows a GUI toast and returns a message id. | `logseq.showMsg` (an own no-op) | out-of-scope | There is no GUI (ADR-0003), so the call is accepted and dropped: nothing is displayed, and the answer is the fixed message-id string `notification` of the official shape so a client that only checks for success can finish connecting. No parity is claimed. | `extension_reads_are_all_answered`; `method_names_with_and_without_the_sub_namespace_are_equivalent` |
| 21 | Out of scope | `logseq.App.getAppInfo()`; official names `logseq.App.getInfo()` and `logseq.App.getUserInfo()`: answer HTTP server version and app user info. | `logseq.App.getAppInfo` (an own constant) | out-of-scope | This build answers `{version: <its crate version>, supportDb: false}`: the shape the Logseq Copilot extension reads for its "connected to Logseq vN" message, with the file edition's answer to the DB question (ADR-0001). Version is a host fact, and user info is not answered. No parity is claimed. | `extension_reads_are_all_answered`; `method_names_with_and_without_the_sub_namespace_are_equivalent` |
| 22 | Out of scope | `logseq.UI.closeMsg` and the GUI, plugin-registration and event-listener method families: depend on GUI state, registration or callbacks. | none | out-of-scope | The compatibility record states these are out of scope by design and keep returning `MethodNotExist`; ADR-0003 is not a plugin architecture and ADR-0001 keeps no product fact in process memory. | `method_names_with_and_without_the_sub_namespace_are_equivalent`; `rest_endpoints_are_gone_and_only_post_api_is_answered` |
| 23 | Out of scope | `logseq.Assets.listFilesOfCurrentGraph()`, `logseq.Git.loadIgnoreFile()`, `logseq.Git.saveIgnoreFile()`, `logseq.Git.execCommand()`: asset listing and graph-root Git operations. | none | out-of-scope | These are host or file-Git operations, not reads of the derived read model (ADR-0002, ADR-0003). Two of them write. The compatibility record lists them beyond the 17-read table used as this document's parity basis. | `method_names_with_and_without_the_sub_namespace_are_equivalent`; `rest_endpoints_are_gone_and_only_post_api_is_answered` |
| 24 | Out of scope | `logseq.DB.datascriptQuery` with general Datalog (`[:find ... :where ...]`, `:inputs`, `:rules`): evaluates an advanced query. | none | out-of-scope | ADR-0013 deliberately does not implement advanced queries and refuses them as `Unsupported`; the compatibility record says a full Datalog engine is required and that other input answers `null`, with no guessed results. | `a_query_the_grammar_refuses_is_a_structured_error_not_an_empty_result` |
| 25 | Out of scope | `logseq.DB.q` with `[:find ... :where ...]`: general Datalog answers `null` on the official server. | none | out-of-scope | ADR-0013 refuses advanced queries with the form echoed back instead of `null`; the compatibility record records this input as not implemented. | `a_query_the_grammar_refuses_is_a_structured_error_not_an_empty_result` (refusal only; no equivalence test) |
| 26 | Out of scope | Logseq DB-edition-only methods: answer facts that exist only in the DB edition's entity table. | none | out-of-scope | The compatibility record places DB-edition-only methods out of scope; ADR-0001 makes Markdown files the sole source of product facts, so no entity-only fact is a product fact here. M1 lists the no-file entity kinds. | `method_names_with_and_without_the_sub_namespace_are_equivalent`; `rest_endpoints_are_gone_and_only_post_api_is_answered` |
| 27 | Out of scope | `logseq.Editor.appendBlock`, `logseq.Editor.getPageProperties`, `logseq.Editor.createJournalPage`: older official method names. | `append_block`, `page_properties`, `create_journal` for the surviving behaviour | out-of-scope | The compatibility record's deliberate deviations state these official methods no longer exist: page properties are read from the pre-block, journals use get-or-create, and the dead `appendBlock` was mapped to `appendBlockInPage`. There is no official method to be equivalent to. | `method_names_with_and_without_the_sub_namespace_are_equivalent`; `rest_endpoints_are_gone_and_only_post_api_is_answered` |
| 28 | RPC reads | `logseq.App.checkCurrentIsDBGraph()`, args `[]`: answers whether the connected graph is the DB edition. | `logseq.App.checkCurrentIsDBGraph` (an own constant, no engine read) | deliberate-difference | ADR-0001 makes Markdown files the only source of product facts, so this projection is the file edition and the probe always answers `false` — a client probing a file graph should learn the file answer, not parse a refusal. Measured on the live official file-edition server (2026-11-19) the same call answers `MethodNotExist: check_current_is_db_graph`, so the `false` here is a deliberate choice, not a captured equivalence. It needs no graph read and declares no freshness policy (ADR-0008). | `extension_reads_are_all_answered` |
| 29 | RPC reads | `logseq.Editor.getCurrentPage()`, args `[]`: answers the page the editor currently has open, or `nil`. | `logseq.Editor.getCurrentPage` (an own constant, no engine read) | out-of-scope | There is no editor (ADR-0003), so the official "no current page" value `null` is the honest answer; the client's feature degrades instead of failing with a method-not-found it would have to parse. No parity is claimed. | `extension_reads_are_all_answered`; `get_current_page_answers_null_without_an_editor` |

## Envelope facts

`journal_writes_use_the_configuration_published_by_the_watcher` pins the own write contract after
a successful configuration-changing background scan: the new title resolves, the obsolete title
is refused without changing bytes, and disk/index versions agree after the write.

Bodies exceeding 16 MiB receive 413, including decoded chunked bodies. Every transport rejection,
including either 413 path, is the Fastify error body `{statusCode, error, message}` plus this
surface's `code`, `data.kind`, `data.http_status`, and `Content-Type: application/json`; invalid
requests use code `-32600`, capacity refusals use `-32003` and kind `unavailable`. `incomplete_bodies_do_not_block_other_clients_and_large_bodies_are_refused`
pins both 413 envelopes. The in-crate `std::net` transport uses `httparse` for request heads, caps
heads at 64 KiB (431), and rejects ambiguous Content-Length/Transfer-Encoding framing (400).
There is one request per connection; every final response states Content-Length and Connection:
close. `Expect: 100-continue` is answered only after the transport guard and declared-length cap
pass, before reading the body (`expect_continue_is_sent_after_the_transport_guard_passes`).

One accept thread admits at most 64 connection threads; excess connections receive a best-effort,
nonblocking 503 error (`excess_stalled_connections_receive_a_quick_json_rpc_503`). Each
connection thread owns all socket I/O: reads have a five-second idle timeout and a fifteen-second
total head-plus-body deadline measured from acceptance; response writes have a fifteen-second
total deadline. A read deadline closes the connection after a best-effort 408. A write deadline
closes even a partially written response. Only fully read requests acquire one of sixteen handler
slots; graph dispatch remains serialized by the existing graph lock. Slow clients therefore use
connection slots, not handler slots (`stalled_clients_do_not_exhaust_the_request_workers`).
`transport.rs` unit tests pin idle and total deadlines for heads and bodies, stalled response
readers, slot release, valid/empty bodies, malformed heads/chunks, and smuggling rejections.

Our transport requires a loopback `Host` at the bound port and refuses a head carrying more than
one `Origin` (403 on rejection). `POST /api` requires `Content-Type: application/json`, with
optional parameters (415 otherwise). The `Origin` value itself is not checked: the bearer token,
not the page's origin, is the cross-origin gate (ADR-0018). These are our transport rules, not
official API parity claims;
`transport_guards_check_the_head_and_token_before_dispatch` pins them.

The official surface is `POST /api` with `{method, args}` and a bearer token. The response is the
official wire shape, not a JSON-RPC envelope: a success is HTTP 200 whose body is the result value
itself, with no shell and no id echo; a failure is an HTTP status code with the official Fastify
body `{statusCode, error, message}`, to which this repository additionally states its own `code`
and structured `data` — the official client branches on the status and ignores the body. Measured
on the official server (2026-11-15): a bad token answers 401
`{"statusCode":401,"error":"Unauthorized","message":"Access Denied!"}`; malformed JSON or a missing
method answers 400 Bad Request; an unknown method answers 200 `{"error":"MethodNotExist: nope"}`;
and `getPage` / `getBlock` on a missing target answer 200 `null`. This repository keeps its
stricter status codes as documented differences: an unknown method is 400 with `code` `-32601`,
and a missing page or block is 404 with `code` `-32004`, where the official server answers 200.
This surface now requires the same bearer token (ADR-0018): `Authorization: Bearer <token>`, scheme
case-insensitive, one or more spaces or tabs, any of the configured tokens; a refusal is HTTP 401
with `code` `-32001` / `data.kind "unauthorized"` and never echoes the token.

Method names resolve the official way, measured on the live server: a name starting with `logseq.`
is looked up by its last dot segment with every uppercase letter becoming `_` plus lowercase —
`logseq.Editor.getPage`, `logseq.Editor.get_page`, `logseq.getPage` and `logseq.App.get_current_graph`
all resolve, while `logseq.Editor.getpage` does not — and a name without the prefix is looked up
as written, so the bare snake spellings `get_page`, `get_block`, `search` and `get_user_configs`
resolve and bare `getPage` does not. An unknown name answers `MethodNotExist:` with the looked-up
spelling. The `logseq.Graph.*` writes keep their exact names: the leaf rule is a read alias, not a
second way to call a write (ADR-0014).
The same applies to argument shapes: an official client may send a page identity object (`{name}`, `{uuid}`, `{id: <number>}`) where the command line sends a name string, and a `null` where it omits an optional argument; both are accepted, and `{id}` resolves a projection-derived numeric page id (a stable hash of the folded name, stated as `block.page.id`), because a file graph has no DB entity id. That is an envelope rule, not a read, so it changes no row's classification. The surface also answers the official CORS policy (`@fastify/cors`, `origin "*"`): an `OPTIONS` preflight gets `204` with the allow-origin/methods/headers headers before the token check (a preflight carries no token), and every answer carries `Access-Control-Allow-Origin: *`, so a browser client — the Logseq Copilot extension fetches with `mode: "cors"` from a `chrome-extension://<id>` origin — can call it. Every row above states
the official argument shape; none of those shapes has a freshness argument. Our surface keeps that
positional shape and adds one trailing options object to `args` for the call's own `freshness`,
`limit` and `today`. That object is a decision of our own API, not an official argument: ADR-0008
requires one policy per request, and keeping it beside the arguments makes the call self-contained
without adding a non-standard member to the request envelope. The carrier is not part of any row's
classification.

The official write table holds 13 methods. The matrix above classifies the 19 read methods this
slice serves (one of them a GUI call answered as a no-op). Served writes are a separate surface: every method is `logseq.Graph.*`, is this
repository's own API, and is classified `out-of-scope` for official parity. They are not additions
to the official method table, and an operation's resemblance to an official write is not an
equivalence claim.

## Served writes: own API, not official parity

ADR-0014 is the contract; the file-first library writes remain authoritative for behaviour
(ADR-0004, ADR-0011, ADR-0012). The RPC adapter only translates arguments, results and errors. A
write takes no `freshness` option; with matching configuration signatures it starts no graph-wide
scan. A mismatch builds/catches up a shadow unlocked, then verifies and publishes under the graph
write lock before resolving targets. The shared writer mutex covers one whole library write;
the startup-state mutex is released first and pooled reads can proceed concurrently (ADR-0017).
Separate calls are not combined into an adapter transaction.

The sixteen methods correspond one-to-one with these library writes:

| Own RPC method | `args` | Result value (besides read metadata) |
| --- | --- | --- |
| `logseq.Graph.createPage` | `[name]` | `page` metadata |
| `logseq.Graph.createJournal` | `[date]`, with `date` as `YYYY-MM-DD` | `page` metadata |
| `logseq.Graph.appendBlock` | `[page, text]` | `page` metadata |
| `logseq.Graph.insertBlock` | `[target, text]` | `selector` of the inserted block |
| `logseq.Graph.insertBatch` | `[target, texts]` | `selectors` in `texts` order |
| `logseq.Graph.prependBlockInPage` | `[page, text]` | `selector` of the inserted block |
| `logseq.Graph.updateBlock` | `[selector, text]` | `block` view |
| `logseq.Graph.removeBlock` | `[selector]` | `page` metadata |
| `logseq.Graph.removeBlockRange` | `[first, last]` | `page` metadata; inclusive sibling run and subtrees |
| `logseq.Graph.moveBlock` | `[selector, target]` or `[selector, direction, anchor]` | `page` metadata |
| `logseq.Graph.upsertBlockProperty` | `[selector, key, value]` | `block` view |
| `logseq.Graph.ensureBlockId` | `[selector]` | bare UUID `id`, boolean `created`, durable `block` view |
| `logseq.Graph.removeBlockProperty` | `[selector, key]` | `block` view |
| `logseq.Graph.setBlockCollapsed` | `[selector, collapsed]` | `block` view |
| `logseq.Graph.deletePage` | `[name]` | `page` metadata as it was before deletion |
| `logseq.Graph.commitPage` | `[page, baseVersion, operations]` | `snapshot` and `applied` |

`target` is a one-key object: `{"before": selector}`, `{"after": selector}`,
`{"firstChildOf": selector}` or `{"lastChildOf": selector}`. The alternate three-argument move
spelling is `direction` = `before`/`after`/`first_child_of`/`last_child_of` followed by the anchor
selector. A selector uses the same durable/ephemeral spelling as block reads. `texts` and
`operations` are JSON arrays, and `collapsed` is a JSON boolean. Every write has exactly the
arguments shown: there is no trailing options object and no `freshness` option. `operations` uses
the CLI's ten snake_case operation variants (`edit_text`, `split_block`, `merge_block`,
`unwrap_block`, `indent_block`, `outdent_block`, `insert_block`, `move_block`, `set_collapsed`,
`set_marker`) and child-index paths.

| Own RPC method | Library write | Contract and answer |
| --- | --- | --- |
| `logseq.Graph.createPage` | `create_page` | Create an empty regular page; never rewrite an existing page. Answers page metadata. |
| `logseq.Graph.createJournal` | `create_journal` | Create the journal for one `YYYY-MM-DD` day through the configured file-name and title formats; never rewrite an existing journal. Answers page metadata. |
| `logseq.Graph.appendBlock` | `append_block` | Append one top-level block to a named page. Answers page metadata. |
| `logseq.Graph.insertBlock` | `insert_block` | Insert one block at a `before`, `after`, `firstChildOf` or `lastChildOf` anchor. Answers its committed durable or ephemeral selector. |
| `logseq.Graph.insertBatch` | `insert_batch` | Insert a run of blocks in array order with one page replacement. Answers their committed selectors in input order. |
| `logseq.Graph.prependBlockInPage` | `prepend_block_in_page` | Insert before the first block, or as the first block of an empty page. Answers its committed selector. |
| `logseq.Graph.updateBlock` | `update_block` | Replace one selected block's own text, preserving its children and identity/folded-state properties. Answers the committed block view. |
| `logseq.Graph.removeBlock` | `remove_block` | Remove a selected block and its subtree. Answers page metadata. |
| `logseq.Graph.removeBlockRange` | `remove_block_range` | `[first, last]`: remove an inclusive sibling run and its subtrees in one revision. Either identity at both ends; different pages, different parents and reversed order are refused. Answers page metadata. |
| `logseq.Graph.moveBlock` | `move_block` | Move a subtree to one of the four anchor positions. Cross-page moves accept either identity at both ends and retain write-intent recovery through pre-hash-bound paths or durable ids. Answers page metadata. |
| `logseq.Graph.upsertBlockProperty` | `upsert_block_property` | Set one `key:: value` line; `id::` is the explicit way to grant durable identity. Answers the committed block view. |
| `logseq.Graph.ensureBlockId` | `ensure_block_id` | Explicitly grant Logseq-format durable identity; existing `id::` is a no-op with no revision advance. Pinned by `ensure_block_id_explicitly_grants_identity_and_reports_a_noop_on_repeat`. |
| `logseq.Graph.removeBlockProperty` | `remove_block_property` | Remove one named property line; an absent property is a refusal, not a no-op. Answers the committed block view. |
| `logseq.Graph.setBlockCollapsed` | `set_block_collapsed` | Write the selected block's `collapsed::` property from a boolean. Answers the committed block view. |
| `logseq.Graph.deletePage` | `delete_page` | Move the page file into `logseq/.recycle/`, then remove its projection. Answers the deleted page's metadata. |
| `logseq.Graph.commitPage` | `commit_page` | Apply an ordered positional batch under `baseVersion`, with one file replacement and projection. Answers the committed page snapshot, `applied` count and read metadata. |

Every successful answer carries the library's actual revision and read metadata; read-your-writes
for the written page does not claim that unrelated pages were refreshed. A block selector accepts
the same durable and ephemeral spellings as the reads. An ephemeral selector keeps the library's
implicit source-fingerprint guard; it does not acquire an RPC-supplied replacement version.

`commitPage` takes the opaque page-file version returned by `getPageSnapshot` as `baseVersion`, not
the graph revision or an index version. Operations execute in array order against the candidate
left by the preceding operation. A stale version or any refused operation rejects the whole batch,
including an empty batch's version guard, before source bytes change. The adapter does not apply a
prefix, split the batch, retry operations, rebase paths or merge conflicts. A stale error carries
the current page snapshot so the caller can recompute its paths.

All failures retain structured `code`, `message` and `data`. `data.write_state` is `unwritten`
for a refusal before the file commit and `file_changed` for a partial move, re-read-after-write,
index-after-write or source durability failure after the file was made visible. The library's
`WriteError::write_state()` is the only classifier; the adapter only renders it. The actionable
library facts identify the error class and, where supplied, its page, operation `index` and variant,
ids, paths or current snapshot. A stale error
states `baseVersion` and `current`, where `current` has the snapshot shape. None of the post-file
failures becomes success or an automatic rollback. Startup scanning and open failure refuse writes
without queueing or replaying them.

The official client branches on the transport status; this repository's `code` and `data` remain the
contract for its own clients. A missing or refused bearer token is `-32001`, refused write parameters or
edits are `-32602`, missing pages or blocks are `-32004`, `PageExists` / `AmbiguousBlockId` /
`StaleVersion` are `-32009`, lock contention is `-32003`, and other source, engine or index failures
are `-32603`.
