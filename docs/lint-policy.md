# Lint and format policy

This file answers three questions with numbers instead of memory: which lints this workspace
enables and what each one prevents, how many suppressions those lints cost, and which rules were
measured and refused. It also closes the two questions the policy left open — whether test code is
linted, and whether a `rustfmt.toml` exists — with a decision each.

Nothing here is a wish list. Sections 1–2 describe the current declarations and suppression audit
at `149e2fb` (2026-11-19). Sections 3–4 preserve the original policy experiments on the pinned
toolchain (`rust 1.99.1`, `clippy 0.1.99`, `rustfmt 1.10.0-stable`, all from `mise.toml`), not live
warning/test counts. Their commands can be rerun, but their old file paths, line numbers and site
totals describe the audited revision. Some counts came from one-off scans, as noted there.

`prek.toml` supplies local `pre-commit`, `pre-push` and `commit-msg` hooks. `just check` runs the
non-network gates; [.github/workflows/ci.yml](../.github/workflows/ci.yml) runs `just check` and
`just deny` on Linux and macOS for pull requests and pushes to `main` / `devin/**`. The lint
commands are identical locally and in CI:

```sh
cargo clippy --workspace --all-targets -- -D warnings   # every target, every crate
RUSTDOCFLAGS="-D warnings" cargo doc -p logseq-graph --no-deps
```

`-D warnings` is what makes the `warn`-level tiers below into a gate: a warning is a failed commit.

## 1. The rules: 22 lints, written as 18 names

The "22" is a real number, but only if you expand one group: the policy enables 18 lint *names*, and
`rust_2018_idioms` is a group of five (`rustc -W help | grep rust-2018-idioms` lists
`bare-trait-objects`, `unused-extern-crates`, `ellipsis-inclusive-range-patterns`,
`elided-lifetimes-in-paths`, `explicit-outlives-requirements`). 18 − 1 + 5 = 22. Counting declared
lines instead gives 28: 6 in `Cargo.toml`, 12 in the library root, 10 in the binary root, because the
binary drops the two printing rules.

Verify the declaration sites, not a summary of them:

```sh
grep -n -A20 '\[workspace.lints' Cargo.toml
grep -n -A20 'cfg_attr(' crates/graph/src/lib.rs crates/server/src/main.rs
```

### P0 — `[workspace.lints]` in `Cargo.toml`

These apply to every target of both packages, including `tests/`, `examples/` and `#[cfg(test)]`
code: Cargo passes the `[lints]` table to each compilation. §4a verifies that claim with an
experiment instead of assuming it.

| Lint | Level | What it prevents |
| --- | --- | --- |
| `unsafe_code` | `forbid` | A second soundness claim. SQLite's FFI belongs to `rusqlite`; a local `unsafe` block would be unreviewed `unsafe` in a library that hands out parsed text. `forbid` rather than `deny` so no call site can opt out with an attribute. |
| `missing_docs` | `warn` | An undocumented `pub` item, i.e. an unstated contract. The freshness policy (ADR-0006) and second-resolution observation times are exactly the things a caller gets wrong when the doc is absent. |
| `missing_debug_implementations` | `warn` | A public type that cannot be printed. `assert_eq!` on a page snapshot, and every CLI error path that formats a value, need `Debug`; without it the missing impl is discovered where you need the value. |
| `clippy::dbg_macro` | `warn` | A `dbg!` that ships. `dbg!` writes to stderr and evaluates to its argument, so a stray one in the index scan lands in the middle of a machine-readable answer. |
| `clippy::todo` | `warn` | `todo!()` panics. The CLI's exit-code contract (`0` answer, `1` unreadable, `2` bad invocation, `3` written-but-index-behind) is what scripts branch on; a panic gives 101 and a backtrace instead. |
| `clippy::unimplemented` | `warn` | Same panic, same contract break: an unfinished branch must be an error variant so the exit code stays truthful. |

### P1/P2 — crate roots, production builds only

Both roots wrap these in `#![cfg_attr(not(test), warn(...))]`, so they are off in `#[cfg(test)]`
modules. They are also invisible to `tests/` and `examples/`, which are separate crates. §4a
measures what that exemption holds.

| Lint | Tier | Declared in | What it prevents |
| --- | --- | --- | --- |
| `clippy::expect_used` | P1 | `lib.rs`, `main.rs` | `expect()` panics. A library caller cannot recover, and a CLI panic breaks the exit-code contract above with no message a script can key on. |
| `clippy::unwrap_used` | P1 | `lib.rs`, `main.rs` | The same panic with no message at all: the failure arrives as `called Result::unwrap()` and whatever `Debug` says, not as the error the read already carries. |
| `clippy::panic` | P1 | `lib.rs`, `main.rs` | Explicit `panic!`, including the `unreachable!`-shaped ones. "Nothing here panics on input" is a stated property of the binary; this is the rule that keeps it true. |
| `unreachable_pub` | P1 | `lib.rs`, `main.rs` | `pub` on an item no crate can name. When a refactor makes a module private, the leftover `pub` silently stops being API; the lint flags it at the moment the module moved instead of when a caller breaks. |
| `clippy::print_stdout` | P2 | `lib.rs` only | Printing from a library. The library is loaded into a process whose stdout is a contract (the CLI's answer, `measure`'s table rows); one `println!` in a scan corrupts it. |
| `clippy::print_stderr` | P2 | `lib.rs` only | The same for stderr. Deliberately absent from `main.rs`: a CLI reporting failures, usage, progress and truncation notices on stderr is the crate's job, and linting it would earn 5 `#[expect]`s (`main.rs:172,174,512`, `http/rpc/mod.rs:1563,1584`) around the error and progress reporting. |
| `clippy::wildcard_imports` | P2 | `lib.rs`, `main.rs` | `use crate::domain::*`. A glob adopts whatever the source module adds next, which is a compile error at a distance or a trait in scope nobody wrote; it also makes the name un-greppable. |
| `unused_lifetimes` | P2 | `lib.rs`, `main.rs` | A lifetime parameter the function does not use. In these signatures the named lifetime is the only signal that a return value borrows from an argument; an unused one makes that signal noise. |
| `rust_2018_idioms` | P2 | `lib.rs`, `main.rs` | Five lints that keep borrow and name edges visible: `elided_lifetimes_in_paths` (`&Page` hiding its lifetime), `bare_trait_objects`, `unused_extern_crates`, `ellipsis_inclusive_range_patterns`, `explicit_outlives_requirements`. |
| `clippy::allow_attributes` | P2 | `lib.rs`, `main.rs` | `#[allow]`, which is permanent by construction. It is replaced by `#[expect]`, which the compiler reports as `unfulfilled_lint_expectations` once the code no longer needs it — see §2. |
| `clippy::allow_attributes_without_reason` | P2 | `lib.rs`, `main.rs` | A suppression nobody can re-judge. With `reason = "..."` required, every exception states the fact that makes it temporary. All 38 expectations in this tree carry one. |
| `clippy::let_underscore_must_use` | P2 | `lib.rs`, `main.rs` | `let _ = …` on a `#[must_use]` value: a dropped `io::Result` from a page write, or a dropped guard. It is at zero hits in `src/` today, so it costs nothing to keep. |

Everything else is clippy's default set, denied by `-D warnings`: `clippy::all` (correctness,
suspicious, style, complexity, perf). When this file says "the gate is green", it means that
default set plus the 22 above, at zero hits.

## 2. Suppression budget: 38 `#[expect]`, 38 reasons, 19 files

Count attributes, not `expect(` calls: the great majority of those are assertions in test code
(§4a's historical audit counted 1795 `expect_used` sites outside `src/` production code).
The following command finds real suppressions:

```sh
grep -rn --include='*.rs' -E '#!?\[(expect|allow)\(' crates
```

```sh
python3 - <<'PY'
import os, re
for root, _, names in os.walk("crates"):
    for name in sorted(names):
        if not name.endswith(".rs"):
            continue
        path = os.path.join(root, name)
        source = open(path, encoding="utf-8").read()
        for match in re.finditer(r"#!?\[(expect|allow)\(", source):
            depth, index = 1, match.end()
            while depth:
                depth += {"(": 1, ")": -1}.get(source[index], 0)
                index += 1
            body = source[match.end():index - 1]
            print(match.group(1), path, source[:match.start()].count("\n") + 1,
                  "reason" in body, body.split(",")[0].strip())
PY
```

Result: **38 `#[expect]`, 38 with `reason = "..."` (100%)**, 0 in test code (none inside
`#[cfg(test)]`, none under `tests/`), concentrated as follows:

| File | `#[expect]` |
| --- | --- |
| `crates/graph/src/document/edit/` (7 files) | 16 |
| `crates/server/src/http/rpc/` (`answer.rs`, `methods.rs`, `state.rs`, `serve.rs`) | 5 |
| `crates/graph/src/source/config/mod.rs` | 2 |
| `crates/graph/src/document/parser.rs` | 2 |
| `crates/graph/src/index/query/page.rs` | 1 |
| `crates/graph/src/engine/snapshot.rs` | 1 |
| `crates/graph/src/engine/pool.rs` | 3 |
| `crates/graph/src/engine/scan.rs` | 6 |
| `crates/graph/src/domain/snapshot.rs` | 1 |
| `crates/server/src/main.rs` | 1 |
| **Total** | **38** |

By lint: 37 suppress `clippy::expect_used`, 1 suppresses `clippy::too_many_arguments` (a
warn-by-default clippy lint, not one of the 22). The P1/P2 tier costs 37 exceptions: 31 in the
library and 6 in the binary. Sixteen are in `document/edit/`, reflecting checked locator/span
invariants; the pool and shared scan state also assert that poisoned synchronization state cannot
be recovered safely.

### Why `#[expect]`, not `#[allow]`

`#[allow]` is a decision with no expiry: nothing in the compiler tells you when the suppressed
warning stops happening, so the attribute outlives its reason and the budget grows by convention.
`#[expect]` is checked in both directions. While the warning fires, the expectation is fulfilled and
the build is quiet. When the code changes so the warning no longer fires, rustc emits
`unfulfilled_lint_expectations` (warn by default, `-D warnings` here) and the commit fails. A stale
suppression is therefore a red build, not an exercise in archaeology, and `clippy::allow_attributes`
(§1) is what keeps anyone from reaching for the uncheckable form.

### Why the 6 `#[allow]` in `tests/` are legal

```sh
grep -rn --include='*.rs' -E '#!?\[allow\(' crates
```

prints 6: `crates/graph/tests/support/mod.rs:6` (`#![allow(dead_code)]`), `tests/support/mod.rs:31`
(`#[allow(unused_macros)]`), `tests/support/mod.rs:42` (`#[allow(unused_imports)]`),
`tests/query_read.rs:474` (`#[allow(dead_code)]`), `crates/server/tests/rpc.rs:41`
(`#![allow(non_snake_case)]`), and `crates/server/tests/rpc/writes.rs:869` (`#[allow(dead_code)]`).

They are not a hole in the policy for two reasons. First, they are in integration-test crates: a
file under `crates/*/tests/` is its own crate, so neither crate root's `#![cfg_attr(not(test), …)]`
reaches it — the rules that would reject `#[allow]` (`clippy::allow_attributes`,
`clippy::allow_attributes_without_reason`) are simply not enabled there. Second, `#[expect]` would be
wrong for shared helpers: `tests/support/mod.rs` is compiled into multiple integration-test binaries,
each of which uses a different subset, so an `#[expect(unused_macros)]` would be fulfilled in some
binaries and unfulfilled — a failed build — in the ones that do use the macro. For a shared module,
`#[allow]` is the correct tool: the suppression is per-binary-usage, not per-site.

The honest summary is "no `#[expect]` debt outside `src/`, and 6 `#[allow]`s confined to test code".

## 3. Refused: `clippy::pedantic` and `clippy::nursery`

The counts and paths in this section are the original policy-audit snapshot, not current totals.

Measure the whole group before arguing about individual lints:

```sh
cargo clippy --workspace --all-targets --message-format=json -- \
  -W clippy::pedantic -W clippy::nursery > /tmp/pedantic.json
```

then aggregate by lint, counting a *site* once even though the same build reports it several times.
The script is reused verbatim by §4a:

```sh
cat > lint-count.py <<'PY'
import json, collections, os, sys

def first_cfg_test_line(path):
    for number, line in enumerate(open(path, encoding="utf-8"), 1):
        if "#[cfg(test)]" in line:
            return number
    return None

def region(path, line):
    if "/tests/" in path:
        return "tests/"
    if "/examples/" in path:
        return "examples/"
    start = first_cfg_test_line(path)
    return "src/#[cfg(test)]" if start and line >= start else "src/"

sites, events = set(), 0
for raw in open(sys.argv[1], encoding="utf-8"):
    if not raw.startswith("{"):
        continue
    record = json.loads(raw)
    if record.get("reason") != "compiler-message":
        continue
    message = record["message"]
    lint = (message.get("code") or {}).get("code")
    if not lint or message["level"] not in ("warning", "error"):
        continue
    events += 1
    span = next(span for span in message["spans"] if span["is_primary"])
    sites.add((lint, span["file_name"], span["line_start"], span["byte_start"], span["byte_end"]))

print("events", events, "sites", len(sites), "lints", len({site[0] for site in sites}))
for lint, count in collections.Counter(site[0] for site in sites).most_common():
    print(f"{count:6d}  {lint}")
print()
for (where, lint), count in sorted(collections.Counter(
        (region(site[1], site[2]), site[0]) for site in sites).items()):
    print(f"{count:6d}  {where:20s} {lint}")
PY
python3 lint-count.py /tmp/pedantic.json
```

Two numbers come out, and the difference is the whole point:
- **1780 warning events** — what the build prints. This is the figure that tends to get quoted,
  and it double-counts two things: `--all-targets` compiles each library twice (as the library and
  as its own test harness), and `crates/graph/tests/support/mod.rs` is compiled into each of the 41
  integration-test binaries, so one line of helpers is reported 41 times.
- **872 distinct sites** (lint + file + byte range), 46 distinct lints. That is the number of places
  in the source that would need an `#[expect]`.

Of the 872: 708 are in `src/` production code, 132 under `tests/`, 30 in `#[cfg(test)]` modules, 2
in `examples/`. The split treats everything after a file's first `#[cfg(test)]` as test code, which
is exact on this tree — each of the 26 `src/` files that carries the marker ends with it
(`#[cfg(test)] mod tests {` is the last item), so no production item follows the marker in any of
them:

```sh
for f in $(git ls-files 'crates/*/src/*.rs' 'crates/*/src/**/*.rs'); do
  c=$(grep -n '#\[cfg(test)\]' "$f" | head -1 | cut -d: -f1); [ -z "$c" ] && continue
  awk -v c="$c" 'NR>c && /^(pub|impl |fn |mod |struct |enum |trait |type |const |static )/ \
    && $0 !~ /^mod tests \{/' "$f"
done   # prints nothing: 26 files carry the marker, none of them has an item after it
```

| Lint | Sites | Where |
| --- | --- | --- |
| `clippy::must_use_candidate` | 192 | `src/` |
| `clippy::missing_const_for_fn` | 157 | `src/` |
| `clippy::missing_errors_doc` | 98 | `src/` |
| `clippy::redundant_pub_crate` | 76 | `src/` |
| `clippy::use_self` | 56 | `src/` |
| `clippy::assert_is_empty` | 43 | tests only |
| `clippy::redundant_closure_for_method_calls` | 40 | mixed |
| `clippy::cast_possible_wrap` | 33 | 21 `src/`, 12 tests |
| `clippy::needless_pass_by_value` | 29 | `src/` |
| `clippy::cast_possible_truncation` | 24 | 17 `src/`, 7 tests |
| `clippy::cast_sign_loss` | 13 | `src/` |
| `clippy::too_many_lines` | 12 | mixed |
| `clippy::items_after_statements` | 11 | tests mostly |
| `clippy::match_wildcard_for_single_variants` | 9 | tests |
| the other 32 lints | 79 | — |

The judgement:

> A rule that arrives with a few hundred `#[expect]` attributes is not a rule any more. It is a
> licence to suppress, written in bulk, at the moment the rule landed — and each of those
> attributes then has to carry a `reason` that says something true about a line nobody is going to
> read again.

Enabling `pedantic` and `nursery` here would mean 708 new suppressions in `src/`: 28 today, 736
after. That is a review burden per commit, paid forever, to catch `must_use_candidate`, which is a
documentation-grade suggestion, not a bug class. The tier's value is not zero — it is just not
worth 708 attributes.

Two honest exceptions were considered and rejected individually:

- `clippy::redundant_pub_crate` (76) is refused on style grounds, not on volume: the codebase writes
  `pub(crate)` deliberately inside private modules, and the lint's fix is to delete 76 deliberate
  visibilities. Nothing here is a bug the current form causes.
- `clippy::missing_errors_doc` (98) is refused because the contract is already stated where it is
  read: the crate-level docs carry the freshness and error policy, and an `# Errors` section on 98
  functions would be prose nobody maintains.

### The `cast_*` family: the one that looks like a real risk

`cast_*` totals 72 sites: 51 in `src/`, 20 under `tests/`, 1 in a `#[cfg(test)]` module. It is the
refused family with the largest production site count that names a numeric hazard, and the only one
whose claim — "this cast may wrap, truncate, or lose the sign" — sounds like a bug rather than a
style opinion. It is still refused, because reading the sites shows clippy cannot prove bounds that
the author reads off one line of context. Two examples, both real:

```rust
// crates/server/src/render.rs:21-27 — epoch seconds, printed as RFC 3339 (verbatim).
let seconds = match time.duration_since(UNIX_EPOCH) {
    Ok(after) => after.as_secs() as i64,
    // Before the epoch the duration runs the other way; the conversion below handles a negative
    // second count with `div_euclid`, so a clock set back still prints a real date.
    Err(before) => -(before.duration().as_secs() as i64),
};
```

```rust
// crates/graph/src/document/edit.rs:1045 — indentation columns, in characters not bytes (verbatim).
let delta = target_indent as isize - source_indent as isize;
```

The first is `u64 as i64`: safe until the year 292 277 026 596, and a `try_into()` there would need
an error variant for a clock that cannot exist. The second is `usize as isize`: the values are
indentation widths of a single line of Markdown, bounded by the line length the parser already
holds, and the subtraction is the point — it must be allowed to go negative. A `cast_*` policy
would demand an `#[expect]` plus a `reason` on both, and the reason would say "bounded by
construction", which is what the code already says to a reader.

`clippy::cast_precision_loss` (2 sites) is in a test comparing against captured official answers.
The remaining `src/` sites are not one shape: `len()`/`index as u32` ordinals for block counts the
index stores as `u32`; `u64` content hashes round-tripped through SQLite as `i64`; journal-date
arithmetic already reduced by `rem_euclid(12)` / `rem_euclid(7)`; `level as u8` where the match
pattern is `level @ 1..=6`; `u128 as_nanos() as i64` for a file mtime (`index/baseline.rs:163`); and
`month as u32, day as u32` off `civil_from_days`. Every one is bounded by the line it sits on; none
is a class of bug this repository has shipped.

## 4a. Open question: should test code be linted?

**Decision: no, keep the exemption — and keep it documented as one, not as an accident.**

The original audit verified these two mechanisms; its counts below are historical:

1. `#![cfg_attr(not(test), warn(…))]` exempts `#[cfg(test)]` modules because the attribute is
   compiled out in the test harness build. Measured cost: the test harness is where 595 of the
   sites in the table below live.
2. The 47 test-crate roots under `crates/*/tests/` and `examples/measure.rs` never see a crate-root
   attribute of `lib.rs`/`main.rs`, because each is a separate crate. That the P0 table *does* reach
   them is a Cargo fact, not a guess — the same mechanism that puts `missing_docs` on every public
   item also puts it on the test crate. Verified with a throwaway crate in `/tmp`:

   ```sh
   cargo new --lib linttest && cd linttest
   mkdir -p tests
   printf '[package]\nname="linttest"\nversion="0.1.0"\nedition="2021"\n\n[lints.rust]\nmissing_docs = "deny"\n' > Cargo.toml
   printf '//! demo.\n' > src/lib.rs
   printf 'pub struct Undocumented;\n#[test]\nfn t() {}\n' > tests/it.rs
   cargo check --tests
   # error: missing documentation for the crate
   # error: missing documentation for a struct
   # error: could not compile `linttest` (test "it") due to 2 previous errors
   ```

   The mirror of that experiment is in this repository: all 47 test-crate roots and
   `examples/measure.rs` start with `//!`. The 4 files under `crates/*/tests/` without one
   (`tests/rpc/{files,pages,protocol,queries}.rs`) are `mod` files inside a crate root, not crates,
   so nothing asks them for crate docs. `missing_docs` is why the other 48 carry one.

So the exemption is exactly the P1/P2 tier. Measure what applying it to test code would cost — note
`-W` on the command line reaches test targets, which is what makes this measurable without editing
the crate roots:

```sh
cargo clippy --workspace --all-targets --message-format=json -- \
  -W clippy::expect_used -W clippy::unwrap_used -W clippy::panic \
  -W clippy::print_stdout -W clippy::print_stderr -W clippy::wildcard_imports \
  -W clippy::allow_attributes -W clippy::allow_attributes_without_reason \
  -W clippy::let_underscore_must_use -W unused_lifetimes -W rust_2018_idioms \
  -W unreachable_pub > /tmp/prodset.json
python3 lint-count.py /tmp/prodset.json
```

| Rule | `tests/` | `src/#[cfg(test)]` | `examples/` |
| --- | --- | --- | --- |
| `clippy::expect_used` | 1204 | 539 | 52 |
| `clippy::panic` | 160 | 34 | 1 |
| `clippy::print_stdout` | 29 | 1 | 24 |
| `clippy::unwrap_used` | 16 | 9 | 0 |
| `clippy::let_underscore_must_use` | 15 | 12 | 0 |
| `clippy::allow_attributes_without_reason` | 5 | 0 | 0 |
| `clippy::allow_attributes` | 3 | 0 | 0 |
| `clippy::print_stderr` | 2 | 0 | 1 |
| `unreachable_pub`, `wildcard_imports`, `unused_lifetimes`, `rust_2018_idioms` | 0 | 0 | 0 |
| **Total** | **1434** | **595** | **78** |

2107 sites, of which 1795 are `expect()` and 195 are `panic!`/`assert!`-shaped. The script prints
2112 for this run: that is the 2107 plus the 5 production `print_stderr` sites, all of them in
`crates/server/src/` — a rule that crate deliberately does not declare (§1), so the remaining column
is the one worth reading.

The conclusion follows from the same argument as §3, with one addition specific to tests: in a test,
`expect("the page is projected")` *is* the assertion — the message is the failure output. A rule
that demands 1795 `#[expect]`s to keep that is asking the suite to annotate its own assertions. What
the repository keeps instead is the part that has value in test code: P0 (no `todo!`, no `dbg!`, no
`unsafe`, every crate documented) applies to every test target, and §2's suppression budget stays
measurable precisely because test crates are allowed to use `#[allow]` where the usage is
per-binary.

If the number ever changes shape — e.g. `expect_used` in tests drops to a few dozen because a
`TestResult` helper replaces the calls — then the calculation above is the thing to re-run; it is
one command plus the §3 script.

## 4b. Open question: is a `rustfmt.toml` worth creating?

**Decision: no file. The reasons are measured below, and "we have no rustfmt.toml" is itself the
statement of the convention: rustfmt defaults plus `.editorconfig`.**

The gate is `cargo fmt --all --check` (both in `prek.toml` and `just check`), which on this tree
reports 0 diffs. There is no `rustfmt.toml`. The candidates people normally set were each tested on
the pinned stable `rustfmt 1.10.0-stable`, in a scratch directory, one option per run, against a
control formatted with no config (the `style_edition` row was measured on this tree instead — see
below, it needs no bypass):

```sh
mkdir -p /tmp/fmtcheck && cd /tmp/fmtcheck
cat > base.rs <<'EOF'
use std::io;
use crate::a::B;
use std::fmt;

/// Example:
/// ```
/// let x  =  1;
/// ```
pub fn f(a: u32, b: u32, c: u32, d: u32, e: u32, g: u32, h: u32, i: u32) -> u32 {
    let long_name_variable = 1;
    let total = a + b + c + d + e + g + h + i + long_name_variable;
    total
}

pub fn short() -> u32 { 1 }
EOF
cp base.rs control.rs && rustfmt --emit files control.rs
cp base.rs out.rs && printf 'group_imports = "StdExternalCrate"\n' > rustfmt.toml
rustfmt --emit files out.rs; diff control.rs out.rs; rm rustfmt.toml
```

| Option | On stable `rustfmt 1.10.0` | Effect with a `rustfmt.toml` |
| --- | --- | --- |
| `imports_granularity` | nightly-only | `Warning: can't set `imports_granularity = Crate`, unstable features are only available in nightly channel.` — ignored, output identical to the control |
| `group_imports` | nightly-only | same warning, ignored |
| `format_code_in_doc_comments` | nightly-only | same warning, ignored |
| `wrap_comments`, `format_strings`, `empty_item_single_line`, `indent_style`, `version` | nightly-only / deprecated | same warning (or the `version` deprecation) and ignored |
| `reorder_imports` | stable | honoured |
| `max_width` | stable | honoured |
| `use_small_heuristics` | stable | honoured |
| `style_edition` | stable | **honoured silently — no warning.** `2024` reformats 46 files / 76 hunks; the default follows each crate's `edition` (`2021`), so setting `2021` is a no-op and setting `2024` is a real change |
| `newline_style`, `tab_spaces` | stable | honoured |

So the three options that would actually express an import policy — merge imports, group them std /
external / crate, format the code inside doc comments — cannot be set on the toolchain this
repository pins. What can be set is worth quantifying anyway, because "not available" would be a bad
reason to leave the question open: `cargo fmt --all --check -- --config <option>` passes the option
on the command line, which bypasses the nightly check, so the size of the unstated change can be
measured without writing anything. `style_edition` needs no bypass — it is stable — so it was
measured with a real `rustfmt.toml` and `cargo fmt --all --check`:

| Convention | Files that would change | Hunks | How it was measured |
| --- | --- | --- | --- |
| `group_imports=StdExternalCrate` | 24 | 36 | `--config` (nightly-only option) |
| `imports_granularity=Crate` | 68 | 118 | `--config` (nightly-only option) |
| `use_small_heuristics=Max` | 92 | 1490 | `--config` (stable option) |
| `style_edition=2024` | 46 | 76 | `rustfmt.toml` + `cargo fmt --all --check`, rc=1, no warning |

That is the useful result: there is no "current convention" hiding here to write down. Import order
is rustfmt-default (alphabetical within each `use` block, std and crate paths interleaved), and two
stable options would reformat the tree if someone set them: `use_small_heuristics=Max` (92 of the 101
Rust files) and `style_edition=2024` (46 files — it re-sorts names inside each `use` group, so it is
the option a reader is most likely to reach for, and it is a convention change, not a description of
the current one). A `rustfmt.toml` that expresses today's code would contain only defaults:

| Stable option | Its default | What setting it would say |
| --- | --- | --- |
| `max_width` | `100` | nothing — the default. 995 lines exceed 100 columns: 893 are comment lines (rustfmt does not wrap those without `wrap_comments`) and 102 are string-literal or attribute continuation lines (57 start inside a string, 11 start a `reason =` string, the other 34 are the same kind of continuation by inspection), which rustfmt does not break without `format_strings`. Both options are nightly-only. |
| `tab_spaces` | `4` | nothing — the default, and `.editorconfig` already states `indent_size = 4` for `*.rs` |
| `newline_style` | `Unix` | nothing — the default, and `.editorconfig` already states `end_of_line = lf` |
| `reorder_imports` | `true` | nothing — the default |
| `use_small_heuristics` | `Default` | nothing — the default; `Max` would reformat 92 files |
| `style_edition` | each crate's `edition` = `2021` | nothing — the current behaviour. It is the one stable option that both reads like a real convention and changes the code: `2021` is a no-op, `2024` reformats 46 files / 76 hunks, so the honest alternative to pinning nothing is pinning a change, not a fact |

A file whose whole content restates the defaults, in a repository where `.editorconfig` already
carries the whitespace half of the same facts, is a second source of truth that can only disagree
with the first one later. It stays absent. The day a nightly `rustfmt` is pinned in `mise.toml` (a
separate decision, and one that changes the toolchain, not a config file), `group_imports` and
`imports_granularity` become expressible, and the 36- and 118-hunk numbers above are the size of
that change.

One measurement worth recording so nobody repeats the surprise: passing an unstable option via
`--config` on the command line is *silently honoured* on stable, while the same option in a
`rustfmt.toml` is warned about and ignored. That asymmetry is a rustfmt quirk, not a supported
configuration path — the gate runs plain `cargo fmt --all --check`, which reads no `--config`, so
anything set that way would be unchecked.

## What this policy does not cover, and what proves it

The audit question "is the linting complete?" only has an answer if the answer names the edges.
The non-network `just check` gate is seven checks; this file covers the first two in detail.
CI runs that recipe plus the network-dependent `just deny`; the table below lists local wiring.

| Check | Where it is enforced | Evidence |
| --- | --- | --- |
| `cargo clippy --workspace --all-targets -- -D warnings` | `prek.toml`, `just check` | §1–§4a above |
| `cargo fmt --all --check` | `prek.toml`, `just check` | §4b above |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p logseq-graph --no-deps` | `prek.toml`, `just check` | broken intra-doc link fails the commit; `-p` because the bin and lib targets collide in `target/doc` |
| `typos --force-exclude` | `prek.toml`, `just check` | `.typos.toml` holds the word list and excludes `fixtures/` |
| `python3 scripts/check_perf_doc.py --self-test` | `prek.toml`, `just check` | tests the staged/explicit-list policy; the separate local `perf-doc` hook checks the staged file set |
| `cargo test --workspace` | `prek.toml` (`pre-push`), `just check` | — |
| `python3 scripts/check_commit_msg.py` | `prek.toml` (`commit-msg`, and `--range` on `pre-push`) | `docs/commit-message.md` |

Not covered, by decision: MSRV (`just msrv`, `cargo +1.89 check`, deliberately not a hook — clippy
1.89 does not know every lint name enabled here, so a lint failure there would be a toolchain
mismatch reported as an MSRV failure). CI does not run the MSRV recipe or provision a private
real graph/official Logseq service. Its performance-document check is the self-test, not a
whole-PR changed-file check; staged performance-row enforcement remains a local hook.
