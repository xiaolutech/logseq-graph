//! Projection of a parsed page into the derived rows the index stores.
//!
//! Projection keeps structure, content facts, and each block's own text: hierarchy, sibling order,
//! preorder position, the marker that opened the block, the text `document::facts` reconstructs from
//! the block's own lines, the properties, page refs, block refs, and tags it extracts from that
//! text, the task marker and priority that text leads with, the `id::` UUID a block states when that
//! value is a UUID, and, for a block that states none, the random ephemeral id this layer mints
//! (ADR-0012). The mint is here rather than in
//! [`ProjectedPage::project`] because that projection is a pure function of one parse and stays one:
//! it is what makes the structure, the text, and the facts comparable across runs. The text is not a raw source
//! line: the marker and the block's alignment indentation are
//! removed and continuation lines are joined, so a file can rebuild it and the index stays derived
//! (ADR-0007). Source line numbers and line spans stay out of the rows, so the index never becomes a
//! second copy of the Markdown.
//!
//! Structure and text come from the same [`PageDocument`], so a page is parsed once per replacement.
//! Both walks are the same depth-first order, which is what lets a block's text and facts be
//! addressed by its preorder position.
//!
//! The walk uses an explicit stack instead of recursion, so the input depth that reaches the
//! recursion limit is a policy decision ([`MAX_BLOCK_DEPTH`](super::MAX_BLOCK_DEPTH)) rather than a
//! property of the thread stack.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

use crate::document::{Block, BlockFacts, PageDocument, PageFacts, Property};
use crate::domain::{
    page_name_key, BlockId, BlockMarker, EphemeralId, GraphMetadata, JournalDate, Priority,
    TaskMarker,
};

use super::baseline::FileBaseline;
use super::{advance_revision, baseline, IndexError, MAX_BLOCK_DEPTH};

/// One block of a projected page, in preorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectedBlock {
    /// Depth-first position among the page's blocks, starting at zero.
    ///
    /// This is also the position of the block in [`ProjectedPage::blocks`], so a parent's position
    /// is always smaller than its children's.
    pub(super) preorder: u32,
    /// Preorder position of the parent block.
    pub(super) parent: Option<u32>,
    /// Nesting depth, starting at zero for top-level blocks.
    pub(super) depth: u32,
    /// Position among blocks that share the same parent.
    pub(super) sibling_index: u32,
    /// Marker that opened the block.
    pub(super) marker: BlockMarker,
    /// The block's own content, as [`BlockFacts::text`] reconstructs it from the same parse.
    pub(super) text: String,
    /// The `id::` UUID the block states, or `None` when it states none.
    pub(super) durable_id: Option<BlockId>,
    /// The task marker the block's text leads with, or `None` when the block is not a task.
    pub(super) task_marker: Option<TaskMarker>,
    /// The priority that sits where a priority belongs, or `None`.
    pub(super) priority: Option<Priority>,
}

/// The link facts of one block, identified the way a later reader will compare them.
///
/// `document::facts` de-duplicates a link with an ASCII-only comparison because it keeps text, not
/// identity. [`page_name_key`] is the authority on identity, so a block that writes `[[Ä]] [[ä]]`
/// produces one row here, and the first spelling is the one kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectedLinks {
    page_refs: Vec<(String, String)>,
    tags: Vec<(String, String)>,
}

impl ProjectedLinks {
    fn new(facts: &BlockFacts) -> Self {
        Self {
            page_refs: keyed(facts.page_refs()),
            tags: keyed(facts.tags()),
        }
    }

    /// Page refs as `(name as written, page key)`, one per referenced page, in source order.
    pub(super) fn page_refs(&self) -> &[(String, String)] {
        &self.page_refs
    }

    /// Tags as `(name as written, page key)`, one per tag, in source order.
    pub(super) fn tags(&self) -> &[(String, String)] {
        &self.tags
    }
}

/// Pair each name with its key, dropping a later spelling of a name already present.
fn keyed(names: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(names.len());
    for name in names {
        let key = page_name_key(name);
        if out.iter().any(|(_, existing)| *existing == key) {
            continue;
        }
        out.push((name.clone(), key));
    }
    out
}

/// A page ready to be written as one unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectedPage {
    blocks: Vec<ProjectedBlock>,
    facts: PageFacts,
    links: Vec<ProjectedLinks>,
    title: Option<String>,
}

impl ProjectedPage {
    /// Project every block of `document` into preorder rows, and extract the page's content facts.
    ///
    /// Fails with [`IndexError::TooDeep`] when the page nests deeper than the index supports.
    pub(crate) fn project(document: &PageDocument) -> Result<Self, IndexError> {
        let facts = PageFacts::extract(document);
        let mut blocks = Vec::new();
        let mut pending: Vec<Pending<'_>> = Vec::new();
        push_siblings(&mut pending, document.blocks(), 0, None);

        while let Some(Pending {
            block,
            depth,
            parent,
            sibling_index,
        }) = pending.pop()
        {
            if depth > MAX_BLOCK_DEPTH {
                return Err(IndexError::TooDeep {
                    depth,
                    limit: MAX_BLOCK_DEPTH,
                });
            }

            let preorder = blocks.len() as u32;
            blocks.push(ProjectedBlock {
                preorder,
                parent,
                depth,
                sibling_index,
                marker: block.marker(),
                text: String::new(),
                durable_id: None,
                task_marker: None,
                priority: None,
            });
            push_siblings(&mut pending, block.children(), depth + 1, Some(preorder));
        }

        // Facts and structure come from the same document and walk the same tree, so a mismatch is
        // a defect in this build rather than a property of the input. Refusing the page keeps a
        // half-aligned projection out of the index instead of attaching facts to the wrong blocks.
        if facts.blocks().len() != blocks.len() {
            return Err(IndexError::CorruptRow {
                column: "document.blocks",
                value: format!(
                    "the structure walk found {} blocks but the facts cover {}",
                    blocks.len(),
                    facts.blocks().len()
                ),
            });
        }

        // The lengths agree, so each block takes the text of the fact at its own preorder. Copying
        // it here is what makes one parse feed structure, text, and facts; no step re-parses or
        // re-reads the file.
        for (block, facts) in blocks.iter_mut().zip(facts.blocks()) {
            block.text = facts.text().to_owned();
        }

        // The durable id comes from the same block facts, addressed by the same preorder. What
        // counts as an `id::` is one rule with one definition, in
        // [`BlockFacts::durable_id`](crate::document::BlockFacts::durable_id): normalized key,
        // last-one-wins, and a value that does not parse as a UUID is no id rather than an error.
        // The snapshot read calls the same function, so the column and the read cannot disagree.
        for (block, facts) in blocks.iter_mut().zip(facts.blocks()) {
            block.durable_id = facts.durable_id();
        }

        // The two leading facts come from that same text, through the one definition of each rule:
        // `BlockFacts::task_marker` calls `TaskMarker::leading` and `BlockFacts::priority` calls
        // `Priority::leading_after`, so the parser axis, a query, and this column cannot disagree
        // about whether a block is a task or at what priority. Reading them here is a read of facts
        // already extracted, not another parse of the file.
        for (block, facts) in blocks.iter_mut().zip(facts.blocks()) {
            block.task_marker = facts.task_marker();
            block.priority = facts.priority();
        }

        // Link rows are keyed here, once, so the write path stores what this layer decided instead of
        // comparing names again.
        let links = facts.blocks().iter().map(ProjectedLinks::new).collect();

        let title = PageFacts::title(facts.properties()).map(str::to_owned);
        Ok(Self {
            blocks,
            facts,
            links,
            title,
        })
    }

    /// Blocks in preorder.
    pub(super) fn blocks(&self) -> &[ProjectedBlock] {
        &self.blocks
    }

    /// Number of blocks in the page, at every depth.
    pub(super) fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }

    /// Page properties from the page preamble, in source order.
    pub(super) fn properties(&self) -> &[Property] {
        self.facts.properties()
    }

    pub(crate) fn name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.title.as_deref().unwrap_or(fallback)
    }

    /// Facts of the block at `preorder`.
    ///
    /// `preorder` must name a block of this page, which is what [`ProjectedBlock::preorder`] holds
    /// for every block the write path visits.
    pub(super) fn block_facts(&self, preorder: u32) -> &BlockFacts {
        &self.facts.blocks()[preorder as usize]
    }

    /// Keyed page refs and tags of the block at `preorder`, which must name a block of this page.
    pub(super) fn block_links(&self, preorder: u32) -> &ProjectedLinks {
        &self.links[preorder as usize]
    }
}

/// A block waiting to be emitted, with the position it must be emitted at.
struct Pending<'a> {
    block: &'a Block,
    depth: u32,
    parent: Option<u32>,
    sibling_index: u32,
}

/// Queue siblings in reverse so the stack pops them in source order.
fn push_siblings<'a>(
    pending: &mut Vec<Pending<'a>>,
    blocks: &'a [Block],
    depth: u32,
    parent: Option<u32>,
) {
    for (sibling_index, block) in blocks.iter().enumerate().rev() {
        pending.push(Pending {
            block,
            depth,
            parent,
            sibling_index: sibling_index as u32,
        });
    }
}

impl super::Index {
    /// Replace every derived row of `name` with the page parsed into `document`.
    ///
    /// The replacement is one transaction: the previous rows are deleted, the complete new page is
    /// inserted, and the revision advances exactly once. On any error the previous rows survive.
    /// Returns the metadata of the committed revision.
    ///
    /// "Every derived row" includes the page's properties and each block's properties, page refs,
    /// block refs, and tags. They come from the document passed here, which is the one parse the
    /// caller already did: projection never re-parses, so the rows can never describe text the
    /// caller did not read.
    ///
    /// `name` is stored as written and also as `page_name_key`, which is the page's identity: a
    /// replacement or removal addressed to another spelling of the same page acts on the same row,
    /// so the read model can never hold two pages that Logseq considers one.
    ///
    /// `journal_date` is the calendar day a journal is, and `None` for a regular page. It is the one
    /// input the caller has to bring: the day comes from parsing the journal's file name under the
    /// configured format, and turning a name back into a day is a naming rule this layer does not
    /// own. The two values are paired, not independent — the schema refuses a journal without a day
    /// and a page with one. A caller outside this crate that passes a mismatched pair therefore gets
    /// the constraint failure itself ([`IndexError::Sqlite`]) instead of a named error, so it has to
    /// keep "my inputs disagree" apart from a storage failure by that error alone. The projection
    /// does not check that the day matches the file name either: only the file name format in
    /// `source` can decide that, and a second opinion here could disagree with it.
    ///
    /// The stored `pages.revision` is the graph revision of the write that produced the row, not a
    /// freshness signal: a page whose file has changed since keeps that revision until it is
    /// projected again, and whether the rows still match the files is a separate question answered
    /// by the file baseline and reported through freshness (ADR-0006).
    ///
    /// `relative_path` and `baseline` are the file this projection came from and the observation of
    /// it. They are not optional: `pages.relative_path` references `file_baseline`, so a page row
    /// can only exist for a file something observed, and that column is what answers "which file
    /// holds this page" without discovering the graph again.
    pub fn replace_page_with_baseline(
        &mut self,
        relative_path: &Path,
        name: &str,
        journal_date: Option<JournalDate>,
        document: &PageDocument,
        baseline: FileBaseline,
    ) -> Result<GraphMetadata, IndexError> {
        self.replace(relative_path, name, journal_date, document, baseline)
    }

    /// Replace every derived row of `name` with the page parsed into `document`, as one page of
    /// a multi-page commit.
    ///
    /// See [`Self::replace_page_with_baseline`] for the per-page semantics this shares. The
    /// difference is the transaction this joins: the pages given here are replaced and the
    /// revision advances exactly once — for any number of
    /// pages, including one, in one commit.
    pub fn replace_pages_with_baselines(
        &mut self,
        pages: &[(
            PathBuf,
            String,
            Option<JournalDate>,
            PageDocument,
            FileBaseline,
        )],
    ) -> Result<GraphMetadata, IndexError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let metadata = advance_revision(&tx)?;
        for (relative_path, name, journal_date, document, baseline) in pages {
            write_page(
                &tx,
                relative_path,
                name,
                *journal_date,
                document,
                *baseline,
                metadata.revision(),
            )?;
        }
        tx.commit()?;
        Ok(metadata)
    }
    fn replace(
        &mut self,
        relative_path: &Path,
        name: &str,
        journal_date: Option<JournalDate>,
        document: &PageDocument,
        baseline: FileBaseline,
    ) -> Result<GraphMetadata, IndexError> {
        // A write transaction takes the write lock up front instead of upgrading a read snapshot
        // later, which keeps the revision read and the following writes on one snapshot.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let metadata = advance_revision(&tx)?;
        write_page(
            &tx,
            relative_path,
            name,
            journal_date,
            document,
            baseline,
            metadata.revision(),
        )?;
        tx.commit()?;
        Ok(metadata)
    }
}

/// Write the property rows of one page.
///
/// `ordinal` is the position among the owner's property lines, so the source order and every
/// repeated key survive; "last one wins" is left to the read that asks for one key.
fn write_page_properties(
    tx: &Transaction<'_>,
    page_id: i64,
    properties: &[Property],
) -> Result<(), IndexError> {
    if properties.is_empty() {
        return Ok(());
    }
    let mut insert = tx.prepare_cached(
        "INSERT INTO page_properties (page_id, ordinal, key, normalized_key, value)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (ordinal, property) in properties.iter().enumerate() {
        insert.execute(params![
            page_id,
            ordinal as i64,
            property.key(),
            property.normalized_key(),
            property.value()
        ])?;
    }
    Ok(())
}

/// Write every fact row of one block: properties, page refs, block refs, and tags.
///
/// Each fact is stored as the text it was read from, next to the key that decides what it names.
/// Deciding that a value is a date, that a `tags::` value links, or that a quoted value does not,
/// happens in a later layer that still has this row; see the module documentation.
fn write_block_facts(
    tx: &Transaction<'_>,
    block_id: i64,
    facts: &BlockFacts,
    links: &ProjectedLinks,
) -> Result<(), IndexError> {
    if !facts.properties().is_empty() {
        let mut insert = tx.prepare_cached(
            "INSERT INTO block_properties (block_id, ordinal, key, normalized_key, value)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (ordinal, property) in facts.properties().iter().enumerate() {
            insert.execute(params![
                block_id,
                ordinal as i64,
                property.key(),
                property.normalized_key(),
                property.value()
            ])?;
        }
    }

    if !links.page_refs().is_empty() {
        let mut insert = tx.prepare_cached(
            "INSERT INTO page_refs (block_id, target_name, target_key) VALUES (?1, ?2, ?3)",
        )?;
        for (name, key) in links.page_refs() {
            insert.execute(params![block_id, name, key])?;
        }
    }

    if !facts.block_refs().is_empty() {
        let mut insert =
            tx.prepare_cached("INSERT INTO block_refs (block_id, target_uuid) VALUES (?1, ?2)")?;
        for uuid in facts.block_refs() {
            insert.execute(params![block_id, uuid])?;
        }
    }

    if !links.tags().is_empty() {
        let mut insert =
            tx.prepare_cached("INSERT INTO tags (block_id, name, name_key) VALUES (?1, ?2, ?3)")?;
        for (name, key) in links.tags() {
            insert.execute(params![block_id, name, key])?;
        }
    }

    Ok(())
}

/// Write one page as a unit inside an already-open transaction: delete its previous rows, record
/// the baseline of the file it was projected from, insert the new page with its facts.
///
/// This is the per-page half of [`Index::replace_page_with_baseline`] and
/// [`Index::replace_pages_with_baselines`]; the transaction and the revision advance belong to the
/// caller, so several pages can share one commit. The per-page order is the same the single-page
/// path always used, and it is forced by the constraints, not chosen: the old page row is deleted
/// first because `pages.relative_path` is UNIQUE, so two pages swapping files could not both
/// insert while both old rows stand; the baseline comes before the page row because
/// `pages.relative_path` references it; and the whole page is written before the next one begins
/// so a block's parent row key is already known when the child needs it.
///
/// `journal_date` is the day the journal's file name states, and `None` for a regular page. It is
/// passed in rather than derived here because turning a file name into a day is the journal naming
/// rule in `source`, which this layer must not repeat; the schema's CHECK refuses a day that
/// disagrees with the kind, so a caller cannot store a journal without one.
///
/// The ephemeral ids of the page are decided here too, and this is the only place either identity
/// column is written (ADR-0012): a block that states an `id::` takes none, and a block that states
/// none takes the id its previous row already held for this preorder when the recorded content
/// fingerprint equals the one being stored, or a freshly minted random one otherwise. Identical bytes
/// are identical blocks, so reusing their ids is correct; changed bytes replace every row of the page,
/// so every old id of it stops naming anything.
fn write_page(
    tx: &Transaction<'_>,
    relative_path: &Path,
    name: &str,
    journal_date: Option<JournalDate>,
    document: &PageDocument,
    baseline: FileBaseline,
    revision: u64,
) -> Result<(), IndexError> {
    let page = ProjectedPage::project(document)?;
    write_prepared_page(
        tx,
        relative_path,
        name,
        journal_date,
        &page,
        baseline,
        revision,
    )
}

pub(super) fn write_prepared_page(
    tx: &Transaction<'_>,
    relative_path: &Path,
    name: &str,
    journal_date: Option<JournalDate>,
    page: &ProjectedPage,
    baseline: FileBaseline,
    revision: u64,
) -> Result<(), IndexError> {
    // Deleting the page row is the only deletion: `blocks.page_id` cascades, so the page can
    // never be removed while its blocks survive, and `blocks` in turn cascades to every fact row
    // of a block, while `pages` cascades to the page properties. Foreign key enforcement is a
    // tested invariant of `prepare`, which every open goes through.
    //
    // The row is found by name key, not by spelling: `Ä` and `ä` are one page to Logseq, so
    // replacing one spelling has to replace the row the other spelling produced instead of
    // leaving the read model holding both.
    //
    // Before the deletion, and only when the page already had rows, the ephemeral ids that survive a
    // re-projection are read out: a page whose bytes did not change is the same blocks, so the ids
    // already minted for them are still the right answer and replacing them would expire every
    // block selector a caller holds for no reason (ADR-0012). Every other case mints fresh values.
    let name = page.name(name);
    let name_key = page_name_key(name);
    let relative_path = baseline::path_key(relative_path)?;
    let reusable = match tx
        .query_row(
            "SELECT id FROM pages WHERE relative_path = ?1",
            [&relative_path],
            |row| row.get(0),
        )
        .optional()?
    {
        Some(existing) => {
            let reusable = reusable_ephemeral_ids(tx, existing, baseline.content_hash())?;
            tx.execute("DELETE FROM pages WHERE id = ?1", [existing])?;
            reusable
        }
        None => HashMap::new(),
    };

    let path = Path::new(&relative_path);
    baseline::write(tx, path, baseline)?;

    // The kind is a function of the path, and `source` owns which directories hold which kind. A path
    // outside both cannot be projected, so this reports the column rather than writing a kind no
    // reader could trust.
    let kind = crate::source::kind_of_path(path).ok_or_else(|| IndexError::CorruptRow {
        column: "pages.relative_path",
        value: relative_path.clone(),
    })?;

    tx.execute(
        "INSERT INTO pages (name, name_key, kind, journal_date, relative_path, block_count,
                            revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            name,
            name_key,
            kind.as_str(),
            journal_date.map(|date| date.to_string()),
            relative_path,
            page.block_count(),
            revision,
        ],
    )?;
    let page_id = tx.last_insert_rowid();
    write_page_properties(tx, page_id, page.properties())?;
    super::identity::write_claims(tx, page_id, name, page)?;

    // Preorder guarantees that a parent is inserted before its children, so `block_ids`
    // already holds the row key the child needs. Facts follow the row they belong to, and are
    // addressed by the same preorder position the structure walk produced.
    let mut block_ids = Vec::with_capacity(page.blocks().len());
    for block in page.blocks() {
        let parent_id = match block.parent {
            Some(preorder) => Some(*block_ids.get(preorder as usize).ok_or(
                IndexError::CorruptRow {
                    column: "blocks.parent_id",
                    value: format!("no parent row at preorder {preorder}"),
                },
            )?),
            None => None,
        };
        let (marker_kind, heading_level) = match block.marker {
            BlockMarker::Bullet => ("bullet", None),
            BlockMarker::Heading { level } => ("heading", Some(level)),
        };
        // A block that states an `id::` has no ephemeral id, and one that states none takes the id
        // its previous row already held for this preorder when the bytes are unchanged, or a minted
        // one otherwise. The pair is the identity invariant the schema enforces; this is the only
        // place either column is written.
        let ephemeral_id = match &block.durable_id {
            Some(_) => None,
            None => Some(match reusable.get(&block.preorder) {
                Some(id) => id.clone(),
                None => mint_ephemeral_id()?,
            }),
        };
        let block_id = insert_block(
            tx,
            page_id,
            parent_id,
            block,
            marker_kind,
            heading_level,
            ephemeral_id,
        )?;
        block_ids.push(block_id);
        write_block_facts(
            tx,
            block_id,
            page.block_facts(block.preorder),
            page.block_links(block.preorder),
        )?;
    }

    Ok(())
}

/// Insert one block row and answer its row key.
///
/// `ephemeral_id` is the id the row will carry, or `None` for a block that states an `id::`. A value
/// can still collide with a row of another page, because the ids are random and the table is shared;
/// the UNIQUE index on the column is what makes that a conflict instead of two blocks named by one
/// id, and one re-mint with a fresh value is enough to resolve it — a second conflict is propagated,
/// because by then the random source or the database is broken, and retrying would loop.
fn insert_block(
    tx: &Transaction<'_>,
    page_id: i64,
    parent_id: Option<i64>,
    block: &ProjectedBlock,
    marker_kind: &str,
    heading_level: Option<u8>,
    ephemeral_id: Option<EphemeralId>,
) -> Result<i64, IndexError> {
    let mut ephemeral_id = ephemeral_id;
    let mut minted_once = false;
    loop {
        let stored = ephemeral_id.as_ref().map(EphemeralId::as_str);
        let inserted = tx.execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, durable_id, ephemeral_id,
                                 task_marker, priority)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                page_id,
                parent_id,
                block.preorder,
                block.depth,
                block.sibling_index,
                marker_kind,
                heading_level,
                block.text,
                block.durable_id.as_ref().map(BlockId::as_str),
                stored,
                block.task_marker.map(TaskMarker::as_str),
                block.priority.map(Priority::as_str),
            ],
        );
        match inserted {
            Ok(_rows) => return Ok(tx.last_insert_rowid()),
            Err(error) if !minted_once && stored.is_some() && is_unique_violation(&error) => {
                // The one conflict a row can have here that a fresh value resolves: another block
                // already holds this random id. `(page_id, preorder)` is unique by construction and
                // is not re-minted, so a violation of that one is propagated below.
                minted_once = true;
                ephemeral_id = Some(mint_ephemeral_id()?);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Whether SQLite refused a statement because it would repeat a unique value.
///
/// The distinction matters for the one retry above: re-minting an ephemeral id can only resolve a
/// uniqueness conflict, while every other refusal — a CHECK, a foreign key, a missing table — would
/// fail again with a different random value, so it must be reported instead of retried.
fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(status, _)
            if status.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

/// One random ephemeral id.
///
/// The bytes come from the operating system's random source rather than from anything about the
/// block, which is what makes a stale id unable to name a block by construction: there is no
/// function of position or revision that could recompute it (ADR-0009, ADR-0012). A system that
/// cannot provide random bytes cannot mint identity, so it is reported instead of guessed.
fn mint_ephemeral_id() -> Result<EphemeralId, IndexError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| IndexError::Entropy {
        reason: error.to_string(),
    })?;
    Ok(EphemeralId::from_bytes(bytes))
}

/// The ephemeral ids the page's current rows hold, when those rows describe the same bytes.
///
/// The comparison is the page's recorded content fingerprint against the one this projection is
/// about to store. Equal fingerprints mean one text, one parser version (a different parser version
/// is a different database), and therefore the same block at every preorder, so the id the old row
/// held for that preorder is still the id of that block. That reuse is what keeps a re-projection
/// that changes nothing from expiring every ephemeral selector in the graph, which is the
/// graph-wide granularity ADR-0009 rejected.
///
/// A different fingerprint returns nothing, so the whole page mints new ids and every old one names
/// nothing from that moment. Nothing re-binds an old id to a new row.
fn reusable_ephemeral_ids(
    tx: &Transaction<'_>,
    page_id: i64,
    content_hash: u64,
) -> Result<HashMap<u32, EphemeralId>, IndexError> {
    let stored_hash: Option<i64> = tx
        .query_row(
            "SELECT file_baseline.content_hash
               FROM pages
               JOIN file_baseline ON file_baseline.relative_path = pages.relative_path
              WHERE pages.id = ?1",
            [page_id],
            |row| row.get(0),
        )
        .optional()?;
    if stored_hash.map(|stored| stored as u64) != Some(content_hash) {
        return Ok(HashMap::new());
    }

    let mut statement = tx.prepare(
        "SELECT preorder, ephemeral_id FROM blocks
          WHERE page_id = ?1 AND ephemeral_id IS NOT NULL",
    )?;
    let mut rows = statement.query([page_id])?;
    let mut reusable = HashMap::new();
    while let Some(row) = rows.next()? {
        let preorder: u32 = row.get(0)?;
        let stored: String = row.get(1)?;
        let id = EphemeralId::parse(&stored).map_err(|_| IndexError::CorruptRow {
            column: "blocks.ephemeral_id",
            value: stored,
        })?;
        reusable.insert(preorder, id);
    }
    Ok(reusable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;

    /// One projected block, as the walk over a parsed page would produce it.
    fn block(preorder: u32, text: &str) -> ProjectedBlock {
        ProjectedBlock {
            preorder,
            parent: None,
            depth: 0,
            sibling_index: preorder,
            marker: BlockMarker::Bullet,
            text: text.to_owned(),
            durable_id: None,
            task_marker: TaskMarker::leading(text),
            priority: Priority::leading_after(text, TaskMarker::leading(text)),
        }
    }

    /// Store a page with one block that holds `ephemeral_id`, and answer the index holding it.
    ///
    /// The rows are written directly because this is a test about the mint, not about the parse: one
    /// block already occupies the value the mint would produce only if the random source repeated
    /// itself, which is what the retry exists for and no test can wait for.
    fn page_holding(ephemeral_id: &EphemeralId) -> Index {
        let mut index = Index::open_in_memory().expect("in-memory index");
        let tx = index.conn.transaction().expect("write transaction");
        tx.execute(
            "INSERT INTO file_baseline (relative_path, size, modified_ns, content_hash)
             VALUES ('pages/p.md', 0, NULL, 0)",
            [],
        )
        .expect("baseline");
        tx.execute(
            "INSERT INTO pages (name, name_key, kind, relative_path, block_count, revision)
             VALUES ('p', 'p', 'page', 'pages/p.md', 1, 0)",
            [],
        )
        .expect("page");
        tx.execute(
            "INSERT INTO blocks (page_id, parent_id, preorder, depth, sibling_index,
                                 marker_kind, heading_level, text, ephemeral_id)
             VALUES (1, NULL, 0, 0, 0, 'bullet', NULL, 'a', ?1)",
            [ephemeral_id.as_str()],
        )
        .expect("block holding the value the mint is about to repeat");
        tx.commit().expect("commit the fixture");
        index
    }

    #[test]
    fn a_minted_id_that_collides_with_a_stored_one_is_minted_again() {
        // A repeated random value is the one conflict a fresh value resolves, and the UNIQUE index
        // is what turns it into a conflict instead of two blocks named by one id. Handing the insert
        // a value another row already holds is how the collision is staged without controlling the
        // random source.
        let taken = EphemeralId::parse("01234567-89ab-cdef-0123-456789abcdef").expect("valid UUID");
        let mut index = page_holding(&taken);

        let tx = index.conn.transaction().expect("write transaction");
        let stored = insert_block(
            &tx,
            1,
            None,
            &block(1, "b"),
            "bullet",
            None,
            Some(taken.clone()),
        )
        .expect("a collision on the ephemeral index is resolved by minting again");
        let ephemeral: String = tx
            .query_row(
                "SELECT ephemeral_id FROM blocks WHERE id = ?1",
                [stored],
                |row| row.get(0),
            )
            .expect("read the stored id");
        tx.commit().expect("commit");

        assert_ne!(
            ephemeral,
            taken.as_str(),
            "the row that was here keeps the value it took first"
        );
        let overwritten: String = index
            .conn
            .query_row("SELECT ephemeral_id FROM blocks WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read the first block's id");
        assert_eq!(overwritten, taken.as_str());
    }
}

#[cfg(test)]
#[path = "projection_facts_tests.rs"]
mod facts_tests;

#[cfg(test)]
#[path = "projection_tasks_tests.rs"]
mod tasks_tests;
