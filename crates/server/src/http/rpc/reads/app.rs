use serde_json::{json, Value};

use super::super::call::Call;
use super::super::error::RequestError;
use super::super::state::Served;
use super::graph::graph_url;

/// The official client's edition probe. The one read that is a fact about this implementation
/// rather than about the graph: a file-first projection is never the DB edition (ADR-0001), so
/// the answer is `false` without a graph read and declares no freshness policy.
pub(in crate::http::rpc) fn check_current_is_db_graph(
    _served: &Served,
    _call: &Call,
) -> Result<Value, RequestError> {
    Ok(json!(false))
}

/// The version the official client shows on a successful connection, and the same edition answer
/// the probe above gives: this build has no DB edition to support (ADR-0001).
pub(in crate::http::rpc) fn app_info(
    _served: &Served,
    _call: &Call,
) -> Result<Value, RequestError> {
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "supportDb": false,
    }))
}

/// The official client's toast. No GUI exists here (ADR-0003), so the call is accepted and
/// dropped; the answer keeps the official shape — a message-id string — so a client that only
/// checks for success can continue.
pub(in crate::http::rpc) fn show_msg(
    _served: &Served,
    _call: &Call,
) -> Result<Value, RequestError> {
    Ok(json!("notification"))
}

/// The official answer is app state: theme, interface language, start of week, and the journal
/// date format. A file-first projection states what the graph files hold — the graph identity,
/// the format this surface serves, and the graph's own journal title pattern — and omits the
/// app-state keys, which no file states (parity row #3). The format is read from `config.edn` on
/// each call — a rare read, one small file — so a config edit the watcher re-reads cannot leave
/// the answer stale.
pub(in crate::http::rpc) fn user_configs(
    served: &Served,
    _call: &Call,
) -> Result<Value, RequestError> {
    let url = graph_url(&served.graph_root);
    Ok(json!({
        "currentGraph": url,
        "me": {"repos": [{"url": url, "nfs?": true}]},
        "preferredFormat": "markdown",
        "preferredDateFormat": logseq_graph::source::journal_page_title_format(&served.graph_root),
    }))
}

/// The page the editor has open. There is no editor (ADR-0003), and the official API answers
/// `null` when no page is open, so that is the honest answer; the client's feature degrades
/// instead of failing with a method-not-found it would have to parse.
pub(in crate::http::rpc) fn get_current_page(
    _served: &Served,
    _call: &Call,
) -> Result<Value, RequestError> {
    Ok(Value::Null)
}
