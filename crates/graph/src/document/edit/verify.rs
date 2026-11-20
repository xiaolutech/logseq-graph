//! Post-edit structural and content verification.

use super::super::facts::{BlockFacts, PageFacts};
use super::super::line::LineSpan;
use super::super::parser::{Block, PageDocument};
use super::error::EditError;
use super::locator::{facts_of, find_block};
use super::property::property_lines;
use super::render::{indent_width, RenderedLine};
use super::subtree::place_of;
use crate::domain::BlockMarker;

/// One block the bytes an edit produced must read back as.
pub(super) struct ExpectedBlock {
    /// First source line of the block's own span in the output.
    pub(super) start: usize,
    /// First source line of the block that must hold this one, or `None` when it must be top-level.
    ///
    /// The level is a promise like the text is. Bytes that read back as the right block in the wrong
    /// place are not the edit that was asked for, and every other claim here can hold while a block
    /// silently changed parents: the block count, the text, and even the indentation width are all
    /// satisfied by a block one level off when the file indents unevenly.
    pub(super) parent: Option<usize>,
    pub(super) marker: BlockMarker,
    /// Leading whitespace width of the block's first line, in characters.
    pub(super) indent: usize,
    /// The block's editable text, as [`BlockFacts::editable_text`] answers it.
    pub(super) text: String,
    /// How strictly `text` must match.
    pub(super) text_match: TextMatch,
    /// Property lines the block states, as the key written and its line offset inside the own span.
    pub(super) properties: Vec<(String, usize)>,
    /// Number of direct children the block has.
    pub(super) children: usize,
}

/// How strictly a claim compares the block text it says the bytes hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextMatch {
    /// The text must read back exactly.
    Exact,
    /// The text may lose the leading whitespace run of its first line.
    ///
    /// Only [`PageDocument::split_block_text`]'s new block is claimed this way. The parser strips the
    /// whole whitespace run after a marker, so a tail that starts with spaces cannot read back with
    /// them; the edit writes those bytes after the marker anyway instead of moving the caller's offset
    /// to the next non-blank character, and this is the one difference it lets the parse show.
    LeadingWhitespaceStripped,
}

/// What an edit claims about the bytes it produced.
pub(super) struct Expected {
    /// Number of blocks the page must hold, at every depth.
    pub(super) blocks: usize,
    /// Blocks the edit placed, each named by the first source line of its own span.
    pub(super) blocks_at: Vec<ExpectedBlock>,
}

impl ExpectedBlock {
    /// Whether `document` holds this block exactly as claimed.
    fn holds(&self, document: &PageDocument, facts: &PageFacts) -> bool {
        // A block's own lines start exactly one line per source line, so the first line of a span is
        // its address: no two blocks can own the same line.
        let found = document
            .blocks_in_preorder()
            .into_iter()
            .find(|block| block.own_span().start() == self.start);
        let Some(found) = found else {
            return false;
        };
        let block = facts_of(facts, found.own_span());
        let text = match self.text_match {
            TextMatch::Exact => self.text.clone(),
            // Trimming the claim and trimming the parse are the same answer: what a first line cannot
            // state is one leading whitespace run, and nothing else.
            TextMatch::LeadingWhitespaceStripped => {
                self.text.trim_start_matches([' ', '\t']).to_owned()
            }
        };
        found.marker() == self.marker
            && parent_line(document, self.start) == Some(self.parent)
            && indent_width(document.lines()[self.start].text()) == self.indent
            && block.editable_text() == text
            && property_lines(block) == self.properties
            && found.children().len() == self.children
    }
}

/// The property lines of `block` as they land in `rendered`: the key the file wrote, and the offset
/// inside the block's own lines the render put it at.
///
/// Reading the offsets off the render is what keeps a re-parse check honest when the content around
/// a property grows or shrinks: the property line keeps its bytes and its side of the content, and
/// its offset is whatever the render made it, not the offset the source happened to hold.
pub(super) fn rendered_properties(
    block: &BlockFacts,
    rendered: &[RenderedLine],
) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (at, line) in rendered.iter().enumerate() {
        let Some(offset) = line.source else {
            continue;
        };
        #[expect(
            clippy::expect_used,
            reason = "`render_text_lines` sets `source` only for an own line it copied instead of \
                      writing content, and the offsets it treats as content are exactly \
                      `editable_line_offsets`, which `BlockFacts` defines as the own lines that are \
                      not properties"
        )]
        let property = block
            .properties()
            .iter()
            .find(|property| property.line() - block.own_span().start() == offset)
            .expect("a copied own line that is a property offset is one of the block's properties");
        out.push((property.key().to_owned(), at));
    }
    out
}

/// The block the block at line `start` lives under: `Some(None)` when it is top-level, and `None` when
/// no block starts on that line.
///
/// Traversal is a stack rather than recursion, because a document's depth is bounded by memory and
/// this module never recurses over one.
fn parent_line(document: &PageDocument, start: usize) -> Option<Option<usize>> {
    let mut pending: Vec<(&[Block], Option<usize>)> = vec![(document.blocks(), None)];
    while let Some((siblings, parent)) = pending.pop() {
        for block in siblings {
            if block.own_span().start() == start {
                return Some(parent);
            }
            pending.push((block.children(), Some(block.own_span().start())));
        }
    }
    None
}

/// Parse the bytes an edit produced and check every claim the edit made about them.
///
/// Writing bytes that look edited is not the same as writing bytes that are the edit: the parser is
/// the only authority on what a line becomes, so the result is parsed back and each claim is checked
/// against that parse. One failed claim refuses the edit, which is the cheap answer — the caller
/// writes only what it receives, so a refusal leaves the page as it was.
pub(super) fn reparse_expect(source: &str, expected: &Expected) -> Result<(), EditError> {
    let document = PageDocument::parse(source);
    let facts = PageFacts::extract(&document);
    let held = document.blocks_in_preorder().len() == expected.blocks
        && expected
            .blocks_at
            .iter()
            .all(|claim| claim.holds(&document, &facts));
    if held {
        Ok(())
    } else {
        Err(EditError::ContinuationChangesStructure)
    }
}

/// Where an indentation edit put the subtree it moved.
pub(super) struct Moved {
    /// The moved block's own lines in the document the edit started from.
    pub(super) block: LineSpan,
    /// First output line of the moved block.
    pub(super) start: usize,
    /// Leading whitespace width the block must read back with.
    pub(super) indent: usize,
    /// First output line of the block that must hold it, or `None` when it must be top-level.
    pub(super) parent: Option<usize>,
}

/// Check what [`PageDocument::indent_block_text`] and [`PageDocument::outdent_block_text`] moved.
///
/// `neighbours` names every block whose direct child count the move changes, with the count it holds
/// afterwards: an indent takes one child away from the level the block leaves and gives one to the
/// block it joins, an outdent the other way round, and both are promises the bytes must keep. A block
/// that only changes level is not a neighbour — the level moves, not the children. The block count
/// itself cannot change: an indentation edit moves lines and never adds or removes a block.
pub(super) fn expect_moved(
    document: &PageDocument,
    facts: &PageFacts,
    source: &str,
    moved: Moved,
    neighbours: &[(LineSpan, usize)],
) -> Result<(), EditError> {
    #[expect(
        clippy::expect_used,
        reason = "both callers pass `self`, the document they resolved `Moved::block` on before \
                  moving those lines, so the span names a block of what they hand over"
    )]
    let block = find_block(document, moved.block).expect("the moved block is in this document");
    let block_facts = facts_of(facts, moved.block);
    let mut blocks_at = vec![ExpectedBlock {
        start: moved.start,
        parent: moved.parent,
        marker: block.marker(),
        indent: moved.indent,
        text: block_facts.editable_text(),
        text_match: TextMatch::Exact,
        properties: property_lines(block_facts),
        children: block.children().len(),
    }];
    for (span, children) in neighbours {
        #[expect(
            clippy::expect_used,
            reason = "every neighbour span came from `block_span` or `place_of` on the same document \
                      the caller passed, not from the re-parse"
        )]
        let neighbour = find_block(document, *span).expect("the neighbour is in this document");
        let neighbour_facts = facts_of(facts, *span);
        blocks_at.push(ExpectedBlock {
            start: span.start(),
            parent: place_of(document, *span)
                .parent
                .map(|parent| parent.start()),
            marker: neighbour.marker(),
            indent: indent_width(document.lines()[span.start()].text()),
            text: neighbour_facts.editable_text(),
            text_match: TextMatch::Exact,
            properties: property_lines(neighbour_facts),
            children: *children,
        });
    }
    let expected = Expected {
        blocks: document.blocks_in_preorder().len(),
        blocks_at,
    };
    reparse_expect(source, &expected)
}
