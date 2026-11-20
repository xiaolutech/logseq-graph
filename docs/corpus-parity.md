# Corpus parity with the official graph parser

This is the second differential harness. `docs/logseq-api-parity.md` compares this repository's
answers with what the official **HTTP API** answered for a private graph
(`fixtures/expected/`); this one compares its **projection** with what the official **graph parser**
makes of a public, pinned corpus (`fixtures/corpus/`).

The corpus is [`logseq/docs`](https://github.com/logseq/docs) at tag `v0.9.2`: 294 page files, all of
them readable by whoever clones the tag. The parser is
[`logseq/og`](https://github.com/logseq/og) at commit `6e7afa8e`, the file edition of Logseq, running
`logseq.graph-parser.cli/parse-graph` under `nbb-logseq` — the same entry point its own integration
tests use.

## Why not run its tests

The upstream repositories cannot be pointed at this one. Their tests are in three layers and none of
them speaks the HTTP surface described in `docs/logseq-api-parity.md`:

| Layer | Where | What it actually asserts |
| --- | --- | --- |
| Graph parser | `deps/graph-parser/test/**` in `logseq/og` | pure-function tables (`property_test`, `text_test`, `page_ref_test`, `file_name_test`, `mldoc_test`) and datascript entities after `parse-file` |
| Plugin API | `src/test/logseq/api/**` in `logseq/logseq` | `logseq.api.editor/get_page` and friends called as ClojureScript functions, against state the test transacts itself |
| End-to-end | `e2e-tests/logseq-api.spec.ts` | Playwright driving the UI, calling `window.logseq.api[method]` inside the page |

The HTTP envelope lives in `src/electron/electron/server.cljs` (one `POST /api` route that maps
`logseq.<Namespace>.<method>` to `<namespace>@<snake_case_method>` and forwards it to the renderer's
`POST /api` handler), and the only upstream test that touches it is
`src/test/electron/server_auth_test.cljs`: a table of bearer tokens. No upstream test asks what any
method answers.

Two consequences. Their suite cannot validate this repository's API surface, and running it as-is
against this repository would need a shim that reimplements `logseq.api.*` as HTTP calls — after
which most assertions still fail, because they are written against datascript entity shape
(`:block/title`, `:db/id`, `:block/left`) that ADR-0001, ADR-0002 and ADR-0012 deliberately do not
build. What is reusable is the **corpus**: `docs-graph-assertions` in
`deps/graph-parser/src/logseq/graph_parser/test/docs_graph_helper.cljs` pins counts and frequency
tables for exactly this public graph, and those are facts about Markdown rather than about their
database.

## What is compared

One row per page: the page's graph-relative path, the name it answers to, how many content blocks it
holds, and a digest over those blocks' texts. `crates/graph/tests/corpus_parity.rs` recomputes the
same three facts from this repository and compares.

Three normalization rules make the two sides comparable. Each is a stated invariant, not a widened
threshold:

| Rule | Why |
| --- | --- |
| A leading Markdown heading marker (`#{1,6} `) is removed from every block's text | this repository keeps a block's marker beside its text (ADR-0012), so a heading's text is `Usage` here and `## Usage` there. Answer shape, not corpus content. |
| A page's pre-block is not a content block | the official projection keeps a page's preamble in a pre-block entity; this repository keeps it in the page document (ADR-0013). The pre-block is exactly the entity with no counterpart here. |
| The page digest is order-independent: the wrapping sum of per-block FNV-1a hashes | the official sibling order is a derived `:block/left` linked list, and on this corpus it does not reconstruct into a preorder (measured: two blocks of `pages/Changelog.md` name the same left neighbour). Pinning order against it would pin noise. Preorder is pinned by this crate's own tests instead. |

The digest itself is FNV-1a 64-bit, the hash this repository already uses for content fingerprints,
implemented twice: in `scripts/corpus_parity.cljs` (BigInt) and in the test (wrapping `u64`). Both
assert the published vectors, which is what makes the two comparable rather than coincidental.

## Pinned differences

Nothing is filtered: a page that differs today is listed with the rule that decides it, and a page
that starts differing fails the test with both digests. Measured on the pinned corpus: 274 pages on
both sides, 5,189 content blocks as the official parser counts them.

No page-name, normalized-block-text, or content-block-count differences remain in this corpus.

Two classes were defects here and are fixed:

- **A fenced code block whose info string holds a backtick** (`pages/Custom theme.md`) opened a fence
  and the fence swallowed every line to the end of the page. CommonMark 4.5 does not open a fence
  there and neither does the official parser, so `document` applies that rule.
  `a_backtick_info_string_opens_no_fence` pins it.
- **`*` and `+` opened blocks here.** They are valid CommonMark bullets and are plain text to
  Logseq, which opens a block on `-` alone. Four pages carried such lines (`pages/Tasks.md` 26
  against 29, `pages/Refactoring_of_logseq.md` 9 against 15, `pages/term___bullet.md` 7 against 9,
  `pages/Built-in Properties.md` 21 against 23). Only `-` opens a block here now too, so
  **content-block counts agree on every page of the corpus** and those four pages left both pinned
  lists. `only_a_dash_opens_a_block` and `star_and_plus_lines_are_continuations_at_any_indent` pin
  the rule, including a top-level `*` line becoming preamble.

`PARSER_VERSION` moved to 4 across the two fixes, because in both cases the bytes did not move and
what they project to did. It moved to 5 for the link-label tag rule below, for the same reason:
no block text changed, but the tag rows did.

On the reference graph (14,178 pages) this second rule is the larger one: 429 `*` or `+` lines in 19
files outside fences and `#+BEGIN_*` regions, and every one of them is terminal output, an org-mode
logbook entry, a code fragment, or an import from another outliner — a `curl -v` line such as
`* Connected to example.com (192.0.2.10)`, or `* State "DONE" from "TODO" [...]`. None is an
entry a reader would jump to, so making them text removes fake blocks rather than removing entries.
No such line appears at zero indentation anywhere in that graph.

Version 7 also matches bare `-` continuations, truly empty boundary lines, and unclosed code
fences. The seven corpus text differences are gone; normalized block texts and counts agree on
every corpus page. Exact boundary whitespace and fence behavior are pinned by the
source-string probe below as well. Rendering still retains all original source bytes.

### A synthetic-graph probe: page identity and pre-block properties

Using the official graph parser at `6e7afa8eb040686ff057156ee877193b581dd369`
(the parser pinned by the capture harness), a file stating `title:: Chosen/Parent/Leaf`
produces that name with a file and two namespace parents without files. The filename
does not become an alias. The same file's `alias:: Also known, [[Other Alias]]` produces
two alias entities; `tags:: plain tag, [[Wrapped, Tag]], #hash` produces three tag
entities. `related:: prose [[Unfiled]] #related-tag` produces two target entities, but
`description:: "[[Ignored]]"` produces no `Ignored` page. A first dash block's
`title::` and `alias::` are block properties, not page title or aliases. YAML front
matter also supplies the title and tags.

`page_identity_follows_the_official_pre_block_probe` pins the relevant entities, backlink
and promotion behavior. The corpus test compares every projected file's title against
the original capture rather than maintaining a title-rename exception list.

### Continuation and fence probe

Captured from `logseq/og` at `6e7afa8eb040686ff057156ee877193b581dd369` using
`logseq.graph-parser.cli/parse-graph` under `nbb-logseq`. The table uses escaped source strings
and arrays of `:block/content` in entity creation order, excluding pre-blocks. The matching
source-string regression is `official_parser_continuation_and_fence_projection` in
`document/facts/block.rs`; it also asserts byte-for-byte source rendering.

| Case | Source | Official block texts |
| --- | --- | --- |
| bare | `"-\n  first\n  second\n- next\n"` | `["  first\nsecond","next"]` |
| bare-three | `"-\n  first\n  second\n  third\n- next\n"` | `["  first\nsecond\nthird","next"]` |
| bare-deep | `"  -\n      first\n      second\n      third\n- next\n"` | `["      first\n  second\n  third","next"]` |
| bare-child | `"-\n  first\n  second\n  - child\n- next\n"` | `["  first\nsecond","child","next"]` |
| blank | `"- a\n\n- b\n"` | `["a","b"]` |
| blank-spaces | `"- a\n  \n- b\n"` | `["a\n","b"]` |
| blank-two | `"- a\n\n\n- b\n"` | `["a","b"]` |
| blank-child | `"- a\n\n  - child\n- b\n"` | `["a","child","b"]` |
| blank-middle | `"- a\n  first\n\n  second\n- b\n"` | `["a\nfirst\n\nsecond","b"]` |
| unclosed | `"- ```\n  code\n- sibling\n"` | `["```\ncode","sibling"]` |
| unclosed-child | `"- ```\n  code\n  - child\n"` | `["```\ncode","child"]` |
| unclosed-parent | `"- parent\n  - ```\n    code\n- next\n"` | `["parent","```\ncode","next"]` |
| unclosed-eof | `"- ```\n  code\n"` | `["```\ncode"]` |
| unclosed-child-parent-sibling | `"- parent\n  - ```\n    code\n- sibling\n"` | `["parent","```\ncode","sibling"]` |
| closed | `"- ```\n  code\n  ```\n- sibling\n"` | `["```\ncode\n```","sibling"]` |
| close-after-sibling | `"- ```\n  code\n- sibling\n  ```\n- next\n"` | `["```\ncode\n- sibling\n```","next"]` |
| close-after-child | `"- ```\n  code\n  - child\n  ```\n- next\n"` | `["```\ncode\n- child\n```","next"]` |
| bare-space | `"- \n  first\n  second\n"` | `["\nfirst\nsecond"]` |
| bare-region | `"-\n  #+BEGIN_QUERY\n  first\n  second\n  #+END_QUERY\n"` | `["  #+BEGIN_QUERY\nfirst\nsecond\n#+END_QUERY"]` |
| bare-four-indent | `"-\n    first\n    second\n"` | `["    first\n  second"]` |
| bare-nested | `"- parent\n  -\n    first\n    second\n"` | `["parent","    first\nsecond"]` |
| unclosed-preamble | `"```\ncode\n- b\n"` | `["b"]` |
| bare-property | `"-\n id:: 11111111-1111-1111-1111-111111111111\n- next\n"` | `[" id:: 11111111-1111-1111-1111-111111111111","next"]` |

The official choices are intentionally not made uniform: a bare dash drops its empty marker
line, preserves the first continuation's entire indentation, and strips the dash indentation
plus two bytes on later lines. A dash followed by a space keeps the empty first line instead.
Truly empty lines disappear at block boundaries, but whitespace-only lines and interior blanks
survive. An unmatched code fence does not hide later block markers, even children; a matched
fence can span a sibling marker. All of these measured choices are reproduced here.
Consequently appending after an unmatched code fence now creates a block rather than failing
with `EndsInsideOpaqueRegion`; an unmatched `#+BEGIN_*` region still prevents an append.

To reproduce, install the oracle tools as described under Re-capturing, check out the pinned
parser, install its `deps/graph-parser` npm dependencies, and run this program there with
`npx nbb-logseq -cp test probe.cljs`. Set `source` to any source string above (the graph and
file paths are synthetic; `:files` supplies their bytes):

```clojure
(ns probe
  (:require [logseq.graph-parser.cli :as gp] [datascript.core :as d]))
(def source "-\n  first\n  second\n- next\n")
(let [{:keys [conn]} (gp/parse-graph "/tmp/probe"
                      {:verbose false
                       :files [{:file/path "/tmp/probe/pages/probe.md"
                                :file/content source}]})]
  (println (->> (d/q '[:find [(pull ?b [*]) ...] :where [?b :block/content]] @conn)
                (remove :block/pre-block?) (sort-by :db/id)
                (mapv :block/content) clj->js js/JSON.stringify)))
```

On this VM Maven Central returned HTTP 429; a `central` mirror pointing to
`https://maven-central.storage-download.googleapis.com/maven2/` in `~/.m2/settings.xml`
allowed dependency resolution without changing the pinned parser or its dependencies.

### A synthetic-graph probe: `#` inside a Markdown link label

The corpus harness compares block texts and counts, not tags, so a tag rule needs its own probe. The
probe is a one-file graph run through the same pinned parser (`logseq.graph-parser.cli/parse-graph`,
`logseq/og` at `6e7afa8e`), with `pages/probe.md` holding one block per line below. A block's
`:block/refs` is the official projection's tag answer (a tag refs its tag page); the answers below
are what the parser produced, not an interpolation of Logseq's Markdown dialect.

```text
- [Logseq Office Hours #2: Building processes with queries](https://example.com/)
- [link with #inner tag](https://example.com/x)
- [mid #tag here](https://example.com/m)
- ![alt #img](https://example.com/i.png)
- [pre #[[tag in label]] post](https://example.com/t)
- before #untagged then [label #masked](https://example.com/a) then #afterlink
- bare brackets [x #y] no link
- reference style [label #z][refdef]
- [[[inner page]]](https://example.com/nested)
- [page ref in label [[inner]] plain](https://example.com/p)
- see #2 bare
```

| Shape | Official answer |
| --- | --- |
| a `#` anywhere inside `[label](url)`, an image alt, or a bracketed `#[[...]]` in a label | no tag, no ref |
| `#`s before and after the same label (`before #untagged ... then #afterlink`) | `untagged` and `afterlink` are tags; `#masked` is not |
| bare brackets `[x #y]` without a link | tag `y]` (the name runs on: brackets alone do not suppress) |
| reference style `[label #z][refdef]` | tag `z][refdef]` — **not** a link to this parser |
| `[[[inner page]]](url)` | ref `[inner page`: a link whose whole label is one page ref still refs it (`Nested_link`) |
| `[page ref in label [[inner]] plain](url)` | nothing: a page ref mixed into a label's prose is dropped |
| `see #2 bare` | tag `2` |

Two consequences. First, the unresolved boundary is resolved: `document::facts` masks inline link
labels for **tag extraction alone** (see `mask_link_labels`), leaving page refs and block refs on the
unmasked line, so the `Nested_link` row above is untouched rather than decided by accident. The
reference-style and bare-bracket tag names are garbled on the official side; this repository answers
the names the tag characters spell (`z`, `y`). `a_hash_inside_a_markdown_link_label_is_not_a_tag` and
`a_hash_inside_brackets_without_a_link_still_counts` make both halves of the rule fail loudly.
Second, this is an oracle, not a gate: reproducing it needs the same tooling as
`capture_corpus_parity.sh` (node, babashka, a JDK), and the probe graph lives in this document rather
than in `fixtures/`, which holds only corpus digests. The rule itself needs no tooling at all — the
two tests above pin it from source strings.

## What the official parser reads and this repository does not

`parse-graph` reads **303** files from the corpus root, and the fixture keeps only the 274 that both
sides claim. The rest, named so the exclusion is not silent:

| Files | Count | Why it is out of scope |
| --- | --- | --- |
| `.org` pages under `pages/` and `journals/` | 20 | this repository parses Markdown only |
| Markdown at the graph root (`README.md`, `CONTRIBUTING.md`, `LICENSE.md`) | 3 | the official discovery answers them as pages; this repository answers `pages/` and `journals/` only. A real difference, not covered here |
| `whiteboards/*.edn` | 1 | whiteboard state, not a page file |
| `logseq/*.edn`, `logseq/*.css`, `bb.edn` | 5 | configuration and assets |

## Re-capturing

```text
scripts/capture_corpus_parity.sh [--graph <corpus-root>] [--output <path>] [--with-texts]
```

The script clones both repositories at the revisions named above into a temporary directory, installs
the parser's node dependencies, parses the corpus, and writes `fixtures/corpus/docs-v0.9.2.json`
(40 KB). Both checkouts are removed on exit and neither is modified.

This is an oracle, not a gate. It needs node, npm, a JDK and babashka — the parser's `nbb` program
resolves its Clojure dependencies through the Clojure CLI, which needs a JVM — and none of those are
installed by this repository's gates, whose tools are pinned in `mise.toml`. On a machine that has
them, run it as:

```text
mise exec babashka@<version> java@<version> -- bash scripts/capture_corpus_parity.sh
```

The test that reads the fixture needs none of this:

```text
LOGSEQ_GRAPH_CORPUS=/path/to/docs-v0.9.2 cargo test -p logseq-graph --test corpus_parity
```

With the variable unset every test prints `SKIP=` and passes. A variable that names something other
than a directory fails instead of skipping.

## Licence

`logseq/docs` and `logseq/og` are AGPL-3.0 and this repository is MIT. The fixture therefore holds
digests and counts — this repository's own computation over a public corpus — and never a copy of the
corpus text.

To see *what* differs on a page, re-run the capture with `--with-texts` and an output path outside
the tree. The script refuses a path inside the repository, because that output does hold corpus text:

```text
scripts/capture_corpus_parity.sh --graph <corpus-root> --with-texts --output /tmp/corpus-texts.json
```
