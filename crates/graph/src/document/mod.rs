//! Lossless, line-oriented Markdown document model.
//!
//! The model keeps every source line and its original line ending, so parsing and rendering are
//! byte-exact inverses. It records outliner structure as half-open line spans and extracts content
//! facts (block text, properties, refs, tags) as pure functions of the parsed text. Block identity
//! and edits belong to later layers.

mod edit;
mod facts;
mod line;
mod parser;

pub use edit::{BlockLocator, EditError, MoveTarget};
pub use facts::{
    extract_block_refs, extract_page_refs, extract_tags, BlockFacts, PageFacts, Property,
};
pub(crate) use facts::{normalized_property_key, property_list_targets};
pub use line::{LineEnding, LineSpan, SourceLine};
pub use parser::{parse, Block, PageDocument};
