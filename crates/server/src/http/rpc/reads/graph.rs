use serde::Serialize;
use serde_json::{json, Value};

use super::super::answer::{answered, page_id, PageEntry};
use super::super::call::Call;
use super::super::error::{internal, RequestError};
use super::super::state::Served;
use super::super::watch::WatchStatus;

/// One entry of the graph's `:favorites` list.
#[derive(Serialize)]
struct FavoriteEntry {
    name: String,
    exists: bool,
}

#[derive(Serialize)]
struct GetCurrentGraphResult {
    /// The graph root's own name and path, the two fields the official method answers and an
    /// editor shows; the counts and index facts beside them are this surface's own additions.
    name: String,
    path: String,
    /// The official `url`: `logseq_local_` and the absolute path, the identity a client stores.
    url: String,
    pages: usize,
    blocks: u64,
    /// The logical index path the process was given, not the generation file it selects.
    index_path: String,
    /// Bytes of the live published generation: its database plus `-wal`, `-shm` and `-journal`.
    index_size: u64,
    /// Bytes of an unpublished shadow generation while a full reprojection builds one.
    index_building_size: u64,
    /// Bytes of every other generation still on disk (the logical base once a pointer selects
    /// another, replaced generations, crashed shadows awaiting cleanup).
    index_retired_size: u64,
    /// Always stated and always `false` here: an answer made while the scan runs is the plain
    /// progress object, not this read.
    scanning: bool,
    /// The background watcher's mode and loss counters since the process started.
    watch: WatchStatus,
}

#[derive(Serialize)]
struct GetAllPagesResult {
    pages: Vec<PageEntry>,
}

#[derive(Serialize)]
struct GetFavoritesResult {
    favorites: Vec<FavoriteEntry>,
}

pub(in crate::http::rpc) fn get_current_graph(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    // The one method answered while the startup scan runs, and the reason it is this one:
    // a caller polling this port learns "still working, N of M" instead of "nothing is
    // listening" (ADR-0008). `Served::read` refuses every other method while scanning.
    if let Some((scanned, total)) = served.scanning() {
        return Ok(json!({
            "name": graph_name(&served.graph_root),
            "path": served.graph_root.display().to_string(),
            "url": graph_url(&served.graph_root),
            "scanning": true,
            "scanned": scanned,
            "total": total,
            "watch": served.watch.status(),
        }));
    }
    let ((pages, usage), meta) = served.read(|graph| {
        let read = graph.reads(freshness).pages().map_err(internal)?;
        let meta = read.meta().clone();
        let usage = graph
            .index_storage_usage()
            .map_err(internal)?
            .ok_or_else(|| {
                RequestError::Internal(format!(
                    "the index at {} is not file-backed",
                    served.index_path.display()
                ))
            })?;
        Ok(((read.into_value(), usage), meta))
    })?;
    let blocks: u64 = pages.iter().map(|page| u64::from(page.block_count())).sum();
    Ok(answered((
        GetCurrentGraphResult {
            name: graph_name(&served.graph_root),
            path: served.graph_root.display().to_string(),
            url: graph_url(&served.graph_root),
            pages: pages.iter().filter(|page| page.has_file()).count(),
            blocks,
            index_path: served.index_path.display().to_string(),
            index_size: usage.live(),
            index_building_size: usage.building(),
            index_retired_size: usage.retired(),
            scanning: false,
            watch: served.watch.status(),
        },
        meta,
    )))
}

/// The graph root's own name, which is the `name` the official `getCurrentGraph` answers.
pub(in crate::http::rpc) fn graph_name(root: &std::path::Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The graph identity the official API states as `url` and `currentGraph`: `logseq_local_` and the
/// graph root's absolute path, which is the spelling Logseq itself uses for a local file graph.
pub(in crate::http::rpc) fn graph_url(root: &std::path::Path) -> String {
    format!("logseq_local_{}", root.to_string_lossy())
}

pub(in crate::http::rpc) fn get_all_pages(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let (pages, meta) = served.read(|graph| {
        let read = graph.reads(freshness).pages().map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let list = pages
        .iter()
        .map(|page| PageEntry {
            name: page.name().to_owned(),
            id: page_id(page.name()),
            kind: page.kind().as_str(),
            journal_date: page.journal_date().map(|day| day.to_string()),
            block_count: page.block_count(),
            has_file: page.has_file(),
        })
        .collect();
    Ok(answered((GetAllPagesResult { pages: list }, meta)))
}

pub(in crate::http::rpc) fn get_favorites(
    served: &Served,
    call: &Call,
) -> Result<Value, RequestError> {
    let freshness = call.options.freshness;
    let (favorites, meta) = served.read(|graph| {
        let read = graph.reads(freshness).favorites().map_err(internal)?;
        let meta = read.meta().clone();
        Ok((read.into_value(), meta))
    })?;
    let list = favorites
        .iter()
        .map(|favorite| FavoriteEntry {
            name: favorite.name().to_owned(),
            exists: favorite.exists(),
        })
        .collect();
    Ok(answered((GetFavoritesResult { favorites: list }, meta)))
}
