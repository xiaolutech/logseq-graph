use super::answer::snapshot_value;
use logseq_graph::document::EditError;
use logseq_graph::engine::{WriteError, WriteState};
use serde_json::{json, Map, Value};

pub(crate) fn error_code(error: &WriteError) -> i32 {
    match error {
        WriteError::PageNotFound { .. }
        | WriteError::BlockNotFound { .. }
        | WriteError::EphemeralNotFound { .. } => -32004,
        WriteError::PageExists { .. }
        | WriteError::AmbiguousBlockId { .. }
        | WriteError::StaleVersion { .. } => -32009,
        WriteError::Lock(_) => -32003,
        WriteError::Edit(_)
        | WriteError::RangeAcrossPages
        | WriteError::PageNameNotEncodable { .. }
        | WriteError::Operation { .. } => -32602,
        WriteError::IdentityGeneration { .. }
        | WriteError::Engine(_)
        | WriteError::Source(_)
        | WriteError::Index(_)
        | WriteError::PartialMove { .. }
        | WriteError::ReReadAfterWrite { .. }
        | WriteError::IndexAfterWrite { .. } => -32603,
    }
}

pub(crate) fn error_status(error: &WriteError) -> u16 {
    match error_code(error) {
        -32602 => 400,
        -32004 => 404,
        -32009 => 409,
        -32003 => 503,
        _ => 500,
    }
}

fn edit_data(error: &EditError) -> Value {
    let (kind, detail) = match error {
        EditError::EmptyContent => ("empty-content", json!({})),
        EditError::RangeNotSiblings => ("range-not-siblings", json!({})),
        EditError::RangeReversed => ("range-reversed", json!({})),
        EditError::MultilineContent => ("multiline-content", json!({})),
        EditError::EndsInsideOpaqueRegion => ("ends-inside-opaque-region", json!({})),
        EditError::BlockNotFound => ("block-not-found", json!({})),
        EditError::PathOutOfRange => ("path-out-of-range", json!({})),
        EditError::AmbiguousBlockId => ("ambiguous-block-id", json!({})),
        EditError::ContinuationChangesStructure => ("continuation-changes-structure", json!({})),
        EditError::InterleavedProperties => ("interleaved-properties", json!({})),
        EditError::InvalidHeadingLevel { level } => {
            ("invalid-heading-level", json!({"level": level}))
        }
        EditError::TargetInsideMovedSubtree => ("target-inside-moved-subtree", json!({})),
        EditError::MoveTargetNotFound => ("move-target-not-found", json!({})),
        EditError::PropertyNotFound => ("property-not-found", json!({})),
        EditError::InvalidPropertyKey => ("invalid-property-key", json!({})),
        EditError::TextOffsetOutOfRange { offset, len } => (
            "text-offset-out-of-range",
            json!({"offset": offset, "len": len}),
        ),
        EditError::TextOffsetSplitsCharacter { offset } => {
            ("text-offset-splits-character", json!({"offset": offset}))
        }
        EditError::NoPreviousSibling => ("no-previous-sibling", json!({})),
        EditError::TopLevelCannotOutdent => ("top-level-cannot-outdent", json!({})),
    };
    let mut data = Map::new();
    data.insert("edit_kind".to_owned(), json!(kind));
    data.insert("reason".to_owned(), json!(error.to_string()));
    if let Value::Object(detail) = detail {
        data.extend(detail);
    }
    Value::Object(data)
}

pub(crate) fn error_data(error: &WriteError) -> Value {
    let kind = match error {
        WriteError::IdentityGeneration { .. } => "identity-generation",
        WriteError::RangeAcrossPages => "range-across-pages",
        WriteError::Lock(_) => "write-lock",
        WriteError::Engine(_) => "write-engine",
        WriteError::Source(_) => "write-source",
        WriteError::Index(_) => "write-index",
        WriteError::Edit(_) => "write-edit",
        WriteError::PageNotFound { .. } => "page-not-found",
        WriteError::PageNameNotEncodable { .. } => "page-name-not-encodable",
        WriteError::PageExists { .. } => "page-exists",
        WriteError::BlockNotFound { .. } => "block-not-found",
        WriteError::EphemeralNotFound { .. } => "ephemeral-not-found",
        WriteError::AmbiguousBlockId { .. } => "ambiguous-block-id",
        WriteError::PartialMove { .. } => "partial-move",
        WriteError::StaleVersion { .. } => "stale-version",
        WriteError::Operation { .. } => "operation",
        WriteError::ReReadAfterWrite { .. } => "re-read-after-write",
        WriteError::IndexAfterWrite { .. } => "index-after-write",
    };
    let mut data = Map::new();
    data.insert("kind".to_owned(), json!(kind));
    data.insert("reason".to_owned(), json!(error.to_string()));
    data.insert(
        "write_state".to_owned(),
        json!(match error.write_state() {
            WriteState::Unwritten => "unwritten",
            WriteState::FileChanged => "file_changed",
        }),
    );
    let detail = match error {
        WriteError::IdentityGeneration { reason } => json!({"cause": reason}),
        WriteError::RangeAcrossPages => json!({}),
        WriteError::Lock(cause) => json!({"cause": cause.to_string()}),
        WriteError::Engine(cause) => json!({"cause": cause.to_string()}),
        WriteError::Source(cause) => json!({"cause": cause.to_string()}),
        WriteError::Index(cause) => json!({"cause": cause.to_string()}),
        WriteError::Edit(cause) => edit_data(cause),
        WriteError::PageNotFound { page } => json!({"page": page}),
        WriteError::PageNameNotEncodable { name, reason } => {
            json!({"name": name, "cause": reason})
        }
        WriteError::PageExists { page, path } => {
            json!({"page": page, "path": path.display().to_string()})
        }
        WriteError::BlockNotFound { id } => {
            json!({"id": id.to_string(), "selector_kind": "durable"})
        }
        WriteError::EphemeralNotFound { id } => {
            json!({"id": id.to_string(), "selector_kind": "ephemeral"})
        }
        WriteError::AmbiguousBlockId { id, pages } => json!({"id": id.to_string(), "pages": pages}),
        WriteError::PartialMove {
            from,
            from_path,
            to,
            to_path,
            cause,
        } => json!({
            "from": from,
            "from_path": from_path.display().to_string(),
            "to": to,
            "to_path": to_path.display().to_string(),
            "cause": cause,
        }),
        WriteError::StaleVersion {
            page,
            base_version,
            current,
        } => json!({
            "page": page,
            "baseVersion": base_version.as_str(),
            "current": snapshot_value(current),
        }),
        WriteError::Operation {
            page,
            index,
            operation,
            cause,
        } => {
            let mut detail = Map::new();
            detail.insert("page".to_owned(), json!(page));
            detail.insert("index".to_owned(), json!(index));
            detail.insert("operation".to_owned(), json!(operation));
            if let Value::Object(edit) = edit_data(cause) {
                detail.extend(edit);
            }
            Value::Object(detail)
        }
        WriteError::ReReadAfterWrite { page, path } => {
            json!({"page": page, "path": path.display().to_string()})
        }
        WriteError::IndexAfterWrite { page, path, cause } => json!({
            "page": page,
            "path": path.display().to_string(),
            "cause": cause.to_string(),
        }),
    };
    if let Value::Object(detail) = detail {
        data.extend(detail);
    }
    Value::Object(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_errors_separate_refusal_from_file_change() {
        let id = logseq_graph::domain::BlockId::parse("01234567-89ab-cdef-0123-456789abcdef")
            .expect("valid id");
        let missing = WriteError::BlockNotFound { id };
        assert_eq!(error_code(&missing), -32004);
        assert_eq!(error_data(&missing)["write_state"], "unwritten");

        let operation = WriteError::Operation {
            page: "alpha".to_owned(),
            index: 2,
            operation: "IndentBlock",
            cause: EditError::NoPreviousSibling,
        };
        assert_eq!(error_code(&operation), -32602);
        let data = error_data(&operation);
        assert_eq!(data["kind"], "operation");
        assert_eq!(data["index"], 2);
        assert_eq!(data["operation"], "IndentBlock");
        assert_eq!(data["edit_kind"], "no-previous-sibling");

        let partial = WriteError::PartialMove {
            from: "alpha".to_owned(),
            from_path: "pages/alpha.md".into(),
            to: "bravo".to_owned(),
            to_path: "pages/bravo.md".into(),
            cause: "destination changed".to_owned(),
        };
        assert_eq!(error_code(&partial), -32603);
        assert_eq!(error_data(&partial)["write_state"], "file_changed");

        let committed = WriteError::Source(logseq_graph::source::SourceError::Committed {
            path: "pages/alpha.md".into(),
            step: "pages".into(),
            source: std::io::Error::other("directory sync failed"),
        });
        assert_eq!(error_data(&committed)["write_state"], "file_changed");
    }
}
