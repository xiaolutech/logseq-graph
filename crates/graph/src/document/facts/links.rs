use std::borrow::Cow;

use super::VisibleLines;

/// Page names referenced by `[[...]]` in `text`, in source order, duplicates removed.
///
/// Accepts `[[name]]`, `[[name|alias]]`, and `[[name][description]]`; the alias or description is
/// dropped. A namespaced name also references each parent namespace, so `[[a/b/c]]` yields `a`,
/// `a/b`, and `a/b/c`, which is how Logseq links namespaces. Code produces nothing: text inside
/// fenced code blocks, inside `#+BEGIN_*` regions, and inside inline code spans is skipped.
pub fn extract_page_refs(text: &str) -> Vec<String> {
    extract_links(text).page_refs
}

/// Block references `((uuid))` in `text`, lowercased, in source order, duplicates removed.
///
/// Only a full 8-4-4-4-12 hexadecimal UUID counts, and code regions are skipped as in
/// [`extract_page_refs`].
pub fn extract_block_refs(text: &str) -> Vec<String> {
    extract_links(text).block_refs
}

/// Tags in `text` without their `#`, in source order, duplicates removed.
///
/// Accepts `#tag` (letters, digits, `_`, `-`) and `#[[multi word tag]]`. A `#` glued to a
/// preceding word character is not a tag, so `C#`, `##heading`, and a URL fragment stay text, and a
/// name inside square brackets is not a tag either, which keeps Logseq's `[#A]` priority marker out.
/// A `#` inside the label of a Markdown inline link or image is not a tag, because the official
/// parser reads the label as one `Link` node and extracts nothing from it; see
/// `mask_link_labels` for the measured evidence and the boundary. Code regions are skipped as in
/// [`extract_page_refs`].
pub fn extract_tags(text: &str) -> Vec<String> {
    extract_links(text).tags
}

pub(crate) fn property_list_targets(value: &str) -> Vec<String> {
    let visible = strip_inline_code(value);
    let visible = mask_link_labels(&visible);
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut previous = None;
    let mut cursor = 0;
    while let Some(ch) = visible[cursor..].chars().next() {
        let rest = &visible[cursor..];
        let bracketed = rest
            .strip_prefix("[[")
            .map(|after| (2, after))
            .or_else(|| rest.strip_prefix("#[[").map(|after| (3, after)));
        let target = bracketed
            .and_then(|(prefix, after)| {
                let mut depth = 1;
                let mut at = 0;
                while let Some(pair) = after
                    .get(at..)?
                    .as_bytes()
                    .windows(2)
                    .position(|pair| pair == b"[[" || pair == b"]]")
                {
                    at += pair;
                    if after.as_bytes()[at] == b'[' {
                        depth += 1;
                    } else {
                        depth -= 1;
                        if depth == 0 {
                            return Some((&after[..at], prefix + at + 2));
                        }
                    }
                    at += 2;
                }
                None
            })
            .or_else(|| {
                let after = rest.strip_prefix('#')?;
                let end = tag_end(after);
                (end > 0 && starts_tag(previous) && !inside_brackets(previous, &after[end..]))
                    .then_some((&after[..end], end + 1))
            });
        if let Some((target, len)) = target {
            property_plain_targets(&mut out, &visible[plain_start..cursor]);
            if !target.trim().is_empty() {
                push_unique(&mut out, target.trim());
            }
            cursor += len;
            plain_start = cursor;
            previous = visible[..cursor].chars().next_back();
        } else {
            cursor += ch.len_utf8();
            previous = Some(ch);
        }
    }
    property_plain_targets(&mut out, &visible[plain_start..]);
    out
}

fn property_plain_targets(out: &mut Vec<String>, plain: &str) {
    for name in plain
        .split([',', '，'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        push_unique(out, name);
    }
}

/// Links found in one run of text.
#[derive(Debug, Default)]
pub(super) struct Links {
    pub(super) page_refs: Vec<String>,
    pub(super) block_refs: Vec<String>,
    pub(super) tags: Vec<String>,
}

/// Extract every link kind from `text` in one pass over its visible lines.
pub(super) fn extract_links(text: &str) -> Links {
    let mut links = Links::default();
    for (_, line) in VisibleLines::new(text.split('\n').enumerate()) {
        let visible = strip_inline_code(line);
        collect_page_refs(&visible, &mut links.page_refs);
        collect_block_refs(&visible, &mut links.block_refs);
        // Tags alone see the link-masked line: the official parser drops a `#` inside a link
        // label, and it drops a `[[page]]` inside one too, so refs are the next question and
        // masking them here would answer it by accident.
        let tag_scoped = mask_link_labels(&visible);
        collect_tags(&tag_scoped, &mut links.tags);
    }
    links
}
/// Remove inline code spans from one line.
///
/// A span runs between two backtick runs of equal length, as in CommonMark. An unmatched run stays
/// literal, so a stray backtick cannot swallow the rest of the line.
fn strip_inline_code(line: &str) -> Cow<'_, str> {
    if !line.contains('`') {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        // A backtick byte cannot appear inside a multi-byte character, so byte scanning is safe.
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run = run_length(bytes, index);
        match closing_run(bytes, index + run, run) {
            Some(end) => {
                out.push_str(&line[copied..index]);
                index = end;
                copied = end;
            }
            None => index += run,
        }
    }
    out.push_str(&line[copied..]);
    Cow::Owned(out)
}

/// Length of the run of backticks starting at `start`.
fn run_length(bytes: &[u8], start: usize) -> usize {
    bytes[start..].iter().take_while(|b| **b == b'`').count()
}

/// End offset of the first backtick run of exactly `len` at or after `from`.
fn closing_run(bytes: &[u8], from: usize, len: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let run = run_length(bytes, index);
            if run == len {
                return Some(index + run);
            }
            index += run;
        } else {
            index += 1;
        }
    }
    None
}

/// Collect `[[name]]` references from one visible line.
fn collect_page_refs(line: &str, out: &mut Vec<String>) {
    let mut rest = line;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let name = after[..end].split(['|', ']']).next().unwrap_or("").trim();
        if !name.is_empty() {
            push_ref_with_namespaces(out, name);
        }
        rest = &after[end + 2..];
    }
}

/// Push a page name and, for a namespaced name, each parent namespace before it.
fn push_ref_with_namespaces(out: &mut Vec<String>, name: &str) {
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() == 1 || segments.iter().any(|segment| segment.is_empty()) {
        push_unique(out, name);
        return;
    }
    let mut prefix = String::new();
    for segment in segments {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        push_unique(out, &prefix);
    }
}

/// Collect `((uuid))` references from one visible line.
fn collect_block_refs(line: &str, out: &mut Vec<String>) {
    let mut rest = line;
    while let Some(start) = rest.find("((") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("))") else {
            break;
        };
        let inner = after[..end].trim();
        if is_uuid(inner) {
            push_unique(out, &inner.to_ascii_lowercase());
        }
        rest = &after[end + 2..];
    }
}

/// Whether `value` is a 8-4-4-4-12 hexadecimal UUID.
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Blank the label text of Markdown inline links and images in `line`, for tag extraction alone.
///
/// The rule this enforces is the official parser's, measured on a probe graph run through
/// `logseq.graph-parser.cli/parse-graph` (`logseq/og` at `6e7afa8e`, the revision the corpus
/// fixture pins): a block reading `[Logseq Office Hours #2: Building processes with
/// queries](https://example.com/)` projects **no** tag named `2`, `[link with #inner tag](url)` no
/// tag named `inner`, and `![alt #img](url)` no tag named `img`, while the same `#`s outside a
/// label project tags. mldoc reads the label as one `Link` node and nothing inside it is a `Tag`
/// node, so tag extraction skips the whole label.
///
/// Only the label of an inline link is blanked, and only for tags — page refs and block refs keep
/// the unmasked line, because the same parser does extract a reference from a link whose whole
/// label is one (`[[[inner]]](url)` answers the ref), so masking those here would be a second,
/// unevidenced rule. Two measured boundaries keep the mask narrow:
///
/// - A reference-style `[label #z][refdef]` is **not** a link to the official parser: it read the
///   `#z` as a tag whose name ran on as `z][refdef]`, so a `]` followed by `[` does not mask.
///   The cleaner name `z` this repository answers is the closer of the two.
/// - Bare brackets do not mask either: `[x #y] no link` read as the tag `y]` there. The `[#A]`
///   priority and the `[#9846](pull)` link text stay suppressed by `inside_brackets`, which
///   covers the `#` glued to the brackets themselves.
///
/// A label cannot hold an unescaped `]`, so the opener is found with a bracket stack rather than
/// a search backwards; nested labels such as `[a [b] c](url)` mask as one label, matching the
/// parser that reads them as one node. Blanking replaces whole bytes with spaces, which keeps the
/// output valid UTF-8 however the label was encoded, and the fast path returns the line unchanged
/// when it holds no `](` at all — most lines hold no link, so the common cost is one substring
/// search.
fn mask_link_labels(line: &str) -> Cow<'_, str> {
    if !line.contains("](") {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let mut masked = line.as_bytes().to_vec();
    let mut openers: Vec<usize> = Vec::new();
    let mut any = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'[' => openers.push(index),
            b']' => {
                let Some(open) = openers.pop() else {
                    continue;
                };
                if bytes.get(index + 1) == Some(&b'(') {
                    // Every replaced byte becomes an ASCII space, so multi-byte characters inside
                    // the label collapse to spaces and the line stays valid UTF-8.
                    for target in &mut masked[open + 1..index] {
                        *target = b' ';
                    }
                    any = true;
                }
            }
            _ => {}
        }
    }
    if any {
        // Unreplaced bytes are the line's own and replaced ones are ASCII spaces, so this is
        // always valid UTF-8; the lossy conversion is the total form of that invariant, because it
        // cannot invent bytes the line never had.
        Cow::Owned(String::from_utf8_lossy(&masked).into_owned())
    } else {
        Cow::Borrowed(line)
    }
}

/// Collect `#tag` and `#[[tag]]` names from one visible line.
fn collect_tags(line: &str, out: &mut Vec<String>) {
    let mut previous = None;
    let mut rest = line;
    while let Some(current) = rest.chars().next() {
        if current == '#' && starts_tag(previous) {
            let after = &rest[1..];
            match after.strip_prefix("[[") {
                Some(bracketed) => {
                    if let Some(end) = bracketed.find("]]") {
                        let name = bracketed[..end].trim();
                        if !name.is_empty() && !inside_brackets(previous, &bracketed[end + 2..]) {
                            push_unique(out, name);
                        }
                    }
                }
                None => {
                    let end = tag_end(after);
                    if end > 0 && !inside_brackets(previous, &after[end..]) {
                        push_unique(out, &after[..end]);
                    }
                }
            }
        }
        previous = Some(current);
        rest = &rest[current.len_utf8()..];
    }
}

/// Whether a tag may start after `previous`, which is the character before the `#`.
fn starts_tag(previous: Option<char>) -> bool {
    match previous {
        None => true,
        Some(c) => !(c.is_alphanumeric() || matches!(c, '_' | '/' | '#' | '\\')),
    }
}

/// Whether the tag sits inside square brackets, where it is something else.
///
/// Logseq writes a block's priority as `[#A]`, `[#B]`, or `[#C]`, and a link whose text is a `#`
/// reference, as in `[#9846](https://github.com/logseq/logseq/pull/9846)`, is link text rather than
/// a tag. Without this check both mint tags named after a letter or a pull request number, which is
/// the most common false tag in the reference graph.
fn inside_brackets(previous: Option<char>, rest: &str) -> bool {
    previous == Some('[') && rest.starts_with(']')
}

/// Byte offset just past the last character of the tag name that `text` starts with.
///
/// A name is made of `[alphanumeric, '_', '-', '/']`. A namespace stays inside one name, because
/// Logseq's `get-tag` takes the whole text of the tag node, so `#a/b` is the single tag `a/b`. A
/// name cannot start with `/` (`#/foo` is not a tag), cannot end with `/` (`#foo/` names `foo`),
/// and cannot hold an empty level (`#foo//bar` names `foo`).
fn tag_end(text: &str) -> usize {
    let mut end = 0;
    let mut previous_slash = false;
    for (offset, c) in text.char_indices() {
        if !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')) {
            break;
        }
        if c == '/' && (end == 0 || previous_slash) {
            break;
        }
        previous_slash = c == '/';
        end = offset + c.len_utf8();
    }
    if end > 0 && text.as_bytes()[end - 1] == b'/' {
        end -= 1;
    }
    end
}

/// Append `name` unless an ASCII-case-insensitive equal is already present.
///
/// A link is a set membership, so a repeated `[[x]]` is one fact, and the first spelling and
/// position win. Case folding here is ASCII-only because this layer keeps text, not identity: the
/// authority on whether two spellings are one page is
/// [`page_name_key`](crate::domain::page_name_key), and the projection collapses refs and tags by
/// that key. So `[[Ä]] [[ä]]` stays two entries here and one row there, which is the same row
/// `[[Ä]] [[Ä]]` produces.
fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(name))
    {
        out.push(name.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{parse, PageFacts, Property};
    fn facts(source: &str) -> PageFacts {
        PageFacts::extract(&parse(source))
    }

    #[test]
    fn links_inside_a_property_value_are_block_links() {
        // Logseq's own Properties page uses this line to show that a value links like any text.
        let page =
            facts("- 说明\n  description:: [[Logseq]] is the fastest #triples #[[text editor]]\n");
        let block = &page.blocks()[0];

        // A bracketed tag is a page reference too, which is what Logseq's docs describe.
        assert_eq!(block.page_refs(), ["Logseq", "text editor"]);
        assert_eq!(block.tags(), ["triples", "text editor"]);
        assert_eq!(
            block.property("description").map(Property::value),
            Some("[[Logseq]] is the fastest #triples #[[text editor]]")
        );

        // Logseq treats a quoted value as unlinked text. This layer keeps the quotes and the refs
        // because quoting is a property-value rule, not a lexical one; the projection subtracts
        // it using the raw value it still has here.
        let quoted = facts("- 说明\n  description:: \"[[Logseq]]\"\n");
        assert_eq!(
            quoted.blocks()[0]
                .property("description")
                .map(Property::value),
            Some("\"[[Logseq]]\"")
        );
    }

    #[test]
    fn page_refs_include_namespace_parents_and_drop_aliases() {
        assert_eq!(
            extract_page_refs("[[ArgoCD]] 与 [[a/b/c]] 与 [[目标|别名]] 与 [[x][描述]]"),
            vec!["ArgoCD", "a", "a/b", "a/b/c", "目标", "x"]
        );
        // A link is one fact no matter how often it appears.
        assert_eq!(extract_page_refs("[[A]] [[a]]"), vec!["A"]);
        // Broken and empty links produce nothing.
        assert_eq!(
            extract_page_refs("[[]] [[  ]] [[unclosed"),
            Vec::<String>::new()
        );
        // A name with an empty namespace segment is kept as written, not expanded.
        assert_eq!(extract_page_refs("[[a//b]]"), vec!["a//b"]);
    }

    #[test]
    fn page_refs_and_tags_ignore_code_spans() {
        assert_eq!(extract_page_refs("`[[隐藏]]` 外 [[可见]]"), vec!["可见"]);
        assert_eq!(
            extract_page_refs("```rust\n[[fenced]]\n```\n[[outside]]"),
            vec!["outside"]
        );
        assert_eq!(
            extract_page_refs("#+BEGIN_SRC\n[[hidden]]\n#+END_SRC\n[[outside]]"),
            vec!["outside"]
        );
        // An unmatched backtick is literal, so it does not swallow the rest of the text.
        assert_eq!(extract_page_refs("`unclosed [[kept]]"), vec!["kept"]);

        assert_eq!(extract_tags("`#logseq` 外 #real"), vec!["real"]);
        assert_eq!(
            extract_tags("```rust\n# 注释 #fenced\n```\n尾部 #outside"),
            vec!["outside"]
        );
    }

    #[test]
    fn block_refs_require_a_well_formed_uuid() {
        assert_eq!(
            extract_block_refs(
                "((61745DEB-EEED-4426-8020-EB9E5C90D558)) ((not-a-uuid)) ((61745deb-eeed-4426-8020-eb9e5c90d55))"
            ),
            vec!["61745deb-eeed-4426-8020-eb9e5c90d558"]
        );
        // Wrong dash positions are not a UUID even at the right length.
        assert_eq!(
            extract_block_refs("((61745deb-eeed-4426-8020eb9e5c90d558-))"),
            Vec::<String>::new()
        );
        assert_eq!(
            extract_block_refs("`((61745deb-eeed-4426-8020-eb9e5c90d558))`"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn tags_need_a_word_boundary_and_a_plain_name() {
        assert_eq!(
            extract_tags("C# 与 #real 与 #中文标签"),
            vec!["real", "中文标签"]
        );
        // A URL fragment, a heading marker, and a directive are not tags.
        assert_eq!(
            extract_tags("https://example.com/page#frag"),
            Vec::<String>::new()
        );
        // The `#` follows a `/`, the character a fragment follows in a URL without a path.
        assert_eq!(
            extract_tags("https://example.com/#frag"),
            Vec::<String>::new()
        );
        assert_eq!(extract_tags("##heading"), Vec::<String>::new());
        assert_eq!(extract_tags("#+BEGIN_QUERY"), Vec::<String>::new());
        // A namespaced tag is one tag, because Logseq's `get-tag` keeps the namespace.
        assert_eq!(extract_tags("#a/b"), vec!["a/b"]);
        assert_eq!(extract_tags("#n1/n2/n3"), vec!["n1/n2/n3"]);
        // A name cannot start or end with `/`, and `//` ends it.
        assert_eq!(extract_tags("#/foo"), Vec::<String>::new());
        assert_eq!(extract_tags("#foo/"), vec!["foo"]);
        assert_eq!(extract_tags("#foo//bar"), vec!["foo"]);
        // A priority marker is not a tag named after its letter.
        assert_eq!(extract_tags("TODO [#A] 紧急"), Vec::<String>::new());
        assert_eq!(extract_tags("[#C]"), Vec::<String>::new());
        // Link text that starts with `#` is a reference to a pull request, not a tag.
        assert_eq!(
            extract_tags("[#9846](https://github.com/logseq/logseq/pull/9846)"),
            Vec::<String>::new()
        );
        // The same digits written as a bare tag stay a tag.
        assert_eq!(extract_tags("见 #9846 与 [#A]"), vec!["9846"]);
        // A bracketed tag keeps its spaces and is one fact.
        assert_eq!(
            extract_tags("#[[text editor]] #[[Text Editor]]"),
            vec!["text editor"]
        );
        // The bracketed spelling agrees with the bare one about the namespace.
        assert_eq!(extract_tags("#[[foo/bar]]"), vec!["foo/bar"]);
        assert_eq!(extract_tags("#tag/path"), extract_tags("#[[tag/path]]"));
        assert_eq!(extract_tags("#tag, #tag."), vec!["tag"]);
    }

    #[test]
    fn a_hash_inside_a_markdown_link_label_is_not_a_tag() {
        // Every case here is the official parser's answer, measured on a probe graph run through
        // `logseq.graph-parser.cli/parse-graph` (`logseq/og` at `6e7afa8e`, the revision the
        // corpus fixture pins): none of these labels produced a tag or a ref. mldoc reads the
        // label as one `Link` node and nothing inside it is a `Tag` node.
        assert_eq!(
            extract_tags(
                "[Logseq Office Hours #2: Building processes with queries](https://example.com/)"
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            extract_tags("[link with #inner tag](https://example.com/x)"),
            Vec::<String>::new()
        );
        assert_eq!(
            extract_tags("[mid #tag here](https://example.com/m)"),
            Vec::<String>::new()
        );
        // An image's alt text is a link label of the same kind.
        assert_eq!(
            extract_tags("![alt #img](https://example.com/i.png)"),
            Vec::<String>::new()
        );
        // The bracketed spelling is suppressed inside a label too.
        assert_eq!(
            extract_tags("[pre #[[tag in label]] post](https://example.com/t)"),
            Vec::<String>::new()
        );
        // The same line keeps every tag outside the label, which is what separates this rule
        // from a blanket "no tags near brackets".
        assert_eq!(
            extract_tags(
                "before #untagged then [label #masked](https://example.com/a) then #afterlink"
            ),
            vec!["untagged", "afterlink"]
        );
        // A nested label is one label, as it is one node to the parser.
        assert_eq!(
            extract_tags("[a [b #nested] c](https://example.com/n)"),
            Vec::<String>::new()
        );
        // A `#` after a masked label still counts, because the scan sees the real `]` delimiter.
        assert_eq!(
            extract_tags("[masked #m](https://example.com/) #after"),
            vec!["after"]
        );
    }

    #[test]
    fn a_hash_inside_brackets_without_a_link_still_counts() {
        // Measured on the same probe: the official parser does not read `[x #y]` as a link — the
        // tag it minted ran on as `y]` — so brackets without `](` do not mask.
        assert_eq!(extract_tags("bare brackets [x #y] no link"), vec!["y"]);
        // A reference-style `[label #z][refdef]` is not a link there either; the tag it minted was
        // the garbled `z][refdef]`. This repository answers the name the tag characters spell.
        assert_eq!(
            extract_tags("reference style [label #z][refdef]"),
            vec!["z"]
        );
        // The `#` glued to the brackets stays suppressed as before: `[#A]` is a priority marker
        // and `[#9846](url)` is link text.
        assert_eq!(extract_tags("TODO [#A] priority"), Vec::<String>::new());
        assert_eq!(
            extract_tags("[#9846](https://github.com/logseq/logseq/pull/9846)"),
            Vec::<String>::new()
        );
        // A page ref is not a link label: masking is for tags alone and the bracket stack never
        // sees `](` in `[[x #y]]`, so a tag inside a page-ref name keeps its current answer.
        assert_eq!(extract_tags("[[x #y]] and #z"), vec!["y", "z"]);
    }
}
