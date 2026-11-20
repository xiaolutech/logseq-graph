use crate::domain::{PageCandidate, QueryContext, QueryExpr, QueryResult, Read, SearchHit};
use crate::engine::{EngineError, Freshness, Graph, Reads};
use crate::index::QueryExecError;

impl Graph {
    /// Blocks whose projected text contains `query`, at most `limit` of them.
    ///
    /// This is a literal substring search over the block text stored from the last scan, not a word,
    /// prefix, or fuzzy search. Case is folded through [`search_key`](crate::domain::search_key),
    /// which folds Unicode one character at a time without changing the character count, so
    /// `ärger` finds `Ärger` and the snippet window cuts the right characters. That is deliberately
    /// different from page identity, which folds Unicode through
    /// [`page_name_key`](crate::domain::page_name_key): identity may fold `İ` into two characters
    /// and search must not, because its match position names a position in the stored text. A query
    /// of at least three characters uses a case-sensitive trigram FTS5 index over folded text.
    /// One- and two-character queries consisting entirely of Han, kana, or Hangul use a second
    /// FTS5 index of folded CJK unigram/bigram tokens; non-CJK characters break bigram adjacency.
    /// Other short queries scan with the same fold (or SQLite's equivalent `lower` for ASCII).
    ///
    /// `limit` bounds the number of hits and is part of the SQL, so a query never reads every match
    /// into memory. A `limit` of zero answers with no hits and an empty query never matches, because
    /// a substring search with an empty needle would otherwise return every block; there is no
    /// unbounded form of this read.
    ///
    /// Indexed queries probe at most 101 matches without ranking. At most
    /// 100 matches are ranked by BM25; more matches use FTS rowid order without ranking, and the
    /// caller's limit still applies independently. Scan-fallback queries use unspecified order.
    /// Each hit names its page as
    /// written and the block's preorder position in that page, with a bounded snippet around the
    /// match. The preorder is a position in the revision this read reports, not a stable block
    /// handle (ADR-0005), so a caller relocates by reading the page tree at the same revision. The
    /// result says what the last scan stored: a file changed since that scan becomes searchable only
    /// after the next scan, which is what the freshness policy (ADR-0006) controls.
    pub fn search(&mut self, query: &str, limit: u32) -> Result<Read<Vec<SearchHit>>, EngineError> {
        self.read(|index| index.search(query, limit))
    }

    /// Pages a name lookup can jump to, ordered with page names ahead of aliases.
    ///
    /// Unlike [`Graph::search`], which answers blocks whose text contains a substring, this
    /// answers pages: a page's own name and the values of its `alias::` property are both
    /// first-class spellings. The answer is a list, not one page, because a shared alias names
    /// several pages. The first entry is the default jump; the rest are alternatives a caller can
    /// offer. See [`crate::index::Index::page_candidates`](crate::index::Index::page_candidates) for the order.
    pub fn page_candidates(
        &mut self,
        text: &str,
        limit: u32,
    ) -> Result<Read<Vec<PageCandidate>>, EngineError> {
        self.read(|index| index.page_candidates(text, limit))
    }

    /// Answer one Logseq simple query against the projection.
    ///
    /// The text is the DSL itself, not the surface that holds it: a `{{query …}}` macro body or a
    /// `#+BEGIN_QUERY` block has already been stripped by whoever read the file.
    ///
    /// The query is parsed **before** the freshness policy runs, so a syntax error or an
    /// unsupported form costs no scan: there is nothing to read when the question itself is not a
    /// question this build runs. A query that parses but matches nothing is a successful read with
    /// an empty result — the two outcomes are deliberately different, because "no block matches"
    /// and "this build cannot run your query" call for different reactions.
    ///
    /// `context` carries the day relative operands count from and the number of results the caller
    /// will accept. Neither has a default: a library that read the clock would answer differently
    /// on two machines, and one that guessed "no limit" would let `(not …)` transfer a whole graph.
    ///
    /// Every block in the answer carries a [`crate::domain::BlockSelector`] — the
    /// durable id its file states, or the ephemeral id the projection minted (ADR-0005, ADR-0012) —
    /// so an answer can be fed straight back to a write.
    pub fn query(
        &mut self,
        text: &str,
        context: &QueryContext,
    ) -> Result<Read<QueryResult>, EngineError> {
        let expr = QueryExpr::parse(text)?;
        self.query_expr(&expr, context)
    }

    /// Answer an already-parsed query.
    ///
    /// Same read as [`Self::query`], for a caller that parses once and re-runs the same question —
    /// an editor redrawing a query block, or a loop that refreshes the graph between answers.
    pub fn query_expr(
        &mut self,
        expr: &QueryExpr,
        context: &QueryContext,
    ) -> Result<Read<QueryResult>, EngineError> {
        self.query_expr_with(self.freshness, expr, context)
    }

    /// Answer a parsed query under one freshness policy.
    ///
    /// Separate from [`Self::query_expr`] so a [`Reads`] builder can answer under its own policy,
    /// the way every other read in that builder does.
    fn query_expr_with(
        &mut self,
        freshness: Freshness,
        expr: &QueryExpr,
        context: &QueryContext,
    ) -> Result<Read<QueryResult>, EngineError> {
        self.read_answer(freshness, |index, _| match index.query(expr, context) {
            Ok(value) => Ok(value),
            Err(QueryExecError::Query(error)) => Err(EngineError::Query(error)),
            Err(QueryExecError::Index(error)) => Err(EngineError::Index(error)),
        })
    }
}

impl Reads<'_> {
    /// Answer one Logseq simple query under this builder's freshness policy.
    ///
    /// Same read as [`Graph::query`] and the same parse-before-scan rule: an unusable query costs
    /// no scan.
    pub fn query(
        &mut self,
        text: &str,
        context: &QueryContext,
    ) -> Result<Read<QueryResult>, EngineError> {
        let expr = QueryExpr::parse(text)?;
        self.graph.query_expr_with(self.freshness, &expr, context)
    }

    /// Answer an already-parsed query under this builder's freshness policy.
    ///
    /// Same read as [`Graph::query_expr`], under this builder's freshness policy.
    pub fn query_expr(
        &mut self,
        expr: &QueryExpr,
        context: &QueryContext,
    ) -> Result<Read<QueryResult>, EngineError> {
        self.graph.query_expr_with(self.freshness, expr, context)
    }

    /// Blocks whose projected text contains `query`, at most `limit` of them.
    pub fn search(&mut self, query: &str, limit: u32) -> Result<Read<Vec<SearchHit>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.search(query, limit))
    }

    /// Same answers, same errors, and same freshness rule as [`Graph::page_candidates`].
    pub fn page_candidates(
        &mut self,
        text: &str,
        limit: u32,
    ) -> Result<Read<Vec<PageCandidate>>, EngineError> {
        self.graph
            .read_with(self.freshness, |index| index.page_candidates(text, limit))
    }
}
