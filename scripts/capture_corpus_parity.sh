#!/usr/bin/env bash
#
# Capture the official graph parser's answers for one corpus, as the fixture
# `crates/graph/tests/corpus_parity.rs` compares against.
#
# The corpus is `logseq/docs` at the pinned tag and the parser is `logseq/og` at the pinned commit.
# Both are cloned into a temporary directory that the script removes on exit, so the capture is
# reproducible and leaves nothing behind. The corpus and the parser are read only.
#
# Usage:
#   scripts/capture_corpus_parity.sh [--graph <corpus-root>] [--output <path>] [--with-texts]
#
#   --graph   Use an existing checkout of the corpus instead of cloning it. It must be the pinned
#             revision; the script checks the file count and refuses a graph that is not.
#   --output  Where to write the fixture. Default: fixtures/corpus/docs-v0.9.2.json
#   --with-texts
#             Also write each page's block texts, for attributing a difference a digest reports.
#             That output is corpus text under AGPL-3.0: it requires --output, and the script
#             refuses a path inside this repository.
#
# This is an oracle, not a gate: it needs node, npm, babashka and a JDK (the parser's nbb program
# resolves its dependencies through the Clojure CLI), which the repository's gates deliberately do
# not install. The test that reads the fixture needs none of them.
#
# Exit status: 0 captured, 1 a step failed, 2 usage error or a missing tool.
set -uo pipefail

readonly PARSER_REPOSITORY="https://github.com/logseq/og"
readonly PARSER_COMMIT="6e7afa8eb040686ff057156ee877193b581dd369"
readonly CORPUS_REPOSITORY="https://github.com/logseq/docs"
readonly CORPUS_TAG="v0.9.2"
# Page files the corpus holds under pages/ and journals/. A checkout with another count is another
# revision, and a fixture captured from it would not describe the pinned one.
readonly CORPUS_PAGE_FILES=274

graph=""
output=""
with_texts=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --graph) [[ $# -ge 2 ]] || { echo "error: --graph needs a path" >&2; exit 2; }
                 graph="$2"; shift 2 ;;
        --output) [[ $# -ge 2 ]] || { echo "error: --output needs a path" >&2; exit 2; }
                  output="$2"; shift 2 ;;
        --with-texts) with_texts="1"; shift ;;
        -h|--help) sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; exit 2 ;;
    esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || exit 2
cd "$repo_root" || exit 2
output="${output:-fixtures/corpus/docs-v0.9.2.json}"
case "$output" in
    /*) ;;
    *) output="$repo_root/$output" ;;
esac
mkdir -p "$(dirname "$output")" || exit 2
output="$(cd "$(dirname "$output")" && pwd)/$(basename "$output")"

if [[ -n "$with_texts" ]]; then
    case "$output" in
        "$repo_root"/*) echo "error: --with-texts output must be outside $repo_root" >&2; exit 2 ;;
    esac
fi

for tool in node npm npx git; do
    command -v "$tool" >/dev/null || { echo "error: $tool is not on PATH" >&2; exit 2; }
done
# nbb-logseq shells out to `bb`, which resolves the parser's Clojure dependencies through the
# Clojure CLI, which needs a JVM. Both are checked here so a missing tool is named before a clone.
command -v bb >/dev/null || { echo "error: babashka (bb) is not on PATH; see docs/corpus-parity.md" >&2; exit 2; }
command -v java >/dev/null || { echo "error: a JDK is not on PATH; see docs/corpus-parity.md" >&2; exit 2; }

scratch="$(mktemp -d)" || exit 2
cleanup() { rm -rf "$scratch"; }
trap cleanup EXIT

# The corpus. A caller-provided checkout is verified by file count; a cloned one is pinned by tag.
if [[ -n "$graph" ]]; then
    graph="$(cd "$graph" && pwd)" || { echo "error: --graph is not a directory: $graph" >&2; exit 2; }
    count="$(find "$graph/pages" "$graph/journals" -name '*.md' -type f | wc -l | tr -d ' ')"
    if [[ "$count" != "$CORPUS_PAGE_FILES" ]]; then
        echo "error: $graph holds $count Markdown page files, not the pinned $CORPUS_PAGE_FILES" >&2
        echo "error: capture from a clone of $CORPUS_REPOSITORY at $CORPUS_TAG instead" >&2
        exit 2
    fi
else
    echo "==> cloning $CORPUS_REPOSITORY at $CORPUS_TAG"
    graph="$scratch/docs"
    git clone --quiet --depth 1 --branch "$CORPUS_TAG" -c advice.detachedHead=false \
        "$CORPUS_REPOSITORY" "$graph" || exit 1
fi

echo "==> cloning $PARSER_REPOSITORY at ${PARSER_COMMIT:0:8}"
parser="$scratch/og"
git clone --quiet --filter=blob:none --no-checkout "$PARSER_REPOSITORY" "$parser" || exit 1
git -C "$parser" checkout --quiet "$PARSER_COMMIT" || exit 1

echo "==> installing the parser's node dependencies"
(cd "$parser/deps/graph-parser" && npm install --no-audit --no-fund --silent) || exit 1

echo "==> parsing $graph"
cp scripts/corpus_parity.cljs "$parser/deps/graph-parser/corpus-parity.cljs" || exit 1
(
    cd "$parser/deps/graph-parser" &&
    LOGSEQ_CORPUS_GRAPH="$graph" \
    LOGSEQ_CORPUS_OUTPUT="$output" \
    LOGSEQ_CORPUS_PARSER_COMMIT="$PARSER_COMMIT" \
    LOGSEQ_CORPUS_TAG="$CORPUS_TAG" \
    LOGSEQ_CORPUS_TEXTS="$with_texts" \
        npx nbb-logseq -cp test corpus-parity.cljs
) || exit 1

echo "==> wrote $output"
