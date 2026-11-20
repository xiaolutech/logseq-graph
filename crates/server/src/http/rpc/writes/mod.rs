//! Translation between `logseq.Graph` write calls and the library's write facade.

mod answer;
mod batch;
mod blocks;
mod error;
mod journals;
mod moves;
mod pages;
mod params;
mod properties;

use super::error::RequestError;
use super::state::Served;
pub(crate) use error::{error_code, error_data, error_status};
use logseq_graph::domain::{BlockSelector, JournalDate, PageOperation};
use logseq_graph::engine::{Graph, MoveTarget};
use params::invalid;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteMethod {
    CreatePage,
    CreateJournal,
    AppendBlock,
    InsertBlock,
    InsertBatch,
    PrependBlockInPage,
    UpdateBlock,
    RemoveBlock,
    RemoveBlockRange,
    EnsureBlockId,
    MoveBlock,
    UpsertBlockProperty,
    RemoveBlockProperty,
    SetBlockCollapsed,
    DeletePage,
    CommitPage,
}

impl WriteMethod {
    fn canonical(self) -> &'static str {
        super::methods::Method::Write(self).canonical()
    }

    fn arity(self) -> usize {
        super::methods::Method::Write(self).positional()
    }

    fn alternate_arity(self) -> Option<usize> {
        super::methods::Method::Write(self).alternate_positional()
    }
}

#[derive(Debug)]
pub(crate) enum WriteRequest {
    CreatePage {
        name: String,
    },
    CreateJournal {
        date: JournalDate,
    },
    AppendBlock {
        page: String,
        text: String,
    },
    InsertBlock {
        target: MoveTarget,
        text: String,
    },
    InsertBatch {
        target: MoveTarget,
        texts: Vec<String>,
    },
    PrependBlockInPage {
        page: String,
        text: String,
    },
    UpdateBlock {
        selector: BlockSelector,
        text: String,
    },
    RemoveBlock {
        selector: BlockSelector,
    },
    RemoveBlockRange {
        first: BlockSelector,
        last: BlockSelector,
    },
    EnsureBlockId {
        selector: BlockSelector,
    },
    MoveBlock {
        selector: BlockSelector,
        target: MoveTarget,
    },
    UpsertBlockProperty {
        selector: BlockSelector,
        key: String,
        value: String,
    },
    RemoveBlockProperty {
        selector: BlockSelector,
        key: String,
    },
    SetBlockCollapsed {
        selector: BlockSelector,
        collapsed: bool,
    },
    DeletePage {
        name: String,
    },
    CommitPage {
        page: String,
        base_version: String,
        operations: Vec<PageOperation>,
    },
}

pub(crate) fn parse(method: WriteMethod, args: Vec<Value>) -> Result<WriteRequest, RequestError> {
    if let Some(index) = args
        .iter()
        .position(|argument| argument.get("freshness").is_some())
    {
        return Err(invalid(
            method,
            "writes do not accept freshness; their source-file guards belong to the library",
            json!({"index": index, "parameter": "freshness", "allowed": []}),
        ));
    }
    let valid_arity = args.len() == method.arity() || Some(args.len()) == method.alternate_arity();
    if !valid_arity {
        return Err(invalid(
            method,
            format!(
                "takes {} positional arguments, not {}",
                method.arity(),
                args.len()
            ),
            json!({"expected": method.arity(), "actual": args.len()}),
        ));
    }
    match method {
        WriteMethod::CreatePage => pages::parse_create_page(method, &args),
        WriteMethod::CreateJournal => journals::parse_create_journal(method, &args),
        WriteMethod::AppendBlock => pages::parse_append_block(method, &args),
        WriteMethod::InsertBlock => blocks::parse_insert_block(method, &args),
        WriteMethod::InsertBatch => batch::parse_insert_batch(method, &args),
        WriteMethod::PrependBlockInPage => pages::parse_prepend_block_in_page(method, &args),
        WriteMethod::UpdateBlock => blocks::parse_update_block(method, &args),
        WriteMethod::RemoveBlock => blocks::parse_remove_block(method, &args),
        WriteMethod::RemoveBlockRange => Ok(WriteRequest::RemoveBlockRange {
            first: params::selector(method, &args[0], 0)?,
            last: params::selector(method, &args[1], 1)?,
        }),
        WriteMethod::EnsureBlockId => Ok(WriteRequest::EnsureBlockId {
            selector: params::selector(method, &args[0], 0)?,
        }),
        WriteMethod::MoveBlock => moves::parse_move_block(method, &args),
        WriteMethod::UpsertBlockProperty => properties::parse_upsert_block_property(method, &args),
        WriteMethod::RemoveBlockProperty => properties::parse_remove_block_property(method, &args),
        WriteMethod::SetBlockCollapsed => blocks::parse_set_block_collapsed(method, &args),
        WriteMethod::DeletePage => pages::parse_delete_page(method, &args),
        WriteMethod::CommitPage => batch::parse_commit_page(method, &args),
    }
}

pub(crate) fn handle(served: &Served, request: &WriteRequest) -> Result<Value, RequestError> {
    served.with_graph(|graph| write(graph, request))
}

fn write(graph: &mut Graph, request: &WriteRequest) -> Result<Value, RequestError> {
    let answer = match request {
        WriteRequest::CreatePage { name } => pages::create_page(graph, name),
        WriteRequest::CreateJournal { date } => journals::create_journal(graph, date),
        WriteRequest::AppendBlock { page, text } => pages::append_block(graph, page, text),
        WriteRequest::InsertBlock { target, text } => blocks::insert_block(graph, target, text),
        WriteRequest::InsertBatch { target, texts } => batch::insert_batch(graph, target, texts),
        WriteRequest::PrependBlockInPage { page, text } => {
            pages::prepend_block_in_page(graph, page, text)
        }
        WriteRequest::UpdateBlock { selector, text } => blocks::update_block(graph, selector, text),
        WriteRequest::RemoveBlock { selector } => blocks::remove_block(graph, selector),
        WriteRequest::RemoveBlockRange { first, last } => graph
            .remove_block_range(first, last)
            .map(answer::page_result),
        WriteRequest::EnsureBlockId { selector } => properties::ensure_block_id(graph, selector),
        WriteRequest::MoveBlock { selector, target } => moves::move_block(graph, selector, target),
        WriteRequest::UpsertBlockProperty {
            selector,
            key,
            value,
        } => properties::upsert_block_property(graph, selector, key, value),
        WriteRequest::RemoveBlockProperty { selector, key } => {
            properties::remove_block_property(graph, selector, key)
        }
        WriteRequest::SetBlockCollapsed {
            selector,
            collapsed,
        } => blocks::set_block_collapsed(graph, selector, collapsed),
        WriteRequest::DeletePage { name } => pages::delete_page(graph, name),
        WriteRequest::CommitPage {
            page,
            base_version,
            operations,
        } => batch::commit_page(graph, page, base_version, operations),
    };
    answer.map_err(RequestError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_arguments_are_typed_and_never_carry_a_read_policy() {
        let error = parse(
            WriteMethod::AppendBlock,
            vec![
                json!("alpha"),
                json!("text"),
                json!({"freshness": "verify"}),
            ],
        )
        .expect_err("writes have no freshness argument");
        assert_eq!(error.code(), -32602);
        assert_eq!(error.data()["kind"], "invalid-write-params");

        let request = parse(
            WriteMethod::InsertBlock,
            vec![
                json!({"lastChildOf": "ephemeral:01234567-89ab-cdef-0123-456789abcdef"}),
                json!("new"),
            ],
        )
        .expect("typed insert");
        let WriteRequest::InsertBlock {
            target: MoveTarget::LastChildOf(BlockSelector::Ephemeral(id)),
            text,
        } = request
        else {
            panic!("wrong request");
        };
        assert_eq!(id.as_str(), "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(text, "new");
    }

    #[test]
    fn every_served_write_has_a_typed_argument_parser() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        let inputs = [
            (WriteMethod::CreatePage, vec![json!("new")]),
            (WriteMethod::CreateJournal, vec![json!("2026-11-12")]),
            (
                WriteMethod::AppendBlock,
                vec![json!("alpha"), json!("text")],
            ),
            (
                WriteMethod::InsertBlock,
                vec![json!({"after": id}), json!("text")],
            ),
            (
                WriteMethod::InsertBatch,
                vec![json!({"before": id}), json!(["one", "two"])],
            ),
            (
                WriteMethod::PrependBlockInPage,
                vec![json!("alpha"), json!("text")],
            ),
            (WriteMethod::UpdateBlock, vec![json!(id), json!("text")]),
            (WriteMethod::RemoveBlock, vec![json!(id)]),
            (WriteMethod::RemoveBlockRange, vec![json!(id), json!(id)]),
            (WriteMethod::EnsureBlockId, vec![json!(id)]),
            (
                WriteMethod::MoveBlock,
                vec![json!(id), json!({"firstChildOf": id})],
            ),
            (
                WriteMethod::MoveBlock,
                vec![json!(id), json!("last_child_of"), json!(id)],
            ),
            (
                WriteMethod::UpsertBlockProperty,
                vec![json!(id), json!("key"), json!("value")],
            ),
            (
                WriteMethod::RemoveBlockProperty,
                vec![json!(id), json!("key")],
            ),
            (WriteMethod::SetBlockCollapsed, vec![json!(id), json!(true)]),
            (WriteMethod::DeletePage, vec![json!("alpha")]),
            (
                WriteMethod::CommitPage,
                vec![json!("alpha"), json!("version"), json!([])],
            ),
        ];
        for (method, args) in inputs {
            parse(method, args)
                .unwrap_or_else(|error| panic!("{} did not parse: {error:?}", method.canonical()));
        }
    }
}
