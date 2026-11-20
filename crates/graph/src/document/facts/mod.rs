//! Pure extraction of content facts from a parsed page.
//!
//! A fact is something the Markdown text states about a page, read without deciding what it means:
//! each block's own text, `key:: value` properties, page refs, block refs, and tags. Extraction
//! reads one [`PageDocument`] and touches neither files nor SQL, so it replays offline and is
//! testable against source strings.
//!
//! These types live in `document`, not `domain`, because they are an intermediate product of
//! parsing rather than a view handed to a caller. `index` projects them into rows; `domain` holds
//! only what a query returns, together with revision metadata (ADR-0003).
//!
//! Extraction also stops where interpretation would start. Property values stay raw text: this
//! layer does not type them, split comma-separated lists, follow `tags::` values to pages, or
//! honour a quoted value that suppresses links. Those rules depend on `logseq/config.edn` and on
//! Logseq compatibility, so they belong to the projection layer, which still has the raw value.
//!
//! Traversal is iterative, so document depth is bounded by memory rather than by the thread stack.

use super::parser::{trim_indent, Block, Fence, PageDocument};

mod block;
mod links;
mod properties;

pub use block::BlockFacts;
pub(crate) use links::property_list_targets;
pub use links::{extract_block_refs, extract_page_refs, extract_tags};
pub(crate) use properties::normalized_property_key;
pub use properties::Property;
pub(super) use properties::ID_PROPERTY;

use properties::{extract_page_properties, lookup};

/// Facts about one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFacts {
    properties: Vec<Property>,
    blocks: Vec<BlockFacts>,
}

impl PageFacts {
    /// Extract the facts of one parsed page.
    pub fn extract(document: &PageDocument) -> Self {
        Self {
            properties: extract_page_properties(document),
            blocks: extract_blocks(document),
        }
    }

    /// Page properties: pre-block properties in source order, duplicates included.
    ///
    /// The pre-block precedes any outliner block. A property inside a dash block belongs to that
    /// block instead, even when it is the first one.
    pub fn properties(&self) -> &[Property] {
        &self.properties
    }

    /// Last page property written under `key`, compared by normalized key.
    pub fn property(&self, key: &str) -> Option<&Property> {
        lookup(&self.properties, key)
    }

    /// A nonempty title in the page's properties.
    pub(crate) fn title(properties: &[Property]) -> Option<&str> {
        lookup(properties, "title")
            .map(Property::value)
            .filter(|title| !title.is_empty())
    }

    pub(crate) fn title_in(text: &str) -> Option<String> {
        let document = PageDocument::parse(text);
        Self::title(&extract_page_properties(&document)).map(str::to_owned)
    }

    /// Block facts in depth-first source order, parallel to
    /// [`PageDocument::blocks_in_preorder`](crate::document::PageDocument::blocks_in_preorder).
    pub fn blocks(&self) -> &[BlockFacts] {
        &self.blocks
    }
}

/// Lines of a text with opaque regions removed.
///
/// Fenced code and `#+BEGIN_*` regions are opaque: the delimiter lines and everything between them
/// carry no facts. Tracking is the same one the parser uses to decide what is outliner structure,
/// so a line that cannot open a block cannot produce facts either.
struct VisibleLines<I> {
    lines: I,
    fence: Option<Fence>,
}

impl<'a, I: Iterator<Item = (usize, &'a str)>> VisibleLines<I> {
    fn new(lines: I) -> Self {
        Self { lines, fence: None }
    }
}

impl<'a, I: Iterator<Item = (usize, &'a str)>> Iterator for VisibleLines<I> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        // Splitting the borrow lets the tracker and the line iterator be held at the same time.
        let Self { lines, fence } = self;
        for (index, line) in lines {
            if consume(fence, line) {
                return Some((index, line));
            }
        }
        None
    }
}

/// Feed one line to the opaque-region tracker and report whether the line is visible.
fn consume(fence: &mut Option<Fence>, line: &str) -> bool {
    let trimmed = trim_indent(line);
    if let Some(active) = *fence {
        if active.closes(trimmed) {
            *fence = None;
        }
        return false;
    }
    match Fence::open(trimmed) {
        Some(opening) => {
            *fence = Some(opening);
            false
        }
        None => true,
    }
}

/// Extract facts of every block, depth first, with an explicit stack.
fn extract_blocks(document: &PageDocument) -> Vec<BlockFacts> {
    let mut facts = Vec::new();
    let mut stack: Vec<std::slice::Iter<'_, Block>> = vec![document.blocks().iter()];
    while let Some(children) = stack.last_mut() {
        let Some(block) = children.next() else {
            stack.pop();
            continue;
        };
        facts.push(BlockFacts::extract(document, block));
        stack.push(block.children().iter());
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse;

    #[test]
    fn block_facts_align_with_the_block_structure() {
        let document = parse("type:: x\n\n- a\n\t- b\n- c\n");
        let facts = PageFacts::extract(&document);
        let preorder = document.blocks_in_preorder();

        assert_eq!(facts.blocks().len(), preorder.len());
        for (block, source) in facts.blocks().iter().zip(preorder) {
            assert_eq!(block.own_span(), source.own_span());
        }
    }

    #[test]
    fn deeply_nested_documents_extract_without_recursing() {
        // Extraction uses an explicit stack, so a 10000-level document must finish instead of
        // overflowing the thread stack.
        const LEVELS: usize = 10_000;
        let mut source = String::new();
        for depth in 0..LEVELS {
            source.push_str(&"\t".repeat(depth));
            source.push_str("- nested #deep\n");
        }

        let document = parse(&source);
        let facts = PageFacts::extract(&document);

        assert_eq!(facts.blocks().len(), LEVELS);
        assert_eq!(facts.blocks()[LEVELS - 1].text(), "nested #deep");
        assert_eq!(facts.blocks()[LEVELS - 1].tags(), ["deep".to_owned()]);
        drop(facts);
        drop(document);
    }
}
