//! The HTTP/JSON adapter: one `POST /api` surface over the engine's reads and writes.
//!
//! This module is a translation layer and nothing else (ADR-0003): it parses one
//! `{method, args}` call, calls the engine read or write the method maps to, and serializes that
//! result. It holds no SQL, opens no graph file, and parses no `PageDocument`; every fact about the
//! graph comes from [`logseq_graph::engine::Graph`]'s public surface. The one fact taken from the filesystem is the index
//! file's size for `getCurrentGraph`, which `info` on the command line reports the same way.
//!
//! Method names have the official `logseq.<Namespace>.<method>` shape, and the official
//! namespace-discard rule is honoured: `logseq.getPage` and `logseq.Editor.getPage` name the same
//! method because dispatch uses the leaf name. Methods in the `logseq.Graph.*` namespace are this
//! repository's own reads (`getFavorites`, `getAllTags`, `getPagesWithTag`, `getPageVersion`,
//! `getPageSnapshot`, `query`); they are not part of the official API contract. The `selector`
//! fields of a tree, a block, a search hit and a query match, the `freshness` option, `truncated`,
//! `limit` and `today` are likewise this repository's own semantics, not official ones.
//!
//! One call is one request and one freshness policy (ADR-0008). Positional arguments are strings;
//! an optional trailing options object carries `freshness` (`last`, `check`, `verify`, default
//! `last`), `limit` and `today`. Two options objects would be two policies and are refused. The two
//! file-tier reads, `getPageVersion` and `getPageSnapshot`, declare no policy at all (ADR-0008's
//! amendment); a `freshness` option beside them is an error rather than permission to skip the file
//! read. The adapter never caches, retries, or upgrades a policy, and every answer carries the
//! observation it was made under — the [`logseq_graph::domain::ReadMeta`] revision, whether the engine can vouch for the
//! index, and when the last scan started.
//!
//! `Graph.query` is the one method whose input is a language rather than a name: its argument is a
//! simple-query expression, and a `limit` it refuses above [`methods::MAX_QUERY_LIMIT`]. A query
//! the grammar
//! refuses is a structured invalid-params error naming the query, never an empty answer: `[]` would
//! read as "no block matches". Every matched block states a `selector`, the same spelling
//! `getBlock` accepts, so an answer can be acted on instead of re-derived.
//!
//! Errors pair the transport's HTTP status with the official Fastify fields `statusCode`,
//! `error` and `message` plus this surface's `code` and structured `data`: the former HTTP status
//! classes are migrated one by one — 400 to `-32602`, 404 to `-32004`, 409 to `-32009`, 500 to
//! `-32603`, 503 to `-32003` — and protocol-level failures use the standard `-32700` (parse
//! error), `-32600` (invalid request) and `-32601` (method not found). `data.kind` names the
//! class, `data.http_status` carries the former status, and an ambiguity names every page
//! involved. The HTTP status line is kept as a transport hint; the error object is the contract.
//!
//! Requests share a `SharedGraph`: four read-only connections and one serialized write handle.
//! The filesystem watcher owns a separate handle and SQLite connection.
//! A background scan never holds `Mutex<State>`: default reads keep answering the last committed
//! revision while it walks the filesystem. The handles share only freshness facts and scan exclusion.
//!
//! The socket is bound before that projection starts and the scan runs on its own thread, so the
//! port answers for the whole time it is indexing (ADR-0008). Until the graph is open,
//! `getCurrentGraph` answers the scan's progress — `{"scanning": true, "scanned": N, "total": M}` —
//! and every other method answers an error whose `data` carries the same three fields. Once the
//! graph is open `getCurrentGraph` states `"scanning": false` beside its usual fields, so a caller
//! branches on a field that is always there instead of treating an absent one as false.

use serde_json::Value;

mod answer;
mod call;
mod error;
mod methods;
mod reads;
mod serve;
mod state;
mod transport;
mod watch;
mod writes;

use call::Call;
use error::RequestError;
use methods::Method;
use state::Served;

pub(crate) use serve::serve;

/// Route one parsed call and build its `result`.
fn handle(served: &Served, call: &Call) -> Result<Value, RequestError> {
    if let Some(write) = &call.write {
        return writes::handle(served, write);
    }
    match call.method {
        Method::GetCurrentGraph => reads::graph::get_current_graph(served, call),
        Method::GetAllPages => reads::graph::get_all_pages(served, call),
        Method::GetFavorites => reads::graph::get_favorites(served, call),
        Method::GetPage => reads::pages::get_page(served, call),
        Method::GetPageBlocksTree => reads::pages::get_page_blocks_tree(served, call),
        Method::GetPageLinkedReferences => reads::pages::get_page_linked_references(served, call),
        Method::GetPageVersion => reads::pages::get_page_version(served, call),
        Method::GetPageSnapshot => reads::pages::get_page_snapshot(served, call),
        Method::GetAllTags => reads::tags::get_all_tags(served, call),
        Method::GetPagesWithTag => reads::tags::get_pages_with_tag(served, call),
        Method::Search => reads::search::search(served, call),
        Method::Query => reads::query::query(served, call),
        Method::GetBlock => reads::blocks::get_block(served, call),
        Method::CheckCurrentIsDBGraph => reads::app::check_current_is_db_graph(served, call),
        Method::AppInfo => reads::app::app_info(served, call),
        Method::ShowMsg => reads::app::show_msg(served, call),
        Method::DbQ => reads::query::db_q(served, call),
        Method::UserConfigs => reads::app::user_configs(served, call),
        Method::GetCurrentPage => reads::app::get_current_page(served, call),
        Method::Write(_) => Err(RequestError::Internal(
            "a write call reached the read dispatcher without its parsed request".to_owned(),
        )),
    }
}
