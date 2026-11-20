use super::error::{answer_after_write, WriteError};
use super::target::check_ephemeral_freshness;
#[cfg(doc)]
use crate::document::EditError;
use crate::document::PageDocument;
use crate::domain::{BlockSelector, PageMetadata, Read};
use crate::engine::Graph;
use crate::index::IndexError;

impl Graph {
    /// Delete the block the selector names, and every block under it.
    ///
    /// A durable id names one block (ADR-0005), and an ephemeral id names the block its index row
    /// holds for as long as that row exists and the page's bytes are the ones it was projected from
    /// (ADR-0012) — the write checks exactly that before it deletes anything. Deleting a block in an
    /// outliner deletes its subtree: a child without its parent is a block the structure never had. The deleted range is one contiguous run of source
    /// lines, so nothing outside it changes — the page's preamble, sibling blocks, trailing blank
    /// lines, line endings, and BOM all survive.
    ///
    /// # Why this answers [`PageMetadata`] and not the removed block
    ///
    /// The block is gone, so there is no row to answer with: the projection no longer holds it, and
    /// inventing a view from the bytes this call deleted would describe content that does not exist.
    /// What this write can honestly promise is what [`Graph::append_block`] promises — the page
    /// exists at this revision and holds this many blocks — in the same shape, because removing a
    /// block and appending one are the two directions of the same fact. A page left with no block at
    /// all is a success with a `block_count` of zero: the page line, and the file, stay.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::update_block`]'s, and the target is found the same way:
    ///
    /// 1. **Take the write lock.**
    /// 2. **Resolve the selector to its page and address**, exactly as [`Graph::update_block`]'s
    ///    step 2 does: a durable id through the index with the scan fallback, an ephemeral id through
    ///    the one index that holds it.
    /// 3. **Find that page's file and read it.**
    /// 4. **Check the bytes, then apply the pure edit.** An ephemeral target's fingerprint is
    ///    compared with the bytes just read, so a page another program changed is
    ///    [`WriteError::StaleVersion`] rather than a deletion of whatever block now stands at that
    ///    path (ADR-0012). For a durable write, [`EditError::BlockNotFound`] here means the index row
    ///    outlived the `id::` line the file states, and it arrives as [`WriteError::Edit`], not as
    ///    [`WriteError::BlockNotFound`]: the row was enough to find the right file, and the file then
    ///    refused the edit.
    /// 5. **Replace the file atomically.**
    /// 6. **Re-observe, re-read, and re-parse** the bytes that are actually on disk.
    /// 7. **Replace the whole page in one transaction** with the baseline of the observation.
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives, and it
    /// reprojects only the page it rewrote.
    pub fn remove_block(
        &mut self,
        selector: &BlockSelector,
    ) -> Result<Read<PageMetadata>, WriteError> {
        self.remove_block_range(selector, selector)
    }

    /// Delete the inclusive sibling run from `first` through `last`, including their subtrees.
    ///
    /// Both selectors pass the same lock and fingerprint gates as [`Self::remove_block`].
    /// Different pages, different parents, and reversed sibling order are refused before the
    /// single file replacement and page projection. Equal endpoints remove one subtree.
    pub fn remove_block_range(
        &mut self,
        first: &BlockSelector,
        last: &BlockSelector,
    ) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_selector_write(&[first, last])?;

        // Step 2: resolve the selector to its page and address, exactly as the update does.
        let target = self.write_target(first)?;
        let end = self.write_target(last)?;
        if target.page() != end.page() {
            return Err(WriteError::RangeAcrossPages);
        }

        // Step 3: resolve the page name the index stored to the file that holds it.
        let file = self.locate_page(target.page())?;

        // Step 4: the edit is applied to the bytes on disk and stays a pure transformation
        // until the file is replaced. The fingerprint gate runs first, for the same reason as the
        // update's: an ephemeral block's path is a coordinate in the bytes its row was projected
        // from, so deleting by that path in any other bytes could delete a neighbour.
        let original = file.read_text()?;
        check_ephemeral_freshness(file.name(), &original, target.page_fingerprint())?;
        check_ephemeral_freshness(file.name(), &original, end.page_fingerprint())?;
        let document = PageDocument::parse(&original);
        let candidate = if first == last {
            document.remove_block_text(target.locator())?
        } else {
            document.remove_block_range_text(target.locator(), end.locator())?
        };

        // Steps 5 to 7 are the shared single-page commit. The caller still holds the write lock.
        let committed = self.commit_page_edit(&file, candidate)?;

        // The metadata comes from the projection, like the append's: the block count is a fact about
        // what was stored, and a page reduced to its preamble is a page with zero blocks, not a
        // missing row. Step 7 just committed this page, so the row is there.
        let metadata = self
            .index
            .page_metadata(file.name())
            .map_err(|cause| answer_after_write(file.name(), file.path(), cause))?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: file.name().to_owned(),
                path: file.path().to_owned(),
                cause: IndexError::CorruptRow {
                    column: "pages.name",
                    value: file.name().to_owned(),
                },
            })?;

        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }
}
