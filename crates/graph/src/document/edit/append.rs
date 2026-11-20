//! Appending a final top-level block.

use super::super::line::{LineEnding, SourceLine};
use super::super::parser::{PageDocument, UTF8_BOM};
use super::error::EditError;
use super::BLOCK_MARKER;

impl PageDocument {
    /// Append one top-level block whose content is `text`, and return the new source text.
    ///
    /// The new line is `"- " + text`, with `text` copied verbatim: no trimming and no escaping, so
    /// content that already looks like a marker is written as `- - x` or `- # x` and renders as the
    /// text it is. Leading whitespace in `text` therefore joins the whitespace after the marker,
    /// which the parser strips again: the bytes are as given, while the parsed block text starts at
    /// the first non-blank character.
    ///
    /// The insertion point is the start of the trailing blank lines, that is, the end of the last
    /// non-blank line. Appending there keeps the document's trailing blank lines last instead of
    /// pushing the new block behind them.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::EmptyContent`] for empty or whitespace-only content,
    /// [`EditError::MultilineContent`] for content containing a line break, and
    /// [`EditError::EndsInsideOpaqueRegion`] when the document ends inside an unclosed opaque
    /// region.
    pub fn append_block(&self, text: &str) -> Result<String, EditError> {
        if text.trim().is_empty() {
            return Err(EditError::EmptyContent);
        }
        if text.contains('\n') || text.contains('\r') {
            return Err(EditError::MultilineContent);
        }

        let source = self.insert_last_line(&format!("{BLOCK_MARKER}{text}"));

        // The edit is correct only if the appended line really becomes a top-level block. Asking
        // the parser, instead of re-deriving its fence tracking here, keeps "what is a block" in
        // one function; the price is one extra parse per edit.
        if PageDocument::parse(&source).blocks().len() != self.blocks().len() + 1 {
            return Err(EditError::EndsInsideOpaqueRegion);
        }

        Ok(source)
    }

    /// Insert `line`, which has no line ending of its own, at the end of the last non-blank line and
    /// render the whole document.
    fn insert_last_line(&self, line: &str) -> String {
        let insert = self.trailing().start();
        let lines = self.lines();
        let mut out = String::new();

        if self.has_bom() {
            out.push(UTF8_BOM);
        }

        for (index, source_line) in lines[..insert].iter().enumerate() {
            source_line.write_to(&mut out);
            // A last line without a newline is the only insertion point that needs a newline of
            // its own: the appended block must start on its own line, while the file must still
            // end without a newline.
            if index + 1 == insert && !source_line.has_ending() {
                out.push_str(self.line_ending(insert).as_str());
            }
        }

        out.push_str(line);
        // Trailing blank lines follow the insertion point, so the appended line needs a newline or
        // it would fuse with them. Otherwise the appended line is the last line, and the document's
        // final-newline state decides.
        if insert < lines.len() || self.ends_with_newline() {
            out.push_str(self.line_ending(insert).as_str());
        }

        for source_line in &lines[insert..] {
            source_line.write_to(&mut out);
        }

        out
    }

    /// Line ending for the appended line: the one used by the line before the insertion point.
    ///
    /// A file may mix endings, so the local convention beats both the first line and a hard-coded
    /// LF. When the line before the insertion point is a last line without a newline, the search
    /// walks back to the nearest line that has one; an empty document falls back to LF.
    pub(super) fn line_ending(&self, insert: usize) -> LineEnding {
        self.lines()[..insert]
            .iter()
            .rev()
            .find_map(SourceLine::ending)
            .unwrap_or(LineEnding::Lf)
    }
}
