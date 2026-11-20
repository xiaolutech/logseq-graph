use logseq_graph::domain::JournalDate;
use logseq_graph::engine::Freshness;
use serde_json::{json, Map, Value};

use super::error::{params_error, request_error, RequestError};
use super::methods::{self, Method};
use super::writes;

/// The freshness values a request may spell, as the error lists them.
const FRESHNESS_VALUES: &str = "last, check, verify";

/// Resolve a method name the way the official server does: a `logseq.` name resolves by the snake
/// form of its leaf, a bare name as written; writes keep their exact `logseq.Graph` name.
fn resolve_method(name: &str) -> Result<Method, RequestError> {
    methods::resolve(name).map_err(|leaf| RequestError::MethodNotFound {
        method: name.to_owned(),
        leaf,
    })
}

/// One call's parsed arguments: positional strings, the one policy, and its optional knobs.
pub(super) struct Call {
    pub(super) method: Method,
    pub(super) positional: Vec<String>,
    pub(super) options: Options,
    /// Set when the single positional argument was an official `{id: <number>}` page identity: the
    /// numeric page id to resolve, rather than the name string.
    pub(super) page_id: Option<u64>,
    pub(super) write: Option<writes::WriteRequest>,
}

/// The trailing options object of `args`.
pub(super) struct Options {
    /// The one policy this request declares; `LastScan` when it names none.
    pub(super) freshness: Freshness,
    /// `limit` for `search` and `query`; absent means the published default.
    pub(super) limit: Option<u32>,
    /// `today` for `query`; absent means the server's UTC day.
    pub(super) today: Option<JournalDate>,
}

/// Parse one JSON request object into a call, once, before any engine read.
pub(super) fn parse_call(value: Value) -> Result<Call, RequestError> {
    let object = value.as_object().ok_or_else(|| {
        request_error(
            "one request carries exactly one call; a batch is not answered",
            json!({}),
        )
    })?;
    for key in object.keys() {
        if !matches!(key.as_str(), "method" | "args" | "id" | "jsonrpc") {
            return Err(request_error(
                format!("unknown request member {key:?}"),
                json!({"field": key, "allowed": ["method", "args", "id", "jsonrpc"]}),
            ));
        }
    }
    let name = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            request_error(
                "the request needs a string method",
                json!({"field": "method"}),
            )
        })?;
    let method = resolve_method(name)?;
    let args: Vec<Value> = match object.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values.clone(),
        Some(_) => {
            return Err(request_error(
                "args must be an array",
                json!({"field": "args", "expected": "an array"}),
            ));
        }
    };
    if let Method::Write(write_method) = method {
        let write = writes::parse(write_method, args)?;
        return Ok(Call {
            method,
            positional: Vec::new(),
            options: Options {
                freshness: Freshness::LastScan,
                limit: None,
                today: None,
            },
            page_id: None,
            write: Some(write),
        });
    }
    let mut positional = Vec::new();
    let mut page_id_identity: Option<u64> = None;
    let mut option_objects: Vec<Map<String, Value>> = Vec::new();
    for (index, argument) in args.iter().enumerate() {
        match argument {
            Value::String(text) => {
                if !option_objects.is_empty() {
                    return Err(params_error(
                        "invalid-params",
                        "the options object must be the last argument",
                        json!({"index": index}),
                    ));
                }
                positional.push(text.clone());
            }
            Value::Object(fields) => {
                if positional.len() < method.positional() {
                    // An official client addresses a page or a block with an identity object:
                    // `{name}`, `{uuid}` or `{id: <number>}`. Reduce it to the spelling this
                    // projection can look up. A numeric DB entity id has no counterpart in a file
                    // graph (ADR-0001), so its decimal text names no page and the read answers
                    // not-found; the official numeric-id path is a documented difference.
                    let named = ["name", "originalName", "uuid"]
                        .iter()
                        .find_map(|key| fields.get(*key))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let identity = named.or_else(|| {
                        fields.get("id").and_then(Value::as_u64).map(|id| {
                            page_id_identity = Some(id);
                            id.to_string()
                        })
                    });
                    let Some(identity) = identity else {
                        return Err(params_error(
                            "invalid-params",
                            format!("argument {index} states no page name, uuid or id"),
                            json!({"index": index, "expected": "name | uuid | id"}),
                        ));
                    };
                    positional.push(identity);
                } else {
                    option_objects.push(fields.clone());
                }
            }
            // A browser client addresses a page by a bare numeric entity id: the extension's
            // `getPage` sends `[id || uuid || name]` from its identity argument, so a page read can
            // carry a number where the command line carries a name. Keep the decimal text as the
            // positional (the arity stays right) and remember the id so `getPage` resolves it by
            // `page_id`.
            Value::Number(number) => {
                if !option_objects.is_empty() {
                    return Err(params_error(
                        "invalid-params",
                        "the options object must be the last argument",
                        json!({"index": index}),
                    ));
                }
                let Some(id) = number.as_u64() else {
                    return Err(params_error(
                        "invalid-params",
                        format!("argument {index} is a number but not a page id"),
                        json!({"index": index, "expected": "a non-negative integer"}),
                    ));
                };
                if positional.len() < method.positional() {
                    page_id_identity = Some(id);
                }
                positional.push(id.to_string());
            }
            // A browser client that omits an optional argument sends `[uuid, undefined]`, which
            // `JSON.stringify` writes as `[uuid, null]`. Once every positional argument is stated
            // there is nothing for the null to name, so it is simply absent.
            Value::Null if positional.len() >= method.positional() => {}
            _ => {
                return Err(params_error(
                    "invalid-params",
                    format!(
                        "argument {index} must be a string, an identity object, or the options object"
                    ),
                    json!({"index": index, "expected": "a string | name/uuid/id | options"}),
                ));
            }
        }
    }
    if option_objects.len() > 1 {
        return Err(params_error(
            "multiple-policies",
            "one request carries exactly one freshness policy; two options objects were given",
            json!({"objects": option_objects.len()}),
        ));
    }
    if positional.len() != method.positional() {
        return Err(params_error(
            "invalid-params",
            format!(
                "{} takes {} positional argument(s), not {}",
                method.canonical(),
                method.positional(),
                positional.len()
            ),
            json!({"expected": method.positional(), "actual": positional.len()}),
        ));
    }

    let empty = Map::new();
    let fields = option_objects.first().unwrap_or(&empty);
    let allowed = method.allowed_options();
    for key in fields.keys() {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        if key == "freshness" && allowed.is_empty() {
            return Err(params_error(
                "no-policy-allowed",
                format!(
                    "{} answers from the page file's current bytes, so it takes no freshness \
                     parameter; {FRESHNESS_VALUES} name which scan the other methods answer from",
                    method.canonical()
                ),
                json!({"method": method.canonical(), "parameter": "freshness"}),
            ));
        }
        return Err(params_error(
            "invalid-params",
            format!("{} does not take a {key:?} option", method.canonical()),
            json!({"parameter": key, "allowed": allowed}),
        ));
    }

    let freshness = match fields.get("freshness") {
        None => Freshness::LastScan,
        Some(Value::String(raw)) => match raw.as_str() {
            "last" => Freshness::LastScan,
            "check" => Freshness::CheckNow,
            "verify" => Freshness::VerifyContent,
            other => {
                return Err(params_error(
                    "invalid-freshness",
                    format!("unknown freshness {other:?}; the values are {FRESHNESS_VALUES}"),
                    json!({
                        "parameter": "freshness",
                        "value": other,
                        "allowed": ["last", "check", "verify"],
                    }),
                ));
            }
        },
        Some(_) => {
            return Err(params_error(
                "invalid-freshness",
                "freshness must be a string",
                json!({
                    "parameter": "freshness",
                    "allowed": ["last", "check", "verify"],
                }),
            ));
        }
    };

    let limit = match fields.get("limit") {
        None => None,
        Some(value) => {
            let max = method.limit_max().unwrap_or(methods::MAX_QUERY_LIMIT);
            let Some(number) = value.as_u64() else {
                return Err(params_error(
                    "invalid-limit",
                    "limit must be a whole number",
                    json!({"parameter": "limit", "max": max}),
                ));
            };
            if number > u64::from(u32::MAX) || number > u64::from(max) {
                return Err(params_error(
                    "invalid-limit",
                    format!("limit must be at most {max}"),
                    json!({"parameter": "limit", "max": max, "value": number}),
                ));
            }
            Some(number as u32)
        }
    };

    let today = match fields.get("today") {
        None => None,
        Some(Value::String(raw)) => match crate::render::parse_day(raw) {
            Some(day) => Some(day),
            None => {
                return Err(params_error(
                    "invalid-day",
                    format!("today must be a date as YYYY-MM-DD, not {raw:?}"),
                    json!({"parameter": "today", "value": raw}),
                ));
            }
        },
        Some(_) => {
            return Err(params_error(
                "invalid-day",
                "today must be a string YYYY-MM-DD",
                json!({"parameter": "today"}),
            ));
        }
    };

    Ok(Call {
        method,
        positional,
        options: Options {
            freshness,
            limit,
            today,
        },
        page_id: page_id_identity,
        write: None,
    })
}

/// The one positional argument a method that takes one was validated to have.
///
/// `parse_call` refuses a call whose positional count is not [`Method::positional`], so every arm of
/// [`super::handle`] for such a method does have its argument. This states that as a refusal rather than an
/// assertion, so the handler keeps no panic path at all: the answer a caller gets for a call that
/// slipped past validation is an internal error naming the method, not a dead server.
pub(super) fn one_argument(call: &Call) -> Result<&str, RequestError> {
    call.positional.first().map(String::as_str).ok_or_else(|| {
        RequestError::Internal(format!(
            "{} requires one positional argument and none reached the handler",
            call.method.canonical()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_error(value: Value) -> RequestError {
        match parse_call(value) {
            Ok(_) => panic!("the call should have been refused"),
            Err(error) => error,
        }
    }

    #[test]
    fn every_canonical_name_resolves_to_the_same_method_and_error_lists() {
        let canonical = [
            "logseq.Editor.getCurrentGraph",
            "logseq.Editor.getAllPages",
            "logseq.Graph.getFavorites",
            "logseq.Editor.getPage",
            "logseq.Editor.getPageBlocksTree",
            "logseq.Editor.getPageLinkedReferences",
            "logseq.Graph.getAllTags",
            "logseq.Graph.getPagesWithTag",
            "logseq.App.search",
            "logseq.Graph.getPageVersion",
            "logseq.Graph.getPageSnapshot",
            "logseq.Graph.query",
            "logseq.Editor.getBlock",
            "logseq.App.checkCurrentIsDBGraph",
            "logseq.App.getAppInfo",
            "logseq.showMsg",
            "logseq.DB.q",
            "logseq.App.getUserConfigs",
            "logseq.Editor.getCurrentPage",
            "logseq.Graph.createPage",
            "logseq.Graph.createJournal",
            "logseq.Graph.appendBlock",
            "logseq.Graph.insertBlock",
            "logseq.Graph.insertBatch",
            "logseq.Graph.prependBlockInPage",
            "logseq.Graph.updateBlock",
            "logseq.Graph.removeBlock",
            "logseq.Graph.removeBlockRange",
            "logseq.Graph.ensureBlockId",
            "logseq.Graph.moveBlock",
            "logseq.Graph.upsertBlockProperty",
            "logseq.Graph.removeBlockProperty",
            "logseq.Graph.setBlockCollapsed",
            "logseq.Graph.deletePage",
            "logseq.Graph.commitPage",
        ];
        for canonical in canonical {
            let method = resolve_method(canonical).expect("a canonical method resolves");
            assert_eq!(method.canonical(), canonical);
        }

        let error = resolve_method("logseq.Editor.nope").expect_err("unknown method");
        assert_eq!(error.data()["methods"], json!(methods::read_names()));
        assert_eq!(error.data()["write_methods"], json!(methods::write_names()));
        assert_eq!(methods::read_names().len(), 19);
        assert_eq!(methods::write_names().len(), 16);
    }

    #[test]
    fn namespace_discard_applies_to_reads_but_not_writes() {
        for canonical in methods::read_names() {
            let method = resolve_method(canonical).expect("a canonical read resolves");
            let leaf = canonical.rsplit('.').next().expect("a method leaf");
            for equivalent in [
                format!("logseq.{leaf}"),
                format!("logseq.Any.Namespace.{leaf}"),
            ] {
                assert_eq!(
                    resolve_method(&equivalent).expect("equivalent read"),
                    method
                );
            }
        }

        for name in ["logseq.createPage", "logseq.Editor.createPage"] {
            let error = resolve_method(name).expect_err("writes require the Graph namespace");
            assert_eq!(error.data()["leaf"], json!("create_page"));
            assert_eq!(error.message(), "MethodNotExist: create_page");
        }
    }

    #[test]
    fn unknown_methods_keep_the_original_name_and_leaf() {
        for (name, leaf) in [
            // A name without the `logseq.` prefix is looked up as written, so its whole text is
            // the leaf the error states.
            ("Editor.getPage", "Editor.getPage"),
            ("logseq.Editor.nope", "nope"),
            ("logseq.Editor.", ""),
        ] {
            let error = resolve_method(name).expect_err("unknown method");
            assert_eq!(error.code(), -32601);
            assert_eq!(error.data()["method"], json!(name));
            assert_eq!(error.data()["leaf"], json!(leaf));
            assert_eq!(error.message(), format!("MethodNotExist: {leaf}"));
        }
    }

    #[test]
    fn read_arity_options_and_limit_ceilings_come_from_the_method_spec() {
        let error = parse_error(json!({"method": "logseq.Editor.getPage", "args": []}));
        assert_eq!(error.data()["expected"], json!(1));
        assert_eq!(error.data()["actual"], json!(0));
        assert_eq!(
            error.message(),
            "logseq.Editor.getPage takes 1 positional argument(s), not 0"
        );

        let error = parse_error(json!({
            "method": "logseq.Editor.getPage",
            "args": ["alpha", {"limit": 2}]
        }));
        assert_eq!(error.data()["allowed"], json!(["freshness"]));

        let error = parse_error(json!({
            "method": "logseq.Graph.getPageVersion",
            "args": ["alpha", {"freshness": "check"}]
        }));
        assert_eq!(error.data()["kind"], json!("no-policy-allowed"));

        for method in ["logseq.App.search", "logseq.Graph.query"] {
            let error = parse_error(json!({"method": method, "args": ["x", {"limit": 1001}]}));
            assert_eq!(error.data()["max"], json!(1000));
        }
        let error = parse_error(json!({
            "method": "logseq.Graph.query",
            "args": ["(task TODO)", {"today": true}]
        }));
        assert_eq!(error.data()["kind"], json!("invalid-day"));
    }

    #[test]
    fn write_arity_comes_from_the_method_spec_without_retyping_arguments() {
        for name in ["logseq.Graph.createPage", "logseq.Graph.moveBlock"] {
            let resolved = resolve_method(name).expect("write resolves");
            let Method::Write(method) = resolved else {
                panic!("expected a write");
            };
            let error = writes::parse(method, Vec::new()).expect_err("write arity is enforced");
            assert_eq!(error.data()["expected"], json!(resolved.positional()));
            assert_eq!(error.data()["actual"], json!(0));
            assert_eq!(
                error.message(),
                format!(
                    "{name}: takes {} positional arguments, not 0",
                    resolved.positional()
                )
            );
        }

        let method = resolve_method("logseq.Graph.moveBlock").expect("moveBlock resolves");
        assert_eq!(method.positional(), 2);
        assert_eq!(method.alternate_positional(), Some(3));
    }
}
