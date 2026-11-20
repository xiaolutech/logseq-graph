#!/usr/bin/env python3
"""Refuse a commit that changes a measured hot path without a PERFORMANCE.md row.

Two modes:

* Staged-set mode (default) inspects the git index. This is what the local
  `perf-doc` hook in `prek.toml` calls, with no arguments;
  its `VERDICT=` lines and exit codes are fixed.
* Explicit-list mode inspects a supplied set of changed paths, with no git calls
  and no merge check, for callers checking a whole change set. Current CI runs the
  self-test through `just check`, not a whole-PR file-list check.

Run standalone:

    python3 scripts/check_perf_doc.py                       # staged set (default)
    python3 scripts/check_perf_doc.py --self-test
    python3 scripts/check_perf_doc.py --files PATH...      # explicit paths from argv
    python3 scripts/check_perf_doc.py --files-from PATH    # one path per line from a file
    python3 scripts/check_perf_doc.py --files-from -       # one path per line from stdin

With `--files-from`, blank lines are ignored; `-` means stdin. In both explicit
forms the list is the final change set, so `in_merge()` is deliberately skipped:
those paths are the caller's change set, not an index that may be mid-merge.

Why this exists: `PERFORMANCE.md` records what each change did to the numbers. Unlike `HANDOFF.md`,
which describes current state and therefore contradicts the code the moment it goes stale, a missing
performance row contradicts nothing — it just makes the file quietly older. So the check is the
pressure, not the convention.

What it does not do: it cannot tell whether the row you added is any good, and `git commit
--no-verify` still bypasses it. It only removes the failure mode where a hot-path change lands with
no row at all.

Why `--self-test` strips `GIT_*` from the environment: it builds throwaway repositories, and a
linked worktree's pre-commit hook exports an absolute `GIT_DIR` / `GIT_INDEX_FILE`. Inherited, those
variables send the throwaway repos' `git add` into the caller's real index. `check()` and
`check_files()` keep the caller's environment on purpose: the staged-set mode exists to read the
very index the hook was handed.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import tempfile

# A path is here because a change under it can move a number in PERFORMANCE.md, not because the file
# is important. Adding a directory here is a decision: every later commit touching it must carry a
# row, including a row that says `not measured`.
HOT_PATHS = (
    "crates/graph/src/index/",
    "crates/graph/src/engine/",
    "crates/graph/src/document/edit/",
    "crates/graph/src/document/parser.rs",
    "crates/graph/src/document/facts/",
    "crates/graph/src/source/",
    "crates/server/src/http/rpc/",
)

PERF_DOC = "PERFORMANCE.md"


def clean_git_env() -> dict[str, str]:
    """The caller's environment without any git repository-selection variable."""
    return {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}


def git(*args: str) -> str:
    """Run git in the current directory and return stdout."""
    return subprocess.run(
        ("git", *args), check=True, capture_output=True, text=True
    ).stdout


def staged_files() -> list[str]:
    """Files the commit would add or modify, relative to the repository root."""
    output = git("diff", "--cached", "--name-only", "--diff-filter=ACMR")
    return [line for line in output.splitlines() if line]


def in_merge() -> bool:
    """A merge commit replays work already argued for in the branch's own commits."""
    return (
        subprocess.run(
            ("git", "rev-parse", "-q", "--verify", "MERGE_HEAD"), capture_output=True
        ).returncode
        == 0
    )


def hot(files: list[str]) -> list[str]:
    """The subset of `files` that sits on a measured hot path."""
    return [f for f in files if any(f.startswith(prefix) for prefix in HOT_PATHS)]


def check_files(files: list[str]) -> int:
    """Verdict for an explicit list of changed files.

    Pure with respect to git: no index reads and no merge check. The OK wording is
    kept verbatim from the staged-set path so the local hook's output is unchanged;
    in explicit mode it is read as "present in the change set", not literally staged.
    """
    if PERF_DOC in files:
        print(f"VERDICT=OK ({PERF_DOC} is staged)")
        return 0

    touched = hot(files)
    if not touched:
        print("VERDICT=OK (no measured hot path is staged)")
        return 0

    print("VERDICT=NEEDS_PERF_ROW")
    print()
    print(f"Staged on a measured hot path, but {PERF_DOC} is not staged:")
    for path in touched:
        print(f"  {path}")
    print()
    print(f'Add a row under "What each change did" in {PERF_DOC}. If the change was not')
    print("measured, write `not measured` and one sentence saying why.")
    print()
    print("A row is required even when the answer is 'nothing moved': say that, and why.")
    print("The cost of the row is one line; the cost of a missing row is a file that is")
    print("silently wrong about what the code costs.")
    return 1


def check() -> int:
    """Print a verdict for the staged set and return the process exit code."""
    if in_merge():
        print("VERDICT=OK (merge commit; the rows were argued in the branch commits)")
        return 0
    return check_files(staged_files())


def read_file_list(source: str) -> list[str]:
    """Read newline-separated paths from `source`; `-` means stdin. Ignore blank lines."""
    text = sys.stdin.read() if source == "-" else pathlib.Path(source).read_text()
    return [line for line in text.splitlines() if line.strip()]


def self_test() -> int:
    """Exercise the check against throwaway repositories, so the check itself is tested."""
    script = str(pathlib.Path(__file__).resolve())
    hot_path = "crates/graph/src/index/mod.rs"
    cold_path = "crates/graph/src/domain/block.rs"

    # Staged-set cases exercise the local hook. Explicit-list cases deliberately
    # stage nothing, proving they read only the supplied list and never the index.
    cases = (
        dict(name="hot path, no doc", stage=(hot_path,), want=1),
        dict(name="hot path, with doc", stage=(hot_path, PERF_DOC), want=0),
        dict(name="cold path, no doc", stage=(cold_path,), want=0),
        dict(name="doc alone", stage=(PERF_DOC,), want=0),
        dict(name="nothing staged", want=0),
        dict(name="--files: hot path, no doc", argv=("--files", hot_path), want=1),
        dict(name="--files: hot path, with doc", argv=("--files", hot_path, PERF_DOC), want=0),
        dict(name="--files: cold path only", argv=("--files", cold_path), want=0),
        dict(name="--files-from -: hot, no doc", argv=("--files-from", "-"), stdin=hot_path + "\n", want=1),
        dict(name="--files-from -: blanks + cold", argv=("--files-from", "-"), stdin="\n\n" + cold_path + "\n\n", want=0),
        dict(name="--files-from FILE: hot, no doc", argv=("--files-from", "list.txt"), list_file=("list.txt", hot_path + "\n"), want=1),
        dict(
            name="--files: document directory lookalikes are cold",
            argv=("--files", "crates/graph/src/document/edits/mod.rs", "crates/graph/src/document/facts_notes.rs"),
            want=0,
        ),
    )
    cases += tuple(
        case
        for path in (
            "crates/graph/src/document/edit/mod.rs",
            "crates/graph/src/document/edit/tests/local.rs",
            "crates/graph/src/document/facts/mod.rs",
            "crates/graph/src/document/facts/properties.rs",
        )
        for case in (
            dict(name=f"{path}, no doc", stage=(path,), want=1),
            dict(name=f"{path}, with doc", stage=(path, PERF_DOC), want=0),
            dict(name=f"--files: {path}, no doc", argv=("--files", path), want=1),
        )
    )

    failures = 0
    for case in cases:
        failures += not run_case(case, script, clean_git_env())

    # A linked worktree's pre-commit hook exports an absolute GIT_DIR and GIT_INDEX_FILE,
    # where a plain repository exports the relative `.git/index`. Run one case with
    # those set in *this* process, which is what the throwaway repos used to inherit:
    # they must still build correctly, and nothing may appear where the variables point.
    with tempfile.TemporaryDirectory() as hostile:
        foreign_git_dir = pathlib.Path(hostile) / "gitdir"
        foreign_index = pathlib.Path(hostile) / "index"
        hostile_vars = {"GIT_DIR": str(foreign_git_dir), "GIT_INDEX_FILE": str(foreign_index)}
        saved = {key: os.environ.get(key) for key in hostile_vars}
        os.environ.update(hostile_vars)
        try:
            failures += not run_case(
                dict(
                    name="hot path, no doc, with the hook's GIT_DIR + GIT_INDEX_FILE set",
                    stage=(hot_path,),
                    want=1,
                    absent=(foreign_index, foreign_git_dir),
                ),
                script,
                clean_git_env(),
            )
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value

    total = len(cases) + 1
    print(f"\nVERDICT={'OK' if failures == 0 else 'FAILED'} ({total - failures}/{total})")
    return 1 if failures else 0


def run_case(case: dict, script: str, env: dict[str, str]) -> bool:
    """Build a throwaway repo, drive `script` in it once, and report whether it matched."""
    expected = case["want"]
    output: tuple[str, str] | None = None
    try:
        with TemporaryRepo() as root:
            for path in case.get("stage", ()):
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("x\n")
                subprocess.run(("git", "add", path), cwd=root, check=True, env=clean_git_env())

            list_file = case.get("list_file")
            if list_file:
                (root / list_file[0]).write_text(list_file[1])

            result = subprocess.run(
                (sys.executable, script, *case.get("argv", ())),
                cwd=root,
                input=case.get("stdin"),
                capture_output=True,
                text=True,
                env=env,
            )
            output = (result.stdout, result.stderr)
            ok = result.returncode == expected
            detail = f"exit {result.returncode}, wanted {expected}"
    except subprocess.CalledProcessError as error:
        # Building the throwaway repo must not depend on the caller's git environment;
        # when it does, blame the command instead of dying in a traceback.
        ok = False
        detail = f"{' '.join(map(str, error.cmd))} exited {error.returncode}, wanted a usable repo"

    for path in case.get("absent", ()):
        if path.exists():
            ok = False
            detail += f"; but {path} was created"
    print(f"{'ok  ' if ok else 'FAIL'} {case['name']}: {detail}")
    if not ok and output:
        print(output[0], output[1])
    return ok


class TemporaryRepo:
    """A git repository in a temporary directory with one initial commit.

    Every git call runs with `clean_git_env()`: `cwd=` only picks a working directory,
    it does not stop git from honouring `GIT_DIR` / `GIT_INDEX_FILE` inherited from the
    caller, which in a linked worktree hook point at that worktree's real metadata.
    """

    def __enter__(self) -> pathlib.Path:
        self._tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self._tmp.name)
        env = clean_git_env()

        def run(*args: str) -> None:
            subprocess.run(args, cwd=root, check=True, capture_output=True, env=env)

        run("git", "init", "-q")
        run("git", "config", "user.email", "self-test@example.com")
        run("git", "config", "user.name", "self test")
        run("git", "config", "commit.gpgsign", "false")
        (root / "README.md").write_text("x\n")
        run("git", "add", "README.md")
        run("git", "commit", "-qm", "initial")
        return root

    def __exit__(self, *_: object) -> None:
        self._tmp.cleanup()


if __name__ == "__main__":
    argv = sys.argv[1:]
    if "--self-test" in argv:
        sys.exit(self_test())
    if argv and argv[0] == "--files":
        sys.exit(check_files(argv[1:]))
    if argv and argv[0] == "--files-from":
        if len(argv) != 2:
            print("usage: check_perf_doc.py --files-from PATH", file=sys.stderr)
            sys.exit(2)
        sys.exit(check_files(read_file_list(argv[1])))
    sys.exit(check())
