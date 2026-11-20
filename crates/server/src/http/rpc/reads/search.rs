use logseq_graph::domain::PageMatch;
use serde::Serialize;
use serde_json::Value;

use super::super::answer::{answered, block_id, Selector};
use super::super::call::{one_argument, Call};
use super::super::error::{internal, RequestError};
use super::super::state::Served;

/// One hit of a search answer. `selector` is stated, not optional: an index row always carries
/// exactly one identity. `text` keeps the field name the old search answer published: its value is
/// the engine's bounded snippet, but renaming a published field is a protocol change a consumer
/// only discovers at runtime.
#[derive(Serialize)]
struct SearchHitEntry {
    page: String,
    preorder: u32,
    selector: Selector,
    text: String,
}

/// One page a search can jump to. `via` says whether the page's own name or an `alias::` value
/// matched, and is always present so a consumer branches on a value rather than on a key.
#[derive(Serialize)]
struct PageCandidateEntry {
    name: String,
    via: &'static str,
    alias: Option<String>,
}

/// One block of the official `logseq.App.search` answer: the field names the official file edition
/// sends, which the official client maps into its result list before reading each block again.
#[derive(Serialize)]
struct OfficialSearchBlock {
    #[serde(rename = "block/uuid")]
    uuid: String,
    #[serde(rename = "block/content")]
    content: String,
    #[serde(rename = "block/page")]
    page: String,
}

/// The most block hits the official `logseq.App.search` answer states; a client compensates for the
/// cap with its own paging, so the group must be capped and `has-more?` stated.
const OFFICIAL_SEARCH_BLOCK_CAP: usize = 10;

#[derive(Serialize)]
struct SearchResult {
    query: String,
    limit: u32,
    /// The official shape: the block entries the official client maps into its result list, capped
    /// at [`OFFICIAL_SEARCH_BLOCK_CAP`] like the official answer.
    blocks: Vec<OfficialSearchBlock>,
    /// The official shape: page names as strings, which the client reads as page identifiers.
    pages: Vec<String>,
    /// This projection has no page-content or file hits; the official client still reads the keys.
    #[serde(rename = "pages-content")]
    pages_content: Vec<Value>,
    files: Vec<Value>,
    /// The official `has-more?`: whether the block group holds more hits than it states. A client
    /// that shows a "more blocks" affordance branches on it, so it is always a boolean.
    #[serde(rename = "has-more?")]
    has_more: bool,
    /// This surface's own richer answer, kept beside the official fields: every hit keeps its
    /// selector and page position, and the page candidates keep `via` and the matched alias.
    hits: Vec<SearchHitEntry>,
    page_candidates: Vec<PageCandidateEntry>,
}

pub(in crate::http::rpc) fn search(served: &Served, call: &Call) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let query = one_argument(call)?;
    let limit = call
        .options
        .limit
        .unwrap_or(crate::args::DEFAULT_SEARCH_LIMIT);
    // One extra hit proves whether the official 10-block cap truncated the group, and the
    // caller's own limit still bounds this surface's `hits`: ask for the larger of the two
    // plus one, then split.
    let fetch = limit
        .max(OFFICIAL_SEARCH_BLOCK_CAP as u32)
        .saturating_add(1);
    // One request, one freshness decision: both reads answer under the same revision.
    let ((hits, candidates), meta) = served.read(|graph| {
        let mut reads = graph.reads(freshness);
        let hits = reads.search(query, fetch).map_err(internal)?;
        let meta = hits.meta().clone();
        let candidates = reads.page_candidates(query, limit).map_err(internal)?;
        Ok(((hits.into_value(), candidates.into_value()), meta))
    })?;
    let has_more = hits.len() > OFFICIAL_SEARCH_BLOCK_CAP;
    let list = hits
        .iter()
        .take(limit as usize)
        .map(|hit| SearchHitEntry {
            page: hit.page().to_owned(),
            preorder: hit.preorder(),
            selector: Selector::of(hit.selector()),
            text: hit.snippet().to_owned(),
        })
        .collect();
    let blocks = hits
        .iter()
        .take(OFFICIAL_SEARCH_BLOCK_CAP)
        .map(|hit| OfficialSearchBlock {
            uuid: block_id(hit.selector()),
            // The search read states a bounded snippet; the official entry carries the
            // block's content field, and the official client reads the block again anyway.
            content: hit.snippet().to_owned(),
            page: hit.page().to_owned(),
        })
        .collect();
    let pages = candidates
        .iter()
        .map(|candidate| candidate.name().to_owned())
        .collect();
    let page_candidates = candidates
        .iter()
        .map(|candidate| PageCandidateEntry {
            name: candidate.name().to_owned(),
            via: match candidate.via() {
                PageMatch::Name => "name",
                PageMatch::Alias => "alias",
            },
            alias: candidate.alias().map(str::to_owned),
        })
        .collect();
    Ok(answered((
        SearchResult {
            query: query.to_owned(),
            limit,
            blocks,
            pages,
            pages_content: Vec::new(),
            files: Vec::new(),
            has_more,
            hits: list,
            page_candidates,
        },
        meta,
    )))
}
