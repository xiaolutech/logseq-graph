//! Local edit errors.

use std::fmt;

/// Failure of a local edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The endpoints of a removal range have different parents.
    RangeNotSiblings,
    /// The last endpoint precedes the first among their siblings.
    RangeReversed,
    /// The content is empty or only whitespace, so the block would carry nothing.
    EmptyContent,
    /// The content contains a line break that the operation cannot represent.
    ///
    /// [`crate::document::PageDocument::append_block`] rejects every line break, because the block it writes is one
    /// line. [`crate::document::PageDocument::update_block_text`] accepts `\n`, because a block's text is multi-line
    /// in the file, but rejects `\r` in both cases: the parser treats a carriage return before a
    /// line feed as part of the line ending, so trailing `\r` would silently disappear from the
    /// block text.
    MultilineContent,
    /// The appended line would not become its own block.
    ///
    /// This happens when the document ends inside an unclosed opaque region — a fenced code block
    /// or a `#+BEGIN_*` region — because the parser reads everything up to the closing marker as
    /// that region's content. Returning the text anyway would produce a file that looks edited and
    /// is not.
    EndsInsideOpaqueRegion,
    /// No block states `id:: <block_id>`, or the preorder path names no block of this page.
    ///
    /// One variant covers both spellings on purpose. A miss is a miss at the same step of every
    /// edit — the locator was resolved before any line was read — and the engine's write error
    /// distinguishes the two identities before the edit runs, so a caller never receives this error
    /// without already knowing which kind of address it used.
    BlockNotFound,
    /// A path names no block of this page.
    ///
    /// The two path spellings miss for the same reasons and report the same variant: an empty path
    /// names no block at all; [`crate::document::BlockLocator::Path`] with a first element no top-level block's
    /// preorder matches, or [`crate::document::BlockLocator::Indices`] with a top-level position the page does not
    /// have; and either spelling with a later element the level at that depth does not have, which
    /// includes a block a previous operation of the same batch removed.
    ///
    /// Kept apart from [`EditError::BlockNotFound`] so a path miss is never confused with an
    /// `id::` miss in logs and assertions: an out-of-range path is a caller holding coordinates
    /// from another revision, which is exactly the condition a handle expires on.
    PathOutOfRange,
    /// More than one block states the same `id::`.
    ///
    /// A durable id names exactly one block (ADR-0005). A file where two blocks claim the same one
    /// is corrupt, and picking either block would write the edit into an arbitrary place.
    AmbiguousBlockId,
    /// A continuation line would become a block, a property line, or part of the page's trailing
    /// blank space instead of belonging to the block's text.
    ///
    /// That text is structure, not content: writing it would turn one "change the text" into a
    /// structural change that the next scan projects into the index. Editing a property or a child
    /// block is a different operation.
    ContinuationChangesStructure,
    /// The block states a property line between two of its content lines.
    ///
    /// An editor shows only the editable text and never sends a property line back, so a block
    /// whose properties sit inside its content has no single place to write that text: writing it
    /// where the content was would move a property, and writing it after the properties would move
    /// the content the caller is looking at. Refusing is the honest answer; guessing where the
    /// property belongs is not an edit the caller asked for.
    InterleavedProperties,
    /// A heading marker level outside `1..=6`.
    ///
    /// Markdown stops at six `#` characters and the parser reads at most six, so a larger level
    /// would write a line that is not the block marker the caller asked for.
    InvalidHeadingLevel {
        /// The count of `#` characters the caller asked for; only `1..=6` is accepted.
        level: u8,
    },
    /// The move target is the moved block itself, or a block inside the subtree it moves.
    ///
    /// Either way the block would have to end up inside itself: `before` and `after` name a block
    /// that the same edit removes, and a child target would make the subtree its own descendant.
    ///
    /// One variant covers both cases because they are one mistake: the target has to be outside
    /// the range being cut out, and both spellings put it inside.
    TargetInsideMovedSubtree,
    /// No block states the id named as a move target.
    ///
    /// Kept apart from [`EditError::BlockNotFound`] on purpose. A move names two ids, and a caller
    /// that is told only "no block states that id" cannot tell which of the two was wrong.
    MoveTargetNotFound,
    /// The block states no property with the key the edit was asked to remove.
    PropertyNotFound,
    /// The property key is empty, or contains `::`, a line break, or leading/trailing whitespace.
    InvalidPropertyKey,
    /// The split offset is past the end of the block's editable text.
    ///
    /// The offset counts bytes of [`crate::document::BlockFacts::editable_text`], and the text's own length is a valid
    /// offset: it puts everything in the first half. A larger number names a position the block does
    /// not have, and clamping it would split the block somewhere the caller never pointed at.
    TextOffsetOutOfRange {
        /// The byte offset the caller gave into [`crate::document::BlockFacts::editable_text`].
        offset: usize,
        /// Length of that text; the only offsets this edit accepts are `0..=len`.
        len: usize,
    },
    /// The split offset lands inside one UTF-8 character of the block's editable text.
    ///
    /// A caret sits between characters and never inside one, so this offset was counted against a
    /// text that has changed since.
    TextOffsetSplitsCharacter {
        /// The byte offset that falls inside a UTF-8 character instead of between two.
        offset: usize,
    },
    /// The block has no previous sibling, so there is nothing above it to merge into or indent under.
    ///
    /// A top-level first block is the case: the file states no block before it at its own level, and
    /// its parent level is the page itself.
    NoPreviousSibling,
    /// A top-level block has no parent level to outdent to.
    ///
    /// Indentation is a block's depth in the file, so a block already at depth zero has no shallower
    /// place to go.
    TopLevelCannotOutdent,
}

impl fmt::Display for EditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeNotSiblings => formatter.write_str("range endpoints are not siblings"),
            Self::RangeReversed => formatter.write_str("range last endpoint precedes first"),
            Self::EmptyContent => formatter.write_str("block content is empty or only whitespace"),
            Self::MultilineContent => {
                formatter.write_str("block content must fit on one source line")
            }
            Self::EndsInsideOpaqueRegion => formatter.write_str(
                "the document ends inside an unclosed code fence or #+BEGIN_ region, so the \
                 appended line would become that region's content",
            ),
            Self::BlockNotFound => formatter.write_str("no block states that id"),
            Self::PathOutOfRange => {
                formatter.write_str("the block path names no block of this page")
            }
            Self::AmbiguousBlockId => formatter.write_str("more than one block states that id"),
            Self::ContinuationChangesStructure => formatter.write_str(
                "a continuation line would become a block, a property line, or page trailing \
                 space instead of block text",
            ),
            Self::InterleavedProperties => formatter.write_str(
                "the block states a property line between two of its content lines, so its \
                 editable text has no single place to go",
            ),
            Self::InvalidHeadingLevel { level } => write!(
                formatter,
                "the heading level {level} is outside the six levels Markdown has"
            ),
            Self::TargetInsideMovedSubtree => formatter.write_str(
                "the move target is the block being moved or a block inside its subtree, so the \
                 block would end up inside itself",
            ),
            Self::MoveTargetNotFound => {
                formatter.write_str("no block states the id named as the move target")
            }
            Self::PropertyNotFound => {
                formatter.write_str("the block states no property with that key")
            }
            Self::InvalidPropertyKey => formatter.write_str(
                "the property key is empty, contains `::` or a line break, or has leading or \
                 trailing whitespace",
            ),
            Self::TextOffsetOutOfRange { offset, len } => write!(
                formatter,
                "the split offset {offset} is past the end of the editable text, which is {len} bytes"
            ),
            Self::TextOffsetSplitsCharacter { offset } => write!(
                formatter,
                "the split offset {offset} lands inside one character of the editable text"
            ),
            Self::NoPreviousSibling => {
                formatter.write_str("the block has no previous sibling to merge or indent under")
            }
            Self::TopLevelCannotOutdent => {
                formatter.write_str("a top-level block has no parent level to outdent to")
            }
        }
    }
}

impl std::error::Error for EditError {}
