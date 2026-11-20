#!/usr/bin/env python3
"""Capture read-only answers from the official Logseq HTTP API as regression fixtures.

The official surface is ``POST /api`` with ``{"method": ..., "args": [...]}`` and a bearer
token. This script only calls the read methods listed in CAPTURES, writes only into
``fixtures/expected/``, and never touches the graph directory or the token itself.

Without a token, or when the official server is unreachable, the script does not call the
network for capture: it validates the structure of the fixtures already on disk and reports
that no capture happened. No answer is guessed and no baseline is fabricated.

Usage examples:
    python3 scripts/capture_official_api.py --help
    python3 scripts/capture_official_api.py
    LOGSEQ_API_TOKEN=... python3 scripts/capture_official_api.py \
        --graph my-graph --graph-root /path/to/graph \
        --page TODO --block-uuid <uuid> --namespace a --search TODO \
        --query '(task TODO)' --property-key icon
"""

import argparse
import datetime
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

SCHEMA = "logseq-graph/official-api-baseline/1"
DEFAULT_URL = "http://127.0.0.1:12315/api"
DEFAULT_TIMEOUT = 5.0
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_FIXTURES_DIR = REPO_ROOT / "fixtures" / "expected"

# Option names in CAPTURES refer to argparse destinations. A capture whose options were not
# supplied is skipped, never filled with an invented value.
CAPTURES = (
    {"file": "get_current_graph.json", "method": "logseq.Editor.getCurrentGraph", "args": []},
    {"file": "get_all_pages.json", "method": "logseq.Editor.getAllPages", "args": []},
    {"file": "get_user_configs.json", "method": "logseq.App.getUserConfigs", "args": []},
    {"file": "get_page.json", "method": "logseq.Editor.getPage", "args": ["page"]},
    {"file": "get_page_blocks_tree.json", "method": "logseq.Editor.getPageBlocksTree", "args": ["page"]},
    {"file": "get_page_linked_references.json", "method": "logseq.Editor.getPageLinkedReferences", "args": ["page"]},
    {"file": "get_pages_from_namespace.json", "method": "logseq.Editor.getPagesFromNamespace", "args": ["namespace"]},
    {"file": "get_pages_tree_from_namespace.json", "method": "logseq.Editor.getPagesTreeFromNamespace", "args": ["namespace"]},
    {"file": "get_block.json", "method": "logseq.Editor.getBlock", "args": ["block_uuid"]},
    {"file": "get_block_properties.json", "method": "logseq.Editor.getBlockProperties", "args": ["block_uuid"]},
    {"file": "get_block_property.json", "method": "logseq.Editor.getBlockProperty", "args": ["block_uuid", "property_key"]},
    {"file": "get_previous_sibling_block.json", "method": "logseq.Editor.getPreviousSiblingBlock", "args": ["block_uuid"]},
    {"file": "get_next_sibling_block.json", "method": "logseq.Editor.getNextSiblingBlock", "args": ["block_uuid"]},
    {"file": "app_search.json", "method": "logseq.App.search", "args": ["search"]},
    {"file": "db_q.json", "method": "logseq.DB.q", "args": ["query"]},
)

OFFICIAL_READ_METHODS = {capture["method"] for capture in CAPTURES}

# A second guard: even a typo in the table above must not turn into a write call.
WRITE_VERBS = (
    "create", "insert", "append", "prepend", "update", "remove", "delete", "move",
    "set", "upsert", "rename", "save", "exec", "clear", "show", "register", "push",
)

# Keys that must never appear in a fixture: a bearer token is not a product fact.
FORBIDDEN_KEYS = {"token", "authorization", "api_token", "bearer"}
# A full getAllPages answer is tens of megabytes. The repository keeps a bounded, reproducible
# fact — the answer's size, its field counts and a sample — rather than a dump no one can review.
SUMMARY_BYTES = 256 * 1024
SUMMARY_SAMPLE = 3

CAPTURED_AT_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(Z|[+-]\d{2}:\d{2})$")


def parser():
    """Build the command-line parser. Help never reads or prints the token."""
    p = argparse.ArgumentParser(
        description=(
            "Capture read-only official Logseq HTTP API answers into fixtures/expected/*.json, "
            "or, offline, validate the structure of the fixtures already present."
        ),
        epilog=(
            "The token is read only from LOGSEQ_API_TOKEN and is never written to a file, "
            "printed, or logged. LOGSEQ_API_URL overrides the endpoint. Exit 0: capture "
            "succeeded or offline validation passed. Exit 1: capture or validation failed. "
            "Exit 2: usage error."
        ),
    )
    p.add_argument("--url", default=os.environ.get("LOGSEQ_API_URL", DEFAULT_URL),
                   help="official POST /api endpoint (default: %(default)s)")
    p.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT,
                   help="timeout in seconds for each API call (default: %(default)s)")
    p.add_argument("--fixtures-dir", type=Path, default=DEFAULT_FIXTURES_DIR,
                   help="directory that holds the captured JSON fixtures")
    p.add_argument("--graph", default=os.environ.get("LOGSEQ_GRAPH", ""),
                   help="label of the graph the official server has open; recorded in metadata only")
    p.add_argument("--graph-root", type=Path, default=None,
                   help="directory of that graph; read only, used to record its file counts")
    # Values below are arguments of official methods. None of them has a default: a capture
    # that needs one is skipped rather than guessed.
    p.add_argument("--page", default=None, help="page name for getPage-shaped methods")
    p.add_argument("--block-uuid", default=None, help="block UUID for getBlock-shaped methods")
    p.add_argument("--namespace", default=None, help="namespace for the namespace methods")
    p.add_argument("--search", default=None, help="query string for App.search")
    p.add_argument("--query", default=None, help="simple query DSL string for DB.q")
    p.add_argument("--property-key", default=None, help="property key for getBlockProperty")
    return p


def redact(text, token):
    """Remove the token from any text before it reaches stdout, stderr, or a file."""
    value = str(text)
    if token:
        value = value.replace(token, "[redacted]")
    return value


def endpoint_label(url):
    """Describe the endpoint without userinfo, query, or any credential in the URL."""
    parts = urllib.parse.urlsplit(url)
    host = parts.hostname or "(unknown host)"
    port = f":{parts.port}" if parts.port else ""
    return f"{parts.scheme}://{host}{port}{parts.path}"


def assert_read_only(method, payload):
    """Refuse anything that is not a two-key read call with a whitelisted method."""
    if method not in OFFICIAL_READ_METHODS:
        raise ValueError(f"method is not in the read-only whitelist: {method}")
    leaf = method.rsplit(".", 1)[-1].lower()
    if leaf.startswith(WRITE_VERBS):
        raise ValueError(f"method looks like a write: {method}")
    if set(payload) != {"method", "args"} or not isinstance(payload["args"], list):
        raise ValueError("a request payload may hold only method and args")
    for arg in payload["args"]:
        if not isinstance(arg, (str, int, float, bool, type(None))):
            raise ValueError("method arguments must be JSON scalars")


def call(url, method, args, token, timeout):
    """Call one official read method and return its decoded JSON answer."""
    payload = {"method": method, "args": args}
    assert_read_only(method, payload)
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        raw = response.read()
    return json.loads(raw.decode("utf-8"))


def graph_shape(graph_root, graph_name):
    """Record the graph's file-count shape. This reads the directory and writes nothing."""
    shape = {"name": graph_name or None, "page_files": None,
             "journal_files": None, "markdown_bytes": None}
    if graph_root is None:
        return shape
    if not graph_root.is_dir():
        raise ValueError("graph root is not a directory")
    page_files = journal_files = markdown_bytes = 0
    for path in graph_root.rglob("*.md"):
        if not path.is_file():
            continue
        parent = path.parent.name
        if parent == "pages":
            page_files += 1
        elif parent == "journals":
            journal_files += 1
        markdown_bytes += path.stat().st_size
    shape.update({"page_files": page_files, "journal_files": journal_files,
                  "markdown_bytes": markdown_bytes})
    return shape


def selected_captures(options):
    """Return the captures whose official arguments were all supplied."""
    selected, skipped = [], []
    for capture in CAPTURES:
        args = []
        missing = []
        for name in capture["args"]:
            value = getattr(options, name)
            if value is None or value == "":
                missing.append(f"--{name.replace('_', '-')}")
            else:
                args.append(value)
        if missing:
            skipped.append((capture["file"], ", ".join(missing)))
        else:
            selected.append((capture["file"], capture["method"], args))
    return selected, skipped


def write_fixtures(fixtures_dir, records):
    """Write all records, or none: a temporary file per record, then one atomic rename."""
    fixtures_dir.mkdir(parents=True, exist_ok=True)
    pending = []
    try:
        for filename, record in records:
            handle = tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", suffix=".json.tmp",
                dir=fixtures_dir, delete=False,
            )
            pending.append((Path(handle.name), fixtures_dir / filename))
            with handle:
                json.dump(record, handle, ensure_ascii=False, indent=2, sort_keys=True)
                handle.write("\n")
        for temporary, destination in pending:
            os.replace(temporary, destination)
    finally:
        # A failed batch leaves no partial truth behind.
        for temporary, _ in pending:
            if temporary.exists():
                temporary.unlink()


def validate_fixtures(fixtures_dir, token):
    """Check the structure of existing fixtures. Return the list of problems found."""
    problems = []
    files = sorted(fixtures_dir.rglob("*.json")) if fixtures_dir.is_dir() else []

    def check_no_secret(value, where):
        if isinstance(value, dict):
            for key, child in value.items():
                if str(key).lower() in FORBIDDEN_KEYS:
                    problems.append(f"{where}: forbidden key {key!r}")
                check_no_secret(child, where)
        elif isinstance(value, list):
            for child in value:
                check_no_secret(child, where)
        elif isinstance(value, str) and token and token in value:
            problems.append(f"{where}: token value present")

    for path in files:
        where = str(path.relative_to(fixtures_dir))
        try:
            with path.open(encoding="utf-8") as handle:
                document = json.load(handle)
        except (OSError, ValueError) as error:
            problems.append(f"{where}: {redact(error, token)}")
            continue
        if not isinstance(document, dict):
            problems.append(f"{where}: top level is not an object")
            continue
        if document.get("schema") != SCHEMA:
            problems.append(f"{where}: schema is not {SCHEMA!r}")
        method = document.get("method")
        if not isinstance(method, str) or method not in OFFICIAL_READ_METHODS:
            problems.append(f"{where}: method is not a whitelisted read")
        elif method.rsplit(".", 1)[-1].lower().startswith(WRITE_VERBS):
            problems.append(f"{where}: method looks like a write")
        if not isinstance(document.get("args"), list):
            problems.append(f"{where}: args is not an array")
        captured_at = document.get("captured_at")
        if not isinstance(captured_at, str) or not CAPTURED_AT_RE.match(captured_at):
            problems.append(f"{where}: captured_at is not an ISO-8601 timestamp")
        graph = document.get("graph")
        if not isinstance(graph, dict) or "name" not in graph:
            problems.append(f"{where}: graph is not an object with a name")
        else:
            for key in ("page_files", "journal_files", "markdown_bytes"):
                value = graph.get(key)
                if value is not None and (not isinstance(value, int) or isinstance(value, bool)):
                    problems.append(f"{where}: graph.{key} is not an integer or null")
        if "result" not in document:
            problems.append(f"{where}: result is missing")
        check_no_secret(document, where)
    return problems, len(files)


def summarize(result):
    """Bound a large official answer; small answers stay verbatim."""
    if len(json.dumps(result, ensure_ascii=False).encode("utf-8")) <= SUMMARY_BYTES:
        return result, False
    if isinstance(result, list):
        entries = [x for x in result if isinstance(x, dict)]
        keys = sorted({key for entry in entries for key in entry})
        return {
            "count": len(result),
            "field_counts": {
                key: sum(1 for entry in entries if key in entry) for key in keys
            },
            "truthy_field_counts": {
                key: sum(1 for entry in entries if entry.get(key)) for key in keys
            },
            "sample": result[:SUMMARY_SAMPLE],
        }, True
    if isinstance(result, dict):
        return {"count": len(result), "keys": sorted(result)[:20]}, True
    return {"type": type(result).__name__}, True


def offline(fixtures_dir, token, reason):
    """Validate existing fixtures, state clearly that nothing was captured, and stop."""
    problems, count = validate_fixtures(fixtures_dir, token)
    print(f"OFFLINE (未联网，未采集): {reason}; no capture performed.")
    print(f"Validated {count} existing fixture(s); problems: {len(problems)}.")
    for problem in problems:
        print(f"  invalid: {problem}", file=sys.stderr)
    return 1 if problems else 0


def capture(options, token):
    """Probe the official server, then capture every selected read method."""
    fixtures_dir = options.fixtures_dir.resolve()
    graph_root = options.graph_root.resolve() if options.graph_root else None

    # The graph is read only. Refuse a fixtures directory inside it so capture can never
    # write into the graph root, and refuse a graph root that is not a directory.
    if graph_root is not None:
        if not graph_root.is_dir():
            print("error: graph root is not a directory", file=sys.stderr)
            return 2
        if fixtures_dir == graph_root or graph_root in fixtures_dir.parents:
            print("error: fixtures directory must not be inside the graph root", file=sys.stderr)
            return 2
        if fixtures_dir in graph_root.parents:
            print("error: graph root must not be inside the fixtures directory", file=sys.stderr)
            return 2
    if options.timeout <= 0:
        print("error: timeout must be positive", file=sys.stderr)
        return 2

    label = endpoint_label(options.url)
    try:
        # One read-only probe decides whether the server is reachable. A failed probe is
        # treated as offline, not as an empty answer for any method.
        call(options.url, "logseq.Editor.getCurrentGraph", [], token, options.timeout)
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, ValueError, TimeoutError) as error:
        return offline(fixtures_dir, token,
                       f"{label} is unreachable ({redact(error, token)})")

    selected, skipped = selected_captures(options)
    for filename, missing in skipped:
        print(f"skipped {filename}: supply {missing} (no argument is guessed)")
    if not selected:
        print("error: no capture has a complete argument set", file=sys.stderr)
        return 1

    try:
        shape = graph_shape(graph_root, options.graph)
    except (OSError, ValueError) as error:
        print(f"error: could not read the graph shape: {redact(error, token)}", file=sys.stderr)
        return 1

    stamp = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()
    records = []
    for filename, method, args in selected:
        try:
            result = call(options.url, method, args, token, options.timeout)
        except (urllib.error.URLError, urllib.error.HTTPError, OSError, ValueError, TimeoutError) as error:
            # No partial baseline: one failed method discards the whole batch.
            print(f"error: {method} failed ({redact(error, token)}); nothing was written",
                  file=sys.stderr)
            return 1
        answer, summarized = summarize(result)
        records.append((filename, {
            "schema": SCHEMA,
            "method": method,
            "args": args,
            "captured_at": stamp,
            "graph": shape,
            "summarized": summarized,
            "result": answer,
        }))

    try:
        write_fixtures(fixtures_dir, records)
    except OSError as error:
        print(f"error: could not write fixtures: {redact(error, token)}", file=sys.stderr)
        return 1
    print(f"Captured {len(records)} read-only fixture(s) into {fixtures_dir}.")
    print(f"Graph: {shape['name'] or '(unnamed)'}; "
          f"page files {shape['page_files']}, journal files {shape['journal_files']}, "
          f"Markdown bytes {shape['markdown_bytes']}.")
    return 0


def main(argv=None):
    options = parser().parse_args(argv)
    token = os.environ.get("LOGSEQ_API_TOKEN", "")
    if not token:
        return offline(options.fixtures_dir.resolve(), token,
                       "LOGSEQ_API_TOKEN is not set")
    return capture(options, token)


if __name__ == "__main__":
    # An unhandled exception must not print a traceback that could carry the token.
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception as error:
        _token = os.environ.get("LOGSEQ_API_TOKEN", "")
        print(f"error: {redact(error, _token)}", file=sys.stderr)
        sys.exit(1)
