#!/usr/bin/env python3
"""Run clippy on the backend and compare its warnings against a baseline.

The repository is not clippy-clean: a handful of warnings predate the lint
being enabled, and fixing them is a separate change from whatever you are
working on. `-D warnings` would therefore be red on every PR and get ignored,
which is worse than no check at all.

So instead: every known warning is recorded per lint in
`scripts/clippy_baseline.json`, and this script fails only when a lint appears
more often than the baseline allows. Introducing a warning fails; fixing one
succeeds and tells you to lower the baseline.

Usage (from anywhere in the repo):
    python3 scripts/clippy_baseline.py
    python3 scripts/clippy_baseline.py --update   # rewrite the baseline
"""

from __future__ import annotations

import argparse
import collections
import json
import pathlib
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
BACKEND = REPO_ROOT / "harnesswurm" / "backend"
BASELINE_PATH = pathlib.Path(__file__).resolve().parent / "clippy_baseline.json"
CLIPPY = ["cargo", "clippy", "--lib", "--bins", "--message-format=json"]


def collect_warnings() -> tuple[collections.Counter, dict[str, str]]:
    """Return (count per lint, one example message per lint)."""
    proc = subprocess.run(CLIPPY, cwd=BACKEND, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(f"clippy failed with exit code {proc.returncode}")

    counts: collections.Counter = collections.Counter()
    examples: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") != "compiler-message":
            continue
        message = record["message"]
        if message.get("level") != "warning":
            continue
        code = message.get("code")
        # Summary lines ("3 warnings emitted") carry no lint code; a real
        # warning always does.
        if not code:
            continue
        lint = code["code"]
        counts[lint] += 1
        examples.setdefault(lint, message.get("rendered") or message["message"])
    return counts, examples


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the baseline from the current warnings",
    )
    args = parser.parse_args()

    counts, examples = collect_warnings()

    if args.update:
        BASELINE_PATH.write_text(
            json.dumps(dict(sorted(counts.items())), indent=2) + "\n"
        )
        print(f"Baseline written: {sum(counts.values())} warning(s) across "
              f"{len(counts)} lint(s).")
        return 0

    baseline: dict[str, int] = json.loads(BASELINE_PATH.read_text())

    introduced = {
        lint: (count, baseline.get(lint, 0))
        for lint, count in counts.items()
        if count > baseline.get(lint, 0)
    }
    fixed = {
        lint: (counts.get(lint, 0), allowed)
        for lint, allowed in baseline.items()
        if counts.get(lint, 0) < allowed
    }

    for lint, (now, allowed) in sorted(fixed.items()):
        print(f"fixed: {lint} is now {now}, baseline allows {allowed}")
    if fixed:
        print(
            "\nNice — please lower the baseline in the same PR:\n"
            "    python3 scripts/clippy_baseline.py --update\n"
        )

    if not introduced:
        print(f"OK: {sum(counts.values())} clippy warning(s), all known.")
        return 0

    print("\nNew clippy warnings — please fix these before merging:\n")
    for lint, (now, allowed) in sorted(introduced.items()):
        print(f"  {lint}: {now} occurrence(s), baseline allows {allowed}")
        print("    " + examples[lint].strip().replace("\n", "\n    ") + "\n")
    print(
        "If a warning is genuinely pre-existing and not yours, record it with\n"
        "    python3 scripts/clippy_baseline.py --update\n"
        "and say so in the PR — a growing baseline should be a deliberate,\n"
        "reviewable diff, not a silent one."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
