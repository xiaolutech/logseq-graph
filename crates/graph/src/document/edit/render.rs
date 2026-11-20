//! Source line rendering and indentation helpers.

use super::super::facts::{BlockFacts, PageFacts, ID_PROPERTY};
use super::super::line::{LineSpan, SourceLine};
use super::super::parser::{marker_content_start, PageDocument, UTF8_BOM};
use super::subtree::PastedLine;
use super::COLLAPSED_PROPERTY;

/// One piece of the output of [`PageDocument::paste_subtree`], naming where its bytes come from.
pub(super) enum Piece<'a> {
    /// A line of the document being pasted into, by its line index.
    Line(usize, &'a SourceLine),
    /// A line of the moved subtree, in paste order.
    Pasted(&'a PastedLine),
}

impl PageDocument {
    /// The indentation unit this document states by example.
    ///
    /// The first block in preorder that has a child shows the extra leading whitespace a child
    /// carries: `"\t"` for a tab-indented file, `"  "` for a two-space file. Nothing else in the
    /// file states it, since a block's depth is its indentation and each file may spell that
    /// indentation its own way.
    ///
    /// The default of two spaces is reached only when the document has no nested block at all, and
    /// then no line in the document distinguishes one unit from another: any unit produces the same
    /// bytes when there is no indentation to add.
    pub(super) fn indent_unit(&self) -> String {
        for block in self.blocks_in_preorder() {
            let Some(child) = block.children().first() else {
                continue;
            };
            let parent = lead_whitespace(self.lines()[block.own_span().start()].text());
            let child = lead_whitespace(self.lines()[child.own_span().start()].text());
            if let Some(unit) = child.strip_prefix(parent) {
                if !unit.is_empty() {
                    return unit.to_owned();
                }
            }
        }
        "  ".to_owned()
    }

    /// Property lines that carry the identity and UI state of the block that owns `own_span`.
    ///
    /// Only `id::` and `collapsed::` are returned, with their source line numbers and the key that
    /// matched, so the caller can decide per key whether to write the line back.
    pub(super) fn identity_lines(
        facts: &PageFacts,
        own_span: LineSpan,
    ) -> Vec<(&'static str, usize)> {
        let Some(block) = facts
            .blocks()
            .iter()
            .find(|block| block.own_span() == own_span)
        else {
            return Vec::new();
        };
        block
            .properties()
            .iter()
            .filter_map(|property| match property.normalized_key() {
                ID_PROPERTY => Some((ID_PROPERTY, property.line())),
                COLLAPSED_PROPERTY => Some((COLLAPSED_PROPERTY, property.line())),
                _ => None,
            })
            .collect()
    }

    /// The marker prefix of a block's first own line: its indentation, its marker, and the whitespace
    /// after the marker.
    ///
    /// Sliced out of the source instead of built from [`BlockMarker`], which does not remember
    /// whether the file wrote `-`, `*` or `+`, and is why a split's new block carries the marker
    /// character of the block it came from.
    pub(super) fn marker_prefix(&self, own_span: LineSpan) -> String {
        let marker_line = self.lines()[own_span.start()].text();
        let column = marker_content_start(marker_line).unwrap_or(marker_line.len());
        marker_line[..column].to_owned()
    }

    /// `text` as a block's own content lines under `prefix`: the first line sits on the marker line and
    /// the rest are continuation lines aligned to that line's content column.
    ///
    /// Empty `text` renders a marker without trailing whitespace. A leading empty content line
    /// keeps the marker separator, and a trailing empty continuation keeps its indentation, so
    /// projection retains both. Interior empty lines stay empty. The indentation unit is the
    /// document's own.
    pub(super) fn content_lines(&self, prefix: &str, text: &str) -> Vec<String> {
        let mut prefix = prefix.to_owned();
        let mut pieces = text.split('\n');
        let first = pieces.next().unwrap_or("");
        if !text.is_empty() && !prefix.ends_with([' ', '\t']) {
            prefix.push(' ');
        }
        let first_line = if text.is_empty() {
            prefix.trim_end().to_owned()
        } else {
            format!("{prefix}{first}")
        };
        let align = marker_content_start(&first_line).unwrap_or(first_line.len());
        let indent = indent_of(&self.indent_unit(), align);
        let mut rendered = vec![first_line];
        let mut pieces = pieces.peekable();
        while let Some(piece) = pieces.next() {
            rendered.push(if piece.is_empty() {
                if pieces.peek().is_none() {
                    indent.clone()
                } else {
                    String::new()
                }
            } else {
                format!("{indent}{piece}")
            });
        }
        rendered
    }

    /// The own lines of `block` with its editable text replaced by `text`, every property line copied
    /// from the source at the place it held.
    ///
    /// The block keeps its indentation and its marker bytes. The lines of `text` fill the block's
    /// content lines in order; a text with more lines than the block had content lines writes the
    /// extra lines immediately after the last content line, and one with fewer leaves the remaining
    /// property lines where they are. A block whose marker line itself states a property keeps that
    /// line and writes `text` as continuation lines, because the marker line is then not part of the
    /// editable text.
    ///
    /// "After the last content line" is the whole point of the placement: a block whose properties
    /// sit after its content would otherwise end up with a property line between two content lines,
    /// which is a layout [`PageDocument::set_block_text_text`] refuses and no editor shows. Content
    /// lines stay together, and a property block stays on the one side of them the file chose.
    ///
    /// Each line says whether it copies one of the block's own lines. A caller's re-parse check can
    /// then name where the properties landed instead of assuming their offsets survived, which they
    /// do not when the content grows or shrinks around them.
    pub(super) fn render_text_lines(&self, block: &BlockFacts, text: &str) -> Vec<RenderedLine> {
        let own_span = block.own_span();
        let editable = block.editable_line_offsets();
        let marker_line = self.lines()[own_span.start()].text();
        let prefix = self.marker_prefix(own_span);
        let content = if editable.first() == Some(&0) {
            self.content_lines(&prefix, text)
        } else if text.is_empty() {
            // The marker line holds a property, so an empty text writes no line at all.
            Vec::new()
        } else {
            let align = marker_content_start(marker_line).unwrap_or(marker_line.len());
            let indent = indent_of(&self.indent_unit(), align);
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

        let mut rendered = Vec::with_capacity(own_span.len().max(content.len()));
        let mut pending = content.into_iter();
        // Where the last content line landed, so the lines beyond the block's original content line
        // count join their own kind instead of following the block's trailing property lines.
        let mut content_end = None;
        for offset in 0..own_span.len() {
            if editable.contains(&offset) {
                if let Some(line) = pending.next() {
                    rendered.push(RenderedLine {
                        text: line,
                        source: None,
                    });
                    content_end = Some(rendered.len());
                }
            } else {
                rendered.push(RenderedLine {
                    text: self.lines()[own_span.start() + offset].text().to_owned(),
                    source: Some(offset),
                });
            }
        }
        let extra = pending.map(|text| RenderedLine { text, source: None });
        if let Some(at) = content_end {
            rendered.splice(at..at, extra);
        } else {
            // No content line was written at all: every own line is a property, so the content
            // belongs after all of them.
            rendered.extend(extra);
        }
        if let Some(last) = rendered
            .iter()
            .rposition(|line| line.source.is_none())
            .filter(|last| text.ends_with('\n') && last + 1 < rendered.len())
        {
            rendered[last].text.clear();
        }
        rendered
    }

    /// Render an edit's output in order, copying each kept source line byte for byte.
    ///
    /// A kept line keeps the ending the file gave it, and gains one only when it stops being the
    /// file's last line. A rendered line takes the ending it carries. The output's last line keeps
    /// the document's final-newline state, and the BOM is written first when the source had one.
    pub(super) fn render_order(&self, order: &[Piece<'_>]) -> String {
        let mut out = String::new();
        if self.has_bom() {
            out.push(UTF8_BOM);
        }
        for (position, piece) in order.iter().enumerate() {
            let last = position + 1 == order.len();
            match *piece {
                Piece::Line(index, line) => {
                    out.push_str(line.text());
                    let ending = if last {
                        line.ending()
                    } else {
                        line.ending().or_else(|| Some(self.line_ending(index)))
                    };
                    if let Some(ending) = ending {
                        out.push_str(ending.as_str());
                    }
                }
                Piece::Pasted(line) => {
                    out.push_str(&line.text);
                    let ending = if last {
                        // The run now ends the file: it keeps the file's final-newline state.
                        self.ends_with_newline()
                            .then(|| self.line_ending(self.lines().len()))
                    } else {
                        line.ending
                    };
                    if let Some(ending) = ending {
                        out.push_str(ending.as_str());
                    }
                }
            }
        }
        out
    }
}

/// The leading spaces and tabs of `text`, which is what an outliner reads as depth.
pub(super) fn lead_whitespace(text: &str) -> &str {
    &text[..text.len() - text.trim_start_matches([' ', '\t']).len()]
}

/// Width in characters, not columns, of the leading whitespace of `text`.
///
/// Characters rather than bytes, because a tab is one character however many columns a renderer
/// gives it, and rather than columns, because this module never guesses a tab width: the file states
/// its indentation and the edit follows it.
pub(super) fn indent_width(text: &str) -> usize {
    lead_whitespace(text).chars().count()
}

pub(super) fn indent_of(unit: &str, width: usize) -> String {
    let mut out = String::new();
    while out.chars().count() < width {
        out.push_str(unit);
    }
    out.chars().take(width).collect()
}

/// `text` with its leading whitespace shifted by `delta` characters.
///
/// A positive `delta` is filled from the document's indentation unit, so a tab-indented file gains
/// tabs and a space-indented file gains spaces. A negative one removes exactly that many leading
/// characters and copies the rest of the line unchanged: re-indenting the remainder would reformat
/// whitespace the caller never asked to change.
///
/// A blank line keeps its own bytes, because an empty line with indentation on it is whitespace the
/// file did not have. A line with less whitespace than the removal loses only what it has, so the
/// strip can shorten an indentation but can never eat content; a continuation line inside a code
/// fence may be indented far less than its block.
pub(super) fn shift_indent(text: &str, delta: isize, unit: &str) -> String {
    if delta == 0 || text.trim().is_empty() {
        return text.to_owned();
    }
    if delta > 0 {
        let mut out = indent_of(unit, delta as usize);
        out.push_str(text);
        return out;
    }
    let cut = indent_cut(text, (-delta) as usize);
    text[cut..].to_owned()
}

/// `text` re-indented from the content column `from` to the content column `to`.
///
/// The leading whitespace up to `from` characters is the block's alignment, which belongs to the
/// marker's width; it is replaced by `to` characters of `unit`. Whatever indentation a line carries
/// beyond that column stays as bytes, so a line indented deeper than its block — a fenced code
/// block's inner lines, for one — keeps its relative shape. A blank line keeps its own bytes,
/// because indenting an empty line is whitespace the file never had.
///
/// This is [`shift_indent`] stated against the two columns instead of a delta, because that is what
/// a marker change does: the old and new content columns are read off the two markers, not counted
/// from the edit's own arithmetic.
pub(super) fn reindent(text: &str, from: usize, to: usize, unit: &str) -> String {
    if text.trim().is_empty() {
        return text.to_owned();
    }
    let mut out = indent_of(unit, to);
    out.push_str(&text[indent_cut(text, from)..]);
    out
}

/// Byte offset just past `count` leading spaces and tabs of `text`.
///
/// The offset stops at the first character that is not indentation, so a line indented less than
/// `count` loses only what it has: cutting an indentation may shorten a line's whitespace, never eat
/// its content. Shared by the two edits that move lines between content columns — a negative
/// [`shift_indent`] delta and a marker's old column are the same cut.
fn indent_cut(text: &str, count: usize) -> usize {
    text.char_indices()
        .take(count)
        .take_while(|(_, character)| matches!(character, ' ' | '\t'))
        .map(|(offset, character)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

/// One line of [`PageDocument::render_text_lines`]'s answer, naming where its bytes come from.
pub(super) struct RenderedLine {
    /// The line's own bytes.
    pub(super) text: String,
    /// Offset inside the block's own lines this line copies, or `None` when the edit rendered it.
    pub(super) source: Option<usize>,
}
