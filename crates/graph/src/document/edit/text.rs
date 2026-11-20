//! Block content replacement.

use super::super::facts::PageFacts;
use super::super::line::SourceLine;
use super::super::parser::{marker_content_start, trim_indent, PageDocument, UTF8_BOM};
use super::error::EditError;
use super::locator::{block_shapes, facts_of, BlockLocator};
use super::property::property_sides;
use super::render::{indent_of, Piece};
use super::subtree::PastedLine;

impl PageDocument {
    /// Replace the text of the block that states `id:: <block_id>`, and return the new source text.
    ///
    /// The block is found through the facts of every block, not through the position of a node in
    /// the structure tree: matching `id::` values is what ADR-0005 makes durable, and it does not
    /// depend on traversal order. Comparison is ASCII case-insensitive, because the domain
    /// normalizes block ids to lowercase and `id:: ABCD` therefore names the same block as
    /// `abcd`. Two blocks claiming one id is corrupt input, so it is an error rather than a guess.
    ///
    /// The replaced range is the block's own lines: its marker line and its continuation lines,
    /// not its children. The marker line keeps its original bytes up to the column where its
    /// content starts, so `* a`, `+ a` and `### a` stay what they were; the marker kind alone does
    /// not remember which character was used. Indentation and marker therefore cannot change, and
    /// children keep their own indentation relative to an unchanged parent indent.
    ///
    /// `text` may contain `\n`: continuation lines are indented to the block's content column, the
    /// column where the first line's content starts, so the parsed block text is `text` again.
    /// Empty `text` is allowed and renders as a marker line without trailing whitespace: updating a
    /// block to empty is a legal state, while appending an empty block would create nothing.
    /// Leading whitespace in the first line of `text` joins the whitespace after the marker and is
    /// stripped again by the parser, exactly as in [`PageDocument::append_block`]. A trailing blank
    /// line in `text` is rejected when the block ends the document, because the parser counts
    /// trailing blank lines as page trailing space.
    ///
    /// Identity survives: an `id::` or `collapsed::` property line of the block is written back
    /// after the new content, aligned to the same content column, unless `text` itself declares
    /// that key. Losing `id::` would delete the block's identity and break every `((...))` that
    /// points at it, and losing `collapsed::` would reopen a folded block.
    ///
    /// Every byte outside the replaced lines is copied unchanged: the BOM, each line's own ending,
    /// trailing blank lines, and a missing final newline.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] when no block states the id,
    /// [`EditError::AmbiguousBlockId`] when more than one does, [`EditError::MultilineContent`] for
    /// a carriage return in `text`, and [`EditError::ContinuationChangesStructure`] when a
    /// continuation line would be parsed as a block marker, as a property line, or as page trailing
    /// space instead of block text. A continuation line inside a code fence or `#+BEGIN_` region is
    /// text, so it is accepted even when it looks like a marker or a property.
    pub fn update_block_text(
        &self,
        locator: BlockLocator<'_>,
        text: &str,
    ) -> Result<String, EditError> {
        if text.contains('\r') {
            return Err(EditError::MultilineContent);
        }

        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let start = own_span.start();
        let lines = self.lines();

        // The prefix is the original indentation, marker, and the whitespace after the marker. It is
        // sliced off the source instead of being rendered from `BlockMarker`, which does not
        // remember whether the file used `-`, `*` or `+`.
        let marker_line = lines[start].text();
        let content_column = marker_content_start(marker_line).unwrap_or(marker_line.len());
        let mut marker_prefix = marker_line[..content_column].to_owned();

        let (first_content, continuation) = match text.split_once('\n') {
            Some((first, rest)) => (first, Some(rest)),
            None => (text, None),
        };
        // A bullet marker may be the whole line, with no whitespace after it. Content needs that
        // separator, or the line would not be a marker line at all.
        if !text.is_empty() && !marker_prefix.ends_with([' ', '\t']) {
            marker_prefix.push(' ');
        }
        // Only wholly empty text drops the separator: a leading newline must not become a bare
        // dash, whose projection omits the empty marker line.
        let first_line = if text.is_empty() {
            marker_prefix.trim_end().to_owned()
        } else {
            format!("{marker_prefix}{first_content}")
        };

        // Continuation lines align to where this block's content starts, read back from the line
        // that was just rendered so a trimmed marker line gets its own narrower column. The
        // indentation unit comes from the document, so a tab-indented page stays tab-indented
        // instead of turning into a file that mixes both styles.
        let align = marker_content_start(&first_line).unwrap_or(first_line.len());
        let indent = indent_of(&self.indent_unit(), align);
        let mut rendered = vec![first_line];
        if let Some(rest) = continuation {
            let mut pieces = rest.split('\n').peekable();
            while let Some(line) = pieces.next() {
                if line.is_empty() {
                    rendered.push(if pieces.peek().is_none() {
                        indent.clone()
                    } else {
                        String::new()
                    });
                } else {
                    rendered.push(format!("{indent}{line}"));
                }
            }
        }

        // The identity and UI state of the block are not part of what a caller edits, so they are
        // re-emitted from their original bytes unless the new content declares them itself.
        let declared = PageFacts::extract(&PageDocument::parse(&format!("{}\n", rendered[0])));
        let declares = |key: &str| {
            declared
                .blocks()
                .first()
                .is_some_and(|block| block.property(key).is_some())
        };
        let content_end = rendered.len();
        let mut preserved = Vec::new();
        for (key, line) in Self::identity_lines(&facts, own_span) {
            if declares(key) {
                continue;
            }
            preserved.push(start + rendered.len());
            rendered.push(format!("{indent}{}", trim_indent(lines[line].text())));
        }
        if text.ends_with('\n') && content_end < rendered.len() {
            rendered[content_end - 1].clear();
        }

        // A blank line at the end of the text has nowhere to go when nothing but blank lines
        // follows the block: the parser counts trailing blank lines as page trailing space, so the
        // block would silently lose that line and the page would gain it.
        if rendered.last().is_some_and(|line| line.trim().is_empty())
            && lines[own_span.end()..].iter().all(SourceLine::is_blank)
        {
            return Err(EditError::ContinuationChangesStructure);
        }

        let marker_ending = lines[start]
            .ending()
            .unwrap_or_else(|| self.line_ending(start));
        // The last line of the block keeps the ending the file had there, so a document without a
        // final newline still has none when the edit reaches its last line.
        let last_ending = if own_span.end() == lines.len() {
            lines[own_span.end() - 1].ending()
        } else {
            Some(marker_ending)
        };

        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        for line in &lines[..start] {
            line.write_to(&mut out);
        }
        let last = rendered.len() - 1;
        for (index, line) in rendered.iter().enumerate() {
            out.push_str(line);
            let ending = if index == last {
                last_ending
            } else {
                Some(marker_ending)
            };
            if let Some(ending) = ending {
                out.push_str(ending.as_str());
            }
        }
        for line in &lines[own_span.end()..] {
            line.write_to(&mut out);
        }

        // Writing bytes that look edited is not the same as writing bytes that are the edit. The
        // parser is the only authority on what a line becomes, so the result is parsed and the two
        // structural promises are checked: the same blocks in the same shape, and no property line
        // that this edit invented.
        let after = PageDocument::parse(&out);
        if block_shapes(&after) != block_shapes(self) {
            return Err(EditError::ContinuationChangesStructure);
        }
        let after_facts = PageFacts::extract(&after);
        let Some(edited) = after_facts
            .blocks()
            .iter()
            .find(|block| block.own_span().start() == start)
        else {
            return Err(EditError::ContinuationChangesStructure);
        };
        for property in edited.properties() {
            if property.line() != start && !preserved.contains(&property.line()) {
                return Err(EditError::ContinuationChangesStructure);
            }
        }

        Ok(out)
    }

    /// Replace the editable text of the block a locator names, and return the new source text.
    ///
    /// `text` is [`crate::document::BlockFacts::editable_text`]'s shape: the block's own content without its
    /// property lines, continuation lines joined with `\n`. That is what an editor shows and the
    /// only text it sends back, so this is the edit an editor needs: the marker bytes, the
    /// indentation, every property line, the children, and the block's identity stay what the file
    /// states.
    ///
    /// Property lines are not moved. A block whose properties all sit before its content keeps them
    /// there, a block whose properties all sit after its content keeps them there, and a block that
    /// states properties on both sides keeps each side on its own side. A property line between two
    /// content lines belongs to neither side, so the edit is refused
    /// ([`EditError::InterleavedProperties`]) instead of guessing where that line should go.
    ///
    /// Content is rendered the way [`Self::update_block_text`] renders it: the first line sits on
    /// the marker line after the bytes the marker already had, continuation lines align to the
    /// block's content column with the document's own indentation unit (`Self::indent_unit`), and
    /// empty text leaves a marker line with no trailing whitespace. A block whose marker line
    /// itself states a property has no content line to reuse, so its content is written as
    /// continuation lines after the properties.
    ///
    /// Passing the block's own [`crate::document::BlockFacts::editable_text`] returns the document's own bytes: an
    /// edit that changes nothing has nothing to write. That is also what makes the edit idempotent
    /// on every block, which is the round trip an editor relies on: a bare marker line's trailing
    /// whitespace is not part of the editable text, so re-rendering it could only change a byte the
    /// caller never mentioned, and a layout this edit refuses
    /// ([`EditError::InterleavedProperties`]) still has to come back unchanged when it is handed the
    /// text it already holds.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::BlockNotFound`] / [`EditError::AmbiguousBlockId`] /
    /// [`EditError::PathOutOfRange`] when the locator names no block of this page,
    /// [`EditError::MultilineContent`] for a carriage return in `text`,
    /// [`EditError::InterleavedProperties`] when a property line sits between two content lines, and
    /// [`EditError::ContinuationChangesStructure`] when the rendered bytes would not read back as
    /// the same block shape with exactly this editable text and these property lines — a
    /// continuation line that becomes a block marker or a property line, a marker line that stops
    /// being one, or a trailing blank line that would become page trailing space.
    pub fn set_block_text_text(
        &self,
        locator: BlockLocator<'_>,
        text: &str,
    ) -> Result<String, EditError> {
        if text.contains('\r') {
            return Err(EditError::MultilineContent);
        }

        let facts = PageFacts::extract(self);
        let own_span = Self::block_span(&facts, self, &locator)?;
        let block = facts_of(&facts, own_span);

        if block.editable_text() == text {
            return Ok(self.to_source());
        }

        let (leading, trailing) = property_sides(block);
        if leading.len() + trailing.len() != block.properties().len() {
            return Err(EditError::InterleavedProperties);
        }

        let content = block.editable_line_offsets();
        let lines = self.lines();
        let marker_line = lines[own_span.start()].text();
        let content_column = marker_content_start(marker_line).unwrap_or(marker_line.len());

        // The content goes where the block's content already was: on the marker line when the
        // marker line is content, and on continuation lines when the marker line states a property.
        let mut rendered: Vec<String> = if content.first() == Some(&0) {
            self.content_lines(&self.marker_prefix(own_span), text)
        } else if text.is_empty() {
            Vec::new()
        } else {
            let indent = indent_of(&self.indent_unit(), content_column);
            text.split('\n')
                .map(|piece| {
                    if piece.is_empty() {
                        String::new()
                    } else {
                        format!("{indent}{piece}")
                    }
                })
                .collect()
        };
        if let Some(last) = rendered
            .last_mut()
            .filter(|_| text.ends_with('\n') && !trailing.is_empty())
        {
            last.clear();
        }

        // A blank line at the end of the content has nowhere to go when nothing but blank lines
        // follows the block: the parser counts trailing blank lines as page trailing space, so the
        // block would lose that line and the page would gain it. A property line after the content
        // gives that blank line a place — it is the block's last own line then, and the blank one
        // sits before it — which is why only content can end a block's own lines with a blank line.
        if trailing.is_empty()
            && rendered.last().is_some_and(|line| line.trim().is_empty())
            && lines[own_span.end()..].iter().all(SourceLine::is_blank)
        {
            return Err(EditError::ContinuationChangesStructure);
        }

        // Every rendered content line takes the marker line's ending, which is the convention
        // `update_block_text` writes its new text with: content this edit invented has no ending of
        // its own, and the kept property lines below keep theirs.
        let marker_ending = lines[own_span.start()]
            .ending()
            .unwrap_or_else(|| self.line_ending(own_span.start()));
        let pasted: Vec<PastedLine> = rendered
            .iter()
            .map(|line| PastedLine {
                text: line.clone(),
                ending: Some(marker_ending),
            })
            .collect();

        // Property lines keep their own bytes and their own ending, in the order the file wrote
        // them: the ones before the content stay before it, the ones after it stay after it.
        let mut order: Vec<Piece<'_>> = Vec::new();
        for (index, line) in lines[..own_span.start()].iter().enumerate() {
            order.push(Piece::Line(index, line));
        }
        for offset in &leading {
            let index = own_span.start() + offset;
            order.push(Piece::Line(index, &lines[index]));
        }
        order.extend(pasted.iter().map(Piece::Pasted));
        for offset in &trailing {
            let index = own_span.start() + offset;
            order.push(Piece::Line(index, &lines[index]));
        }
        for (offset, line) in lines[own_span.end()..].iter().enumerate() {
            order.push(Piece::Line(own_span.end() + offset, line));
        }
        let source = self.render_order(&order);

        // Writing bytes that look edited is not the same as writing bytes that are the edit: the
        // parser is the authority on what a line becomes, so the result is parsed back and each
        // promise is checked against it.
        let after = PageDocument::parse(&source);
        if block_shapes(&after) != block_shapes(self) {
            return Err(EditError::ContinuationChangesStructure);
        }
        let after_facts = PageFacts::extract(&after);
        let Some(edited) = after_facts
            .blocks()
            .iter()
            .find(|block| block.own_span().start() == own_span.start())
        else {
            return Err(EditError::ContinuationChangesStructure);
        };
        let kept: Vec<&str> = block
            .properties()
            .iter()
            .map(|property| lines[property.line()].text())
            .collect();
        let written: Vec<&str> = edited
            .properties()
            .iter()
            .map(|property| after.lines()[property.line()].text())
            .collect();
        // Which side of the content each property line is on is a promise too: a property that
        // moved from before the content to after it was not preserved, it was reordered. A block
        // whose content becomes empty has no sides left, so only the bytes and their order are
        // checked there.
        let (after_leading, after_trailing) = property_sides(edited);
        let sides_hold = if edited.editable_line_offsets().is_empty() {
            true
        } else {
            after_leading.len() == leading.len() && after_trailing.len() == trailing.len()
        };
        if edited.editable_text() != text || kept != written || !sides_hold {
            return Err(EditError::ContinuationChangesStructure);
        }

        Ok(source)
    }
}
