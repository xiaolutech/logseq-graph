use logseq_graph::domain::{page_name_key, QueryBlock, QueryContext, QueryOutcome, QueryPage};
use serde::Serialize;
use serde_json::{json, Value};

use super::super::answer::{answered, block_id, marker_text, page_id, Selector};
use super::super::call::{one_argument, Call};
use super::super::error::{query_error, RequestError};
use super::super::state::Served;

use logseq_graph::domain::JournalDate;

/// One block a query matched.
#[derive(Serialize)]
struct QueryMatchEntry {
    page: String,
    preorder: u32,
    depth: u32,
    marker: String,
    text: String,
    task: Option<String>,
    priority: Option<String>,
    selector: Selector,
}

/// One page a query matched.
#[derive(Serialize)]
struct QueryPageEntry {
    name: String,
    kind: &'static str,
    journal_date: Option<String>,
}

/// One answered query: both lists and `truncated`, so a caller never reads a prefix as the whole.
#[derive(Serialize)]
struct QueryResult {
    query: String,
    limit: u32,
    /// The day relative operands counted from, `YYYY-MM-DD`.
    today: String,
    truncated: bool,
    blocks: Vec<QueryMatchEntry>,
    pages: Vec<QueryPageEntry>,
}

/// One query match, as the method states it.
fn query_match(block: &QueryBlock) -> QueryMatchEntry {
    QueryMatchEntry {
        page: block.page().to_owned(),
        preorder: block.preorder(),
        depth: block.depth(),
        marker: marker_text(block.marker()),
        text: block.text().to_owned(),
        task: block.task_marker().map(|task| task.as_str().to_owned()),
        priority: block
            .priority()
            .map(|priority| priority.as_str().to_owned()),
        selector: Selector::of(block.selector()),
    }
}

/// One query match of a page-level query, as the method states it.
fn query_page(page: &QueryPage) -> QueryPageEntry {
    QueryPageEntry {
        name: page.name().to_owned(),
        kind: page.kind().as_str(),
        journal_date: page.journal_date().map(|day| day.to_string()),
    }
}

/// One simple query, run under the request's policy and options, with the facts both spellings of
/// the read need to state their answer.
struct EvaluatedQuery {
    result: logseq_graph::domain::QueryResult,
    meta: super::super::answer::Meta,
    expression: String,
    limit: u32,
    today: JournalDate,
}

/// Run the simple-query read once. `logseq.Graph.query` and `logseq.DB.q` are the same question
/// with two answer shapes (ADR-0013), so the execution and its options live here and only the shape
/// differs at the call site.
fn evaluate_query(
    served: &Served,
    call: &Call,
    freshness: logseq_graph::engine::Freshness,
) -> Result<EvaluatedQuery, RequestError> {
    let expression = one_argument(call)?;
    let limit = call
        .options
        .limit
        .unwrap_or(crate::args::DEFAULT_QUERY_LIMIT);
    let today = match call.options.today {
        Some(day) => day,
        None => crate::render::today(std::time::SystemTime::now()).ok_or_else(|| {
            RequestError::Internal(
                "the system clock states no usable calendar day; name the today option".to_owned(),
            )
        })?,
    };
    let (result, meta) = served.read(|graph| {
        let read = graph
            .reads(freshness)
            .query(expression, &QueryContext::new(today, limit))
            .map_err(|error| query_error(error, expression))?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    Ok(EvaluatedQuery {
        result,
        meta,
        expression: expression.to_owned(),
        limit,
        today,
    })
}

pub(in crate::http::rpc) fn query(served: &Served, call: &Call) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let evaluated = evaluate_query(served, call, freshness)?;
    let (blocks, pages) = match evaluated.result.outcome() {
        QueryOutcome::Blocks(matches) => (matches.iter().map(query_match).collect(), Vec::new()),
        QueryOutcome::Pages(matches) => (Vec::new(), matches.iter().map(query_page).collect()),
    };
    Ok(answered((
        QueryResult {
            query: evaluated.expression,
            limit: evaluated.limit,
            today: evaluated.today.to_string(),
            truncated: evaluated.result.truncated(),
            blocks,
            pages,
        },
        evaluated.meta,
    )))
}

/// One block of the official `logseq.DB.q` answer. The official API returns DB entities; this
/// projection has no entity table (ADR-0001), so the entry states the facts it does have under the
/// names an official consumer reads (`uuid`, `content`, `page`) plus the position this index knows.
fn db_q_block(block: &QueryBlock) -> Value {
    json!({
        // The same id a search hit states, so the official client's deduplication and its
        // `Editor.getBlock` read-back see one identity across the two reads.
        "uuid": block_id(block.selector()),
        "content": block.text(),
        "page": {"name": block.page(), "uuid": page_name_key(block.page()), "id": page_id(block.page())},
        "preorder": block.preorder(),
        "depth": block.depth(),
        "marker": marker_text(block.marker()),
        "task": block.task_marker().map(|task| task.as_str()),
        "priority": block.priority().map(|priority| priority.as_str()),
    })
}

/// One page of the official `logseq.DB.q` answer, in the same spirit as [`db_q_block`].
///
/// The official page entity states a folded `name`, the as-written `originalName`, a `uuid`, and
/// `journal?`. A file page has no minted uuid in this projection (ADR-0001), so `uuid` repeats the
/// folded name — the identity every other read accepts — rather than inventing one.
fn db_q_page(page: &QueryPage) -> Value {
    let folded = page_name_key(page.name());
    json!({
        "uuid": folded,
        "id": page_id(page.name()),
        "name": folded,
        "originalName": page.name(),
        "journal?": page.journal_date().is_some(),
        "kind": page.kind().as_str(),
        // The official `journalDay` is the number YYYYMMDD (fixture: `get_page_linked_references`),
        // not this surface's `journal_date` ISO string.
        "journalDay": page
            .journal_date()
            .map(|day| day.year() * 10_000 + i64::from(day.month()) * 100 + i64::from(day.day())),
    })
}

/// The official spelling answers the official shape: the bare array, with no object wrapper, no
/// `truncated` member and no read metadata. The limit still applies.
pub(in crate::http::rpc) fn db_q(served: &Served, call: &Call) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let evaluated = evaluate_query(served, call, freshness)?;
    Ok(match evaluated.result.outcome() {
        QueryOutcome::Blocks(matches) => Value::Array(matches.iter().map(db_q_block).collect()),
        QueryOutcome::Pages(matches) => Value::Array(matches.iter().map(db_q_page).collect()),
    })
}
