# ADR-0013: Simple queries are executed by the index

Status: Accepted
Date: 2026-11-06

## Context

Logseq's simple queries (`{{query …}}`, `(and (task TODO) [[page]])`, `(between -7d today)`) are the
question form people actually write in a graph. The projection already holds every fact such a query
needs — pages with their kind and journal day, blocks with their text, task marker, priority and
position, properties, references, tags — so the answer is a read of the derived model, not a second
index (ADR-0002).

Two ways to answer it were considered. Compiling the query into the *advanced query* Datalog language
and evaluating that means building a Datalog engine into this library, which is a second query model
with its own semantics, and it would not answer `(task TODO)` without the same projection columns
anyway. Answering it in Rust over a full in-memory graph means materialising the graph per query,
which is exactly what ADR-0002 exists to avoid.

## Decision

- **The query grammar is domain state.** `domain::query` owns `QueryExpr`, `QueryError`,
  `QueryContext` and the result types. Parsing is pure: no IO, no clock, no SQL. `is_page_level`
  decides from the tree's shape whether an expression answers pages or blocks, and nothing else
  decides it.
- **`index` is the only executor.** `index::query_exec` turns a `QueryExpr` into SQL: a leaf becomes
  a sub-select of row keys, `and`/`or`/`not` become `INTERSECT`/`UNION`/`EXCEPT`, and the answer is
  one ordered read. SQLite row keys never leave the index, and no SQL string is exposed publicly.
- **`not` is a complement over the whole table** the answer comes from: every block, or every page.
  That is what the predicate means, and it is why `limit` is mandatory.
- **A page-level predicate inside a block query contributes the blocks of the pages it matches.**
  `(and (page-property k v) [[x]])` is therefore legal and has one meaning, not two.
- **A reference answers four things**: the target page's own blocks, the blocks that reference it
  through `[[…]]` or `#…`, every block **under** those (path references are inherited), and the
  blocks of pages whose property values name the target.
- **Property values are interpreted by `document::facts`' rules, not by SQL.** A value can hold
  references, a comma-separated list, or a scalar; a deterministic SQLite scalar function applies
  those rules to candidate property rows selected by key. Matching owner IDs stay inside SQL,
  without a Rust collection or a per-owner bind parameter.
- **`QueryContext` carries the day relative operands count from and the result limit, with no
  defaults.** A library that read the clock would answer differently on two machines; one that
  assumed "no limit" would let `(not …)` transfer a whole graph.
- **A failure is never an empty answer.** A parse failure or an unsupported form is `QueryError`,
  reported before the freshness policy runs, so it costs no scan; a query that parses and matches
  nothing is a successful read with an empty result.
- **Order is a function of the rows**: `(pages.name_key, blocks.preorder)` for blocks and
  `pages.name_key` for pages, so a rebuilt index answers the same sequence.
- **Every matched block states a `BlockSelector`** — the durable id its file states, or the
  projection-scoped ephemeral id (ADR-0012) — so an answer can be handed straight to a write.

## Deliberately not implemented

- **Advanced queries (Datalog).** `[:find … :where …]`, `:inputs`, `:rules`, and a query map are
  refused as `Unsupported`, with the form echoed back. No guessed answers.
- **Clojure in a query.** `:view`, `:result-transform`, and any function value are presentation
  semantics, not graph semantics; this library answers data.
- **`(sort-by …)`, `(sample …)`, `(all-page-tags)`, `(scheduled …)`, `(deadline …)`.** The first needs
  a product-level time model this projection does not have; the second breaks a deterministic read;
  the third has no settled meaning; the last two have no fact layer.
- **A synthetic pre-block result.** Logseq models a page's preamble as a block; this projection does
  not, and inventing one would produce a result with no position and no selector, breaking ADR-0005.
  `(property …)` therefore matches block properties only.
- **Page properties written on a page's first block.** Already a recorded boundary; a
  `(page-property …)` query does not read them.
- **A configurable comma-separated key list.** Values are still matched by reference and by exact
  text; only the built-in list-valued keys (`tags`, `alias`, `topics`) split on commas.

## Consequences

A caller gets `(task TODO)`, `[[page]]`, `(property k v)`, `(between -7d today)` and their boolean
combinations from one read, with an identity it can write through, and a refused query that says why
instead of returning nothing.

The costs are deliberate: `(not …)` reads a whole table, so an unbounded query is impossible by
construction; advanced queries stay unavailable until something needs them; and a query answers from
the last scan, so a file edited since then becomes visible under a freshness policy (ADR-0006), not
by the query itself.

Known differences from Logseq's own answers: the order (a function of the rows, where Logseq's is its
database's internal id order), a refused query instead of `null`, and no pre-block results. Text
matching uses the character-preserving Unicode search fold, while page identity uses the page-name
key. Journal endpoints accept relative dates and validated `YYYYMMDD` integers as well as indexed
journal page names; named endpoints still require a page the graph holds. Integers are an explicit
extension to the pinned upstream helper. Property text does not preserve upstream parsed value
kinds, so the page-name fallback can also fold quoted/config-excluded plain strings; see the
[query semantics reference](../engine-reference.md#simple-query-semantics) for evidence and limits.
