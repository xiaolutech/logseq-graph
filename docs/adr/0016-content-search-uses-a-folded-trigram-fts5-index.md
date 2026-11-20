# ADR-0016: Content search uses folded FTS5 indexes

Status: Accepted
Date: 2026-11-19

## Context

ADR-0007 makes block text derived searchable state and defines literal substring matching through
`domain::search_key`. Its scan-only execution has no ranking and folds every candidate block for a
non-ASCII needle, including Chinese. Content search needs ranked hits without introducing word
boundaries that would prevent matching unsegmented Chinese text. This decision supersedes only
ADR-0007's search execution and ordering, including its no-FTS bullet; it keeps the text, fold,
snippet, result-shape, and freshness contracts.

## Decision

- Content search (`Graph::reads().search`, including `logseq.App.search` hits) uses SQLite FTS5's
  built-in `trigram case_sensitive 1` tokenizer over `domain::search_key(blocks.text)`. Folding
  happens before tokenization, not inside a second tokenizer-specific case rule. A second index
  uses `unicode61 remove_diacritics 0` over derived CJK unigram and adjacent-bigram tokens.
- Both indexes are contentless with `contentless_delete=1`: block text remains in `blocks.text`, and
  FTS rowids are block rowids. Insert, delete, and text-update triggers maintain it in the same
  transaction as the block projection. Every block-writing connection registers the same
  `search_key` and `search_cjk_tokens` SQL functions. Rebuilding the disposable index projects
  the files again (ADR-0002).
- A folded needle of at least three characters is one quoted FTS phrase, with embedded quotes
  doubled. Operators and punctuation are literal text. Consecutive trigrams enforce the same
  contiguous substring match as `instr` over folded text: no stemming, word boundaries, fuzzy
  matching, or normalization is added.
- A folded needle of one or two characters, all CJK under the predicate below, uses the second
  index. It emits a `u`-prefixed token for every CJK scalar and a `b`-prefixed token for every
  adjacent CJK pair: 中文 becomes `u中 b中文 u文`. Each maximal non-CJK run emits one filler
  token and resets adjacency, so neither 中x文 nor 中，文 emits `b中文`. A one-character
  query matches its unigram token; a two-character query matches its bigram token.
  `search_cjk_tokens` applies the same per-character search fold before deriving tokens.
  Tokenizer categories include all Unicode categories, with space as an explicit separator:
  Han introduced after unicode61's Unicode version and kana combining marks must remain tokens.
  No diacritic removal, segmentation dictionary, normalization, or custom tokenizer FFI is used.
- Both indexed paths use an unranked probe reading at most 101 matching rowids. With at most
  100 matches, hits use full
  FTS5 BM25 ranking, best first, before the SQL limit; ties are unspecified. Above 100 matches,
  hits use ascending FTS rowid order without evaluating rank. This is an unranked overflow
  fallback, not best-of-first-100 ranking, and 100 is not a result-count limit: the caller's
  limit still applies independently. Rank is not exposed in `SearchHit` or the RPC answer.
- Other one- and two-character needles, including mixed queries such as `a中`, retain the bounded
  `instr` scan. ASCII needles use SQLite's equivalent `lower`; other needles use `search_key`.
  Their hit order remains unspecified scan order. Empty queries and zero limits return no hits.
- Snippets still use the original block text, the existing character-preserving fold, and the
  bounded context window with ellipses. The simple-query DSL's content leaves are unchanged.

### CJK predicate

`index::cjk_search::is_cjk` is an explicit scalar-range predicate, not a locale or normalization
rule. It includes these ranges (endpoints inclusive, including reserved slots inside each range):

- Han and radicals: U+2E80–2EFF, U+2F00–2FDF, U+3400–4DBF, U+4E00–9FFF, U+F900–FAFF,
  U+20000–2A6DF, U+2A700–2EE5F, U+2F800–2FA1F, U+30000–3347F. These cover unified
  ideographs, extensions A–J, and compatibility ideographs.
- Kana: U+3041–3096, U+3099–309F, U+30A1–30FA, U+30FC–30FF, U+31F0–31FF,
  U+FF66–FF9F, U+1AFF0–1AFFF, U+1B000–1B16F, including halfwidth and combining forms.
- Hangul syllables and Jamo: U+1100–11FF, U+3130–318F, U+A960–A97F, U+AC00–D7FF.
- Ideographic iteration/zero and tone marks: U+3005–3007, U+302E–302F, U+3031–3035, U+303B.

Spaces and punctuation such as ，、・ are boundaries, not CJK. Characters outside these ranges
remain searchable through the short-query scan or, for longer needles, the trigram index.

## Consequences

The built-in tokenizers give exact substring matching without custom tokenizer code or a
segmentation dictionary. The price of the extra indexes is storage and projection work.
SQLite's BM25 initializes IDF by counting all phrase matches, even when a subquery limits the
ranked candidates. Limiting candidates alone therefore does not bound the expensive initialization.
The unranked overflow fallback avoids that cost, while retaining exact BM25 for smaller hit sets.
Threshold measurements started at 1000: probe plus full ranking of exactly 1000 matching blocks
took 1.67 ms on the synthetic graph. Lowering the threshold to 100 reduced common-term probe
overhead; full ranking at exactly 100 took 0.61–0.62 ms. A threshold of 32 saved essentially
nothing further in the controlled probe. We choose 100; the complete fresh-index benchmark
returns common synthetic Latin hits in 0.33–0.34 ms, not the old scan's 0.06 ms. The fallback
removes the 79 ms spike, but does not promise scan-equivalent latency.

The same-machine before/after measurements, including cold projection, checkpointed index size,
Latin and Chinese hit/miss searches, and short-query fallback, are recorded in
[PERFORMANCE.md](../../PERFORMANCE.md#folded-trigram-search-linux-vm).
On the pinned public corpus the index grows about 2.1×; on 60000 synthetic mixed CJK/Latin blocks
it grows about 1.6× and cold projection costs roughly 4×. The initial always-ranked implementation
took about 79 ms for a synthetic Latin term matching all 60000 blocks, versus 0.06 ms for the old
scan. Profiling showed a roughly 5 ms BM25 initialization cost even with just one ranked candidate.
This is why the overflow path skips rank entirely, rather than ranking a capped candidate set.
No custom bigram/character tokenizer was benchmarked: choosing the built-in tokenizer avoids that
additional code and semantic surface.

For short CJK search, two derived streams were measured with production projection triggers
on the same 60000-block synthetic graph, enriched with common 的/是/我们 and one-block rare
terms. Alternative U emits one CJK scalar per token and one filler per maximal non-CJK run;
two-character queries use an adjacent-unigram phrase. Chosen B emits prefixed unigrams and
bigrams and answers two-character queries with one token. B's total index is about 30.91 MB
versus U's 28.85 MB (about 7% more); 中文 takes 0.197–0.198 ms versus U's 0.327–0.328 ms,
and 我们 takes 0.190–0.196 ms versus U's 0.255–0.258 ms. Cold projection ranges overlap;
the choice favors two-character lookup speed for a modest additional total storage cost.

Against main's scan, B changes a one-block 龘 hit from about 70 ms to 0.127 ms and a miss
from about 70 ms to 0.098 ms. Common early-hit scans are still faster: 的 takes about
0.119 ms on main versus 0.190 ms with B. Latin search is unchanged within measurement noise.
These measurements and the enrichment recipe are in
[PERFORMANCE.md](../../PERFORMANCE.md#short-cjk-search-linux-vm).

The 100-match cutoff applies to the CJK index too. With 60000 matching blocks, full BM25 for
的/是/我们 takes 38.61–39.57 ms on B, versus 0.038–0.042 ms for the bounded probe and
0.138–0.170 ms for probe plus unranked hits. U also costs 35.63–49.20 ms when fully ranked.
Built-in BM25 remains match-count-sensitive, so common short CJK searches skip ranking just
like common trigram searches; smaller match sets retain full BM25.
