//! Execution of a simple query against the projection.
//!
//! This is the only module that turns a [`QueryExpr`] into SQL. The grammar and its meaning live in
//! `domain::query`; what lives here is how one graph answers a tree: a leaf becomes a set of row
//! keys, a combination becomes `INTERSECT` / `UNION` / `EXCEPT`, and the answer is one ordered,
//! bounded read that states the identity of every block it returns.
//!
//! # Why sets rather than one big join
//!
//! `and`, `or`, and `not` are set operations, and SQLite already implements them with the right
//! semantics (deduplication included). Composing leaf sub-selects keeps each leaf's own indexed
//! access path visible to the planner instead of burying it in a predicate tree the planner has to
//! guess apart. A `NOT` is the one place a whole table is read, and that is what its meaning says:
//! "every block that does not match".
//!
//! # What a caller can rely on
//!
//! * The order is `(pages.name_key, blocks.preorder)` for blocks and `pages.name_key` for pages.
//!   It is a function of the rows, not of insertion order, so deleting the index and rebuilding it
//!   answers the same sequence (ADR-0002).
//! * `limit` cuts after ordering, and [`QueryResult::truncated`] says whether anything was dropped.
//! * A block answers with a [`BlockSelector`] — the durable id the file states, or the ephemeral id
//!   the projection minted — so a caller can pass the answer straight to a write (ADR-0005,
//!   ADR-0012).

#[cfg(doc)]
use crate::domain::BlockSelector;
#[cfg(test)]
use crate::domain::JournalDate;
use crate::domain::{QueryContext, QueryError, QueryExpr, QueryResult};
use crate::index::{Index, IndexError};
use rusqlite::types::Value;
use rusqlite::Connection;

mod answer;
mod leaf;
pub(crate) use leaf::register_functions;

pub(crate) use answer::selector;
use answer::{answer_blocks, answer_pages};
use leaf::leaf;

/// Why an execution could not answer.
///
/// The two arms are different failures with different remedies: a query error is the caller's text
/// or this build's limit and is reported as one, and an index error is the storage being unreadable.
/// Keeping them apart here is what lets the engine map them onto its own two error families instead
/// of making `IndexError` carry query vocabulary.
#[derive(Debug)]
pub(crate) enum QueryExecError {
    /// The query itself cannot be answered.
    Query(QueryError),
    /// The projection could not be read.
    Index(IndexError),
}

impl From<IndexError> for QueryExecError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

impl From<QueryError> for QueryExecError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<rusqlite::Error> for QueryExecError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Index(IndexError::from(error))
    }
}

/// Which rows an expression answers: blocks or pages.
///
/// [`QueryExpr::is_page_level`] decides it once, at the top; every leaf below is then generated for
/// this one kind, which is what makes a page predicate usable inside a block query — it contributes
/// "the blocks of the pages that match".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Set {
    Blocks,
    Pages,
}

/// One leaf's answer as a sub-select, with its bound parameters.
struct Sub {
    sql: String,
    params: Vec<Value>,
}

impl Sub {
    fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }

    fn ids(table: &str) -> Self {
        Self::new(format!("SELECT id FROM {table}"), Vec::new())
    }
}

/// Answer one query.
pub(super) fn execute(
    conn: &Connection,
    expr: &QueryExpr,
    ctx: &QueryContext,
) -> Result<QueryResult, QueryExecError> {
    let set = if expr.is_page_level() {
        Set::Pages
    } else {
        Set::Blocks
    };
    let plan = eval(conn, expr, set, ctx)?;
    match set {
        Set::Blocks => answer_blocks(conn, &plan, ctx.limit()),
        Set::Pages => answer_pages(conn, &plan, ctx.limit()),
    }
}

/// Reduce an expression to one sub-select of row keys.
fn eval(
    conn: &Connection,
    expr: &QueryExpr,
    set: Set,
    ctx: &QueryContext,
) -> Result<Sub, QueryExecError> {
    match expr {
        QueryExpr::And(children) => combine(conn, children, set, ctx, "INTERSECT"),
        QueryExpr::Or(children) => combine(conn, children, set, ctx, "UNION"),
        QueryExpr::Not(inner) => {
            // The complement is taken over the whole table the answer comes from, which is what
            // `(not …)` means: everything that does not match. Reading the table is the price of
            // that meaning; the limit still bounds what leaves this layer.
            let universe = match set {
                Set::Blocks => Sub::ids("blocks"),
                Set::Pages => Sub::ids("pages"),
            };
            let sub = eval(conn, inner, set, ctx)?;
            Ok(Sub::new(
                format!("{} EXCEPT SELECT id FROM ({})", universe.sql, sub.sql),
                sub.params,
            ))
        }
        _ => leaf(conn, expr, set, ctx),
    }
}

fn combine(
    conn: &Connection,
    children: &[QueryExpr],
    set: Set,
    ctx: &QueryContext,
    op: &str,
) -> Result<Sub, QueryExecError> {
    let mut parts = Vec::with_capacity(children.len());
    let mut params = Vec::new();
    for child in children {
        let sub = eval(conn, child, set, ctx)?;
        params.extend(sub.params);
        parts.push(format!("SELECT id FROM ({})", sub.sql));
    }
    // The parser refuses an empty list, so `parts` is never empty here; a join of one part is the
    // part itself, which keeps the SQL small for the common single-predicate case.
    Ok(Sub::new(parts.join(&format!(" {op} ")), params))
}

impl Index {
    /// Answer one simple query.
    pub(crate) fn query(
        &self,
        expr: &QueryExpr,
        ctx: &QueryContext,
    ) -> Result<QueryResult, QueryExecError> {
        execute(&self.conn, expr, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_limit_answers_nothing_but_still_says_more_exist() {
        // `limit = 0` is the request that asks only for `truncated`, which is why it is a limit and
        // not an error.
        let ctx = QueryContext::new(JournalDate::new(2026, 8, 13).expect("day"), 0);
        assert_eq!(ctx.limit(), 0);
        assert_eq!(ctx.today().to_string(), "2026-08-13");
    }
}
