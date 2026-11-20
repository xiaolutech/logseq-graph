use super::error::{answer_after_write, is_unique_violation, WriteError};
use crate::document::PageDocument;
#[cfg(doc)]
use crate::domain::page_name_key;
use crate::domain::{JournalDate, PageMetadata, Read};
use crate::engine::{baseline_for, EngineError, Graph};
use crate::index::IndexError;
use crate::source::{self, SourceError, PAGES_DIR};
use std::path::Path;

impl Graph {
    /// Create an empty page named `name` under `pages/`, and answer its metadata.
    ///
    /// The name is first encoded into a file stem ([`NameFormat::encode`](crate::source)), and the
    /// encoder decodes its own answer before returning it: a name it cannot spell exactly is
    /// [`WriteError::PageNameNotEncodable`] instead of a file that decodes to a different page.
    /// Names are compared by [`page_name_key`], the same folding every page read uses, so creating
    /// `foo` while `Foo.md` exists is [`WriteError::PageExists`] and writes no second file. A page
    /// that already exists on disk but not in the index is still refused: the file open is
    /// `O_EXCL`, so nothing is ever replaced.
    ///
    /// The file's content is the empty string, which is a page with no blocks: a page is its file,
    /// and the page line is a row the projection adds, not bytes the file must hold. Creating the
    /// page is what [`Graph::append_block`] can build on.
    ///
    /// # Journals are not created here
    ///
    /// Every page this method writes goes under `pages/`. A journal is identified by a calendar day
    /// and named by two date formats, so it is created from the day through
    /// [`Graph::create_journal`], never from a title: a name that happens to look like a journal
    /// title is an ordinary page, which is why `create_page("Aug 13th, 2026")` writes
    /// `pages/Aug 13th, 2026.md`.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::append_block`]'s, with "read the existing file" replaced by "create
    /// the file", because a create has no previous bytes to transform:
    ///
    /// 1. **Take the write lock**, so another writer's scan cannot interleave with this file
    ///    creation and the page row that follows it.
    /// 2. **Encode the name**, which refuses a name no stem spells exactly.
    /// 3. **Ask the index whether the page exists**, so the error can say where the page already is.
    ///    This is a diagnostic, not the only guard: the file open and the index's uniqueness
    ///    constraints are what actually stop a duplicate.
    /// 4. **Name the file** `pages/<stem>.md`.
    ///
    /// Steps 5 to 8 are `Self::create_page_file`, which [`Graph::create_journal`] shares: create
    /// the file without replacing anything, observe and read what was created, replace the whole
    /// page and its baseline in one transaction, and answer the committed metadata. The failure
    /// rules are that helper's too.
    ///
    /// A failure in steps 1 to 3 leaves the graph exactly as it was. A failure after the file is
    /// created also removes it when the cause is a uniqueness constraint, which is the case a stale
    /// index produces; any other index failure keeps the file and reports
    /// [`WriteError::IndexAfterWrite`], which the next scan repairs. With a matching configuration
    /// generation, this write does not scan the
    /// graph: the page it creates is the only page it observes, and a page it does not touch keeps
    /// whatever the last scan projected.
    pub fn create_page(&mut self, name: &str) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer before anything is observed or created.
        let _lock = self.begin_write()?;

        // Step 2: the name has to become a stem that decodes back to it. The encoder proves that
        // itself, so a refusal here is a page name this build cannot write rather than a guess.
        let stem = self.source.name_format().encode(name).map_err(|error| {
            WriteError::PageNameNotEncodable {
                name: name.to_owned(),
                reason: error.reason().to_owned(),
            }
        })?;

        // Step 3 (and step 4): the one directory a created page goes into, asked about before it is
        // written to, so a duplicate answers with the file the existing page lives in. The page
        // might be absent from the index while its file is still there, which is why this cannot be
        // the only guard.
        let relative_path = Path::new(PAGES_DIR).join(format!("{stem}.md"));
        if self
            .index
            .page_metadata(name)?
            .is_some_and(|page| page.has_file())
        {
            return Err(self.existing_page(name, &relative_path));
        }

        self.create_page_file(&relative_path, name)
    }

    /// Create the journal of `date`, and answer its metadata.
    ///
    /// The file lands under `journals/` at the path `:journal/file-name-format` renders for `date`,
    /// and the page is named by `:journal/page-title-format`, so `create_journal(2026-08-13)` under
    /// the default formats writes an empty `journals/2026_08_13.md` and projects the page
    /// `Aug 13th, 2026`.
    ///
    /// # No title is parsed back into a date
    ///
    /// This operation only goes forward: a day is given, and the two formats render it. Nothing
    /// here reads a page name and guesses a date, so a page that merely looks like a journal —
    /// `pages/Aug 13th, 2026.md`, or a journal title written under a different title format — is
    /// never silently promoted to this day's journal. The refusal that follows from that is the
    /// same one [`WriteError::PageExists`] states: the name is taken by a page of another kind.
    ///
    /// A page's identity here is exactly whatever `:journal/page-title-format` renders, and that
    /// format has to name a date but is not required to name the year: with `MMM do`, the 13th of
    /// August has one title for every year. Two days then share a page name, and the second one to
    /// be created is refused as [`WriteError::PageExists`], pointing at the first one's file. That
    /// is the shape of the configuration, not an approximation made here — the file name still
    /// carries the day, and this method says which file it actually found.
    ///
    /// # An existing journal is not rewritten
    ///
    /// Idempotence here is about bytes, not about calls: a second `create_journal` for a day that
    /// already exists in the index or on disk is [`WriteError::PageExists`], and the file keeps
    /// every byte it had — including text this process never wrote. A caller that wants different
    /// content uses a block write.
    ///
    /// A caller that wants "the journal of this day exists" ensured cannot treat every
    /// [`WriteError::PageExists`] as success, because the check that produced it matches a page
    /// *name*, not a day. The error carries the `page` and the `path` it found, and success is only
    /// claimed when that path is the journal of **this** `date` — which the caller compares against
    /// the day it asked for, or against the title and file name its own formats render. With a
    /// title format that carries no year, the refusal for one day's journal names the other day's
    /// file, and believing it means claiming a journal that was never written.
    ///
    /// # Order and failure
    ///
    /// The steps are [`Graph::create_page`]'s, with the name encoding replaced by the two journal
    /// formats:
    ///
    /// 1. **Take the write lock**, so no scan projects the file while it is created and none
    ///    interleaves with the page row that follows it. The lock and the duplicate detection are
    ///    the ordinary page writes': names are compared by [`page_name_key`], so a journal title
    ///    that collides with an existing page of either kind is refused here, before any file is
    ///    touched.
    /// 2. **Render the path and the title** from `date`, which is `GraphSource::journal_page` and
    ///    cannot fail: both formats were compiled when this graph was opened.
    /// 3. **Ask the index whether the page exists**, so a duplicate answers with the file the
    ///    existing page lives in.
    ///
    /// Steps 4 to 7 are the same `Self::create_page_file` tail: create the file without
    /// replacing anything — an existing file is [`WriteError::PageExists`] with its bytes
    /// untouched — observe and read what was created, replace the whole page and its baseline in
    /// one transaction (removing the new file again when a uniqueness constraint rejects it), and
    /// answer the committed metadata. As with a page, this write does not scan the graph.
    pub fn create_journal(&mut self, date: JournalDate) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer before anything is observed or created.
        let _lock = self.begin_write()?;

        // Step 2: the day decides both halves of the page's identity. Neither format can fail here.
        let (relative_path, title) = self.source.journal_page(date);

        // Step 3: ask the index first, so a duplicate answers with the file the existing page lives
        // in — whichever kind that page is, because the comparison key folds kinds together.
        if self
            .index
            .page_metadata(&title)?
            .is_some_and(|page| page.has_file())
        {
            return Err(self.existing_page(&title, &relative_path));
        }

        self.create_page_file(&relative_path, &title)
    }

    /// Create `relative_path` with an empty body and project it, naming it `name` in every error.
    ///
    /// This is the half [`Graph::create_page`] and [`Graph::create_journal`] share, and the two
    /// differ only in how they arrive at a path and a name: one encodes a page name, the other
    /// renders a date through the journal formats. Both callers take the write lock and ask the
    /// index for a duplicate first, so this assumes the file does not exist yet and that its bytes
    /// are its own.
    fn create_page_file(
        &mut self,
        relative_path: &Path,
        name: &str,
    ) -> Result<Read<PageMetadata>, WriteError> {
        if self.index.page_name_for_path(relative_path)?.is_some() {
            return Err(self.existing_page(name, relative_path));
        }
        // Step 5: create, never replace. A file that is already there keeps its bytes, which is the
        // one case the index check cannot see: the file exists and no row names it.
        match self.source.create_text(relative_path, "") {
            Ok(()) => {}
            Err(SourceError::AlreadyExists { path }) => {
                return Err(WriteError::PageExists {
                    page: name.to_owned(),
                    path,
                });
            }
            Err(error) => return Err(WriteError::Source(error)),
        }

        // Step 6: observe and read the file this call just made. The observation is taken after the
        // creation and before the read, so the baseline records an observation that is not later
        // than the text it describes. `append_block` re-observes because its `PageFile` predates the
        // replacement; this one is taken after the only change this call made.
        let fresh = match self.source.page_at(relative_path) {
            Ok(Some(file)) => file,
            Ok(None) => {
                drop(
                    self.source
                        .remove_text(&self.source.root().join(relative_path)),
                );
                return Err(WriteError::Source(SourceError::InvalidPageName {
                    path: self.source.root().join(relative_path),
                    reason: "the created file is not a discoverable page".to_owned(),
                }));
            }
            Err(error) => {
                drop(
                    self.source
                        .remove_text(&self.source.root().join(relative_path)),
                );
                return Err(WriteError::Source(error));
            }
        };
        let observed = fresh.metadata();
        let fresh_text = match fresh.read_text() {
            Ok(text) => text,
            Err(error) => {
                drop(
                    self.source
                        .remove_text(&self.source.root().join(relative_path)),
                );
                return Err(WriteError::Source(error));
            }
        };
        let fresh_document = PageDocument::parse(&fresh_text);
        let baseline = baseline_for(observed, &fresh_text);

        // One transaction writes the page row and the baseline together. The index can be stale
        // enough that it has a row for this page name or this file path although the caller's own
        // check found none, and SQLite's uniqueness constraints are what make that impossible to
        // commit; then this call's file is removed, because a page file no row points at would make
        // the next scan see two files for one page name and refuse the graph.
        let committed = match self.index.replace_page_with_baseline(
            fresh.relative_path(),
            fresh.name(),
            fresh.journal_date(),
            &fresh_document,
            baseline,
        ) {
            Ok(committed) => committed,
            Err(cause) if is_unique_violation(&cause) => {
                drop(
                    self.source
                        .remove_text(&self.source.root().join(fresh.relative_path())),
                );
                return Err(self.existing_page(name, fresh.relative_path()));
            }
            Err(cause) => {
                return Err(WriteError::IndexAfterWrite {
                    page: fresh.name().to_owned(),
                    path: fresh.path().to_owned(),
                    cause,
                });
            }
        };

        // The metadata of what was stored. The file was empty, so the projection has a page row
        // with no blocks; a missing row means the stored projection contradicts itself.
        let metadata = self
            .index
            .page_metadata(fresh.name())
            .map_err(|cause| answer_after_write(fresh.name(), fresh.path(), cause))?
            .ok_or_else(|| WriteError::IndexAfterWrite {
                page: fresh.name().to_owned(),
                path: fresh.path().to_owned(),
                cause: IndexError::CorruptRow {
                    column: "pages.name",
                    value: fresh.name().to_owned(),
                },
            })?;

        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// Delete the page `name` by moving its file into `logseq/.recycle/`, and answer the metadata
    /// it had before the removal.
    ///
    /// # The file is recycled, not removed
    ///
    /// A page is a Markdown file, and the file is the only copy of its content (ADR-0001). This
    /// write therefore moves it to `logseq/.recycle/` with its directory as a name prefix —
    /// `pages/Some Note.md` becomes `logseq/.recycle/pages_Some Note.md` — instead of unlinking it,
    /// which is what Logseq itself does. The page is gone from the graph as far as every read is
    /// concerned, and a user can still restore it by hand; a name already taken in the recycle bin
    /// gains a millisecond stamp rather than overwriting the earlier copy.
    ///
    /// # Links are not rewritten
    ///
    /// A `[[Some Note]]` in another page keeps pointing at the name it always named. After this
    /// call that name resolves to no page, so the link points at nothing — not a bug, but the same
    /// behaviour Logseq has, and the reason this write touches one file: following the links would
    /// mean rewriting every page that mentions the name, which is a graph-wide write this operation
    /// is not. The caller that wants that does it as its own sequence of block writes.
    ///
    /// # The index loses the page
    ///
    /// The page's derived rows and the baseline of its file are deleted, in the order the foreign
    /// key forces and with the same helper a scan uses when a file has disappeared. A
    /// [`Graph::page_metadata`] or [`Graph::pages`] read after this call therefore answers `None`
    /// and lists no such page.
    ///
    /// # Order and failure
    ///
    /// The steps are the single-file writes' orchestration (ADR-0004):
    ///
    /// 1. **Take the write lock**, so no scan projects the file while it is being moved.
    /// 2. **Find the file by page name**, through the index and, on a miss, one discovery. A page no
    ///    file holds is [`WriteError::PageNotFound`]; this is not idempotent, for the reason
    ///    [`Graph::remove_block`]'s missing block is not: a caller whose belief about the graph is
    ///    wrong is corrected instead of told it succeeded.
    /// 3. **Read the page's metadata** as it stands, because the answer is what the page was, and
    ///    after step 4 the index no longer holds it.
    /// 4. **Move the file into the recycle bin**, which is the first moment anything changes.
    /// 5. **Delete the rows the file backed** — the page row and then its baseline — and answer the
    ///    metadata of the revision that committed.
    ///
    /// A failure in steps 1 to 3 leaves the graph exactly as it was. A failure in step 5 leaves the
    /// file in the recycle bin with the old rows still indexed, reported as
    /// [`WriteError::IndexAfterWrite`]; the next scan sees the file gone and removes them.
    ///
    /// This write does not scan the graph, for the reason [`Graph::append_block`] gives, and it
    /// observes only the one file it removes.
    pub fn delete_page(&mut self, name: &str) -> Result<Read<PageMetadata>, WriteError> {
        // Step 1: serialise with every other writer of this graph before anything is observed.
        let _lock = self.begin_write()?;

        // Step 2: resolve the page name to the file that holds it. A page no file holds is a miss
        // in the authority, and this write never invents a file to delete.
        let file = self.locate_page(name)?;

        // Step 3: the answer is the page as it was, and step 5 deletes the rows it comes from. The
        // row was found by step 2's page address, so its absence here contradicts that lookup.
        let metadata = self.index.page_metadata(name)?.ok_or_else(|| {
            WriteError::Index(IndexError::CorruptRow {
                column: "pages.name",
                value: name.to_owned(),
            })
        })?;

        // Step 4: the file goes to the recycle bin, still byte-for-byte the page it was.
        source::move_to_recycle(self.source.root(), file.relative_path())?;

        // Step 5: drop the rows the file backed, through the same helper a scan uses for a file
        // that disappeared: the page row first, then the baseline the foreign key points it at.
        if let Err(error) = self.remove_file_rows(Some(file.name()), file.relative_path()) {
            return match error {
                EngineError::Index(cause) => Err(WriteError::IndexAfterWrite {
                    page: file.name().to_owned(),
                    path: file.path().to_owned(),
                    cause,
                }),
                other => Err(WriteError::Engine(other)),
            };
        }
        let committed = self
            .index
            .metadata()
            .map_err(|cause| answer_after_write(file.name(), file.path(), cause))?;

        Ok(Read::new(
            metadata,
            self.read_meta(committed, self.is_stale()),
        ))
    }

    /// The page a create refused to duplicate, naming the file the existing page lives in.
    ///
    /// The index is asked by page key, so any spelling of the existing name answers. When no row
    /// names the page — the file exists but was never projected — the requested spelling and the
    /// path are all this error can honestly report.
    fn existing_page(&self, name: &str, relative_path: &Path) -> WriteError {
        let stored = self.index.page_metadata(name).ok().flatten();
        if let Some(metadata) = &stored {
            let path = self
                .index
                .page_file(metadata.name())
                .ok()
                .flatten()
                .unwrap_or_else(|| relative_path.to_owned());
            return WriteError::PageExists {
                page: metadata.name().to_owned(),
                path,
            };
        }
        WriteError::PageExists {
            page: name.to_owned(),
            path: relative_path.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Fixture;
    use super::*;
    use crate::engine::write::WriteState;

    #[test]
    fn a_delete_answer_failure_after_the_recycle_is_file_changed() {
        let fixture = Fixture::new("delete-answer-after");
        fixture.write("pages/alpha.md", "- one\n");
        let mut graph = fixture.open(None);
        let revision = graph.metadata().expect("metadata").into_value().revision();
        graph.index.fail_next_graph_metadata();

        let error = graph
            .delete_page("alpha")
            .expect_err("the post-recycle metadata read is armed to fail");

        assert!(
            matches!(error, WriteError::IndexAfterWrite { .. }),
            "expected the post-commit answer error, got {error:?}"
        );
        assert_eq!(error.write_state(), WriteState::FileChanged);
        assert!(
            !fixture.graph_root.join("pages/alpha.md").exists(),
            "the page file already left pages/"
        );
        assert_eq!(
            fixture.read("logseq/.recycle/pages_alpha.md"),
            "- one\n",
            "the recycled file keeps the deleted bytes"
        );
        assert!(
            graph
                .index
                .page_metadata("alpha")
                .expect("read rows")
                .is_none(),
            "the delete transaction removed the page rows"
        );
        assert_eq!(
            graph.index.metadata().expect("read metadata").revision(),
            revision + 1,
            "the delete transaction committed before the answer read"
        );
    }
}
