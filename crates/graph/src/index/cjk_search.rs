use crate::domain::search_key_char;

/// Script ranges indexed for one- and two-character literal searches.
///
/// Includes Han extensions A–J and compatibility ideographs, kana (including halfwidth and
/// supplementary kana), Hangul syllables and Jamo, and ideographic iteration/zero marks.
/// Punctuation and spaces are boundaries, not CJK: in particular neither comma joins 中 to 文.
pub(super) fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{2e80}'..='\u{2eff}'
            | '\u{2f00}'..='\u{2fdf}'
            | '\u{3005}'..='\u{3007}'
            | '\u{302e}'..='\u{302f}'
            | '\u{3031}'..='\u{3035}'
            | '\u{303b}'
            | '\u{3041}'..='\u{3096}'
            | '\u{3099}'..='\u{309f}'
            | '\u{30a1}'..='\u{30fa}'
            | '\u{30fc}'..='\u{30ff}'
            | '\u{31f0}'..='\u{31ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ff66}'..='\u{ff9f}'
            | '\u{1aff0}'..='\u{1afff}'
            | '\u{1b000}'..='\u{1b16f}'
            | '\u{20000}'..='\u{2a6df}'
            | '\u{2a700}'..='\u{2ee5f}'
            | '\u{2f800}'..='\u{2fa1f}'
            | '\u{30000}'..='\u{3347f}'
            | '\u{1100}'..='\u{11ff}'
            | '\u{3130}'..='\u{318f}'
            | '\u{a960}'..='\u{a97f}'
            | '\u{ac00}'..='\u{d7ff}'
    )
}

/// Prefixed unigram and adjacent-bigram tokens over the search fold, plus non-CJK fillers.
///
/// The u/b prefixes distinguish unigrams from bigrams. Non-CJK runs reset adjacency, so 中x文 and
/// 中，文 never emit b中文. Each such run emits one filler, independent of its length.
/// Tokenizer categories include every Unicode category except the explicit space separator,
/// including Han introduced after unicode61's Unicode version and kana combining marks.
/// No normalization or diacritic removal is permitted.
pub(super) fn tokens(text: &str) -> String {
    let mut out = String::new();
    let mut filler = false;
    let mut previous = None;
    for character in text.chars().map(search_key_char) {
        if is_cjk(character) {
            if let Some(previous) = previous {
                out.push('b');
                out.push(previous);
                out.push(character);
                out.push(' ');
            }
            out.push('u');
            out.push(character);
            out.push(' ');
            filler = false;
            previous = Some(character);
        } else if !filler {
            out.push_str("0 ");
            filler = true;
            previous = None;
        }
    }
    out
}

/// A quoted unigram or bigram token; all other queries keep their existing path.
pub(super) fn phrase(needle: &str) -> Option<String> {
    let mut characters = needle.chars();
    let first = characters.next()?;
    let second = characters.next();
    if !is_cjk(first) || second.is_some_and(|c| !is_cjk(c)) || characters.next().is_some() {
        return None;
    }
    Some(match second {
        Some(second) => format!("\"b{first}{second}\""),
        None => format!("\"u{first}\""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::search_key;

    #[test]
    fn cjk_includes_han_kana_and_hangul_but_not_punctuation_or_latin() {
        for character in [
            '中',
            '書',
            '\u{3400}',
            '\u{20000}',
            '\u{2a700}',
            '\u{2b740}',
            '\u{2b820}',
            '\u{2ceb0}',
            '\u{2ebf0}',
            '\u{2f800}',
            '\u{30000}',
            '\u{31350}',
            '\u{323b0}',
            '\u{33479}',
            '﨑',
            'ぁ',
            'か',
            '\u{3099}',
            'カ',
            'ｶ',
            'ﾞ',
            '\u{1b000}',
            '\u{1aff0}',
            '한',
            'ᄀ',
            'ᅡ',
            '\u{a960}',
            '\u{d7b0}',
            '々',
            '〇',
        ] {
            assert!(is_cjk(character), "{character:?}");
        }
        for character in [
            'a', 'Ä', 'K', 'İ', '，', '、', '・', ' ', '\u{3000}', '\n', '\0',
        ] {
            assert!(!is_cjk(character), "{character:?}");
        }
    }

    #[test]
    fn tokens_preserve_adjacency_and_collapse_only_non_cjk_runs() {
        assert_eq!(tokens("中文"), "u中 b中文 u文 ");
        assert_eq!(tokens("中x文"), "u中 0 u文 ");
        assert_eq!(tokens("中，文"), "u中 0 u文 ");
        assert_eq!(tokens("中・文"), "u中 0 u文 ");
        assert_eq!(tokens("abc\n123中文 xyz"), "0 u中 b中文 u文 0 ");
        assert_eq!(tokens(""), "");
        assert_eq!(tokens("ASCII\0?!"), "0 ");
        let text = "ÄRGER İK 中文 が";
        assert_eq!(tokens(text), tokens(&search_key(text)));
    }

    #[test]
    fn only_one_or_two_all_cjk_scalars_use_the_short_index() {
        assert_eq!(phrase("书").as_deref(), Some("\"u书\""));
        assert_eq!(phrase("中文").as_deref(), Some("\"b中文\""));
        assert_eq!(phrase("が").as_deref(), Some("\"bが\""));
        for needle in ["", "a", "Ä", "a中", "中a", "中，", "中 文", "中文书"] {
            assert_eq!(phrase(needle), None, "{needle:?}");
        }
    }

    #[test]
    fn sql_function_handles_empty_null_and_wrong_types() {
        let index = crate::index::Index::open_in_memory().expect("open index");
        let (null, empty, tokens): (Option<String>, String, String) = index
            .conn
            .query_row(
                "SELECT search_cjk_tokens(NULL), search_cjk_tokens(''), search_cjk_tokens('中文')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("call scalar function");
        assert_eq!(null, None);
        assert_eq!(empty, "");
        assert_eq!(tokens, "u中 b中文 u文 ");
        assert!(index
            .conn
            .query_row("SELECT search_cjk_tokens(1)", [], |row| row
                .get::<_, String>(0))
            .is_err());
    }
}
