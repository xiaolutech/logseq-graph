//! Method identity and request-shape rules for the RPC adapter.

use super::writes::WriteMethod;

/// Search hits answered when the request names more than this.
pub(super) const MAX_SEARCH_LIMIT: u32 = 1000;

/// Query matches answered when the request names more than this.
pub(super) const MAX_QUERY_LIMIT: u32 = 1000;

const FRESHNESS_ONLY: &[&str] = &["freshness"];
/// `Editor.getBlock` takes the official `{includeChildren}` object beside its selector; this
/// surface answers one block and ignores the flag, but it accepts it rather than refusing a client
/// that sends the official argument.
const BLOCK_OPTIONS: &[&str] = &["freshness", "includeChildren"];
const SEARCH_OPTIONS: &[&str] = &["freshness", "limit"];
const QUERY_OPTIONS: &[&str] = &["freshness", "limit", "today"];
const NO_OPTIONS: &[&str] = &[];

/// The reads and writes this surface answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    GetCurrentGraph,
    GetAllPages,
    GetFavorites,
    GetPage,
    GetPageBlocksTree,
    GetPageLinkedReferences,
    GetAllTags,
    GetPagesWithTag,
    Search,
    GetPageVersion,
    GetPageSnapshot,
    Query,
    GetBlock,
    /// `logseq.App.checkCurrentIsDBGraph`: the official client's probe for the DB edition. This
    /// projection is file-first (ADR-0001), so the answer is always `false`, without a graph read.
    CheckCurrentIsDBGraph,
    /// `logseq.App.getAppInfo`: the official `{version, supportDb}` answer, with this build's
    /// version and `supportDb: false` because the file edition has no DB (ADR-0001).
    AppInfo,
    /// `logseq.showMsg`: the official client's toast. There is no GUI here (ADR-0003), so the call
    /// is accepted and dropped and the answer is a synthetic message id of the official shape.
    ShowMsg,
    /// `logseq.DB.q`: the official spelling of the simple-query read, answered with the official
    /// array shape; `logseq.Graph.query` keeps this repository's object shape (ADR-0013).
    DbQ,
    /// `logseq.App.getUserConfigs`: the official client's configuration read. A file-first
    /// projection answers the facts the graph files state — the graph identity, the format this
    /// surface serves, and the journal page title format — and omits the app-state keys.
    UserConfigs,
    /// `logseq.Editor.getCurrentPage`: the page the editor has open. There is no editor here
    /// (ADR-0003), so the official "no current page" answer, `null`, is the honest one.
    GetCurrentPage,
    Write(WriteMethod),
}

#[derive(Clone, Copy)]
struct Spec {
    method: Method,
    canonical: &'static str,
    positional: usize,
    alternate_positional: Option<usize>,
    allowed_options: &'static [&'static str],
    limit_max: Option<u32>,
}

impl Spec {
    const fn read(
        method: Method,
        canonical: &'static str,
        positional: usize,
        allowed_options: &'static [&'static str],
        limit_max: Option<u32>,
    ) -> Self {
        Self {
            method,
            canonical,
            positional,
            alternate_positional: None,
            allowed_options,
            limit_max,
        }
    }

    const fn write(method: WriteMethod, canonical: &'static str, positional: usize) -> Self {
        Self {
            method: Method::Write(method),
            canonical,
            positional,
            alternate_positional: None,
            allowed_options: NO_OPTIONS,
            limit_max: None,
        }
    }

    const fn write_with_alternate(
        method: WriteMethod,
        canonical: &'static str,
        positional: usize,
        alternate_positional: usize,
    ) -> Self {
        Self {
            alternate_positional: Some(alternate_positional),
            ..Self::write(method, canonical, positional)
        }
    }

    fn leaf(&self) -> &'static str {
        self.canonical.rsplit('.').next().unwrap_or(self.canonical)
    }

    fn matches(&self, name: &str, lookup: &str) -> bool {
        match self.method {
            // A write resolves only under its exact canonical name: the `logseq.Graph` namespace is
            // this surface's own contract (ADR-0014), and the official leaf rule is a read alias,
            // never a second way to reach a write.
            Method::Write(_) => name == self.canonical,
            _ => snake_leaf(self.leaf()) == lookup,
        }
    }
}

const SPECS: &[Spec] = &[
    Spec::read(
        Method::GetCurrentGraph,
        "logseq.Editor.getCurrentGraph",
        0,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetAllPages,
        "logseq.Editor.getAllPages",
        0,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetFavorites,
        "logseq.Graph.getFavorites",
        0,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetPage,
        "logseq.Editor.getPage",
        1,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetPageBlocksTree,
        "logseq.Editor.getPageBlocksTree",
        1,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetPageLinkedReferences,
        "logseq.Editor.getPageLinkedReferences",
        1,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetAllTags,
        "logseq.Graph.getAllTags",
        0,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::GetPagesWithTag,
        "logseq.Graph.getPagesWithTag",
        1,
        FRESHNESS_ONLY,
        None,
    ),
    Spec::read(
        Method::Search,
        "logseq.App.search",
        1,
        SEARCH_OPTIONS,
        Some(MAX_SEARCH_LIMIT),
    ),
    Spec::read(
        Method::GetPageVersion,
        "logseq.Graph.getPageVersion",
        1,
        NO_OPTIONS,
        None,
    ),
    Spec::read(
        Method::GetPageSnapshot,
        "logseq.Graph.getPageSnapshot",
        1,
        NO_OPTIONS,
        None,
    ),
    Spec::read(
        Method::Query,
        "logseq.Graph.query",
        1,
        QUERY_OPTIONS,
        Some(MAX_QUERY_LIMIT),
    ),
    Spec::read(
        Method::GetBlock,
        "logseq.Editor.getBlock",
        1,
        BLOCK_OPTIONS,
        None,
    ),
    // No freshness policy: the answer is a fact about this implementation, not about the graph.
    Spec::read(
        Method::CheckCurrentIsDBGraph,
        "logseq.App.checkCurrentIsDBGraph",
        0,
        NO_OPTIONS,
        None,
    ),
    // The official client asks this for the version it shows when a connection succeeds.
    Spec::read(
        Method::AppInfo,
        "logseq.App.getAppInfo",
        0,
        NO_OPTIONS,
        None,
    ),
    // The official message takes `msg` and an optional status/options argument; this surface
    // accepts the message alone, because there is no toast to configure.
    Spec::read(Method::ShowMsg, "logseq.showMsg", 1, NO_OPTIONS, None),
    // The official name for the same simple-query read `logseq.Graph.query` answers. It takes the
    // same options and limit and differs only in the shape it states.
    Spec::read(
        Method::DbQ,
        "logseq.DB.q",
        1,
        QUERY_OPTIONS,
        Some(MAX_QUERY_LIMIT),
    ),
    // The official client's configuration read. A file-first projection answers the facts the graph
    // files state; the app-state keys (theme, language, start of week) are documented differences.
    Spec::read(
        Method::UserConfigs,
        "logseq.App.getUserConfigs",
        0,
        NO_OPTIONS,
        None,
    ),
    // The page the editor has open. There is no editor (ADR-0003), so the official "no current
    // page" answer, `null`, is the honest one and the client's feature degrades instead of failing
    // with a method-not-found it would have to parse.
    Spec::read(
        Method::GetCurrentPage,
        "logseq.Editor.getCurrentPage",
        0,
        NO_OPTIONS,
        None,
    ),
    Spec::write(WriteMethod::CreatePage, "logseq.Graph.createPage", 1),
    Spec::write(WriteMethod::CreateJournal, "logseq.Graph.createJournal", 1),
    Spec::write(WriteMethod::AppendBlock, "logseq.Graph.appendBlock", 2),
    Spec::write(WriteMethod::InsertBlock, "logseq.Graph.insertBlock", 2),
    Spec::write(WriteMethod::InsertBatch, "logseq.Graph.insertBatch", 2),
    Spec::write(
        WriteMethod::PrependBlockInPage,
        "logseq.Graph.prependBlockInPage",
        2,
    ),
    Spec::write(WriteMethod::UpdateBlock, "logseq.Graph.updateBlock", 2),
    Spec::write(WriteMethod::RemoveBlock, "logseq.Graph.removeBlock", 1),
    Spec::write(
        WriteMethod::RemoveBlockRange,
        "logseq.Graph.removeBlockRange",
        2,
    ),
    Spec::write(WriteMethod::EnsureBlockId, "logseq.Graph.ensureBlockId", 1),
    Spec::write_with_alternate(WriteMethod::MoveBlock, "logseq.Graph.moveBlock", 2, 3),
    Spec::write(
        WriteMethod::UpsertBlockProperty,
        "logseq.Graph.upsertBlockProperty",
        3,
    ),
    Spec::write(
        WriteMethod::RemoveBlockProperty,
        "logseq.Graph.removeBlockProperty",
        2,
    ),
    Spec::write(
        WriteMethod::SetBlockCollapsed,
        "logseq.Graph.setBlockCollapsed",
        2,
    ),
    Spec::write(WriteMethod::DeletePage, "logseq.Graph.deletePage", 1),
    Spec::write(WriteMethod::CommitPage, "logseq.Graph.commitPage", 3),
];

impl Method {
    #[expect(
        clippy::expect_used,
        reason = "every Method variant is registered in this private module; a missing spec is a \
                  programming error, not request input"
    )]
    fn spec(self) -> &'static Spec {
        SPECS
            .iter()
            .find(|spec| spec.method == self)
            .expect("every served method has one spec")
    }

    /// The official-shaped name an error and the documentation state.
    pub(super) fn canonical(self) -> &'static str {
        self.spec().canonical
    }

    /// How many positional arguments the method's canonical form takes.
    pub(super) fn positional(self) -> usize {
        self.spec().positional
    }

    /// The other positional count accepted by a write's compatibility form.
    pub(super) fn alternate_positional(self) -> Option<usize> {
        self.spec().alternate_positional
    }

    /// The option keys the method accepts. Empty means it declares no policy at all.
    pub(super) fn allowed_options(self) -> &'static [&'static str] {
        self.spec().allowed_options
    }

    /// The ceiling a `limit` option must not exceed, when the method accepts one.
    pub(super) fn limit_max(self) -> Option<u32> {
        self.spec().limit_max
    }
}

/// The leaf a `logseq.` name is looked up under: its last dot segment with every uppercase letter
/// becoming `_` plus the lowercase form — `getPage` is `get_page` and `checkCurrentIsDBGraph` is
/// `check_current_is_d_b_graph`. A name without the `logseq.` prefix is looked up as written.
/// This is the rule the official server applies, measured from its `MethodNotExist` answers.
fn snake_leaf(leaf: &str) -> String {
    let mut out = String::with_capacity(leaf.len() + 4);
    for character in leaf.chars() {
        if character.is_uppercase() {
            out.push('_');
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// Resolve a method name the official way, and an unknown name into the spelling it was looked
/// up under, which is what the `MethodNotExist` message states.
pub(super) fn resolve(name: &str) -> Result<Method, String> {
    let lookup = match name.strip_prefix("logseq.") {
        Some(rest) => snake_leaf(rest.rsplit('.').next().unwrap_or(rest)),
        None => name.to_owned(),
    };
    SPECS
        .iter()
        .find(|spec| spec.matches(name, &lookup))
        .map(|spec| spec.method)
        .ok_or(lookup)
}

pub(super) fn read_names() -> Vec<&'static str> {
    SPECS
        .iter()
        .filter(|spec| !matches!(spec.method, Method::Write(_)))
        .map(|spec| spec.canonical)
        .collect()
}

pub(super) fn write_names() -> Vec<&'static str> {
    SPECS
        .iter()
        .filter(|spec| matches!(spec.method, Method::Write(_)))
        .map(|spec| spec.canonical)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_resolve_the_official_way() {
        // Every accepted spelling below was measured answered by the official server, and every
        // refused one measured `MethodNotExist`: a `logseq.` name resolves by the snake form of
        // its leaf, a bare name as written.
        let resolved = [
            "logseq.Editor.getPage",
            "logseq.Editor.get_page",
            "logseq.getPage",
            "logseq.Editor.getBlock",
            "logseq.Editor.get_block",
            "logseq.Editor.getCurrentGraph",
            "logseq.App.get_current_graph",
            "logseq.App.checkCurrentIsDBGraph",
            "logseq.App.check_current_is_d_b_graph",
            "logseq.App.getUserConfigs",
            "logseq.App.get_user_configs",
            "logseq.App.search",
            "logseq.DB.q",
            "logseq.showMsg",
            "get_block",
            "get_page",
            "get_all_pages",
            "get_current_graph",
            "get_page_blocks_tree",
            "search",
            "get_user_configs",
            "q",
            "show_msg",
        ];
        for name in resolved {
            let method = resolve(name)
                .unwrap_or_else(|lookup| panic!("{name} resolves ({lookup} was looked up)"));
            assert!(!matches!(method, Method::Write(_)), "{name}");
        }
        // A write resolves only under its exact canonical name.
        assert!(resolve("logseq.Graph.appendBlock").is_ok());
        assert!(resolve("logseq.Graph.createPage").is_ok());

        // The snake leaf is what the lookup used, and the same string the error states.
        for (name, lookup) in [
            ("getPage", "getPage"),
            ("GetPage", "GetPage"),
            ("get-page", "get-page"),
            ("logseq.Editor.getpage", "getpage"),
            (
                "logseq.App.checkCurrentIsDbGraph",
                "check_current_is_db_graph",
            ),
            ("logseq.Editor.nope", "nope"),
            ("logseq.Graph.append_block", "append_block"),
            ("append_block", "append_block"),
            ("logseq.Editor.", ""),
        ] {
            assert_eq!(resolve(name), Err(lookup.to_owned()), "{name}");
        }
    }
}
