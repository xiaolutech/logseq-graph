//! A page a name lookup can jump to.
//!
//! A lookup answers a list, not one page: a name can be a page's own name or one of the values in
//! its `alias::` property, and a shared alias names several pages. Ordering the list is the read's
//! job; choosing which entry to jump to is the caller's.

/// How one candidate answered the lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageMatch {
    /// The query matched the page's own name.
    Name,
    /// The query matched one value of the page's `alias::` property.
    Alias,
}

/// One page a name lookup can jump to.
///
/// `name` is the file page's name as written, so it is what a page read accepts. `via` says which
/// spelling matched; when it is [`PageMatch::Alias`] the `alias` is the value that matched, so a
/// surface can say "this name is an alias of that page" instead of pretending the two are one
/// entity. A page appears once: a hit on its own name outranks a hit on an alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCandidate {
    name: String,
    via: PageMatch,
    alias: Option<String>,
}

impl PageCandidate {
    pub(crate) fn new(name: String, via: PageMatch, alias: Option<String>) -> Self {
        Self { name, via, alias }
    }

    /// The file page's name as written, the value a page read accepts.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Which spelling matched the query.
    pub fn via(&self) -> PageMatch {
        self.via
    }

    /// The `alias::` value that matched, present exactly when `via` is [`PageMatch::Alias`].
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
}
