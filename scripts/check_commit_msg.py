#!/usr/bin/env python3
"""Check a commit message's subject against the repository's `<area>: <change>` rule.

Run by the `commit-msg` prek hook with the message file git passes as the only argument, and by the
`commit-subject-range` pre-push hook over every commit a push carries:

    python3 scripts/check_commit_msg.py .git/COMMIT_EDITMSG      # commit-msg: one message
    python3 scripts/check_commit_msg.py --range BASE...HEAD      # pre-push: every new commit

The rule lives in `docs/commit-message.md`. The check is deliberately narrow: it validates the
subject shape and nothing else, because a rule that rejects half the real history teaches people to
bypass it. Merge commits, reverts, and fixup/squash commits are exempt — their subjects are
generated or quote another commit's.

A range that cannot be resolved is a failure, not a pass: a check that silently skips itself when
the remote-tracking ref is missing is a check nobody can rely on.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

AREAS = (
    "build",
    "chore",
    "ci",
    "docs",
    "feat",
    "fix",
    "merge",
    "perf",
    "refactor",
    "revert",
    "test",
    "wip",
)
SCOPE = r"[a-z0-9_-]+"
SUBJECT = re.compile(rf"^({'|'.join(AREAS)})(\({SCOPE}\))?: \S")
GENERATED = (
    re.compile(r"^Merge (remote-tracking )?branch "),
    re.compile(r"^Merge (remote-tracking )?pull request "),
    re.compile(r'^Revert "'),
    re.compile(r"^(fixup|squash)! "),
)

OK_VERDICT = "VERDICT=OK"
REFUSE_VERDICT = "VERDICT=BAD_SUBJECT"
BAD_RANGE_VERDICT = "VERDICT=BAD_RANGE"


def exempt(subject: str) -> bool:
    """Git-generated subjects whose shape is not ours to demand."""
    return any(pattern.match(subject) for pattern in GENERATED)


def check_subject(subject: str) -> str | None:
    """The reason a subject is refused, or `None` when it passes."""
    if not subject.strip():
        return "the subject line is empty"
    if exempt(subject):
        return None
    if SUBJECT.match(subject) is None:
        areas = ", ".join(AREAS)
        return (
            f"the subject must start with `<area>: ` or `<area>(<scope>): ` "
            f"(areas: {areas}); see docs/commit-message.md"
        )
    return None


def check_message_file(path: pathlib.Path) -> int:
    """The `commit-msg` hook entry: check the message git is about to store."""
    # Comment lines (`# ...`) are editor annotations, not part of the message; git's cleanup
    # strips them, so the check must not see them either. Leading blank lines are the editor's
    # cursor position, not content; a subject cannot start on line 2.
    lines = [
        line
        for line in path.read_text(encoding="utf-8").splitlines()
        if not line.startswith("#")
    ]
    while lines and not lines[0].strip():
        lines.pop(0)
    if not lines:
        print(REFUSE_VERDICT)
        print("REASON=the commit message is empty (or only comments)")
        return 1
    subject = lines[0]
    if len(lines) > 1 and lines[1].strip():
        print(REFUSE_VERDICT)
        print("REASON=the subject runs on: separate it from the body with one blank line")
        print(f"SUBJECT={subject}")
        print(f"SECOND_LINE={lines[1]}")
        return 1
    reason = check_subject(subject)
    if reason is None:
        print(OK_VERDICT)
        return 0
    print(REFUSE_VERDICT)
    print(f"REASON={reason}")
    print(f"SUBJECT={subject}")
    return 1


def git(*args: str) -> str:
    """Run git in the current repository and return stdout."""
    return subprocess.run(("git", *args), check=True, capture_output=True, text=True).stdout


def check_range(rev_range: str) -> int:
    """Check every commit subject in a revision range."""
    try:
        log = git("log", "--format=%H%x1f%s%x1e", rev_range)
    except subprocess.CalledProcessError as error:
        # Fail closed: an unresolvable base means the check could not look, and a check that
        # reports OK because it did not look is worse than no check at all.
        stderr = (error.stderr or "").strip()
        print(BAD_RANGE_VERDICT)
        print(f"REASON=could not read the range {rev_range!r}: {stderr}")
        print("Fetch the base ref (git fetch origin) or pass an explicit --range BASE...HEAD.")
        return 1
    failures: list[str] = []
    for record in log.split("\x1e"):
        record = record.strip()
        if not record:
            continue
        sha, subject = record.split("\x1f")
        reason = check_subject(subject)
        if reason is not None:
            failures.append(f"{sha[:10]} {subject} — {reason}")
    if failures:
        print(REFUSE_VERDICT)
        for failure in failures:
            print(f"BAD={failure}")
        print("Fix the subjects (see docs/commit-message.md), then rebase or amend before pushing.")
        return 1
    print(OK_VERDICT)
    return 0


def self_test() -> int:
    """Exercise the subject check against known-good and known-bad subjects."""
    cases = (
        ("docs: the rows name the commits", None),
        ("feat(server): one JSON-RPC surface", None),
        ("build(graph): enforce the P1 lints", None),
        ("merge: where a per-edit write's time goes", None),
        ("Merge branch 'test/official-parity'", None),
        ('Revert "docs: something"', None),
        ("fixup! docs: something", None),
        ("update stuff", "must start with"),
        ("docs:no space after the colon", "must start with"),
        ("docs: ", "must start with"),  # refused; the exact wording is not pinned
        ("WIP: half-done thing", "must start with"),
        ("feat(Server): one JSON-RPC surface", "must start with"),
        ("chore: refuse a hot-path commit", None),
        ("chore(justfile): run the local binary", None),
    )
    failures = 0
    for subject, expected in cases:
        reason = check_subject(subject)
        refused = reason is not None
        wanted = expected is not None
        # `docs: ` is refused for the empty subject; either refusal reason is a pass.
        ok = refused == wanted and (reason is None or expected is None or expected in reason)
        print(f"{'ok  ' if ok else 'FAIL'} {subject!r}: reason={reason!r}")
        if not ok:
            failures += 1

    # The file mode: comment stripping and the run-on-subject rule.
    with tempfile() as tmp:
        root = pathlib.Path(tmp)
        message = root / "msg"
        message.write_text("# the editor's comment\n\ndocs: real subject\n", encoding="utf-8")
        ok = check_message_file(message) == 0
        print(f"{'ok  ' if ok else 'FAIL'} comment lines are stripped")
        failures += not ok

        message.write_text("docs: a subject\nthat runs on\n", encoding="utf-8")
        ok = check_message_file(message) == 1
        print(f"{'ok  ' if ok else 'FAIL'} a run-on subject is refused")
        failures += not ok

        message.write_text("docs: a subject\n\nand a body\n", encoding="utf-8")
        ok = check_message_file(message) == 0
        print(f"{'ok  ' if ok else 'FAIL'} a subject with a body passes")
        failures += not ok

    print(f"\nVERDICT={'OK' if failures == 0 else 'FAILED'}")
    return 1 if failures else 0


def tempfile():
    import tempfile as _mod

    return _mod.TemporaryDirectory()


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    if argv and argv[0] == "--range" and len(argv) == 2:
        return check_range(argv[1])
    if len(argv) == 1:
        return check_message_file(pathlib.Path(argv[0]))
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
