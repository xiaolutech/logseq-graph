//! Page names the graph's owner pinned in Logseq's sidebar.
//!
//! The list lives in `logseq/config.edn` and is configuration, not derived state: a name is
//! reported whether or not a page holds it. [`Favorite::exists`] is the separate fact a display
//! needs, because a pinned page that was deleted, renamed, or never created is a real state of the
//! person's configuration. Dropping such a name here would edit that configuration to match the
//! current graph, silently and only on this side.

/// One entry of `:favorites`, and whether a page of that name exists at this revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    name: String,
    exists: bool,
}

impl Favorite {
    pub(crate) fn new(name: String, exists: bool) -> Self {
        Self { name, exists }
    }

    /// The page name as the configuration writes it, not as a file name spells it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the derived model holds a page this name folds to, at the revision of the read.
    ///
    /// The comparison is the page-name rule ([`page_name_key`](super::page_name_key)), so a pinned
    /// `inbox` is found when the page is stored as `Inbox`.
    pub fn exists(&self) -> bool {
        self.exists
    }
}
