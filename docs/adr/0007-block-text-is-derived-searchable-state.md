# ADR-0007: Block text is derived searchable state

Status: Accepted; search execution and ordering superseded by [ADR-0016](0016-content-search-uses-a-folded-trigram-fts5-index.md)
Date: 2026-10-29

ADR-0016 replaces the scan-only execution, unspecified hit order, and no-FTS decision below.
The derived block text, folding, literal substring, bounded snippet, and freshness contracts remain.

## Context

ADR-0001 makes Markdown files the only owner of product facts, and ADR-0002 makes SQLite a derived
read model that stores no raw source lines and no persistent source maps. Those two decisions leave
search with a choice: read and parse files per query, or store enough derived content to answer a
query inside the read model.

Block text is already a derived value in `document::facts`. For every block, extraction reconstructs
the block's own content from its source lines: the marker and the block's content indentation are
removed, continuation lines are joined with `\n`, and the result is exactly the text the block states.
That value is not a raw source line and not a source map. It carries no line number, no span, and no
file position, and the file can rebuild it at any time by parsing the block again. Schema version 4
stored the structure and the facts extracted from that text but discarded the text itself, so the
read model could not answer which block contains a substring.

## Decision

- Schema version 5 adds `blocks.text`, `NOT NULL`. An empty block stores the empty string, never
  `NULL`.
- The stored value is `BlockFacts::text()` from the same parse that produced the block structure and
  the fact rows. Projection neither re-parses the page nor reads the file again.
- Block text is derived state, not source representation. Raw source lines, line numbers, source
  maps, and spans remain forbidden in SQLite; a parser change rebuilds the column rather than
  migrating it.
- Search is a bounded literal substring match over the stored text. Both sides pass through
  `domain::search_key`, the one rule for text matching: each character folds to its lowercase when
  that mapping is one character and does not turn a non-ASCII character into ASCII, and is kept as
  written otherwise. The fold therefore preserves the character count, so a match position in the
  folded text is the same position in the stored text (the snippet window cuts there). This is not
  the page-name rule: `page_name_key` folds Unicode as a whole, may change the length (`İ`), and
  answers a question about identity, while search answers a question about text. It is not a word,
  prefix, token, or fuzzy match, and it does no normalization (`ß` and `ss` stay different).
- An ASCII query uses SQLite's built-in `lower` instead of the fold: for an ASCII needle the two
  statements answer identically, because only an ASCII character can fold into an ASCII needle
  character. A query containing any non-ASCII character uses the folded form; both paths share the
  folded needle and the same stored text, so content search and a simple query's content leaf agree.
- An empty query matches nothing, because a substring search with an empty needle would otherwise
  return every block. A limit of zero returns no hits, and no unbounded form exists: the limit is
  part of the SQL statement, so the read model never materializes every match.
- A hit carries the page name as written, the block's preorder position, and a bounded snippet with
  `…` marking a truncated edge. Preorder is a position inside the answering revision, not a durable
  block handle; ADR-0005 has not introduced one. A caller relocates by re-reading the page tree at
  the same revision.
- Page-tree reads and `BlockNode` keep their structure-only shape. Block text is reachable through
  search, never through a page read, so reading one page does not pull the text of every block.
- Search runs through the same freshness policy as every other read (ADR-0006). What it answers is
  what the last scan stored; a file changed since that scan becomes searchable after the next scan.
- SQLite's FTS5 is not used. It is a second index with its own tokenizer, ranking, and rebuild
  semantics, and no benchmark in this repository has shown that the bounded scan is too slow.

## Consequences

The read model grows by the block text it stores. On the reference graph (314 files, 6353 blocks,
485,499 bytes of block text), a cold index is 1,163,264 bytes; the same schema with every block text
blanked and vacuumed is 655,360 bytes. The text column therefore adds 507,904 bytes, about 78%, which
makes the index roughly 1.8 times its textless size. The source files on that graph hold 547,309
bytes, so the stored block text is 88.7% of the source bytes and the index is 2.1 times the source
volume: the read model carries what is effectively a second copy of the text. That is accepted because
the copy is derived, disposable, and rebuilt in one scan, and it buys queries that never re-read or
re-parse a file. It is not a claim that the text is free. The same graph's cold start took 264-287 ms
across runs, against 257-282 ms for the schema-4 build on the same machine, a difference inside
run-to-run noise; one bounded search for `logseq` with limit 20 returned 20 hits in 498-549 µs across runs.

The cost of a parser-version change is a full rebuild of this column together with the rest of the
derived state. The stored text is the parse result at the last scan, so it can lag the files until a
scan observes them. Search has no ranking, so results arrive in unspecified scan order, and it has no
word boundary semantics. A full scan still grows with the number of blocks, which is acceptable until
a benchmark shows otherwise; adding FTS5 later is a new decision, not an implied one.
