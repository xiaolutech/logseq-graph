//! Identity of a page name.
//!
//! A page name is written text, and two spellings of it can denote one page. Every layer that has to
//! decide whether two names are the same page calls [`page_name_key`], so discovery's duplicate
//! check, the index's page key, and a `[[ref]]` target cannot drift apart.

/// Key that decides whether two page names denote one page: the name lowercased.
///
/// Logseq compares page names case-insensitively and applies no Unicode normalization. So this is
/// Unicode's default lowercase mapping and nothing else:
///
/// - `Ä` and `ä` are one page, and so are `Ärger ÜBER` and `ärger über`.
/// - `ß` and `ss` are two pages: lowercase mapping is not full case folding.
/// - `é` written as one code point and as `e` plus a combining accent are two pages, because
///   normalization would also merge names that Logseq keeps apart.
/// - The mapping is applied per character as Unicode defines it, not ASCII-only, so a name outside
///   ASCII folds the same way the file system and Logseq fold it.
///
/// The key is derived from the name, never from a path or a file name encoding: a page's name is its
/// decoded file name and a journal's name is its formatted date title, and a page and a journal
/// collide only when those displayed names collide.
///
/// This is the page-name rule. Property keys have their own rule (`_` becomes `-`, see
/// [`Property::normalized_key`](crate::document::Property::normalized_key)); the two are deliberately
/// not shared, because Logseq renames underscores in property keys and never in page names.
pub fn page_name_key(name: &str) -> String {
    name.to_lowercase()
}

/// The names a list-valued property value holds.
///
/// `alias::`, `tags::`, and `topics::` are comma-separated lists of page names. Brackets around an
/// item (`[[name]]`) are decoration, not part of the name. Splitting lives here so a simple query
/// and a name lookup cannot disagree about what one item is.
pub fn list_value_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .map(|item| {
            item.strip_prefix("[[")
                .and_then(|rest| rest.strip_suffix("]]"))
                .unwrap_or(item)
                .trim()
        })
        .filter(|item| !item.is_empty())
}

/// Whether one list-valued property value names `wanted`.
///
/// Both sides go through [`page_name_key`], the one rule for when two spellings name one page. A
/// raw `==` here would make `zmi` and `ZMI` two different aliases while every page read treats
/// them as one name.
pub fn list_value_matches(value: &str, wanted: &str) -> bool {
    let wanted = wanted.trim();
    let wanted = wanted
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .unwrap_or(wanted)
        .trim();
    let key = page_name_key(wanted);
    list_value_items(value).any(|item| page_name_key(item) == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_folds_case_per_character_and_nothing_else() {
        assert_eq!(page_name_key("Ärger ÜBER"), "ärger über");
        assert_eq!(page_name_key("Straße"), "straße");
        assert_eq!(
            page_name_key("setting/enable journals"),
            "setting/enable journals"
        );

        // Not full case folding: `ß` does not become `ss`.
        assert_ne!(page_name_key("Straße"), page_name_key("Strasse"));
        // Not normalization: a precomposed and a decomposed `é` stay two keys.
        assert_ne!(page_name_key("\u{e9}"), page_name_key("e\u{301}"));
        // Not the property-key rule: an underscore stays an underscore.
        assert_eq!(page_name_key("Done_At"), "done_at");
    }

    #[test]
    fn a_list_value_matches_under_the_page_name_rule() {
        assert!(list_value_matches("zmi, another", "ZMI"));
        assert!(list_value_matches("[[架构决策记录]], ADRs", "架构决策记录"));
        assert!(!list_value_matches("zmi", "z mix"));
        assert_eq!(
            list_value_items(" [[a]] , b ,").collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
