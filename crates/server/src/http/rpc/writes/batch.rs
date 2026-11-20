use super::super::answer::{envelope, Selector};
use super::super::error::RequestError;
use super::answer::{snapshot_value, written};
use super::params::{invalid, string, target};
use super::{WriteMethod, WriteRequest};
use logseq_graph::domain::{PageCommit, PageOperation};
use logseq_graph::engine::{Graph, MoveTarget, WriteError};
use serde_json::{json, Value};

pub(super) fn parse_insert_batch(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    let texts = args[1]
        .as_array()
        .ok_or_else(|| {
            invalid(
                method,
                "argument 1 (texts) must be an array of strings",
                json!({"index": 1, "parameter": "texts", "expected": "string[]"}),
            )
        })?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                invalid(
                    method,
                    format!("texts[{index}] must be a string"),
                    json!({"index": 1, "parameter": "texts", "item": index}),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WriteRequest::InsertBatch {
        target: target(method, &args[0], 0)?,
        texts,
    })
}

pub(super) fn insert_batch(
    graph: &mut Graph,
    target: &MoveTarget,
    texts: &[String],
) -> Result<Value, WriteError> {
    let texts = texts.iter().map(String::as_str).collect::<Vec<_>>();
    graph.insert_batch(target.clone(), &texts).map(|read| {
        written(read, |selectors, meta| {
            envelope(
                meta,
                json!({"selectors": selectors.iter().map(Selector::of).collect::<Vec<_>>()}),
            )
        })
    })
}

pub(super) fn parse_commit_page(
    method: WriteMethod,
    args: &[Value],
) -> Result<WriteRequest, RequestError> {
    Ok(WriteRequest::CommitPage {
        page: string(method, args, 0, "page")?,
        base_version: string(method, args, 1, "baseVersion")?,
        operations: crate::batch::parse_operations_value(&args[2]).map_err(|reason| {
            invalid(
                method,
                format!("argument 2 (operations) is invalid: {reason}"),
                json!({"index": 2, "parameter": "operations"}),
            )
        })?,
    })
}

pub(super) fn commit_page(
    graph: &mut Graph,
    page: &str,
    base_version: &str,
    operations: &[PageOperation],
) -> Result<Value, WriteError> {
    let commit = PageCommit::new(page, base_version.to_owned().into(), operations.to_vec());
    graph.commit_page(&commit).map(|read| {
        written(read, |result, meta| {
            envelope(
                meta,
                json!({
                    "snapshot": snapshot_value(result.snapshot()),
                    "applied": result.applied(),
                }),
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    #[test]
    fn commit_uses_the_shared_operation_grammar() {
        let request = parse(
            WriteMethod::CommitPage,
            vec![
                json!("alpha"),
                json!("0123456789abcdef"),
                json!([{"op": "set_collapsed", "block": [0], "collapsed": true}]),
            ],
        )
        .expect("valid commit");
        let WriteRequest::CommitPage { operations, .. } = request else {
            panic!("wrong request");
        };
        assert_eq!(
            operations,
            vec![PageOperation::SetCollapsed {
                block: vec![0],
                collapsed: true,
            }]
        );
    }
}
