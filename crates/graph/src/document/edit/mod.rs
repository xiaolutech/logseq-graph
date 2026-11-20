//! Pure local edits to a parsed page.
//!
//! This is the third step of ADR-0004: turning a structured intent into the exact source bytes a
//! write must store. The result is a `String`, not an edited [`crate::document::PageDocument`], because the caller
//! writes those bytes and then re-reads and re-parses the file (ADR-0004, step 6). Returning bytes
//! is honest about that: an edited in-memory tree would only claim what the file is supposed to
//! contain.
//!
//! Four single-page edits live here, all pure functions from a parsed page to new source bytes:
//!
//! - [`crate::document::PageDocument::append_block`] adds one top-level block with single-line content. Only
//!   top-level blocks are appended: the new line carries no indentation, so it can never land
//!   inside an existing subtree.
//! - [`crate::document::PageDocument::update_block_text`] replaces the text of the block that states a given
//!   durable id. It replaces one block's own lines and nothing else, and it keeps that block's
//!   marker bytes and its identity lines. Its `text` is the block's *whole* text, property lines
//!   included, and it re-emits the identity lines after the new content.
//! - [`crate::document::PageDocument::set_block_text_text`] replaces the same block's *editable* text, the one an
//!   editor shows: property lines are never part of it and never move, so a property line before
//!   the content stays before it and one after it stays after it.
//! - [`crate::document::PageDocument::set_block_marker_text`] rewrites the marker bytes of one block's first line —
//!   bullet to heading, heading to bullet, or one heading level to another — and re-aligns that
//!   block's own continuation and property lines to the new content column.
//! - [`crate::document::PageDocument::remove_block_text`] deletes the block that states a given durable id together
//!   with every block under it, which is what deleting a block means in an outliner.
//! - [`crate::document::PageDocument::move_block_text`] cuts the block that states a given durable id and its whole
//!   subtree out of the page and pastes it next to another block, named by its durable id.
//! - [`crate::document::PageDocument::insert_block_text`] mints one new block with single-line content at a position
//!   named by [`MoveTarget`], and answers the preorder path the new block holds in the result.
//! - [`crate::document::PageDocument::upsert_block_property_text`] and [`crate::document::PageDocument::remove_block_property_text`]
//!   write one `key:: value` property line of the block a locator names: upsert replaces the line
//!   the key already has or inserts one after the block's other properties, remove deletes it.
//!
//! The fifth edit spans two pages: [`crate::document::PageDocument::move_block_text_to_page`] cuts the same range
//! out of one [`crate::document::PageDocument`] and pastes it next to an anchor in another, returning both new
//! source texts.
//!
//! Five structure edits sit beside them, each still one pure page transformation:
//! [`crate::document::PageDocument::split_block_text`] cuts a block in two at a byte offset of its editable text,
//! [`crate::document::PageDocument::merge_block_text`] folds a block into the block above it,
//! [`crate::document::PageDocument::unwrap_block_text`] deletes a block and lifts its children into its place, and
//! [`crate::document::PageDocument::indent_block_text`] / [`crate::document::PageDocument::outdent_block_text`] move a block and its
//! subtree one indentation level deeper or shallower. An editor's Enter, Backspace, Delete, Tab and
//! Shift-Tab mean these five, in that order. Their editable text is
//! [`crate::document::BlockFacts::editable_text`]: the block's own lines without its property lines.
//!
//! All of them copy everything outside the edited lines byte for byte: the BOM, the line endings of
//! untouched lines, trailing blank lines, and a missing final newline all survive.

mod append;
mod error;
mod indent;
mod insert;
mod locator;
mod marker;
mod property;
mod render;
mod structure;
mod subtree;
mod text;
mod verify;

pub use error::EditError;
pub use locator::{BlockLocator, MoveTarget};

#[cfg(test)]
mod tests;

/// Marker written for an appended block. A dash is Logseq's default.
const BLOCK_MARKER: &str = "- ";

/// Property key that carries a block's collapsed state in the outliner UI.
const COLLAPSED_PROPERTY: &str = "collapsed";
