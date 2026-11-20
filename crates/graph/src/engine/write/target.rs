use super::error::WriteError;
use crate::document::BlockLocator;
use crate::domain::{page_name_key, BlockId, BlockSelector, BlockView};
use crate::engine::snapshot::{page_version, snapshot_from_text, version_of_fingerprint};
use crate::engine::{EngineError, Graph, ScanMode};
use crate::index::{DurableLookup, LocatedPageBlock};
use crate::source::{PageFile, WriteLock};

/// The page and address within it that one write targets.
///
/// The two identity kinds carry different facts, and a tuple of options would leave every caller
/// re-deriving which combination is legal. A durable write edits by `id::` value and answers through
/// the id; an ephemeral write edits by the child-index path its row names, and carries the
/// fingerprint of the bytes those rows were projected from, because that is the fact a write has to
/// check before it may touch the file (ADR-0012).
pub(super) enum WriteTarget {
    /// The write was asked with a durable id: the page it lives on, and the id itself.
    Durable { page: String, id: BlockId },
    /// The write was asked with an ephemeral id: where that row says the block is.
    Ephemeral(EphemeralTarget),
}

impl WriteTarget {
    /// Name of the page the write edits.
    pub(super) fn page(&self) -> &str {
        match self {
            Self::Durable { page, .. } => page,
            Self::Ephemeral(target) => &target.page,
        }
    }

    /// The address the edit resolves inside that page.
    pub(super) fn locator(&self) -> BlockLocator<'_> {
        match self {
            Self::Durable { id, .. } => BlockLocator::Id(id.as_str()),
            Self::Ephemeral(target) => BlockLocator::Indices(&target.child_path),
        }
    }

    /// Fingerprint of the bytes the target's positions were computed against, for an ephemeral
    /// target; `None` for a durable one, whose address needs no such check.
    pub(super) fn page_fingerprint(&self) -> Option<u64> {
        match self {
            Self::Durable { .. } => None,
            Self::Ephemeral(target) => Some(target.page_fingerprint),
        }
    }

    /// Where the write reads its answer back after the page was replaced.
    pub(super) fn answer_address(&self) -> AnswerAddress {
        match self {
            Self::Durable { id, .. } => AnswerAddress::Durable(id.clone()),
            Self::Ephemeral(target) => AnswerAddress::Preorder(target.preorder),
        }
    }
}

/// What the index row behind an ephemeral selector says about where its block is.
///
/// Every field is a fact about the bytes those rows were projected from, which is why the
/// fingerprint travels with them: `preorder` and `child_path` are coordinates in one observation,
/// and `page_fingerprint` is what says whether the file an edit is about to transform still *is*
/// that observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EphemeralTarget {
    /// Name of the page the block lives on, as written in `pages.name`.
    page: String,
    /// Depth-first position of the block within that page, at that projection.
    pub(super) preorder: u32,
    /// Child-index path from the page root down to the block, at that projection.
    pub(super) child_path: Vec<u32>,
    /// Content fingerprint of the page's bytes as that projection read them.
    page_fingerprint: u64,
}

/// The block one end of a move or an insert hangs off, resolved to a page and an address in it.
///
/// A durable anchor is the `id::` value itself, which the edit resolves against the file. An
/// ephemeral anchor resolves through the index to a child-index path and the fingerprint of the
/// bytes that path was computed against, so the write can refuse to place an edit relative to a
/// coordinate its file no longer agrees with.
pub(super) struct ResolvedTarget {
    /// Name of the page the anchor block lives on, as written in `pages.name`.
    pub(super) page: String,
    /// Address of the anchor block inside that page.
    pub(super) locator: OwnedLocator,
    /// Content fingerprint of the page's bytes the anchor's row was projected from, when the anchor
    /// is ephemeral. `None` for a durable anchor, which the file itself resolves.
    pub(super) page_fingerprint: Option<u64>,
}

/// One block address that outlives the borrow of a selector.
///
/// [`BlockLocator`] borrows its id or path, which is what the document layer wants; a resolved
/// target owns the value it was resolved to, so this is the same two addresses in owned form with a
/// conversion back at the point the edit runs.
pub(super) enum OwnedLocator {
    /// The `id::` value a durable selector names.
    Id(BlockId),
    /// The child-index path an ephemeral selector's row names.
    Indices(Vec<u32>),
}

impl OwnedLocator {
    /// The same address in the form the document layer reads.
    pub(super) fn as_locator(&self) -> BlockLocator<'_> {
        match self {
            Self::Id(id) => BlockLocator::Id(id.as_str()),
            Self::Indices(path) => BlockLocator::Indices(path),
        }
    }
}

/// Where a single-block write reads its answer back after the page was replaced.
///
/// Every write that changes one block reprojects the whole page, so the row it has to answer with is
/// a new row: an ephemeral id from before the edit does not exist any more (ADR-0012). The address
/// below is one the edit itself cannot invalidate — a preorder position survives a text or property
/// change, a preorder path survives a property change, and a durable id survives a text change —
/// which is what makes it a reliable way to name the row the replacement just stored.
pub(super) enum AnswerAddress {
    /// The durable id the edit did not change.
    Durable(BlockId),
    /// The block's preorder position in its page.
    Preorder(u32),
    /// The block's preorder path in its page.
    Path(Vec<u32>),
}

/// The path of one block of `page`, rendered the way an answer names it in a corrupt-row report.
pub(super) fn answer_address(page: &str, path: &[u32]) -> String {
    let joined = path
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    format!("{page}:{joined}")
}

/// Refuse to edit bytes that are not the ones an ephemeral selector's row was projected from.
///
/// This is the gate that makes an ephemeral id safe to write through, and it is an invariant of the
/// identity rather than an implementation detail (ADR-0012). The id names a row; the row's preorder
/// and child path are coordinates **in the bytes that row was projected from**. When another program
/// changed the file and nothing scanned it since, the id still exists and the fingerprint still
/// matches nothing on disk: applying the coordinates to the new bytes would edit whichever block now
/// stands at that position. So the bytes are compared first and the write refuses before any edit is
/// computed, which is what makes the failure cost zero bytes rather than one wrong block.
///
/// The refusal is the same one a versioned batch of operations gets, with the same payload: the base
/// version is the fingerprint the rows recorded, rendered exactly as a page version is, and `current`
/// is the page as the file holds it now, so a caller retries against `current.version()` instead of
/// re-reading the page. A durable selector has no fingerprint and passes straight through: it names
/// the block by what the file itself states, so an edit that finds no such line refuses on its own
/// terms.
pub(super) fn check_ephemeral_freshness(
    page: &str,
    text: &str,
    page_fingerprint: Option<u64>,
) -> Result<(), WriteError> {
    let Some(fingerprint) = page_fingerprint else {
        return Ok(());
    };
    let base_version = version_of_fingerprint(fingerprint);
    if page_version(text) == base_version {
        return Ok(());
    }
    Err(WriteError::StaleVersion {
        page: page.to_owned(),
        base_version,
        current: Box::new(snapshot_from_text(page, text)),
    })
}

/// The public view of one stored row, as a write that just committed it answers.
pub(super) fn view_of(located: LocatedPageBlock) -> BlockView {
    BlockView::new(
        located.page,
        located.preorder,
        located.depth,
        located.parent_preorder,
        located.text,
        located.marker,
        located.selector,
    )
}

impl Graph {
    pub(in crate::engine) fn begin_write(&mut self) -> Result<WriteLock, EngineError> {
        loop {
            let lock = WriteLock::acquire(&self.write_lock_dir, Some(self.write_lock_wait))?;
            self.refresh_index()?;
            if let Some(source) = self.scan_state.published_source() {
                if source.root() == self.source.root() {
                    self.source = source;
                }
            }
            if self.index.configuration_matches(self.source.signature())? {
                return Ok(lock);
            }
            drop(lock);
            self.scan(ScanMode::TrustStat)?;
        }
    }

    pub(super) fn begin_selector_write(
        &mut self,
        selectors: &[&BlockSelector],
    ) -> Result<WriteLock, WriteError> {
        let mut scanned = false;
        loop {
            let lock = self.begin_write()?;
            let mut missing = false;
            for selector in selectors {
                if let BlockSelector::Durable(id) = selector {
                    missing |=
                        Self::page_in_lookup(self.index.block_by_durable_id(id)?, id)?.is_none();
                }
            }
            if !missing {
                return Ok(lock);
            }
            if scanned {
                let files = self.source.discover()?;
                let mut changed = files.len() != self.index.file_baseline_paths()?.len();
                for file in files {
                    if self
                        .index
                        .file_baseline(file.relative_path())?
                        .is_none_or(|b| b.metadata() != file.metadata())
                    {
                        changed = true;
                        break;
                    }
                }
                if !changed {
                    return Ok(lock);
                }
            }
            drop(lock);
            self.scan(ScanMode::ReadEveryFile)?;
            scanned = true;
        }
    }

    /// The file that holds `page`, read from the index in one query, or from discovery on a miss.
    ///
    /// A page row stores the file it was projected from, so the ordinary answer is one seek on
    /// `pages.name_key` and one stat of that path. A name-addressed write used to discover the graph
    /// instead — a directory listing per Logseq directory, every file name decoded, every duplicate
    /// check — to rebuild a map the previous scan had already computed and written down.
    ///
    /// The stored path can also be stale, and the files are the authority: another process may have
    /// renamed or removed the file after the last projection, and a `logseq/config.edn` change can
    /// make the same path denote another page. A journal is the case that shows why the path alone is
    /// not an address: `journals/2024-04-23.md` becomes a date through `:journal/file-name-format`
    /// and a page name through `:journal/page-title-format`, so changing the second format renames
    /// the page without touching the file. A page with no row, a row whose file is gone, and a row
    /// whose file now decodes to a different name all fall back to one full discovery, and only a
    /// miss there means this graph has no such page. The write that follows replaces the page in
    /// step 7 with the path it actually projected, so a row discovery had to correct is corrected in
    /// the index too.
    pub(in crate::engine) fn locate_page(&self, page: &str) -> Result<PageFile, WriteError> {
        let key = page_name_key(page);
        if let Some(relative_path) = self.index.page_file(page)? {
            // The resolved file has to denote the page this write names, not merely sit at the path
            // the index remembers: the row can outlive a configuration change that renamed the page.
            // Writing a file that now belongs to another page would edit a page the caller never
            // named, so a name mismatch is a miss like any other and goes to discovery.
            if let Some(file) = self.source.page_at(&relative_path)? {
                if self
                    .index
                    .file_baseline(&relative_path)?
                    .is_some_and(|baseline| baseline.metadata() == file.metadata())
                {
                    if let Some(name) = self.index.page_name_for_path(&relative_path)? {
                        return Ok(file.with_page_name(name));
                    }
                }
                let actual = file.name_in(&file.read_text()?);
                if page_name_key(&actual) == key {
                    return Ok(file.with_page_name(actual));
                }
            }
        }

        for file in self.source.discover()? {
            if self
                .index
                .file_baseline(file.relative_path())?
                .is_some_and(|baseline| baseline.metadata() == file.metadata())
            {
                continue;
            }
            let actual = file.name_in(&file.read_text()?);
            if page_name_key(&actual) == key {
                return Ok(file.with_page_name(actual));
            }
        }
        Err(WriteError::PageNotFound {
            page: page.to_owned(),
        })
    }

    /// The page and address the write `selector` names, or the refusal a write answers before
    /// anything is read or written.
    ///
    /// This is the write-side counterpart of the read path's `locate_block`, and the two identity
    /// kinds reach it by the only paths that exist for them. A durable id goes through the index as
    /// [`Self::page_stating`] always did, scan fallback included. An ephemeral id is one equality on
    /// the unique `blocks.ephemeral_id` index and nothing else: a miss is
    /// [`WriteError::EphemeralNotFound`], never a scan, because scanning every file could not
    /// produce a row that holds this id (ADR-0012).
    ///
    /// What the ephemeral answer carries beyond the page is the pair the rest of the write is built
    /// on: the child-index path the document layer edits by, and the content fingerprint of the
    /// bytes those rows were projected from. The path is only a coordinate while the file still is
    /// those bytes, so carrying both is what lets step 4 refuse an edit whose file moved on without
    /// this index hearing about it.
    pub(super) fn write_target(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<WriteTarget, WriteError> {
        match selector {
            BlockSelector::Durable(id) => {
                let page = self.page_stating(id)?;
                Ok(WriteTarget::Durable {
                    page,
                    id: id.clone(),
                })
            }
            BlockSelector::Ephemeral(id) => {
                let located = self
                    .index
                    .block_by_ephemeral_id(id)?
                    .ok_or_else(|| WriteError::EphemeralNotFound { id: id.clone() })?;
                Ok(WriteTarget::Ephemeral(EphemeralTarget {
                    page: located.page,
                    preorder: located.preorder,
                    child_path: located.child_path,
                    page_fingerprint: located.page_fingerprint,
                }))
            }
        }
    }

    /// Resolve under the lock after begin_selector_write has prepared any durable miss.
    fn page_stating(&mut self, id: &BlockId) -> Result<String, WriteError> {
        Self::page_in_lookup(self.index.block_by_durable_id(id)?, id)?
            .ok_or_else(|| WriteError::BlockNotFound { id: id.clone() })
    }

    /// The page one lookup sends this write to, or `None` when the rows state no such id.
    ///
    /// `Several` splits on the page count, deliberately, because the two cases have different
    /// authorities. Rows on more than one page cannot be judged by reading one file, so the write
    /// refuses here, before any file is opened. Rows on one page go through: the index only knows
    /// that page's rows hold the id twice, and it may be stale, while the file is what the edit is
    /// applied to and [`PageDocument`] already refuses a file that states the id twice. Letting the
    /// document layer answer that keeps one rule in one place, and it is also why a same-page
    /// duplicate still surfaces as [`WriteError::Edit`] rather than as a new error.
    fn page_in_lookup(lookup: DurableLookup, id: &BlockId) -> Result<Option<String>, WriteError> {
        match lookup {
            DurableLookup::Absent => Ok(None),
            DurableLookup::One(located) => Ok(Some(located.page)),
            DurableLookup::Several { pages } => match pages.as_slice() {
                [page] => Ok(Some(page.clone())),
                // Two pages or more. The empty case cannot arise — `Several` is built from rows that
                // exist — and is refused with the same error rather than given a page to write to.
                _ => Err(WriteError::AmbiguousBlockId {
                    id: id.clone(),
                    pages,
                }),
            },
        }
    }

    /// The block one end of a move or an insert hangs off, as a page and an address inside it.
    ///
    /// The anchor goes through the same resolution as the block being moved — [`Self::write_target`]
    /// for an ephemeral id, [`Self::page_stating`] for a durable one — because both ends of a move
    /// are the same kind of fact: a page the write is about to read and transform. What comes back
    /// is the page the same-page-or-two decision compares, the address the edit resolves, and, for
    /// an ephemeral anchor, the fingerprint of the bytes that address was computed against.
    pub(super) fn resolve_anchor(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<ResolvedTarget, WriteError> {
        match selector {
            BlockSelector::Durable(id) => Ok(ResolvedTarget {
                page: self.page_stating(id)?,
                locator: OwnedLocator::Id(id.clone()),
                page_fingerprint: None,
            }),
            BlockSelector::Ephemeral(_) => {
                let WriteTarget::Ephemeral(target) = self.write_target(selector)? else {
                    unreachable!("an ephemeral selector resolves to an ephemeral target")
                };
                Ok(ResolvedTarget {
                    page: target.page,
                    locator: OwnedLocator::Indices(target.child_path),
                    page_fingerprint: Some(target.page_fingerprint),
                })
            }
        }
    }
}
