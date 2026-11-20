//! JSON input for the CLI's batch commit command.

use logseq_graph::domain::{BatchMoveTarget, BlockMarker, PageOperation};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InputOperation {
    EditText {
        block: Vec<u32>,
        text: String,
    },
    SplitBlock {
        block: Vec<u32>,
        offset: usize,
    },
    MergeBlock {
        block: Vec<u32>,
    },
    UnwrapBlock {
        block: Vec<u32>,
    },
    IndentBlock {
        block: Vec<u32>,
    },
    OutdentBlock {
        block: Vec<u32>,
    },
    InsertBlock {
        after: Option<Vec<u32>>,
        text: String,
    },
    MoveBlock {
        block: Vec<u32>,
        target: InputMoveTarget,
    },
    SetCollapsed {
        block: Vec<u32>,
        collapsed: bool,
    },
    SetMarker {
        block: Vec<u32>,
        marker: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMoveTarget {
    Before { anchor: Vec<u32> },
    After { anchor: Vec<u32> },
    FirstChildOf { anchor: Vec<u32> },
    LastChildOf { anchor: Vec<u32> },
}

pub(crate) fn parse_operations(json: &str) -> Result<Vec<InputOperation>, String> {
    serde_json::from_str(json).map_err(|error| error.to_string())
}

/// Decode the same snake_case operations from an already-parsed JSON array.
pub(crate) fn parse_operations_value(value: &Value) -> Result<Vec<PageOperation>, String> {
    let input: Vec<InputOperation> =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    input
        .iter()
        .enumerate()
        .map(|(index, input)| {
            operation(input).map_err(|error| format!("operation {index}: {error}"))
        })
        .collect()
}

pub(crate) fn operation(input: &InputOperation) -> Result<PageOperation, String> {
    Ok(match input {
        InputOperation::EditText { block, text } => PageOperation::EditText {
            block: block.clone(),
            text: text.clone(),
        },
        InputOperation::SplitBlock { block, offset } => PageOperation::SplitBlock {
            block: block.clone(),
            offset: *offset,
        },
        InputOperation::MergeBlock { block } => PageOperation::MergeBlock {
            block: block.clone(),
        },
        InputOperation::UnwrapBlock { block } => PageOperation::UnwrapBlock {
            block: block.clone(),
        },
        InputOperation::IndentBlock { block } => PageOperation::IndentBlock {
            block: block.clone(),
        },
        InputOperation::OutdentBlock { block } => PageOperation::OutdentBlock {
            block: block.clone(),
        },
        InputOperation::InsertBlock { after, text } => PageOperation::InsertBlock {
            after: after.clone(),
            text: text.clone(),
        },
        InputOperation::MoveBlock { block, target } => PageOperation::MoveBlock {
            block: block.clone(),
            target: match target {
                InputMoveTarget::Before { anchor } => BatchMoveTarget::Before {
                    anchor: anchor.clone(),
                },
                InputMoveTarget::After { anchor } => BatchMoveTarget::After {
                    anchor: anchor.clone(),
                },
                InputMoveTarget::FirstChildOf { anchor } => BatchMoveTarget::FirstChildOf {
                    anchor: anchor.clone(),
                },
                InputMoveTarget::LastChildOf { anchor } => BatchMoveTarget::LastChildOf {
                    anchor: anchor.clone(),
                },
            },
        },
        InputOperation::SetCollapsed { block, collapsed } => PageOperation::SetCollapsed {
            block: block.clone(),
            collapsed: *collapsed,
        },
        InputOperation::SetMarker { block, marker } => PageOperation::SetMarker {
            block: block.clone(),
            marker: parse_marker(marker)?,
        },
    })
}

fn parse_marker(marker: &str) -> Result<BlockMarker, String> {
    if marker == "-" {
        return Ok(BlockMarker::Bullet);
    }
    let level = marker.len();
    if (1..=6).contains(&level) && marker.bytes().all(|byte| byte == b'#') {
        return Ok(BlockMarker::Heading { level: level as u8 });
    }
    Err(format!(
        "invalid marker {marker:?}: expected - or one to six # characters"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_parse_with_paths_and_move_targets() {
        let input = parse_operations(
            r#"[{"op":"insert_block","after":null,"text":"new"},{"op":"move_block","block":[1],"target":{"first_child_of":{"anchor":[0]}}}]"#,
        )
        .expect("valid operations");
        assert_eq!(
            operation(&input[0]).expect("insert maps"),
            PageOperation::InsertBlock {
                after: None,
                text: "new".to_owned(),
            }
        );
        assert_eq!(
            operation(&input[1]).expect("move maps"),
            PageOperation::MoveBlock {
                block: vec![1],
                target: BatchMoveTarget::FirstChildOf { anchor: vec![0] },
            }
        );
    }

    #[test]
    fn malformed_or_unknown_input_is_refused() {
        assert!(parse_operations("not json").is_err());
        assert!(parse_operations(r#"[{"op":"unknown"}]"#).is_err());
        assert!(parse_operations(r#"[{"op":"merge_block","block":[0],"extra":true}]"#).is_err());
        assert!(parse_marker("#######").is_err());
        assert!(parse_marker("*").is_err());
        assert_eq!(parse_marker("##"), Ok(BlockMarker::Heading { level: 2 }));
    }

    #[test]
    fn json_values_decode_every_operation_without_changing_the_cli_grammar() {
        let value = serde_json::json!([
            {"op":"edit_text","block":[0],"text":"changed"},
            {"op":"split_block","block":[0,1],"offset":2},
            {"op":"merge_block","block":[1]},
            {"op":"unwrap_block","block":[2]},
            {"op":"indent_block","block":[3]},
            {"op":"outdent_block","block":[4]},
            {"op":"insert_block","after":null,"text":"top level"},
            {"op":"insert_block","after":[5],"text":"after"},
            {"op":"move_block","block":[6],"target":{"before":{"anchor":[7]}}},
            {"op":"move_block","block":[8],"target":{"after":{"anchor":[9]}}},
            {"op":"move_block","block":[10],"target":{"first_child_of":{"anchor":[11]}}},
            {"op":"move_block","block":[12],"target":{"last_child_of":{"anchor":[13]}}},
            {"op":"set_collapsed","block":[14],"collapsed":true},
            {"op":"set_marker","block":[15],"marker":"-"}
        ]);

        assert_eq!(
            parse_operations_value(&value).expect("all operations decode"),
            vec![
                PageOperation::EditText {
                    block: vec![0],
                    text: "changed".to_owned(),
                },
                PageOperation::SplitBlock {
                    block: vec![0, 1],
                    offset: 2,
                },
                PageOperation::MergeBlock { block: vec![1] },
                PageOperation::UnwrapBlock { block: vec![2] },
                PageOperation::IndentBlock { block: vec![3] },
                PageOperation::OutdentBlock { block: vec![4] },
                PageOperation::InsertBlock {
                    after: None,
                    text: "top level".to_owned(),
                },
                PageOperation::InsertBlock {
                    after: Some(vec![5]),
                    text: "after".to_owned(),
                },
                PageOperation::MoveBlock {
                    block: vec![6],
                    target: BatchMoveTarget::Before { anchor: vec![7] },
                },
                PageOperation::MoveBlock {
                    block: vec![8],
                    target: BatchMoveTarget::After { anchor: vec![9] },
                },
                PageOperation::MoveBlock {
                    block: vec![10],
                    target: BatchMoveTarget::FirstChildOf { anchor: vec![11] },
                },
                PageOperation::MoveBlock {
                    block: vec![12],
                    target: BatchMoveTarget::LastChildOf { anchor: vec![13] },
                },
                PageOperation::SetCollapsed {
                    block: vec![14],
                    collapsed: true,
                },
                PageOperation::SetMarker {
                    block: vec![15],
                    marker: BlockMarker::Bullet,
                },
            ]
        );

        let cli_input = parse_operations(&value.to_string()).expect("CLI accepts the same JSON");
        let cli_operations = cli_input
            .iter()
            .map(operation)
            .collect::<Result<Vec<_>, _>>()
            .expect("CLI converts the same operations");
        assert_eq!(
            parse_operations_value(&value).expect("RPC accepts the same JSON"),
            cli_operations
        );
    }

    #[test]
    fn json_values_preserve_empty_paths_and_large_offsets_for_engine_validation() {
        let value = serde_json::json!([
            {"op":"merge_block","block":[]},
            {"op":"insert_block","after":[],"text":"new"},
            {"op":"move_block","block":[],"target":{"after":{"anchor":[]}}},
            {"op":"split_block","block":[0],"offset":18446744073709551615u64}
        ]);

        assert_eq!(
            parse_operations_value(&value).expect("shape and edit rules belong to the engine"),
            vec![
                PageOperation::MergeBlock { block: vec![] },
                PageOperation::InsertBlock {
                    after: Some(vec![]),
                    text: "new".to_owned(),
                },
                PageOperation::MoveBlock {
                    block: vec![],
                    target: BatchMoveTarget::After { anchor: vec![] },
                },
                PageOperation::SplitBlock {
                    block: vec![0],
                    offset: usize::MAX,
                },
            ]
        );
    }

    #[test]
    fn json_values_accept_exactly_the_bullet_and_heading_markers() {
        for level in 1..=6 {
            let value = serde_json::json!([{
                "op":"set_marker",
                "block":[0],
                "marker":"#".repeat(level)
            }]);
            assert_eq!(
                parse_operations_value(&value).expect("valid heading marker"),
                vec![PageOperation::SetMarker {
                    block: vec![0],
                    marker: BlockMarker::Heading { level: level as u8 },
                }]
            );
        }

        for marker in ["", "# ", "#######", "*", "+", "--", "＃"] {
            let value = serde_json::json!([{
                "op":"set_marker",
                "block":[0],
                "marker":marker
            }]);
            let error = parse_operations_value(&value).expect_err("invalid marker");
            assert!(error.starts_with("operation 0: invalid marker "), "{error}");
            assert!(
                error.ends_with(": expected - or one to six # characters"),
                "{error}"
            );
        }
    }

    #[test]
    fn json_value_errors_retain_their_location_and_reject_bad_paths() {
        let not_an_array = parse_operations_value(&serde_json::json!({"op":"merge_block"}))
            .expect_err("an object is not an operations array");
        assert!(not_an_array.contains("invalid type: map"), "{not_an_array}");

        let bad_path = parse_operations_value(&serde_json::json!([
            {"op":"merge_block","block":[0,-1]}
        ]))
        .expect_err("child indices are unsigned");
        assert!(
            bad_path.contains("invalid value: integer `-1`"),
            "{bad_path}"
        );

        let bad_marker = parse_operations_value(&serde_json::json!([
            {"op":"merge_block","block":[0]},
            {"op":"set_marker","block":[1],"marker":"*"}
        ]))
        .expect_err("invalid marker");
        assert_eq!(
            bad_marker,
            "operation 1: invalid marker \"*\": expected - or one to six # characters"
        );
    }
}
