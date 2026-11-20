//! The simple-query DSL: what a query asks, and what an answer is.
//!
//! This module owns the query grammar and the values a query answers with. It owns nothing else: it
//! has no clock, no IO, no SQL, and no knowledge of pages beyond the spelling a query used. Parsing
//! a query produces a [`QueryExpr`], which is a tree of predicates; answering it is the executor's
//! work, and the executor is the only code that turns a tree into SQL (ADR-0003 places queries in
//! the domain for exactly this reason — the grammar and the meaning are one rule, and a second copy
//! of it inside the index would be a second version of the same vocabulary).
//!
//! # A failure is never an empty answer
//!
//! [`QueryExpr::parse`] either answers the tree the text denotes or a [`QueryError`]. There is no
//! third outcome: text the parser does not understand is never quietly treated as "matches
//! nothing", because an empty result reads as a fact about the graph — "no block matches" — and the
//! caller cannot tell that apart from "this build cannot run your query". The error variants are
//! split by what the caller can do about it:
//!
//! * [`QueryError::Syntax`] — the text is not a query: an unbalanced or empty form, an unterminated
//!   string, an argument count no form takes, a nesting depth past [`QueryExpr::MAX_DEPTH`].
//! * [`QueryError::Unsupported`] — the text is a query of a kind this build refuses: a Datalog
//!   vector or map, `sort-by`, `sample`, `all-page-tags`, `scheduled`, `deadline`, or a form name
//!   that is not part of the simple-query grammar.
//! * [`QueryError::InvalidArgument`] — the form is one this build runs, but one argument is not a
//!   value it denotes: a word that is not a task marker, a letter that is not a priority, a date
//!   operand that is not one of the spellings [`DateOperand`] lists.
//! * [`QueryError::UnresolvedDate`] — the form and the argument are both well formed, but the
//!   operand names nothing in this graph. Only an executor can observe that, so the parser never
//!   answers this variant; it is defined here because the executor reports it through the same type.
//!
//! # Two things this layer deliberately does not decide
//!
//! * A page name, a tag, a property value, and searched text are kept exactly as written. Folding a
//!   name to its page-name key, folding a property key, and folding text case are all answers that
//!   depend on the projection, so they belong to the executor. Keeping the spelling here means a
//!   query can be displayed back to the person who wrote it.
//! * A property key is *not* normalized. The rule for that is `document::Property::normalized_key`
//!   — lowercase with `_` folded to `-`, and no second spelling rule — and it lives in `document`
//!   because that is where a property key is first read. A lowercase rule copied into this module
//!   would be the second version of it, so the parser only strips the leading `:` a query writes and
//!   the executor applies the one rule.
//!
//! # Clock and size
//!
//! Nothing here reads a clock. [`QueryContext`] carries the day a relative date operand is measured
//! from and the number of results a caller will accept, and it carries no defaults: a library that
//! guessed "today" would answer differently on two machines, and one that guessed "no limit" would
//! make `(not …)` an unbounded transfer.

mod parse;

use std::fmt;

use super::{BlockMarker, BlockSelector, JournalDate, PageKind, Priority, TaskMarker};

/// One simple query, as a tree of predicates.
///
/// The tree is what the text denotes, not a rendering of it: `(not a b)` and `(not (and a b))` are
/// the same tree, and `[[page]]`, `#page`, and `(page-ref page)` are the same [`PageRef`](Self::PageRef).
/// The executor matches on this type exhaustively, so adding a variant is a compile error there
/// until the variant has a meaning — that is the point of a typed tree rather than a string.
///
/// Every variant is one predicate. `and` and `or` are the only binary shapes; there is no `xor`, no
/// `if`, and no negation normal form, because the DSL has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExpr {
    /// `(and p…)`: every child matches. A query of several top-level forms is an implicit `and`.
    ///
    /// The parser refuses an empty list, so a variant always holds at least one child. A one-child
    /// `and` is kept as written rather than collapsed into its child: the shape a caller wrote is
    /// worth keeping, and nothing downstream behaves differently for it.
    And(Vec<QueryExpr>),
    /// `(or p…)`: at least one child matches. Never empty, for the reason [`And`](Self::And) states.
    Or(Vec<QueryExpr>),
    /// `(not p…)`: none of the children matches.
    ///
    /// Several children are one predicate — "does not match all of these" — so the parser normalizes
    /// `(not a b)` to this variant holding `And([a, b])` and never produces a `Not` of an `Or`.
    Not(Box<QueryExpr>),
    /// `[[page]]`, `#tag`, or `(page-ref page)`: a reference to that page, or a block inside one.
    ///
    /// A tag is a page reference: Logseq writes `#x` and `[[x]]` for the same page, so one variant
    /// answers both. The name is the spelling the query used, and the executor compares it under
    /// the page-name key.
    PageRef(String),
    /// `(task TODO …)`: a block whose text leads with one of these markers.
    Task(Vec<TaskMarker>),
    /// `(priority A …)`: a block that carries one of these priorities.
    Priority(Vec<Priority>),
    /// `(property key)` or `(property key value)`: a block property, and optionally its value.
    ///
    /// `key` has the leading `:` a query may write removed and is otherwise as written — see this
    /// module's note on why normalization is the executor's. `value` is raw text: a property value
    /// can hold references and a comma-separated list, and interpreting that is the executor's
    /// question, not the grammar's.
    Property {
        /// Property key, without the leading `:`.
        key: String,
        /// Property value to compare against, or `None` for "the key exists".
        value: Option<String>,
    },
    /// `(page-property key)` or `(page-property key value)`: a property of the page a result sits on.
    PageProperty {
        /// Property key, without the leading `:`.
        key: String,
        /// Property value to compare against, or `None` for "the key exists".
        value: Option<String>,
    },
    /// `(page-tags tag…)`: a page carrying every one of these tags.
    PageTags(Vec<String>),
    /// `(namespace x)`: the page `x` and every page under it.
    Namespace(String),
    /// `(page x)`: the blocks of page `x`.
    ///
    /// This is a block-level predicate, not a page-level one: it names a page to take blocks from,
    /// not pages to report. [`is_page_level`](Self::is_page_level) says so, which is what makes a
    /// bare `(page x)` answer blocks.
    Page(String),
    /// `(journal)`: a block on a journal page.
    Journal,
    /// `(between start end)`: a block on a journal page whose date is in the closed interval.
    ///
    /// The operands are dates, not page names, even when the query spells one as `[[Dec 5th,
    /// 2020]]`: that spelling is a journal page title and only a graph can say which day it names,
    /// so the parser keeps it as [`DateOperand::JournalPage`] and the executor resolves it.
    Between(DateOperand, DateOperand),
    /// A bare word or a double-quoted string: text the block's own text must contain.
    Content(String),
}

impl QueryExpr {
    /// How deeply a query may nest.
    ///
    /// The parser refuses a form that would sit deeper than this, and it refuses it before building
    /// anything. The limit is a guard against a caller handing this library a thousand opening
    /// parentheses, not a statement about what a person may write: 32 levels of boolean nesting is
    /// already more than a query anyone reads.
    pub const MAX_DEPTH: usize = 32;

    /// Parse one simple query.
    ///
    /// The input is the DSL itself, not the surface that holds it: a `{{query …}}` macro body or a
    /// `#+BEGIN_QUERY` block has already been stripped by whoever read the file. Text that is not a
    /// query is an error and never an empty expression — see this module's note on failures.
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        parse::parse(input)
    }

    /// Whether this expression answers pages instead of blocks.
    ///
    /// The answer depends on nothing but the shape, which is why it is a method on the tree and not
    /// a decision the executor makes per leaf: only page properties, page tags, and namespaces are
    /// questions about a page. `and`, `or`, and `not` answer pages when everything they combine
    /// does. Everything else — including [`Page`](Self::Page), which names a page in order to take
    /// its blocks — answers blocks.
    ///
    /// This is the whole rule: an expression that is page-level is answered with a list of pages,
    /// and one that is not is answered with a list of blocks. A caller therefore reads the kind of
    /// answer off the query, and never has to guess from a count.
    pub fn is_page_level(&self) -> bool {
        match self {
            Self::PageProperty { .. } | Self::PageTags(_) | Self::Namespace(_) => true,
            Self::And(children) | Self::Or(children) => {
                children.iter().all(QueryExpr::is_page_level)
            }
            Self::Not(inner) => inner.is_page_level(),
            Self::PageRef(_)
            | Self::Task(_)
            | Self::Priority(_)
            | Self::Property { .. }
            | Self::Page(_)
            | Self::Journal
            | Self::Between(..)
            | Self::Content(_) => false,
        }
    }
}

/// One side of a `(between start end)` interval.
///
/// The two spellings that name a day without needing the graph are folded here into the arithmetic
/// the executor runs: a week is exactly seven days and a year is exactly twelve calendar months, so
/// `+2w` is [`OffsetDays`](Self::OffsetDays)(14) and `+1y` is [`OffsetMonths`](Self::OffsetMonths)(12).
/// Folding once, in the parser, is what keeps the executor from carrying a unit around.
///
/// `today`, `yesterday`, and `tomorrow` are offsets too — no variant is named after them, because
/// they are the same arithmetic measured from [`QueryContext::today`]. Keeping the spelling out of
/// the tree is what makes the tree mean the same thing on any day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateOperand {
    /// A journal-day integer (`YYYYMMDD`), independent of existing pages.
    Day(JournalDate),
    /// `today` and `now`: the context's day, unchanged.
    ///
    /// `now` is the same operand as `today`: Logseq's journals hold days, so a time of day names the
    /// day it falls in, and the context is the only thing that knows which day that is.
    Today,
    /// A calendar-day offset: `+3d`, `-1d`, `+2w`, `yesterday` (`-1`), `tomorrow` (`+1`).
    OffsetDays(i64),
    /// A calendar-month offset: `+1m`, `-2m`, `+1y`, `-1y`.
    ///
    /// It is months, not days, because a month is not a fixed number of them: the executor clamps to
    /// the last day of a shorter month, which is `JournalDate::checked_add_months`.
    OffsetMonths(i64),
    /// `[[<journal page name>]]`: a day spelled as the title of a journal page.
    ///
    /// Every absolute spelling belongs here — `[[2020-12-05]]`, `[[Dec 5th, 2020]]`,
    /// `[[2025-09-17 Wednesday]]` — because the graph is the only thing that knows which day a given
    /// title denotes: the title format is configuration, and re-implementing it here would be a
    /// second format rule. The executor resolves the name against the index; a name that is no
    /// journal page of this graph is [`QueryError::UnresolvedDate`].
    JournalPage(String),
}

/// What a query is answered against: the day relative operands count from, and how much to answer.
///
/// Both fields are required, and neither has a default. A library that read the clock would answer
/// differently on two machines and could not be tested by a fixture; a library that assumed "no
/// limit" would let a `(not …)` predicate transfer a whole graph. The boundary owns both decisions:
/// the CLI or the HTTP layer takes the local day and its own default limit, and passes them here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryContext {
    today: JournalDate,
    limit: u32,
}

impl QueryContext {
    /// The day `today`, `yesterday`, `tomorrow`, and every offset are measured from, and the number
    /// of results the caller will accept.
    ///
    /// A `limit` of zero is not an error: it asks for nothing, which is what a caller that wants
    /// only [`QueryResult::truncated`] asked for. The executor truncates after ordering, so a
    /// non-zero limit is always the first N results of the order the answer states.
    pub fn new(today: JournalDate, limit: u32) -> Self {
        Self { today, limit }
    }

    /// The day relative operands are measured from.
    pub fn today(self) -> JournalDate {
        self.today
    }

    /// The most results the caller will accept.
    pub fn limit(self) -> u32 {
        self.limit
    }
}

/// What a query answered.
///
/// The outcome's kind follows [`QueryExpr::is_page_level`]: a page-level expression answers pages,
/// every other expression answers blocks. `truncated` says whether the caller's limit cut the answer
/// short, so an empty list and a full list are never the only two things a caller can conclude —
/// "there are more" is its own fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    pub(crate) outcome: QueryOutcome,
    pub(crate) truncated: bool,
}

impl QueryResult {
    /// The blocks or pages that matched.
    pub fn outcome(&self) -> &QueryOutcome {
        &self.outcome
    }

    /// Whether at least one match was dropped because of [`QueryContext::limit`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// The two shapes an answer can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome {
    /// Matches of a block-level query.
    Blocks(Vec<QueryBlock>),
    /// Matches of a page-level query.
    Pages(Vec<QueryPage>),
}

/// One block a query matched.
///
/// It carries what a result list shows and what a caller needs to name the block again, and nothing
/// more: the facts a query filtered on are handed back so a caller can show why a block matched
/// (`task_marker`, `priority`) without a second read per block. The block's children are not
/// attached — a query answers blocks, not subtrees.
///
/// Fields are visible inside the crate because the executor is the only builder of this type; a
/// caller reads it through the accessors below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryBlock {
    pub(crate) page: String,
    pub(crate) preorder: u32,
    pub(crate) depth: u32,
    pub(crate) text: String,
    pub(crate) marker: BlockMarker,
    pub(crate) task_marker: Option<TaskMarker>,
    pub(crate) priority: Option<Priority>,
    pub(crate) selector: BlockSelector,
}

impl QueryBlock {
    /// Name of the page the block belongs to, in the spelling the projection stored.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Depth-first position of the block within its page, starting at zero.
    ///
    /// A position, not identity: it orders one page's blocks at one revision (ADR-0005). Use
    /// [`selector`](Self::selector) to name the block again after this answer.
    pub fn preorder(&self) -> u32 {
        self.preorder
    }

    /// Nesting depth of the block, starting at zero for a page's top-level blocks.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The block's own text, as the projection holds it: property lines and task marker included,
    /// children not.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The structural marker the block's line opened with: a bullet or a heading.
    pub fn marker(&self) -> BlockMarker {
        self.marker
    }

    /// The task marker the block's text leads with, when it leads with one.
    ///
    /// This is a content fact read out of [`text`](Self::text), not the structural
    /// [`marker`](Self::marker): a block can be a bullet and a task, and most blocks are only the
    /// first.
    pub fn task_marker(&self) -> Option<TaskMarker> {
        self.task_marker
    }

    /// The priority the block carries, when it carries one.
    pub fn priority(&self) -> Option<Priority> {
        self.priority
    }

    /// The selector that names this block, durable or ephemeral (ADR-0005, ADR-0012).
    ///
    /// A query answers "which blocks match", so the identity it hands out is the one the caller can
    /// pass straight back to a write, not the identity the query was asked with.
    pub fn selector(&self) -> &BlockSelector {
        &self.selector
    }
}

/// One page a query matched.
///
/// A page-level query is a question about pages, so its answer is a page and what kind of page it is:
/// the kind and the journal day are already known to the index, and a caller filtering journals out
/// of its own listing should not have to read each page again to find out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPage {
    pub(crate) name: String,
    pub(crate) kind: PageKind,
    pub(crate) journal_date: Option<JournalDate>,
}

impl QueryPage {
    /// The page's name in the spelling the projection stored.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the page is a journal or an ordinary page.
    pub fn kind(&self) -> PageKind {
        self.kind
    }

    /// The day the page is a journal for, and `None` for an ordinary page.
    pub fn journal_date(&self) -> Option<JournalDate> {
        self.journal_date
    }
}

/// Why a query could not be parsed, or could not be answered.
///
/// The four variants split by what a caller can do about the failure, which is the only split worth
/// having: a syntax error is the query's own fault and names the byte it went wrong at, an
/// unsupported form is this build's limit and names the form, an invalid argument is a value the
/// form does not denote, and an unresolved date is a well-formed operand that names no page of this
/// graph. None of them means "no match" — that is an empty [`QueryResult`], a different answer
/// entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// The text is not a well-formed query.
    Syntax {
        /// Byte offset into the query text where the problem was noticed.
        ///
        /// It is where the parser gave up, not necessarily where a person would start counting: an
        /// unclosed form reports the position of its opening parenthesis.
        position: usize,
        /// What was expected and what was found.
        detail: String,
    },
    /// The text is a query of a kind this build does not run.
    Unsupported {
        /// The form as it appeared, so the caller can see what was refused.
        form: String,
    },
    /// A form this build runs was given an argument it does not denote.
    InvalidArgument {
        /// Form name, lowercased.
        form: String,
        /// The argument as written.
        argument: String,
        /// What the form accepts instead.
        detail: String,
    },
    /// A date operand is well formed but names no journal page of this graph.
    ///
    /// Only an executor can observe this, so [`QueryExpr::parse`] never answers it; it is here
    /// because the failure travels to the caller through the same type as the parse failures.
    UnresolvedDate {
        /// The operand as written.
        operand: String,
        /// Why it did not resolve.
        detail: String,
    },
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { position, detail } => write!(
                formatter,
                "this query is not well formed at byte {position}: {detail}"
            ),
            Self::Unsupported { form } => write!(
                formatter,
                "`{form}` is not part of the simple-query grammar this build runs, so the query was \
                 refused rather than answered with no results"
            ),
            Self::InvalidArgument {
                form,
                argument,
                detail,
            } => write!(
                formatter,
                "`{argument}` is not an argument `({form} …)` accepts: {detail}"
            ),
            Self::UnresolvedDate { operand, detail } => write!(
                formatter,
                "the date `{operand}` does not name a journal page of this graph: {detail}"
            ),
        }
    }
}

impl std::error::Error for QueryError {}
