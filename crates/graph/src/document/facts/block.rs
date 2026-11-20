use super::super::line::{LineSpan, SourceLine};
use super::super::parser::{marker_content_start, trim_indent, Block, PageDocument};
use super::links::extract_links;
use super::properties::{extract_properties, lookup};
use super::{Property, ID_PROPERTY};
use crate::domain::{BlockId, Priority, TaskMarker};

/// Facts about one block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockFacts {
    own_span: LineSpan,
    text: String,
    text_start: usize,
    properties: Vec<Property>,
    page_refs: Vec<String>,
    block_refs: Vec<String>,
    tags: Vec<String>,
}

impl BlockFacts {
    pub(super) fn extract(document: &PageDocument, block: &Block) -> Self {
        let own_span = block.own_span();
        let (text, skipped) = block_text(document.raw_lines(own_span));
        let text_start = own_span.start() + skipped;
        let properties = extract_properties(
            text.split('\n')
                .enumerate()
                .map(|(offset, line)| (text_start + offset, line)),
        );
        let links = extract_links(&text);
        Self {
            own_span,
            text,
            text_start,
            properties,
            page_refs: links.page_refs,
            block_refs: links.block_refs,
            tags: links.tags,
        }
    }

    /// Source lines this block owns: its marker line and continuation lines, not its children.
    ///
    /// The span lets a caller join facts back to the structure without relying on traversal order.
    pub const fn own_span(&self) -> LineSpan {
        self.own_span
    }

    /// The block's own content, with the marker and the block's content indentation removed.
    ///
    /// Continuations lose the marker's content indentation. A bare `-` instead drops its empty
    /// marker line, keeps the first continuation verbatim, and removes the marker's indentation
    /// plus two bytes from the rest, matching the official parser. Truly empty lines at a block
    /// boundary are omitted; whitespace-only lines and interior blank lines remain. Source lines
    /// are unchanged, and projected lines are joined with `\n`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The task marker this block's text leads with, or `None` when the block is not a task.
    ///
    /// The rule is [`TaskMarker::leading`], called rather than restated: the parser axis and a
    /// query ask the same question about the same text, so the answer has one home (ADR-0003).
    /// What makes the call valid is the contract [`BlockFacts::text`] keeps — byte 0 is the
    /// block's first content byte, so a marker that led the source line sits at byte 0 here and
    /// the indentation is already gone instead of reading as leading whitespace.
    ///
    /// The text keeps the marker word: this reports a fact about the text, it does not edit it, and
    /// an editor replacing the whole text still sees what the file wrote.
    pub fn task_marker(&self) -> Option<TaskMarker> {
        TaskMarker::leading(&self.text)
    }

    /// The priority the block's text leads with, in the position a priority belongs.
    ///
    /// [`Priority::leading_after`] decides it, given the marker [`Self::task_marker`] found: with a
    /// marker the group must follow it, without one the group must begin the text. Both answers
    /// come from the same [`BlockFacts::text`], so a `[#A]` later in a description is not the
    /// block's priority.
    pub fn priority(&self) -> Option<Priority> {
        Priority::leading_after(&self.text, self.task_marker())
    }

    /// The block's own lines that are not property lines, joined by `\n`: what an editor edits.
    ///
    /// [`BlockFacts::text`] keeps the property lines because the projection stores the whole block
    /// text. An editor shows the content and writes a property through its own command, so the text
    /// an edit replaces is this one, and a block whose own lines are all property lines has an empty
    /// editable text. Everything else matches [`BlockFacts::text`]: no trailing newline, blank lines
    /// kept, lines in source order.
    pub fn editable_text(&self) -> String {
        self.text
            .split('\n')
            .enumerate()
            .filter(|(offset, _)| {
                !self
                    .properties
                    .iter()
                    .any(|property| property.line() == self.text_start + offset)
            })
            .map(|(_, line)| line)
            .collect::<Vec<&str>>()
            .join("\n")
    }

    /// Property lines found in [`BlockFacts::text`], in source order, duplicates included.
    ///
    /// See [`BlockFacts::property`] for how a repeated key resolves.
    pub fn properties(&self) -> &[Property] {
        &self.properties
    }

    /// Source-line offsets within the own span that are not property lines.
    ///
    /// Edits use these positions to preserve properties. They include marker and boundary lines
    /// omitted from the projected text. A marker line that states a property is excluded too.
    pub(crate) fn editable_line_offsets(&self) -> Vec<usize> {
        let start = self.own_span.start();
        let properties: Vec<usize> = self
            .properties
            .iter()
            .map(|property| property.line() - start)
            .collect();
        (0..self.own_span.len())
            .filter(|offset| !properties.contains(offset))
            .collect()
    }

    /// Last property written under `key`, compared by normalized key.
    pub fn property(&self, key: &str) -> Option<&Property> {
        lookup(&self.properties, key)
    }

    /// The durable identity this block states, when its `id::` value is one (ADR-0005).
    ///
    /// This is the only definition of the rule: the projection stores it in `blocks.durable_id`, the
    /// snapshot reports it as a block's `durable_id`, and both call this function, so the two cannot
    /// drift apart. Three consequences of using the ordinary property lookup:
    ///
    /// - The key is matched in its normalized form, so `ID::` and `Id::` reach it like any other
    ///   property.
    /// - A repeated key answers its **last** line, the same last-one-wins a later read of the
    ///   property rows applies.
    /// - A value that does not parse as a `8-4-4-4-12` hexadecimal UUID answers `None` rather than
    ///   an error. The line is ordinary text to every other reader, and Logseq does not refuse such
    ///   a page either: one bad line must not make a whole file unreadable. The stored property rows
    ///   keep the value verbatim, so nothing is lost.
    pub fn durable_id(&self) -> Option<BlockId> {
        self.property(ID_PROPERTY)
            .and_then(|property| BlockId::parse(property.value()).ok())
    }

    /// Distinct pages this block's text references through `[[...]]`, in source order.
    ///
    /// Property lines count, because they are part of the block's text.
    pub fn page_refs(&self) -> &[String] {
        &self.page_refs
    }

    /// Distinct block references `((uuid))` in this block's text, lowercased, in source order.
    pub fn block_refs(&self) -> &[String] {
        &self.block_refs
    }

    /// Distinct `#tag` names without the `#`, in source order.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}
/// Render a block's own text from the lines its `own_span` covers.
fn block_text(mut lines: &[SourceLine]) -> (String, usize) {
    while lines.last().is_some_and(|line| line.text().is_empty()) {
        lines = &lines[..lines.len() - 1];
    }
    let Some(first) = lines.first() else {
        return (String::new(), 0);
    };
    let first_text = first.text();
    // A block always starts on a marker line, so the marker lookup succeeds; falling back to the
    // indentation alone keeps the function total instead of panicking on an impossible input.
    let align = marker_content_start(first_text).unwrap_or_else(|| indent_width(first_text));
    let bare = trim_indent(first_text) == "-" && lines.len() > 1;
    let (head, rest, align) = if bare {
        (lines[1].text(), &lines[2..], indent_width(first_text) + 2)
    } else {
        (
            &first_text[align.min(first_text.len())..],
            &lines[1..],
            align,
        )
    };
    let mut text = String::from(head);
    for line in rest {
        text.push('\n');
        text.push_str(without_alignment(line.text(), align));
    }
    (text, usize::from(bare))
}

/// Width in bytes of the leading spaces and tabs of `line`.
fn indent_width(line: &str) -> usize {
    line.len() - trim_indent(line).len()
}

/// Remove up to `align` bytes of leading whitespace.
fn without_alignment(line: &str, align: usize) -> &str {
    &line[indent_width(line).min(align)..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{parse, PageFacts};
    fn facts(source: &str) -> PageFacts {
        PageFacts::extract(&parse(source))
    }

    fn block_texts(source: &str) -> Vec<String> {
        facts(source)
            .blocks()
            .iter()
            .map(|block| block.text().to_owned())
            .collect()
    }

    /// Editable texts of every block, in preorder.
    fn editable_texts(source: &str) -> Vec<String> {
        facts(source)
            .blocks()
            .iter()
            .map(BlockFacts::editable_text)
            .collect()
    }

    #[test]
    fn official_parser_continuation_and_fence_projection() {
        // Captured with og@6e7afa8's parse-graph; reproduction and answers: docs/corpus-parity.md.
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "bare",
                "-\n  first\n  second\n- next\n",
                &["  first\nsecond", "next"],
            ),
            (
                "bare-three",
                "-\n  first\n  second\n  third\n- next\n",
                &["  first\nsecond\nthird", "next"],
            ),
            (
                "bare-deep",
                "  -\n      first\n      second\n      third\n- next\n",
                &["      first\n  second\n  third", "next"],
            ),
            (
                "bare-child",
                "-\n  first\n  second\n  - child\n- next\n",
                &["  first\nsecond", "child", "next"],
            ),
            ("blank", "- a\n\n- b\n", &["a", "b"]),
            ("blank-spaces", "- a\n  \n- b\n", &["a\n", "b"]),
            ("blank-two", "- a\n\n\n- b\n", &["a", "b"]),
            (
                "blank-child",
                "- a\n\n  - child\n- b\n",
                &["a", "child", "b"],
            ),
            (
                "blank-middle",
                "- a\n  first\n\n  second\n- b\n",
                &["a\nfirst\n\nsecond", "b"],
            ),
            (
                "unclosed",
                "- ```\n  code\n- sibling\n",
                &["```\ncode", "sibling"],
            ),
            (
                "unclosed-child",
                "- ```\n  code\n  - child\n",
                &["```\ncode", "child"],
            ),
            (
                "unclosed-parent",
                "- parent\n  - ```\n    code\n- next\n",
                &["parent", "```\ncode", "next"],
            ),
            ("unclosed-eof", "- ```\n  code\n", &["```\ncode"]),
            (
                "unclosed-child-parent-sibling",
                "- parent\n  - ```\n    code\n- sibling\n",
                &["parent", "```\ncode", "sibling"],
            ),
            (
                "closed",
                "- ```\n  code\n  ```\n- sibling\n",
                &["```\ncode\n```", "sibling"],
            ),
            (
                "close-after-sibling",
                "- ```\n  code\n- sibling\n  ```\n- next\n",
                &["```\ncode\n- sibling\n```", "next"],
            ),
            (
                "close-after-child",
                "- ```\n  code\n  - child\n  ```\n- next\n",
                &["```\ncode\n- child\n```", "next"],
            ),
            (
                "bare-space",
                "- \n  first\n  second\n",
                &["\nfirst\nsecond"],
            ),
            (
                "bare-region",
                "-\n  #+BEGIN_QUERY\n  first\n  second\n  #+END_QUERY\n",
                &["  #+BEGIN_QUERY\nfirst\nsecond\n#+END_QUERY"],
            ),
            (
                "bare-four-indent",
                "-\n    first\n    second\n",
                &["    first\n  second"],
            ),
            (
                "bare-nested",
                "- parent\n  -\n    first\n    second\n",
                &["parent", "    first\nsecond"],
            ),
            ("unclosed-preamble", "```\ncode\n- b\n", &["b"]),
            (
                "bare-property",
                "-\n id:: 11111111-1111-1111-1111-111111111111\n- next\n",
                &[" id:: 11111111-1111-1111-1111-111111111111", "next"],
            ),
            ("empty", "", &[]),
            ("bare-empty", "-", &[""]),
        ];
        for (name, source, expected) in cases {
            let document = parse(source);
            assert_eq!(document.to_source(), *source, "{name}");
            let actual = block_texts(source);
            println!("{name}={actual:?}");
            assert_eq!(actual, *expected, "{name}");
        }
    }

    #[test]
    fn a_bare_markers_projected_properties_keep_their_source_lines() {
        let blocks = facts("-\n  first\n  id:: 61111111-1111-1111-1111-111111111111\n\n- next\n");
        let block = &blocks.blocks()[0];
        assert_eq!(block.properties()[0].line(), 2);
        assert_eq!(block.editable_text(), "  first");
    }

    #[test]
    fn block_text_drops_marker_and_alignment_only() {
        // Trailing whitespace, inner tabs, and blank continuation lines survive: the text is the
        // block's own source, not a normalized rendering of it.
        assert_eq!(
            block_texts("- first  \n  continuation\t\n\n  tail\n"),
            vec!["first  \ncontinuation\t\n\ntail"]
        );

        // A continuation line indented less than the content column loses only its own indentation.
        assert_eq!(
            block_texts("### heading\n  short\n\t- child\n"),
            vec!["heading\nshort", "child"]
        );

        // A marker with no content yields an empty text, and so does a bare bullet.
        assert_eq!(block_texts("-\n- \n"), vec!["", ""]);
    }

    #[test]
    fn block_text_keeps_fenced_code_verbatim() {
        let source = "- 代码\n\t- ```bash\n\t  # 注释行\n\t  echo hi\n\t  ```\n";
        let texts = block_texts(source);
        assert_eq!(texts[0], "代码");
        assert_eq!(texts[1], "```bash\n# 注释行\necho hi\n```");
    }

    #[test]
    fn editable_text_of_a_block_without_properties_is_its_text() {
        for source in ["- a", "- first  \n  second\t\n\n  fourth\n", "-\n- \n"] {
            let blocks = facts(source);
            for block in blocks.blocks() {
                assert_eq!(block.editable_text(), block.text(), "{source:?}");
            }
        }
    }

    #[test]
    fn editable_text_drops_the_id_and_collapsed_lines() {
        assert_eq!(
            editable_texts(
                "- 正文\n  id:: 11111111-2222-3333-4444-555555555555\n  collapsed:: true\n"
            ),
            vec!["正文"]
        );
    }

    #[test]
    fn editable_text_drops_a_custom_property_too() {
        assert_eq!(editable_texts("- 正文\n  todo:: x\n"), vec!["正文"]);
    }

    #[test]
    fn editable_text_keeps_the_order_a_property_line_interrupts() {
        // The property is removed, not the line break it occupied: `a` and `b` are still two lines.
        assert_eq!(editable_texts("- a\n  id:: u\n  b\n"), vec!["a\nb"]);
    }

    #[test]
    fn editable_text_keeps_continuation_lines_and_blank_lines_in_order() {
        assert_eq!(
            editable_texts("- one\n  two\n\n  id:: u\n  three\n\t\n  four\n"),
            vec!["one\ntwo\n\nthree\n\nfour"]
        );
    }

    #[test]
    fn editable_text_is_empty_when_the_marker_line_states_a_property() {
        // A property line may be the block's own marker line, and then nothing the editor shows is
        // left of it. The line stays part of `text()`, which is why the two answers differ.
        assert_eq!(editable_texts("- id:: u\n"), vec![""]);
        assert_eq!(editable_texts("- id:: u\n  more\n"), vec!["more"]);
        assert_eq!(facts("- id:: u\n").blocks()[0].text(), "id:: u");
    }

    #[test]
    fn durable_id_is_the_last_id_line_whose_value_is_a_uuid() {
        const ID_A: &str = "61111111-1111-1111-1111-111111111111";
        const ID_B: &str = "62222222-2222-2222-2222-222222222222";

        let id_of = |source: &str| {
            facts(source).blocks()[0]
                .durable_id()
                .as_ref()
                .map(BlockId::as_str)
                .map(str::to_owned)
        };

        // A block that states no `id::`, and one whose value is not a UUID, name no durable block.
        assert_eq!(id_of("- no id\n"), None);
        assert_eq!(id_of("- x\n  id:: not-a-uuid\n"), None);
        assert_eq!(
            id_of("- x\n  id:: 62222222-2222-2222-2222-22222222222\n"),
            None
        );

        // The key is matched in its normalized form, so Logseq's own casing reaches the same line.
        // A block never states `i_d::`, because the normalized key would be `i-d`.
        for source in [
            format!("- x\n  id:: {ID_A}\n"),
            format!("- x\n  ID:: {ID_A}\n"),
            format!("- x\n  Id:: {ID_A}\n"),
        ] {
            assert_eq!(id_of(&source), Some(ID_A.to_owned()), "{source:?}");
        }

        // A repeated key answers its last line, the same last-one-wins a read of the property rows
        // applies: the later id wins, and a later malformed one removes the identity.
        assert_eq!(
            id_of(&format!("- x\n  id:: {ID_A}\n  id:: {ID_B}\n")),
            Some(ID_B.to_owned())
        );
        assert_eq!(id_of(&format!("- x\n  id:: {ID_A}\n  id:: nope\n")), None);
    }
}
