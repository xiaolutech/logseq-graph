//! Which kind of page a file holds.

/// The kind of page a source file holds.
///
/// A kind is a fact about a page, not about a directory: it is part of what identifies the page a
/// caller asked for, and a journal answers `name` with a formatted date title while a page answers
/// it with its decoded file name. The directory the file currently sits in is what discovery uses
/// to decide the kind, which is why the value travels through `source` and `index` — but it belongs
/// here for the same reason [`BlockMarker`](super::BlockMarker) does: a layer that parses or stores
/// a page must not define a second vocabulary for what it found (ADR-0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageKind {
    /// A page: a file under `pages/`, named by its decoded file name.
    Page,
    /// A journal: a file under `journals/`, named by the date in its file name.
    Journal,
}

impl PageKind {
    /// Every kind, in the order a listing reports them.
    pub const ALL: [PageKind; 2] = [PageKind::Page, PageKind::Journal];

    /// The spelling this kind is stored and transmitted as.
    ///
    /// The index has two columns that hold it (`file_baseline.page_kind` and `pages.kind`) and a
    /// `CHECK` constraint in each, so the spelling is one rule rather than a literal repeated in
    /// SQL. A schema test keeps the constraint and this method in step, because SQLite cannot call
    /// Rust.
    pub fn as_str(self) -> &'static str {
        match self {
            PageKind::Page => "page",
            PageKind::Journal => "journal",
        }
    }

    /// The kind a stored spelling denotes, or `None` when it is not one this build writes.
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "page" => Some(PageKind::Page),
            "journal" => Some(PageKind::Journal),
            _ => None,
        }
    }
}

impl std::fmt::Display for PageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_stored_spelling() {
        for kind in PageKind::ALL {
            assert_eq!(PageKind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }

    #[test]
    fn the_stored_spellings_are_the_ones_the_schema_checks() {
        // `pages.kind` CHECKs membership in `('page', 'journal')`. SQLite cannot call Rust, so this is
        // what keeps the literal and `as_str` from drifting apart.
        assert_eq!(PageKind::ALL.map(PageKind::as_str), ["page", "journal"]);
    }

    #[test]
    fn a_spelling_this_build_does_not_write_is_not_guessed() {
        assert_eq!(PageKind::parse(""), None);
        assert_eq!(PageKind::parse("Page"), None);
        assert_eq!(PageKind::parse("daily"), None);
    }
}
