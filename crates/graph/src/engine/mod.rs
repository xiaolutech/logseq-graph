//! The graph facade: scanning the source files into the derived index and serving reads.
//!
//! This module only orchestrates the other boundaries (ADR-0003). It contains no SQL and no
//! Markdown algorithm: `source` opens the root, discovers the files, reads their text, and stats
//! them; `document` parses one such text; `index` stores the projection and the file baseline.
//!
//! There is one scan, and both entry points use it. [`Graph::open`] is "open and scan once";
//! [`Freshness::CheckNow`] and [`Freshness::VerifyContent`] are "scan once, then answer", differing
//! only in whether the scan trusts a matching `(size, mtime)` baseline or reads every file. A scan
//! discovers the files, compares each one with its recorded baseline, and projects only what changed;
//! files whose derived rows are no
//! longer backed by a file lose those rows. `logseq/config.edn` is re-read on every scan, because the
//! configuration decides which page a file denotes, and a changed configuration voids every derived
//! row.
//!
//! [`Graph::refresh_paths`] is the same work at a finer granularity: it stats only the paths it is
//! given, projects the ones that changed, and drops the rows of the ones that disappeared. It exists
//! because a filesystem event names the file that changed (ADR-0008), and the whole-graph stat pass a
//! scan pays is one to two orders of magnitude more expensive than the queries it serves. It refuses
//! to run when the configuration changed, because names are derived from the configuration and only a
//! full scan can re-derive them; the caller falls back to [`Freshness::CheckNow`].
//!
//! The read surface is page-level: the page list, page metadata, a page tree, a page's block text,
//! a page's properties, the pages that reference a page, the graph's tags, the pages that use one
//! tag, and a bounded content search. A page tree is structure only, so a caller that prints a page
//! reads its block text with [`Graph::page_blocks`] instead of one [`Graph::block`] per block. The
//! search returns matching blocks and a snippet, not block text inside a page tree; a block's own
//! facts stay inside the index until ADR-0005 decides how a block is named publicly.
//!
//! [`Graph::page_snapshot`] is the exception to that per-facet shape: it answers a whole page —
//! structure, editable text, properties, and a version of the bytes — from one parse of the file the
//! index points at, so an editor never has to stitch three observations of the same page together.
//!
//! The write surface has six operations: [`Graph::append_block`] appends one top-level block to one
//! existing page, [`Graph::insert_block`] mints one new block at a position named by a
//! [`MoveTarget`] and answers the block it created, [`Graph::update_block`] replaces the text of the
//! block a selector names, by durable id or by ephemeral id, [`Graph::remove_block`] deletes a
//! block and its subtree, [`Graph::move_block`] moves a block next
//! to another one, and [`Graph::create_page`] creates an empty page. Each projects the page again
//! and follows the same ordering — lock, resolve the file, read and edit it, re-observe it, replace
//! the page — documented in the `write` submodule, because that order is what keeps the index from
//! describing content that does not exist (ADR-0004). With matching configuration generations,
//! none scans the graph first: the bytes
//! an edit transforms come from the file it replaces, never from a row, so a stale index page cannot
//! cost a write its content. A move whose two ids are on different pages is the one write that
//! replaces two files: it records the scoped [`MoveIntent`](crate::index::MoveIntent) ADR-0004
//! deferred to the first multi-file operation, and [`Graph::open`] reconciles a surviving intent
//! before serving anything, first reprojecting if its configuration generation is unknown or changed.
//!
//! Reads answer under an explicit freshness policy (ADR-0006). The engine keeps two facts that make
//! that policy honest: when its last scan started, and whether its last scan failed. A failed scan is
//! the only thing the engine can know about divergence in this slice, because it is the only
//! observation it made; a file changed by another process stays unknown until a scan, which is why
//! every result reports the observation time rather than an absolute freshness promise. A failed scan
//! may also mean the scan never reached the files (a broken `logseq/config.edn`, an unreadable
//! directory), so the engine reports that it cannot vouch for the index rather than that the index is
//! outdated.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::domain::Read;
use crate::index::Index;
use crate::source::{self, GraphSource};

mod batch;
mod error;
mod freshness;
mod open;
mod options;
mod pool;
mod progress;
mod projection;
mod read;
mod refresh;
mod reprojection;
mod scan;
mod snapshot;
mod write;

use error::listed;
pub use error::EngineError;
pub use options::{Freshness, OpenOptions, DEFAULT_WRITE_LOCK_WAIT};
pub use pool::{ReadHandle, SharedGraph};
pub use progress::ScanProgress;
use projection::{baseline_for, Projection, ScanMode};
pub use read::Reads;
use read::{locate_block, BlockLookup};
pub use refresh::RefreshReport;
pub use scan::ScanState;
use scan::SharedScanState;
pub use write::{EnsuredBlockId, MoveTarget, WriteError, WriteState};

/// An opened graph: the source files, the derived index scanned from them, and the scan's outcome.
///
/// A single handle is exclusive. [`SharedGraph`] adds bounded, read-only connections for concurrent
/// callers, while retaining this handle and the existing locks for writes and freshness scans.
#[derive(Debug)]
pub struct Graph {
    source: Arc<GraphSource>,
    journal: source::MoveJournal,
    index: Index,
    read_coordinator: Option<Arc<std::sync::Mutex<Graph>>>,
    freshness: Freshness,
    /// Freshness observation and exclusion, optionally shared with another handle of this graph.
    scan_state: SharedScanState,
    /// Directory the cross-process write lock is taken in, from [`source::lock::lock_directory`].
    ///
    /// The lock serialises writers of one graph, so its location follows the graph, not the index:
    /// in a Git working tree it is `<root>/.git`, which every writer of that tree can agree on, and
    /// outside one it lives in the canonical graph root. It is resolved once at open, so a
    /// working-directory change cannot move the lock.
    write_lock_dir: PathBuf,
    /// How long a write waits for that lock, from [`OpenOptions`].
    write_lock_wait: Duration,
    /// How far the scan that is running has got, shared with a caller that kept a handle.
    ///
    /// Every scan this handle runs reports through it, startup and `CheckNow` alike, because the
    /// counter describes the work in progress rather than the graph. A caller that opened with
    /// [`Graph::open`] holds no handle to it, and the scan pays one atomic store per file either way.
    progress: Arc<ScanProgress>,
    /// One-shot fault the engine's own tests arm on this handle; absent from every production build.
    #[cfg(test)]
    test_fault: Option<crate::engine::write::TestFault>,
    #[cfg(test)]
    test_scan_prepare: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    test_shadow_build: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    #[cfg(test)]
    test_shadow_catchup: Option<reprojection::CatchupHook>,
    /// Set when the armed fault fires, so a test can prove it actually ran.
    #[cfg(test)]
    test_fault_fired: Arc<AtomicBool>,
}
