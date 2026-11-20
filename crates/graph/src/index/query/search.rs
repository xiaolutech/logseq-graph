use std::borrow::Cow;

use super::row_selector;
#[cfg(doc)]
use crate::domain::page_name_key;
use crate::domain::{search_key, SearchHit};
use crate::index::cjk_search;
use crate::index::IndexError;
use rusqlite::{params, Connection};

/// Characters of block text kept on each side of a search match.
///
/// A snippet is context, not the whole block: a very long block would otherwise make one hit as
/// expensive as the page it belongs to. The width is fixed so the result shape does not depend on
/// how much text the block happens to hold.
const SNIPPET_RADIUS: usize = 48;

/// Rank all matches only when an unranked probe finds at most this many.
///
/// BM25 initializes IDF by counting every phrase match, even inside a candidate-limited query.
/// Above this threshold, hits use FTS rowid order without evaluating rank.
const RANK_MATCH_THRESHOLD: u32 = 100;

const SEARCH_FTS_PROBE: &str = "\
SELECT count(*) FROM (
    SELECT rowid FROM block_search WHERE block_search MATCH ?1 LIMIT ?2
)";

/// Blocks whose text contains the query, at most the caller's limit.
///
/// The match is `instr` on the text with both sides passed through SQLite's `lower`, so it is a
/// literal substring test: no tokenizing, no stemming, no prefix or fuzzy rule, and `%` or `_` are
/// ordinary characters. This statement answers an **ASCII** query: `lower` folds ASCII only, and
/// [`search_key`] never folds a non-ASCII character into ASCII, so for an ASCII needle the folded
/// form and this form agree on every position (a text character that folds into an ASCII needle
/// character must itself be ASCII, and there the two folds are the same character). The
/// non-ASCII statement is [`SEARCH_FOLDED`].
///
/// This is deliberately *not* the page-name rule: page identity uses [`page_name_key`], which
/// folds Unicode as a whole and may change the length. Search answers `ärger` for `Ärger` and
/// keeps `İ` and U+212A as written; identity and text matching are different questions.
///
/// The snippet is cut here rather than in Rust so a huge block never crosses into memory. `?2` is
/// the radius on each side and `?3` is `2 * radius + the query's character count`, which keeps the
/// whole match inside the window. `?4` is the row limit, so SQLite stops reading once it has enough
/// hits; a limit of zero is answered without running the statement at all and an empty query never
/// matches, because `instr(text, '')` would otherwise match every row.
const SEARCH: &str = "\
SELECT pages.name,
       blocks.preorder,
       blocks.durable_id,
       blocks.ephemeral_id,
       substr(blocks.text,
              max(1, instr(lower(blocks.text), lower(?1)) - ?2),
              ?3),
       instr(lower(blocks.text), lower(?1)),
       length(blocks.text)
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
 WHERE instr(lower(blocks.text), lower(?1)) > 0
 LIMIT ?4";

/// The same question as [`SEARCH`], for a needle that is not pure ASCII.
///
/// Both sides go through the same [`search_key`] fold: the caller binds the folded needle as `?1`,
/// and the statement folds each stored text through the SQL scalar function behind that rule. So
/// `ärger` answers `Ärger`, and a position in the folded key is a position in the stored text —
/// which is why the fold never changes the character count (`İ` and U+212A stay as written; see
/// [`search_key_char`]). The parameter order and every other column are identical to [`SEARCH`].
const SEARCH_FOLDED: &str = "\
SELECT pages.name,
       blocks.preorder,
       blocks.durable_id,
       blocks.ephemeral_id,
       substr(blocks.text,
              max(1, instr(search_key(blocks.text), ?1) - ?2),
              ?3),
       instr(search_key(blocks.text), ?1),
       length(blocks.text)
  FROM blocks
  JOIN pages ON pages.id = blocks.page_id
 WHERE instr(search_key(blocks.text), ?1) > 0
LIMIT ?4";

/// Ranked literal substring matches over the pre-folded trigram index.
const SEARCH_FTS: &str = "\
SELECT pages.name,
       blocks.preorder,
       blocks.durable_id,
       blocks.ephemeral_id,
       substr(blocks.text,
              max(1, instr(search_key(blocks.text), ?1) - ?2),
              ?3),
       instr(search_key(blocks.text), ?1),
       length(blocks.text)
  FROM block_search
  JOIN blocks ON blocks.id = block_search.rowid
  JOIN pages ON pages.id = blocks.page_id
 WHERE block_search MATCH ?5
 ORDER BY rank
 LIMIT ?4";

/// Common-term matches in FTS rowid order, without BM25's global phrase-frequency pass.
const SEARCH_FTS_UNRANKED: &str = "\
SELECT pages.name,
       blocks.preorder,
       blocks.durable_id,
       blocks.ephemeral_id,
       substr(blocks.text,
              max(1, instr(search_key(blocks.text), ?1) - ?2),
              ?3),
       instr(search_key(blocks.text), ?1),
       length(blocks.text)
  FROM block_search
  JOIN blocks ON blocks.id = block_search.rowid
  JOIN pages ON pages.id = blocks.page_id
 WHERE block_search MATCH ?5
 ORDER BY block_search.rowid
 LIMIT ?4";

pub(in crate::index) fn search(
    conn: &Connection,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, IndexError> {
    search_with_threshold(conn, query, limit, RANK_MATCH_THRESHOLD)
}

fn search_with_threshold(
    conn: &Connection,
    query: &str,
    limit: u32,
    rank_threshold: u32,
) -> Result<Vec<SearchHit>, IndexError> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let radius = SNIPPET_RADIUS as i64;
    // A query longer than the whole block still gets a window covering its own length, so the
    // match is never cut out of the snippet by arithmetic. The fold keeps the character count,
    // so the query's length is the match's length in either statement.
    let window = 2 * radius + query.chars().count() as i64;
    let needle = search_key(query);
    let short_phrase = cjk_search::phrase(&needle);
    let short_cjk = short_phrase.is_some();
    let phrase = short_phrase.unwrap_or_else(|| format!("\"{}\"", needle.replace('"', "\"\"")));
    let use_fts = short_cjk || query.chars().count() >= 3;
    let sql = if use_fts {
        let matches: i64 = conn.query_row(
            &fts_statement(SEARCH_FTS_PROBE, short_cjk),
            params![phrase, i64::from(rank_threshold) + 1],
            |row| row.get(0),
        )?;
        if matches <= i64::from(rank_threshold) {
            SEARCH_FTS
        } else {
            SEARCH_FTS_UNRANKED
        }
    } else if query.is_ascii() {
        SEARCH
    } else {
        SEARCH_FOLDED
    };
    let sql = fts_statement(sql, short_cjk);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = if use_fts {
        stmt.query(params![needle, radius, window, i64::from(limit), phrase])?
    } else {
        stmt.query(params![needle, radius, window, i64::from(limit)])?
    };
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let page: String = row.get(0)?;
        let preorder: u32 = row.get(1)?;
        let fragment: String = row.get(4)?;
        let match_at: i64 = row.get(5)?;
        let total: i64 = row.get(6)?;
        out.push(SearchHit::new(
            page,
            preorder,
            row_selector(row.get(2)?, row.get(3)?)?,
            with_ellipses(fragment, match_at, total, radius, window),
        ));
    }
    Ok(out)
}

fn fts_statement(sql: &'static str, short_cjk: bool) -> Cow<'static, str> {
    if short_cjk {
        Cow::Owned(sql.replace("block_search", "block_search_cjk"))
    } else {
        Cow::Borrowed(sql)
    }
}

/// Mark the edges of a snippet that the window cut off.
///
/// `fragment` is the `substr` result, `match_at` is 1-based, and `total` is the block text's length
/// in characters, so the same arithmetic as the SQL decides whether either edge is missing.
fn with_ellipses(fragment: String, match_at: i64, total: i64, radius: i64, window: i64) -> String {
    let start = (match_at - radius).max(1);
    let end = (start - 1 + window).min(total);
    let mut snippet = String::with_capacity(fragment.len() + '…'.len_utf8() * 2);
    if start > 1 {
        snippet.push('…');
    }
    snippet.push_str(&fragment);
    if end < total {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::query_plan;

    #[test]
    fn search_scans_block_text_and_bounds_the_rows_in_sql() {
        // Search has no index to seek: the question is which text contains a substring, so the text
        // rows must be read. What the plan can promise is that the page is reached by primary key
        // and that no temp b-tree stands between the scan and the limit, which would mean every
        // match was materialized and sorted before the bound applied.
        let plan = query_plan(SEARCH);
        assert!(
            plan.iter().any(|step| step.contains("SCAN blocks")),
            "search must scan the block rows: {plan:?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH pages USING INTEGER PRIMARY KEY")),
            "each hit must reach its page by primary key: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "nothing may sort or materialize every match before the limit: {plan:?}"
        );
        assert!(
            SEARCH.contains("LIMIT ?4"),
            "the row bound must be part of the statement, not applied after retrieval"
        );
    }

    #[test]
    fn ranked_search_seeks_the_fts_index_without_a_temporary_sort() {
        let plan = query_plan(SEARCH_FTS);
        assert!(
            plan.iter()
                .any(|step| step.contains("block_search VIRTUAL TABLE INDEX")),
            "search must use FTS: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "FTS must supply rank order: {plan:?}"
        );
        assert!(SEARCH_FTS.contains("LIMIT ?4"));
    }

    #[test]
    fn common_term_search_uses_fts_rowid_order_without_a_temporary_sort() {
        let plan = query_plan(SEARCH_FTS_UNRANKED);
        assert!(
            plan.iter()
                .any(|step| step.contains("block_search VIRTUAL TABLE INDEX")),
            "search must use FTS: {plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "FTS must supply rowid order: {plan:?}"
        );
        assert!(SEARCH_FTS_PROBE.contains("LIMIT ?2"));
        assert!(SEARCH_FTS_UNRANKED.contains("LIMIT ?4"));
    }

    #[test]
    fn short_cjk_search_seeks_the_token_index_without_a_temporary_sort() {
        for sql in [SEARCH_FTS, SEARCH_FTS_UNRANKED] {
            let plan = query_plan(&fts_statement(sql, true));
            assert!(
                plan.iter()
                    .any(|step| step.contains("block_search_cjk VIRTUAL TABLE INDEX")),
                "short CJK search must use its index: {plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("SCAN blocks")),
                "{plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn short_cjk_overflow_uses_rowid_order_without_capping_the_result_count() {
        let mut index = crate::index::Index::open_in_memory().expect("open index");
        let text = format!("- 书中文 {}\n- 书书书中文中文中文\n", "天地".repeat(100));
        crate::index::test_support::project(&mut index, "Page", &text).expect("project page");
        for needle in ["书", "中文"] {
            let ranked = search_with_threshold(&index.conn, needle, 1, 2).expect("ranked search");
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].preorder(), 1, "full ranking at threshold");
            let overflow = search_with_threshold(&index.conn, needle, 1, 1).expect("overflow");
            assert_eq!(overflow.len(), 1);
            assert_eq!(overflow[0].preorder(), 0, "overflow is not ranked");
            assert!(overflow[0].snippet().contains(needle));
            let all = search_with_threshold(&index.conn, needle, 10, 1).expect("all matches");
            assert_eq!(all.len(), 2, "threshold is not a result cap");
        }
    }

    #[test]
    fn a_failed_short_cjk_probe_is_an_error_not_a_search_miss() {
        let index = crate::index::Index::open_in_memory().expect("open index");
        index
            .conn
            .execute("DROP TABLE block_search_cjk", [])
            .expect("drop CJK index");
        assert!(search(&index.conn, "书", 10).is_err());
        assert!(search(&index.conn, "中文", 10).is_err());
    }

    #[test]
    fn ranking_at_the_threshold_switches_to_rowid_order_above_it() {
        let mut index = crate::index::Index::open_in_memory().expect("open index");
        let text = format!(
            "- needle {}\n- needle needle needle\n",
            "padding ".repeat(100)
        );
        crate::index::test_support::project(&mut index, "Page", &text).expect("project page");

        let ranked = search_with_threshold(&index.conn, "needle", 1, 2).expect("ranked search");
        assert_eq!(ranked.len(), 1);
        assert_eq!(
            ranked[0].preorder(),
            1,
            "all matches at the threshold are ranked"
        );

        let unranked = search_with_threshold(&index.conn, "needle", 1, 1).expect("unranked search");
        assert_eq!(unranked.len(), 1);
        assert_eq!(
            unranked[0].preorder(),
            0,
            "overflow uses rowid order, not rank"
        );
        assert!(unranked[0].snippet().contains("needle"));
        let all = search_with_threshold(&index.conn, "needle", 10, 1).expect("all hits");
        assert_eq!(all.len(), 2, "the threshold must not cap the result count");
        assert_eq!(
            all.iter().map(SearchHit::preorder).collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(search_with_threshold(&index.conn, "absent", 10, 1)
            .expect("no hits")
            .is_empty());
        assert!(search_with_threshold(&index.conn, "", 10, 1)
            .expect("empty query")
            .is_empty());
        assert!(search_with_threshold(&index.conn, "needle", 0, 1)
            .expect("zero limit")
            .is_empty());
    }

    #[test]
    fn a_failed_fts_probe_is_an_error_instead_of_a_search_miss() {
        let index = crate::index::Index::open_in_memory().expect("open index");
        index
            .conn
            .execute("DROP TABLE block_search", [])
            .expect("drop FTS");
        assert!(search_with_threshold(&index.conn, "needle", 10, 1).is_err());
    }

    #[test]
    fn updating_block_text_replaces_its_fts_entry() {
        let mut index = crate::index::Index::open_in_memory().expect("open index");
        crate::index::test_support::project(&mut index, "Page", "- original text\n")
            .expect("project page");
        assert_eq!(
            search(&index.conn, "original", 10).expect("search").len(),
            1
        );
        index
            .conn
            .execute("UPDATE blocks SET text = 'ÄRGER 今天学习'", [])
            .expect("update text");
        assert!(search(&index.conn, "original", 10)
            .expect("search")
            .is_empty());
        assert_eq!(search(&index.conn, "ärger", 10).expect("search").len(), 1);
        assert_eq!(
            search(&index.conn, "今天学习", 10).expect("search").len(),
            1
        );
        index
            .conn
            .execute("DELETE FROM blocks", [])
            .expect("delete text");
        assert!(search(&index.conn, "今天学习", 10)
            .expect("search")
            .is_empty());
    }

    #[test]
    fn direct_updates_and_deletes_refresh_short_cjk_tokens() {
        let mut index = crate::index::Index::open_in_memory().expect("open index");
        crate::index::test_support::project(&mut index, "Page", "- 中文书\n")
            .expect("project page");
        for needle in ["书", "中文"] {
            assert_eq!(search(&index.conn, needle, 10).expect("search").len(), 1);
        }
        index
            .conn
            .execute("UPDATE blocks SET text = '新的文字'", [])
            .expect("update");
        for needle in ["书", "中文"] {
            assert!(search(&index.conn, needle, 10).expect("search").is_empty());
        }
        for needle in ["新", "新的"] {
            assert_eq!(search(&index.conn, needle, 10).expect("search").len(), 1);
        }
        index
            .conn
            .execute("DELETE FROM blocks", [])
            .expect("delete");
        for needle in ["新", "新的"] {
            assert!(search(&index.conn, needle, 10).expect("search").is_empty());
        }
    }

    #[test]
    fn a_snippet_marks_only_the_edges_the_window_cut_off() {
        // The fragment and the positions are what the SQL statement supplies; these cases pin the
        // arithmetic that turns them into a snippet, including both ends of a block.
        assert_eq!(with_ellipses("cdefgh".to_owned(), 5, 10, 2, 6), "…cdefgh…");
        assert_eq!(with_ellipses("abcde".to_owned(), 2, 10, 2, 6), "abcde…");
        assert_eq!(with_ellipses("fghij".to_owned(), 9, 10, 2, 6), "…fghij");
        assert_eq!(
            with_ellipses("abcdefghij".to_owned(), 5, 10, 10, 20),
            "abcdefghij"
        );
    }
}
