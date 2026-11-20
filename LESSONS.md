# Lessons

This log starts empty. Do not import an earlier implementation's lesson history.

Record recurring implementation failures here as evidence appears. Each entry should state the
rule, the reason it matters, and concrete evidence such as a commit or test. Promote repeated rules
to an appropriate ADR or automated check rather than treating this file as an architecture manual.

## How to use this file

Read this file before changing code. Write to it after a change lands, not while it is in flight:
review what the commits since this file last changed actually taught.

```
git log --oneline $(git log -1 --format=%H -- LESSONS.md)..HEAD
```

An entry seen once may be an accident. The same rule seen on three separate days is a recurrence,
and that is when it stops being a lesson and starts being a rule:

- If a machine can check it, add the check; the promoted line names the test.
- If it is a decision a person has to make each time, amend the ADR that already owns that area.
- Then replace the entry with one line under `**An unindexed foreign key child column turns every parent delete into a full scan, silently.**
- Why: `blocks.parent_id REFERENCES blocks (id) ON DELETE CASCADE` had no index, so deleting a block
  made SQLite scan the whole `blocks` table to find its children. Replacing one page deletes every
  block of that page, so a write cost `blocks on the page x blocks in the graph`: 17.6 s on a
  1229-block page of the reference graph, 85.7 s to delete the blocks of a 6187-block page. Nothing
  warns, nothing fails, and the plan only says so if you ask: `EXPLAIN QUERY PLAN SELECT id FROM
  blocks WHERE parent_id = 5` was `SCAN blocks`.
- Evidence: `13e2f9b`. Same database, same statement, only the index differing: 85.69 s -> 0.08 s.
  Test: `a_block_delete_finds_children_through_an_index_instead_of_scanning_the_table`. The audit that
  should precede the next cascade column: every FK child column must be the leftmost column of some
  index, and every other one here already is (`pages.relative_path` UNIQUE, `blocks.page_id`,
  `page_properties.page_id`, `block_properties.block_id`, `page_refs.block_id`, `block_refs.block_id`,
  `tags.block_id` — each the leftmost column of a primary key or unique constraint).
- Seen 1x (2026-10-30).

**A performance claim is only as good as its input: measure the shape a user actually writes to.**
- Why: `append` was measured on a one-block page and reported as 0.30 s with "the write itself is
  milliseconds". True for that page and false for the graph: 123 pages of the reference graph have a
  hundred blocks or more, and the same call cost 17.6 s on a 1229-block page. The smallest page hid a
  cost that grew with both the page and the graph, and two slices of optimisation were aimed at the
  part that was already fast.
- Evidence: 2026-10-30, `3862cf8` and `539d1bf` measured on `A Cloud Guru` (1 block); `13e2f9b` fixed
  what that hid. Before reporting a write cost, pick the page by block count from the index
  (`SELECT name, block_count FROM pages ORDER BY block_count DESC`) and report the largest one too.
- Seen 1x (2026-10-30).

## Promoted` naming where the rule now lives.

Three sightings on one day count as one. Keep the line under `## Promoted` instead of deleting it:
without the trace, the same failure comes back as a fresh `Seen 1×` and never reaches three.

## Entry format

```markdown
**The rule, as one sentence.**
- Why: what breaks when this is violated.
- Evidence: commit hash, test name, or date. What the wrong version looked like.
- Seen 3× (2026-11-02, 2026-11-14, 2026-12-01).
```

Dates are ISO and ascending.

## Entries

**A helper reused outside its fixtures is only as general as the fixtures that pinned it.**
- Why: `crates/server/tests/rpc.rs` decoded chunked HTTP bodies by slicing a `&str` at byte offsets.
  Its fixtures were small and ASCII, so no chunk boundary ever landed inside a multi-byte character;
  the served benchmark's 14182-page `getAllPages` answer is chunked above 32 KiB and holds Chinese
  page names, and the same decoder panicked at `end byte index 8192 is not a char boundary`. The
  bug was in the test helper, not the server. The harness now dechunks bytes like the benchmark,
  and two tests pin the case the fixtures missed.
- Evidence: 2026-11-14, `8bad1b0` (`crates/server/examples/measure_serve.rs`'s `dechunk(&[u8])`);
  the first large-graph attempt aborted at `getAllPages`. Fixed on 2026-11-15 in
  `crates/server/tests/rpc.rs`: `dechunk_joins_a_utf8_character_split_across_chunks` builds a cut
  inside a three-byte character, and `get_all_pages_reads_a_chunked_answer_larger_than_32_kib`
  makes the real server produce such an answer. Both failed with the old helper before the fix.
- Seen 1× (2026-11-14).

**Measure the piece you intend to remove, not the operation you intend to speed up.**
- Why: a batch commit costs 12.9 ms per operation on the largest page, and the obvious reading — two
  whole-page parses per operation — was wrong. `PageDocument::parse` of 6192 blocks is 0.69 ms;
  `PageFacts::extract` is 5.06 ms and every operation runs it twice. A change built on the wrong
  premise (hand the re-parse check's document back to the next operation) removed 6 ms out of 269
  and measured as nothing. The component numbers take five minutes to obtain and they decide whether
  the change is worth writing at all.
- Evidence: `perf/batch-parse-reuse`, 2026-11-04: ten operations 268.68 ms before, 269.25 ms after,
  inside the ±5 ms the same run produced. The component table is in `PERFORMANCE.md` under
  structural costs. The change was discarded rather than merged as "strictly less work": it changed
  twelve public signatures to move no number.
- Seen 1× (2026-11-04).

**A recorded observation must never be newer than the content it describes.**
- Why: a baseline holding a modification time taken after the read can pair a newer time with older
  text. The next start matches the baseline, skips the file, and serves a page that will stay stale
  until the file changes again. Nothing reports it: the read succeeds, the tests are green.
- Evidence: `35d6b0a` took the stat after `read_text` and its comment claimed the opposite direction;
  `48f65a8` records the pre-read observation and deletes the API that could stat after a read, so the
  wrong order cannot be written again. Test:
  `a_recorded_baseline_keeps_the_observation_taken_before_the_read`.
- Seen 1× (2026-10-30).

**When two layers must agree on when two names denote the same thing, the rule lives in exactly one
function.**
- Why: three answers to "is this the same page" (Unicode `to_lowercase` in `source`, SQLite
  `COLLATE NOCASE` — ASCII only — on the ref and tag columns, a third rule for property keys) made
  the backlink join `pages.name = page_refs.target_name` neither correct nor indexable: the plan was
  `SCAN` and the join matched nothing for names that differ only in case. Every layer was individually
  reasonable and every test was green.
- Evidence: `6e3f553` and `5759889`; `domain::page_name_key`. Tests:
  `a_backlink_from_another_page_is_found_through_the_page_key`,
  `a_backlink_join_searches_on_both_sides_instead_of_scanning`.
- Seen 1× (2026-10-30).

**A read that cannot answer must say so; an empty result for "I did not look there" is a silent
wrong answer.**
- Why: `page_properties` reads the page preamble only. Logseq also accepts page properties on the
  first block, and that form projects into `block_properties`, so the call answers `Some([])` and the
  caller concludes the page has no properties. A missing answer that looks like an answer is worse
  than an error.
- Evidence: `9941681` documents the boundary and pins it with a test that asserts today's behaviour,
  named so that nobody reads it as the intended end state.
- Seen 1× (2026-10-30).

**A subagent report is a claim, not evidence: verify the direction words, not only the numbers.**
- Why: `35d6b0a` reported that it had changed the stat order to the safer direction. The code took
  the stat after the read, which is the unsafe direction, and the comment asserting safety was wrong
  too. Read the code for any claim of the form "this order is safe because …".
- Evidence: `35d6b0a` versus `48f65a8`.
- Seen 1× (2026-10-30).

**Green gates say nothing about the two most expensive defect classes: wrong order and duplicated
identity rules.**
- Why: 109, 130, and 166 tests were green while the baseline order was reversed and three folding
  rules disagreed. Neither defect fails a test written from the same misunderstanding, and neither
  produces an error at runtime.
- Evidence: `48f65a8`, `6e3f553`. Both were found by an independent read of the diff, not by the suite.
- Seen 1× (2026-10-30).

**When you report a cost, use the denominator a reader would use.**
- Why: "the text column adds 78%" is true and misleading; "the index is 2.1 times the source bytes,
  effectively a second copy of the text" uses the same numbers and tells the truth. Choosing the
  flattering denominator needs no false number.
- Evidence: `08d8e13` compared against a textless index; `42c4ecc` adds the source-size comparison.
- Seen 1× (2026-10-30).

**Pin a known limitation with a test that asserts the limitation, and name the test so it cannot be
read as a goal.**
- Why: an undocumented limitation gets "fixed" by the next person with a worse heuristic. Without a
  test, "a same-size file with a restored mtime is not detected" invites a full re-read of every file
  on every start, which removes the reason the baseline exists.
- Evidence: `a_change_that_keeps_size_and_modification_time_is_not_detected` in
  `tests/engine_freshness.rs`; `a_first_block_property_stays_a_block_property_for_now` in
  `9941681`.
- Seen 1× (2026-10-30).

**`EXPLAIN QUERY PLAN` is not a performance gate: check what the plan reads, not what it is called.**
- Why: a tautological predicate turned `SCAN tags USING INDEX tags_by_name` into
  `SEARCH tags USING INDEX tags_by_name (name_key>?)` while reading the same rows for the same cost.
  Accepting the plan string as proof rewards an expression that buys nothing.
- Evidence: `9941681` removes the predicate after measuring both plans.
- Seen 1× (2026-10-30).

**A branch created before a history rewrite must be rebased onto the new base, never merged.**
- Why: `b789869` added a working-state file that then had to leave the repository, so `main` was
  rebuilt from `448a8ca` with the same tree and new commit ids. Merging a branch based on the old tip
  re-imports the removed commit and the file with it.
- Evidence: `17f189b`; every branch opened before the rewrite was rebased with
  `git rebase --onto main <old-base>` first.
- Seen 1× (2026-10-30).

**Integration-test fixtures get copied silently, once per new test file.**
- Why: `TempGraph` existed in five files before anyone counted: each new test file copied the last
  one, and the fifth copy was found only while consolidating the other four. The duplication is
  invisible in review because every copy looks locally reasonable.
- Evidence: `8c68392` moves the fixture into `tests/support` and adds tests for its cleanup and
  naming. New integration tests use it.
- Seen 1× (2026-10-30).

**A slice that leaves five decisions to the implementer exhausts its context searching instead of
writing.**
- Why: the first `block-identity` dispatch asked for domain types, a schema column, a projection
  change, an engine read method, and a `BlockNode` decision, and answered "you decide" five times.
  After 40 minutes it had zero changed files, zero commits, and a 99.9%-full context of greps in files
  that never contained the thing it was looking for. Broad latitude plus many files buys exploration,
  and exploration is what runs out first.
- Evidence: 2026-10-30, `block-identity` interrupted with `esc esc c-c`. The replacement slice named
  the four files to read, fixed the interface itself, and restricted the change to one new module.
- Seen 1× (2026-10-30).

**A comparison that can never be false is not a guard.**
- Why: `project_file` skipped a file when `baseline.kind() == file.kind() && baseline.content_hash()
  == recorded.content_hash()`. The baseline row is keyed by `relative_path`, and `relative_path` is
  `<pages|journals>/<file>`, so both kinds were a function of the same string and the first half
  could not be false. It read like a change detector for a file moved between the two directories,
  which is exactly the case it could not detect: a move changes the path, so the row it would
  compare against no longer exists. Nothing failed, and the dead comparison kept
  `file_baseline.page_kind` looking load-bearing, so it survived an audit that reasoned about it
  from the code rather than from what makes it false.
- Evidence: found while adding `pages.kind` (`769e385`); the column is gone and the skip test
  compares fingerprints only.
- Seen 1× (2026-11-04).

**A test that builds throwaway repositories inherits the caller's git identity.**
- Why: `check_perf_doc.py --self-test` runs `git init` / `add` / `commit` in a temporary directory
  and only changed `cwd`. A linked worktree's pre-commit hook exports an **absolute** `GIT_DIR` and
  `GIT_INDEX_FILE` (a plain repository exports the relative `.git/index`, which is why the same hook
  passed there). Inherited, those variables sent every throwaway `git add` into the real worktree's
  index and alternated the real `.git/config`: a commit aborted mid-hook, `README.md` was staged as
  a single line, and the config gained `user.name = self test`, `user.email = self-test@example.com`,
  `commit.gpgsign = false`, and `core.bare = true`. The last one is the expensive one — every later
  git command in that clone fails with "this operation must be run in a work tree", and because the
  local identity overrode the global one, every commit made after that carried the wrong author.
  A subprocess that wants its own repository must clear the repository-selection variables, not just
  change directory.
- Evidence: `f51d833` gives `TemporaryRepo` a `clean_git_env()` that drops every `GIT_*` variable and
  adds a regression case that sets an absolute `GIT_DIR` + `GIT_INDEX_FILE` and asserts neither path
  is created. The damage happened twice on 2026-11-11, the second time on a branch that rebased onto
  a `main` still carrying the old script — the polluted config outlives the fix, so a clone that
  ever ran the old self-test has to be checked with `git config --local --list`.
- Seen 2× (2026-11-11).

**CommonMark is not Logseq's Markdown dialect; the official parser is the authority, and it costs two lines and one command to ask it.**
- Why: `*` and `+` are valid CommonMark bullets and are *text* to Logseq, which opens a block only on
  `-`. A parser that follows the specification here disagrees with the application it mirrors: on the
  reference graph that is 429 lines in 19 files outside fences that were blocks here and
  continuation text there, and
  four of the five block-count differences the corpus harness found came from this one rule. The same
  trap sits in fence detection: a backtick fence whose info string holds a backtick opens no fence
  (CommonMark 4.5 *and* Logseq agree), and the cost of guessing was a page whose last three lines were
  swallowed into a code block.
- Evidence: `scripts/capture_corpus_parity.sh` output for `pages/Custom theme.md`, `pages/Tasks.md`,
  `pages/Built-in Properties.md`, `pages/term___bullet.md`; the two-line probe graph that answered
  `- a\n* b` in one round trip. Tests: `a_backtick_info_string_opens_no_fence`,
  `only_a_dash_opens_a_block`, `star_and_plus_lines_are_continuations_at_any_indent`.
  The `*`/`+` rule was then adopted, and the reference graph is the measurement that justified it:
  429 such lines in 19 files, every one of them terminal output, an org-mode logbook entry, a code
  fragment, or an import from another outliner, none at zero indentation. The index holds **401 fewer
  blocks** on the same checkout, so the change removed fake entries rather than real ones. The count
  that mattered was not "how many lines lose block identity" but "what those lines actually are" —
  the first number argued against the fix and the second one settled it.
- Seen 1× (2026-11-12).

**Socket deadlines belong to the layer that owns the accepted socket.**
- Why: tiny_http owns its accepted streams and exposes no read/write-timeout setter, while its lazy
  request-body reader runs on the caller's handler thread. Sixteen partial bodies exhausted all
  sixteen RPC workers; one incomplete-body test left fifteen workers free and missed the failure.
  Moving I/O to bounded connection slots with socket deadlines leaves handler slots for complete
  requests, and also bounds clients that never finish a head or never read a response.
- Evidence: `stalled_clients_do_not_exhaust_the_request_workers` timed out after three seconds
  against the unchanged server on 2026-11-19, then passed with the in-crate transport. Transport unit
  tests use millisecond deadlines to verify head/body/write expiry and connection-slot release.
- Seen 1× (2026-11-19).

**Reading the official source is a hypothesis; only a replay of the captured answer is a detection.**
- Why: this surface is judged by whether a real client can use it, and a client reads *shapes* —
  it maps `result.blocks`, branches on `has-more?`, hands `result.blocks[i]['block/uuid']` back to
  `Editor.getBlock`, and formats a journal title from `getUserConfigs.preferredDateFormat`. The
  parity suite compared the engine's *values* against the captured official answers, and the
  adapter's shapes were written by reading the official Clojure source; neither can see a shape that
  the official server states but this one omits. So `logseq.App.search` answered no `blocks` array at
  all (the client's `result.blocks.map(...)` threw and the panel stayed blank), `block/uuid` carried
  a `durable:` prefix (a client hands it straight back, and a bare id 404'd), `getPage.name` was not
  folded, `DB.q` page entries had no `uuid`, and `getUserConfigs` — which the extension's clip path
  calls at start-up — was not served. Every one passed a green test suite. The captured answers had
  been sitting in `fixtures/expected/` since the previous round, unswept by any assertion.
- Evidence: `crates/server/tests/rpc/official_shapes.rs` replays every capture against a sandbox
  server and asserts each official key is stated with a compatible JSON type, with every exemption
  naming its matrix row; it runs in plain `cargo test` (no reference graph), and it also pins the
  extension's call list — the client that found the bug. The search round trip it demanded
  (`app_search_states_the_official_groups_and_a_bare_block_uuid_round_trips`) fails against the old
  `ephemeral:`-prefixed answer. `docs/logseq-api-parity.md` now separates value parity from wire-shape
  parity and names this suite in the rows it pins. The deepest miss was not a field but the whole
  response envelope: the official server returns the bare result value, and this one wrapped it in a
  JSON-RPC object, so the extension's `baseJson` took `resp.json()` as the result itself and read
  `data.blocks` off it as `undefined`. The evidence is the 2026-11-15 probe of the official server
  on `127.0.0.1:12315` (bare success values, Fastify-shaped 401/400 failures, an unknown method
  answered 200 `{error: ...}`) and the wire-shape and CORS tests added beside `official_shapes.rs`.
- Seen 1× (2026-11-15).

**A guard that is right alone can be wrong beside another guard — test the crossing.**
- Why: the transport's loopback-`Origin` refusal and the bearer-token CORS opening each passed
  their own tests, and the composition still refused the exact caller the CORS policy exists to
  admit: an extension page POSTing from `chrome-extension://<id>` with a valid token got a 403 at
  the head, before the token was ever consulted. Only a case crossing both axes — a foreign
  `Origin` *and* a valid `Authorization`, end to end — could see it. When a feature opens a door
  another feature closed, pin the combination, not each in isolation.
- Evidence: `an_extension_origin_calls_with_the_token` in
  `crates/server/tests/rpc/protocol.rs` POSTs the extension's actual head (`chrome-extension://`
  origin + bearer token) and asserts 200 with `Access-Control-Allow-Origin: *`; the foreign-origin
  cases in `transport_guards_check_the_head_and_token_before_dispatch` pin 401 without the token
  and 200 with it.
- Seen 1× (2026-11-19).

## Promoted

None yet.
