//! Line-oriented parser and source-preserving page AST.
//!
//! Parsing is deliberately shallow: it recognizes outliner block markers and keeps everything else
//! as source lines. Closed code fences and `#+BEGIN_*` regions are opaque, so their contents cannot become
//! blocks. Content facts are extracted by [`super::facts`], and block identity and edits are later
//! layers.
//!
//! Both parsing and destroying a document are iterative, so nesting depth is bounded by memory
//! rather than by the thread stack.

use super::line::{LineEnding, LineSpan, SourceLine};

use crate::domain::BlockMarker;

/// UTF-8 byte order mark. It is stored as document metadata, never as line text.
pub(crate) const UTF8_BOM: char = '\u{feff}';

/// A block and its descendant blocks.
///
/// `own_span` covers the marker line and its continuation lines but not child blocks.
/// `subtree_span` covers the complete contiguous subtree in the source.
///
/// `Clone`, `PartialEq`, and `Eq` are manual because derived implementations recurse once per
/// nesting level and abort the process on deep documents.
#[derive(Debug)]
pub struct Block {
    marker: BlockMarker,
    indent: usize,
    own_span: LineSpan,
    subtree_span: LineSpan,
    children: Vec<Block>,
}

impl Clone for Block {
    /// Clone the subtree iteratively.
    ///
    /// A postorder frame stack tracks each partially cloned block: its source, the next child to
    /// visit, and the children cloned so far. A leaf is assembled immediately; a frame finishes by
    /// assembling the block from its source's scalar fields and the collected children, then
    /// attaches it to its parent frame or returns it as the root.
    fn clone(&self) -> Self {
        struct Frame<'a> {
            source: &'a Block,
            next_child: usize,
            children: Vec<Block>,
        }

        let mut stack = vec![Frame {
            source: self,
            next_child: 0,
            children: Vec::new(),
        }];
        while let Some(frame) = stack.last_mut() {
            if frame.next_child < frame.source.children.len() {
                let child = &frame.source.children[frame.next_child];
                frame.next_child += 1;
                stack.push(Frame {
                    source: child,
                    next_child: 0,
                    children: Vec::new(),
                });
                continue;
            }

            #[expect(
                clippy::expect_used,
                reason = "the `while let Some(frame) = stack.last_mut()` guard is what proves the \
                          stack non-empty, and the child branch `continue`s instead of popping"
            )]
            let frame = stack.pop().expect("stack was checked");
            let block = Block {
                marker: frame.source.marker,
                indent: frame.source.indent,
                own_span: frame.source.own_span,
                subtree_span: frame.source.subtree_span,
                children: frame.children,
            };
            match stack.last_mut() {
                Some(parent) => parent.children.push(block),
                None => return block,
            }
        }
        unreachable!("the initial frame always finishes with a root block")
    }
}

impl PartialEq for Block {
    /// Compare two subtrees iteratively.
    ///
    /// A stack of node pairs visits every descendant pair. Each pair compares the scalar fields and
    /// child count before enqueuing matching children, so structure and content are both checked
    /// without recursion.
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((left, right)) = pending.pop() {
            if left.marker != right.marker
                || left.indent != right.indent
                || left.own_span != right.own_span
                || left.subtree_span != right.subtree_span
                || left.children.len() != right.children.len()
            {
                return false;
            }
            for (left_child, right_child) in left.children.iter().zip(&right.children) {
                pending.push((left_child, right_child));
            }
        }
        true
    }
}

impl Eq for Block {}

impl Block {
    fn new(marker: BlockMarker, indent: usize, line: usize) -> Self {
        let span = LineSpan::new(line, line + 1);
        Self {
            marker,
            indent,
            own_span: span,
            subtree_span: span,
            children: Vec::new(),
        }
    }

    /// Marker syntax that opened this block.
    pub const fn marker(&self) -> BlockMarker {
        self.marker
    }

    /// Leading whitespace width in bytes before the marker.
    pub const fn indent(&self) -> usize {
        self.indent
    }

    /// Marker line plus continuation lines, excluding child blocks.
    pub const fn own_span(&self) -> LineSpan {
        self.own_span
    }

    /// This block and every descendant, as one contiguous source span.
    pub const fn subtree_span(&self) -> LineSpan {
        self.subtree_span
    }

    /// Continuation lines that follow the marker line and belong to this block.
    pub const fn continuation_span(&self) -> LineSpan {
        LineSpan::new(self.own_span.start() + 1, self.own_span.end())
    }

    /// Direct child blocks in source order.
    pub fn children(&self) -> &[Block] {
        &self.children
    }
}

impl Drop for Block {
    /// Destroy the subtree iteratively.
    ///
    /// The derived drop glue recurses once per nesting level, which aborts the process on deep
    /// documents even though parsing is iterative. Emptying each block before it is dropped keeps
    /// every dropped block childless, so dropping a block never recurses into another block.
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children);
        while let Some(mut block) = pending.pop() {
            pending.append(&mut block.children);
            // `block` is childless now, so its own drop stops at the fields it still owns.
        }
    }
}

/// A parsed Markdown page that retains every source line and its original line ending.
///
/// `Clone` and `PartialEq` are manual: they delegate to the iterative [`Block`] implementations so
/// deep documents cannot recurse.
#[derive(Debug)]
pub struct PageDocument {
    has_bom: bool,
    lines: Vec<SourceLine>,
    preamble: LineSpan,
    trailing: LineSpan,
    blocks: Vec<Block>,
}

impl Clone for PageDocument {
    fn clone(&self) -> Self {
        Self {
            has_bom: self.has_bom,
            lines: self.lines.clone(),
            preamble: self.preamble,
            trailing: self.trailing,
            blocks: self.blocks.clone(),
        }
    }
}

impl PartialEq for PageDocument {
    fn eq(&self, other: &Self) -> bool {
        self.has_bom == other.has_bom
            && self.lines == other.lines
            && self.preamble == other.preamble
            && self.trailing == other.trailing
            && self.blocks == other.blocks
    }
}

impl Eq for PageDocument {}

impl PageDocument {
    /// Parse one Markdown page.
    ///
    /// This is a pure operation: it reads no files and keeps no references into `source`.
    pub fn parse(source: &str) -> Self {
        let (body, has_bom) = match source.strip_prefix(UTF8_BOM) {
            Some(rest) => (rest, true),
            None => (source, false),
        };
        let lines = split_lines(body);

        // Blank lines at the end carry no outliner content. Keeping them in their own span makes
        // the final-newline state explicit while still rendering them exactly.
        let content_end = lines
            .iter()
            .rposition(|line| !line.is_blank())
            .map_or(0, |index| index + 1);
        let openings = fence_openings(&lines[..content_end]);
        let preamble_end = find_preamble_end(&lines[..content_end], &openings);

        let mut blocks = Vec::new();
        let mut stack: Vec<OpenBlock> = Vec::new();
        let mut fence: Option<Fence> = None;

        for (index, line) in lines[..content_end].iter().enumerate().skip(preamble_end) {
            let trimmed = trim_indent(line.text());

            if let Some(active) = fence {
                record_continuation(&mut stack, index);
                if active.closes(trimmed) {
                    fence = None;
                }
                continue;
            }

            if let Some(found) = parse_marker(line.text()) {
                while stack
                    .last()
                    .is_some_and(|open| open.block.indent >= found.indent)
                {
                    #[expect(
                        clippy::expect_used,
                        reason = "the `while` condition tests `stack.last()`, so the vector is \
                                  non-empty when the body pops; the body cannot empty it either, \
                                  because `attach` only patches the frames it is given"
                    )]
                    let open = stack.pop().expect("stack was checked");
                    attach(open.block, &mut stack, &mut blocks);
                }

                fence = openings[index];
                stack.push(OpenBlock {
                    block: Block::new(found.marker, found.indent, index),
                });
                extend_subtrees(&mut stack, index + 1);
                continue;
            }

            if stack.is_empty() {
                continue;
            }
            if let Some(top) = stack.last_mut() {
                top.block.own_span = top.block.own_span.with_end(index + 1);
            }
            extend_subtrees(&mut stack, index + 1);
            if fence.is_none() {
                fence = openings[index];
            }
        }

        while let Some(open) = stack.pop() {
            attach(open.block, &mut stack, &mut blocks);
        }

        Self {
            has_bom,
            preamble: LineSpan::new(0, preamble_end),
            trailing: LineSpan::new(content_end, lines.len()),
            lines,
            blocks,
        }
    }

    /// Whether the source started with a UTF-8 byte order mark.
    pub const fn has_bom(&self) -> bool {
        self.has_bom
    }

    /// Every physical source line in order.
    pub fn lines(&self) -> &[SourceLine] {
        &self.lines
    }

    /// Lines before the first outliner block, or before trailing blank lines when there is no block.
    pub const fn preamble(&self) -> LineSpan {
        self.preamble
    }

    /// Trailing blank lines after the last outliner block.
    pub const fn trailing(&self) -> LineSpan {
        self.trailing
    }

    /// Top-level blocks in source order.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Return the raw source lines covered by `span`.
    ///
    /// # Panics
    ///
    /// Panics when the span is outside this document's line range.
    pub fn raw_lines(&self, span: LineSpan) -> &[SourceLine] {
        &self.lines[span.start()..span.end()]
    }

    /// Whether the last source line has a newline sequence.
    pub fn ends_with_newline(&self) -> bool {
        self.lines.last().is_some_and(SourceLine::has_ending)
    }

    /// All blocks in depth-first source order.
    ///
    /// Traversal is iterative: roots are pushed in reverse so the stack yields parents before
    /// children and siblings in source order.
    pub fn blocks_in_preorder(&self) -> Vec<&Block> {
        let mut out = Vec::new();
        let mut pending: Vec<&Block> = self.blocks.iter().rev().collect();
        while let Some(block) = pending.pop() {
            out.push(block);
            pending.extend(block.children.iter().rev());
        }
        out
    }

    /// Render the document back to source bytes.
    ///
    /// This is the inverse of [`PageDocument::parse`], including the BOM, mixed line endings, and
    /// a missing final newline.
    pub fn to_source(&self) -> String {
        let capacity = self
            .lines
            .iter()
            .map(|line| line.text().len() + line.ending().map_or(0, |ending| ending.as_str().len()))
            .sum::<usize>()
            + usize::from(self.has_bom);
        let mut out = String::with_capacity(capacity);
        if self.has_bom {
            out.push(UTF8_BOM);
        }
        for line in &self.lines {
            line.write_to(&mut out);
        }
        out
    }
}

/// Parse one Markdown page into a lossless [`PageDocument`].
pub fn parse(source: &str) -> PageDocument {
    PageDocument::parse(source)
}

#[derive(Debug)]
struct OpenBlock {
    block: Block,
}

struct MarkerMatch<'a> {
    indent: usize,
    /// Offset of the content within the marker, past the marker and the whitespace after it.
    content_start: usize,
    marker: BlockMarker,
    /// Content after a bullet marker, or empty for headings. Used only to detect fences.
    content: &'a str,
}

/// Byte offset in `raw` where a block's own content begins.
///
/// The offset includes the leading indentation, the marker, and the whitespace after the marker, so
/// it is also the column that continuation lines align to. Returns `None` when `raw` is not a block
/// marker line.
pub(crate) fn marker_content_start(raw: &str) -> Option<usize> {
    parse_marker(raw).map(|found| found.indent + found.content_start)
}

fn split_lines(body: &str) -> Vec<SourceLine> {
    let mut lines = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let (text_end, ending) = if index > start && bytes[index - 1] == b'\r' {
            (index - 1, LineEnding::Crlf)
        } else {
            (index, LineEnding::Lf)
        };
        lines.push(SourceLine::new(
            body[start..text_end].to_owned(),
            Some(ending),
        ));
        start = index + 1;
    }

    if start < body.len() {
        lines.push(SourceLine::new(body[start..].to_owned(), None));
    }

    lines
}

pub(crate) fn trim_indent(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

fn fence_openings(lines: &[SourceLine]) -> Vec<Option<Fence>> {
    let mut closers = [0, 0];
    let mut openings = vec![None; lines.len()];
    for (index, line) in lines.iter().enumerate().rev() {
        let trimmed = trim_indent(line.text());
        let content = parse_marker(line.text()).map_or(trimmed, |found| found.content);
        openings[index] = Fence::open(content).filter(|fence| {
            fence.kind == FenceKind::Block
                || closers[usize::from(fence.marker == b'~')] >= fence.len
        });
        if let Some(fence) = Fence::open(trimmed) {
            if fence.kind == FenceKind::Code && fence.closes(trimmed) {
                let longest = &mut closers[usize::from(fence.marker == b'~')];
                *longest = (*longest).max(fence.len);
            }
        }
    }
    openings
}

fn find_preamble_end(lines: &[SourceLine], openings: &[Option<Fence>]) -> usize {
    let mut fence: Option<Fence> = None;

    for (index, line) in lines.iter().enumerate() {
        let text = trim_indent(line.text());
        if let Some(active) = fence {
            if active.closes(text) {
                fence = None;
            }
            continue;
        }
        if let Some(opening) = openings[index].filter(|_| parse_marker(line.text()).is_none()) {
            fence = Some(opening);
            continue;
        }
        if parse_marker(line.text()).is_some() {
            return index;
        }
    }

    lines.len()
}

fn parse_marker(raw: &str) -> Option<MarkerMatch<'_>> {
    let indent = raw.len() - trim_indent(raw).len();
    let rest = &raw[indent..];
    let bytes = rest.as_bytes();
    let first = *bytes.first()?;

    if first == b'-' && (rest.len() == 1 || matches!(bytes[1], b' ' | b'\t')) {
        // Only `-` opens a block. `*` and `+` are valid CommonMark bullets and are plain text to
        // Logseq: a `* State "DONE" ...` logbook line, a `curl -v` line such as `* Connected to`,
        // or a `+ a + b` code fragment is continuation text, not an outliner entry. Measured
        // against the official graph parser, not inferred from the specification.
        let content_start = if rest.len() == 1 {
            1
        } else {
            let after = &rest[1..];
            1 + after.len() - trim_indent(after).len()
        };
        return Some(MarkerMatch {
            indent,
            content_start,
            marker: BlockMarker::Bullet,
            content: &rest[content_start..],
        });
    }

    if first == b'#' {
        let level = bytes
            .iter()
            .take(6)
            .take_while(|byte| **byte == b'#')
            .count();
        if level > 0 && level < bytes.len() && matches!(bytes[level], b' ' | b'\t') {
            let after = &rest[level..];
            return Some(MarkerMatch {
                indent,
                content_start: level + after.len() - trim_indent(after).len(),
                marker: BlockMarker::Heading { level: level as u8 },
                content: "",
            });
        }
    }

    None
}

fn record_continuation(stack: &mut [OpenBlock], index: usize) {
    if let Some(top) = stack.last_mut() {
        top.block.own_span = top.block.own_span.with_end(index + 1);
    }
    extend_subtrees(stack, index + 1);
}

fn extend_subtrees(stack: &mut [OpenBlock], end: usize) {
    for open in stack {
        open.block.subtree_span = open.block.subtree_span.with_end(end);
    }
}

fn attach(block: Block, stack: &mut [OpenBlock], top_level: &mut Vec<Block>) {
    match stack.last_mut() {
        Some(parent) => parent.block.children.push(block),
        None => top_level.push(block),
    }
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FenceKind {
    Code,
    Block,
}

/// An opaque region whose lines are continuations, not outliner structure.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fence {
    kind: FenceKind,
    marker: u8,
    len: usize,
}

impl Fence {
    /// Start an opaque region when `text` (already stripped of indentation) opens one.
    pub(crate) fn open(text: &str) -> Option<Self> {
        if starts_with_ignore_ascii_case(text, "#+BEGIN_") {
            return Some(Self {
                kind: FenceKind::Block,
                marker: 0,
                len: 0,
            });
        }

        let marker = *text.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let len = text
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == marker)
            .count();
        if len < 3 {
            return None;
        }
        // CommonMark 4.5: the info string of a backtick fence may not contain a backtick, so a line
        // whose remainder holds one opens no fence. `- ```:custom-css-url "..."``` ` is such a
        // line: it is inline code in the block's text, and the lines after it stay blocks. A tilde
        // fence is exempt, because its info string may hold backticks.
        if marker == b'`' && text.as_bytes()[len..].contains(&b'`') {
            return None;
        }
        Some(Self {
            kind: FenceKind::Code,
            marker,
            len,
        })
    }

    /// Whether `text` (already stripped of indentation) closes this region.
    pub(crate) fn closes(self, text: &str) -> bool {
        match self.kind {
            FenceKind::Block => starts_with_ignore_ascii_case(text, "#+END_"),
            FenceKind::Code => {
                let bytes = text.as_bytes();
                if bytes.first() != Some(&self.marker) {
                    return false;
                }
                let len = bytes
                    .iter()
                    .take_while(|byte| **byte == self.marker)
                    .count();
                len >= self.len && bytes[len..].iter().all(|byte| matches!(byte, b' ' | b'\t'))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_roundtrip(source: &str) {
        let document = parse(source);
        assert_eq!(document.to_source(), source, "round-trip mismatch");
        assert_spans_partition(&document);
    }

    fn assert_spans_partition(document: &PageDocument) {
        let mut next = document.preamble().start();
        assert_eq!(next, 0);
        next = document.preamble().end();

        for block in document.blocks_in_preorder() {
            assert_eq!(block.own_span().start(), next, "gap before block");
            assert!(block.own_span().end() <= block.subtree_span().end());
            next = block.own_span().end();
        }

        assert_eq!(
            next,
            document.trailing().start(),
            "gap before trailing lines"
        );
        assert_eq!(document.trailing().end(), document.lines().len());
    }

    #[test]
    fn roundtrip_representative_sources() {
        for source in [
            "",
            "\n",
            "\n\n",
            "- A",
            "- A\n",
            "- A\n\n",
            "type:: [[x]]\ntags:: a, b\n\n- A\n\t- B\n",
            "- A\r\n\t- B\r\n",
            "\u{feff}- A\r\n- B\n\t- C\r\n",
            "- 代码\n\t- ```bash\n\t  # 注释\n\t  echo hi\n\t  ```\n\t- 子块\n",
            "- note\n  #+BEGIN_QUERY\n  {:title \"x\"}\n  #+END_QUERY",
        ] {
            assert_roundtrip(source);
        }
    }

    #[test]
    fn original_encoding_and_raw_lines_survive() {
        let source = "\u{feff}- a\r\n  continuation\n- b\r\n";
        let document = parse(source);

        assert!(document.has_bom());
        assert_eq!(document.lines()[0].text(), "- a");
        assert_eq!(document.lines()[0].ending(), Some(LineEnding::Crlf));
        assert_eq!(document.lines()[1].text(), "  continuation");
        assert_eq!(document.lines()[1].ending(), Some(LineEnding::Lf));
        assert_eq!(document.lines()[2].ending(), Some(LineEnding::Crlf));
        assert!(document.ends_with_newline());
        assert_eq!(document.to_source(), source);
    }

    #[test]
    fn trailing_newline_state_and_trailing_span() {
        let document = parse("- a\n\n\n");
        assert!(document.ends_with_newline());
        assert_eq!(document.trailing(), LineSpan::new(1, 3));
        assert_eq!(document.raw_lines(document.trailing()).len(), 2);

        let without_newline = parse("- a");
        assert!(!without_newline.ends_with_newline());
        assert!(without_newline.trailing().is_empty());
    }

    #[test]
    fn preamble_and_block_spans() {
        let document = parse("\n\ntype:: note\n\n- first\n  continuation\n- second\n\t- nested\n");

        assert_eq!(document.preamble(), LineSpan::new(0, 4));
        assert_eq!(document.blocks().len(), 2);

        let first = &document.blocks()[0];
        assert_eq!(first.own_span(), LineSpan::new(4, 6));
        assert_eq!(first.continuation_span(), LineSpan::new(5, 6));
        assert_eq!(first.subtree_span(), LineSpan::new(4, 6));

        let second = &document.blocks()[1];
        assert_eq!(second.own_span(), LineSpan::new(6, 7));
        assert_eq!(second.subtree_span(), LineSpan::new(6, 8));
        assert_eq!(second.children().len(), 1);
        assert_eq!(second.children()[0].own_span(), LineSpan::new(7, 8));
    }

    #[test]
    fn block_spans_are_contiguous_in_preorder() {
        let document = parse("- a\n\t- b\n\t\t- c\n\t- d\n- e\n");
        let blocks = document.blocks_in_preorder();

        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0].subtree_span(), LineSpan::new(0, 4));
        assert_eq!(blocks[1].subtree_span(), LineSpan::new(1, 3));
        assert_eq!(blocks[2].own_span(), LineSpan::new(2, 3));
        assert_eq!(blocks[3].own_span(), LineSpan::new(3, 4));
        assert_eq!(blocks[4].own_span(), LineSpan::new(4, 5));

        for pair in blocks.windows(2) {
            assert_eq!(pair[0].own_span().start(), pair[0].subtree_span().start());
            assert!(pair[0].own_span().end() <= pair[0].subtree_span().end());
        }
    }

    #[test]
    fn fenced_code_contents_are_continuations() {
        let source =
            "- intro\n  ```\n  - not a block\n  #+BEGIN_QUERY\n  - also not\n  #+END_QUERY\n  ```\n  - real child\n";
        let document = parse(source);
        assert_eq!(document.blocks().len(), 1);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 7));
        assert_eq!(document.blocks()[0].children().len(), 1);
        assert_eq!(
            document.blocks()[0].children()[0].own_span(),
            LineSpan::new(7, 8)
        );
        assert_eq!(document.to_source(), source);
    }

    #[test]
    fn marker_line_can_open_a_code_fence() {
        let document = parse("- ```bash\n  - not a block\n  ```\n- after\n");
        assert_eq!(document.blocks().len(), 2);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 3));
        assert!(document.blocks()[0].children().is_empty());
        assert_eq!(document.blocks()[1].own_span(), LineSpan::new(3, 4));
    }

    #[test]
    fn code_fence_closer_ignores_info_text() {
        let document = parse(
            "- ```
  ```not a closer
  - still code
  ```
- after
",
        );
        assert_eq!(document.blocks().len(), 2);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 4));
        assert_eq!(document.blocks()[1].own_span(), LineSpan::new(4, 5));
    }

    #[test]
    fn a_backtick_info_string_opens_no_fence() {
        // Measured against the official graph parser: a fence whose info string holds a backtick is
        // inline code, so the following line is still a block. Before this rule the fence swallowed
        // every line to the end of the page.
        let source = "- ```:custom-css-url \"@import url(x);\"```
- next block
";
        let document = parse(source);
        assert_eq!(document.blocks().len(), 2);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 1));
        assert_eq!(document.blocks()[1].own_span(), LineSpan::new(1, 2));
        assert_eq!(document.to_source(), source);

        // A tilde fence may hold backticks in its info string, so it still opens one.
        let tilde = parse("- ~~~`:x`\n  - inside\n  ~~~\n- after\n");
        assert_eq!(tilde.blocks().len(), 2);
        assert_eq!(tilde.blocks()[0].own_span(), LineSpan::new(0, 3));
        // An info string with no backtick still opens a fence.
        let plain = parse("- ```bash\n  - inside\n  ```\n- after\n");
        assert_eq!(plain.blocks().len(), 2);
        assert_eq!(plain.blocks()[0].own_span(), LineSpan::new(0, 3));
    }

    #[test]
    fn begin_region_contents_are_continuations() {
        let document = parse("- note\n  #+BEGIN_SRC js\n  - not a block\n  #+END_SRC\n  - child\n");
        assert_eq!(document.blocks().len(), 1);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 4));
        assert_eq!(document.blocks()[0].children().len(), 1);
        assert_eq!(
            document.blocks()[0].children()[0].own_span(),
            LineSpan::new(4, 5)
        );
    }

    #[test]
    fn preamble_fence_hides_outliner_lines() {
        let document = parse("```\n- not a block\n```\n- real\n");
        assert_eq!(document.preamble(), LineSpan::new(0, 3));
        assert_eq!(document.blocks().len(), 1);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(3, 4));

        let begin = parse("#+BEGIN_SRC\n- not a block\n#+END_SRC\n- real\n");
        assert_eq!(begin.preamble(), LineSpan::new(0, 3));
        assert_eq!(begin.blocks().len(), 1);
    }

    #[test]
    fn ordered_list_lines_are_not_block_markers() {
        let document = parse("- item\n  1. continuation\n  2. continuation\n");
        assert_eq!(document.blocks().len(), 1);
        assert_eq!(document.blocks()[0].own_span(), LineSpan::new(0, 3));
    }

    #[test]
    fn only_a_dash_opens_a_block() {
        // Measured against the official graph parser: `*` and `+` are text, at any indentation.
        // `- bullet` and `### heading` are blocks; `* star` and `+ plus` are continuation lines of
        // whatever precedes them, and at the top of a file they are preamble.
        let document = parse("- bullet\n### heading\n* star\n+ plus\n");
        assert_eq!(document.blocks().len(), 2);
        assert_eq!(document.blocks()[0].marker(), BlockMarker::Bullet);
        assert_eq!(
            document.blocks()[1].marker(),
            BlockMarker::Heading { level: 3 }
        );
        assert_eq!(document.blocks()[1].own_span(), LineSpan::new(1, 4));

        let top = parse("* star\n+ plus\n- real\n");
        assert_eq!(top.preamble(), LineSpan::new(0, 2));
        assert_eq!(top.blocks().len(), 1);
        assert_eq!(top.blocks()[0].own_span(), LineSpan::new(2, 3));
        assert_eq!(top.to_source(), "* star\n+ plus\n- real\n");
    }

    #[test]
    fn star_and_plus_lines_are_continuations_at_any_indent() {
        // `curl -v` output and org-mode logbook lines are the shapes this rule exists for: they
        // read as bullets to CommonMark and as noise in an outliner.
        for indent in ["  ", "\t", "      "] {
            let source = format!("- a\n{indent}* State \"DONE\" from \"TODO\"\n{indent}+ b\n");
            let document = parse(&source);
            assert_eq!(document.blocks().len(), 1, "source: {source:?}");
            assert_eq!(
                document.blocks()[0].own_span(),
                LineSpan::new(0, 3),
                "source: {source:?}"
            );
            assert_eq!(document.to_source(), source);
        }
    }

    #[test]
    fn raw_line_text_keeps_spaces_and_tabs() {
        let source = "- a  \n\t continuation\t\n";
        let document = parse(source);
        assert_eq!(document.lines()[0].text(), "- a  ");
        assert_eq!(document.lines()[1].text(), "\t continuation\t");
        assert_eq!(document.to_source(), source);
    }

    /// A source whose blocks nest one level per line, up to `levels` levels.
    fn nested_source(levels: usize) -> String {
        let mut source = String::new();
        for depth in 0..levels {
            source.push_str(&"\t".repeat(depth));
            source.push_str("- nested\n");
        }
        source
    }

    #[test]
    fn deeply_nested_document_is_dropped_without_recursing() {
        // Parsing is iterative, but the derived drop glue recursed once per nesting level and
        // aborted the process near 10000 levels. Dropping the document here is the assertion: it
        // must finish instead of aborting.
        const LEVELS: usize = 10_000;
        let document = parse(&nested_source(LEVELS));

        assert_eq!(document.lines().len(), LEVELS);
        assert_eq!(document.blocks().len(), 1);

        // The drop only proves anything if the tree is really that deep.
        let mut block = &document.blocks()[0];
        let mut depth = 0;
        while let Some(child) = block.children().first() {
            block = child;
            depth += 1;
        }
        assert_eq!(depth, LEVELS - 1, "the tree must really be that deep");

        drop(document);
    }

    #[test]
    fn deeply_nested_document_is_cloned_compared_and_traversed_iteratively() {
        // The derived Clone/PartialEq recursed once per nesting level and aborted near 10000
        // levels. Cloning, comparing, and traversing the document here is the assertion: all three
        // must finish instead of aborting.
        const LEVELS: usize = 10_000;
        let source = nested_source(LEVELS);
        let document = parse(&source);

        let clone = document.clone();

        // Failure formatting would recursively debug-print the deep tree, so compare via `assert!`.
        assert!(document == clone);

        let preorder = document.blocks_in_preorder();
        assert_eq!(preorder.len(), LEVELS);
        for (index, block) in preorder.iter().enumerate() {
            assert_eq!(block.indent(), index, "each level adds one tab of indent");
        }
        assert_eq!(
            clone.to_source(),
            source,
            "clone must render the original source"
        );
    }

    #[test]
    fn iterative_equality_detects_scalar_and_structure_differences() {
        let nested = |text: &str| parse(&format!("- {}\n  - {}\n", text, text));

        let left = nested("a");
        let right = nested("a");
        assert!(left == right);

        let different_marker = parse("# a\n  # a\n");
        assert!(left != different_marker);

        let different_indent = parse("- a\n    - a\n");
        assert!(left != different_indent);

        let different_child_count = parse("- a\n  - a\n  - a\n");
        assert!(left != different_child_count);
    }

    #[test]
    fn preorder_is_parent_first_and_sibling_ordered() {
        let document = parse("- a\n  - a1\n  - a2\n- b\n  - b1\n");

        let markers: Vec<String> = document
            .blocks_in_preorder()
            .iter()
            .map(|block| {
                document.raw_lines(block.own_span())[0].text().trim_start()[2..].to_owned()
            })
            .collect();

        assert_eq!(markers, ["a", "a1", "a2", "b", "b1"]);
    }

    #[test]
    fn empty_and_bom_only_sources_roundtrip() {
        let empty = parse("");
        assert!(empty.lines().is_empty());
        assert!(empty.blocks().is_empty());
        assert!(!empty.has_bom());
        assert_eq!(empty.to_source(), "");

        let bom_only = parse("\u{feff}");
        assert!(bom_only.lines().is_empty());
        assert!(bom_only.has_bom());
        assert_eq!(bom_only.to_source(), "\u{feff}");
    }
}
