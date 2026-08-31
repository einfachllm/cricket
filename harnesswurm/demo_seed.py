#!/usr/bin/env python3
"""Fill the Harnesswurm database with one agent session per status, plus one
experiment comparing three agents on the same issue, so the Agents and
Analytics views can be seen working without wiring up a real coding agent (or
spending anything on API calls).

Every row it writes uses a `demo-` session id prefix and can be removed again
with `--clear`, so it never gets confused with real captured traffic.

Usage:
    cd harnesswurm/backend && cargo run          # once, to create the schema
    python3 ../demo_seed.py                      # seed demo sessions
    python3 ../demo_seed.py --clear              # remove them again
    python3 ../demo_seed.py --db /path/to/harnesswurm.db
"""

import argparse
import os
import sqlite3
import sys

SESSION_PREFIX = "demo-"
# The one experiment the comparison view is seeded against.
EXPERIMENT_NAME = "demo-issue-1284"


def connect(db_path):
    if not os.path.exists(db_path):
        sys.exit(
            f"No database at {db_path}.\n"
            "Start the backend once first so it can create the schema:\n"
            "    cd harnesswurm/backend && cargo run"
        )
    db = sqlite3.connect(db_path)
    columns = {row[1] for row in db.execute("PRAGMA table_info(tasks)")}
    if "status" not in columns:
        sys.exit(
            f"The database at {db_path} predates status tracking.\n"
            "Start the current backend once to upgrade the schema in place."
        )
    return db


def clear(db):
    ids = [row[0] for row in db.execute(
        "SELECT id FROM tasks WHERE session_id LIKE ?", (SESSION_PREFIX + "%",)
    )]
    for table in ("metrics", "traffic", "rate_limits", "task_tools"):
        if table_exists(db, table):
            db.executemany(f"DELETE FROM {table} WHERE task_id = ?", [(i,) for i in ids])
    db.execute("DELETE FROM tasks WHERE session_id LIKE ?", (SESSION_PREFIX + "%",))
    # Verdicts outlive the calls they judge (they key on agent + session, not
    # task id), so they need clearing by the same prefix or a re-seed would
    # inherit the last run's answers.
    if table_exists(db, "session_verdicts"):
        db.execute("DELETE FROM session_verdicts WHERE session_id LIKE ?", (SESSION_PREFIX + "%",))
    db.execute("DELETE FROM experiments WHERE name = ?", (EXPERIMENT_NAME,))
    db.commit()
    return len(ids)


def table_exists(db, name):
    return db.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (name,)
    ).fetchone() is not None


def agent_id(db, name):
    db.execute("INSERT OR IGNORE INTO agents (name) VALUES (?)", (name,))
    return db.execute("SELECT id FROM agents WHERE name = ?", (name,)).fetchone()[0]


def experiment_id(db, name):
    db.execute("INSERT OR IGNORE INTO experiments (name, description) VALUES (?, ?)",
               (name, "Same issue, three agents — which one solved it cheaper?"))
    return db.execute("SELECT id FROM experiments WHERE name = ?", (name,)).fetchone()[0]


def set_verdict(db, agent, session, verdict, note=None):
    """Records whether a run solved its task — the judgement the proxy can't make."""
    db.execute(
        "INSERT OR REPLACE INTO session_verdicts (agent_id, session_id, verdict, note)"
        " VALUES (?, ?, ?, ?)",
        (agent_id(db, agent), SESSION_PREFIX + session, verdict, note),
    )


def add_call(db, agent, session, *, status, model, provider, task, started_ago,
             finished_ago=None, http_status=200, stop_reason=None, awaiting=0,
             question=None, error_type=None, error_message=None,
             tokens=(0, 0, 0, 0), cost=None, quota=None, duration_ms=3800,
             experiment=None, tool_calls=3, tools=None):
    """Writes one completed (or still-running) call, exactly as the proxy would."""
    finished = "NULL" if finished_ago is None else f"datetime('now', '-{finished_ago} seconds')"
    db.execute(
        "INSERT INTO tasks (agent_id, experiment_id, task_description, session_id, model_name, provider,"
        " timestamp, status, http_status, error_type, error_message, stop_reason,"
        f" awaiting_input, ttfb_ms, duration_ms, finished_at)"
        f" VALUES (?, ?, ?, ?, ?, ?, datetime('now', '-{started_ago} seconds'),"
        f" ?, ?, ?, ?, ?, ?, ?, ?, {finished})",
        (agent_id(db, agent), experiment, task, SESSION_PREFIX + session, model, provider,
         status, http_status, error_type, error_message, stop_reason, awaiting, 380, duration_ms),
    )
    task_id = db.execute("SELECT last_insert_rowid()").fetchone()[0]

    prompt, completion, cache_write, cache_read = tokens
    db.execute(
        "INSERT INTO metrics (task_id, prompt_tokens, completion_tokens, cache_creation_tokens,"
        " cache_read_tokens, tool_calls_count, latency_ms, cost_estimate)"
        " VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        (task_id, prompt, completion, cache_write, cache_read, tool_calls, 380, cost),
    )
    # Which tools the turn called, so the breakdown can attribute its spend.
    for name, count in (tools or {}).items():
        db.execute(
            "INSERT INTO task_tools (task_id, tool_name, call_count) VALUES (?, ?, ?)",
            (task_id, name, count))
    if question:
        db.execute(
            "INSERT INTO traffic (task_id, agent_question_tool, agent_question_text)"
            " VALUES (?, 'AskUserQuestion', ?)", (task_id, question))
    if quota:
        db.execute(
            "INSERT INTO rate_limits (task_id, provider, requests_limit, requests_remaining,"
            " tokens_limit, tokens_remaining, retry_after_s, observed_at)"
            " VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now', '-10 seconds'))",
            (task_id, provider, quota.get("requests_limit"), quota.get("requests_remaining"),
             quota.get("tokens_limit"), quota.get("tokens_remaining"), quota.get("retry_after_s")))
    return task_id


def seed(db):
    # Waiting on a human, with the question it actually asked.
    add_call(db, "claude-code", "auth-refactor", status="ok", model="claude-opus-5",
             provider="anthropic", started_ago=95, finished_ago=40, stop_reason="tool_use",
             awaiting=1, question="Should refresh-token rotation live in the middleware, or the session store?",
             task="Refactor the auth middleware so token refresh is transparent to handlers",
             tokens=(12000, 800, 4000, 60000), cost=0.14,
             quota={"requests_limit": 1000, "requests_remaining": 780,
                    "tokens_limit": 80000, "tokens_remaining": 41000})

    # Mid tool loop: busy, not blocked on you.
    add_call(db, "aider", "docs-pass", status="ok", model="gpt-4o", provider="openai",
             started_ago=25, finished_ago=18, stop_reason="tool_calls",
             task="Rewrite the README quickstart for the new CLI flags",
             tokens=(4200, 320, 0, 18000), cost=0.031)

    # Blocked by the provider, with its own retry window.
    add_call(db, "opencode", "orm-migration", status="rate_limited", model="claude-opus-5",
             provider="anthropic", started_ago=30, finished_ago=25, http_status=429,
             error_type="rate_limit_error",
             error_message="Number of request tokens has exceeded your per-minute rate limit",
             task="Port the remaining models off the legacy ORM",
             quota={"retry_after_s": 90})

    # Something the human has to go fix.
    add_call(db, "cursor", "graphql-spike", status="error", model="gpt-4o", provider="openai",
             started_ago=700, finished_ago=698, http_status=401,
             error_type="authentication_error", error_message="Incorrect API key provided",
             task="Spike a GraphQL gateway in front of the REST services")

    # Finished its turn a while ago and has been sitting there since.
    add_call(db, "kilo", "symbol-rename", status="ok", model="gpt-4o", provider="openai",
             started_ago=5400, finished_ago=5380, stop_reason="stop", awaiting=1,
             task="Rename the Widget* symbols across the workspace",
             tokens=(88000, 5200, 0, 240000), cost=1.87)

    # Still running. Seeded last and left open, so it shows as "Thinking" —
    # note the backend closes open calls out on startup, so seed after booting.
    add_call(db, "kilo", "flaky-e2e", status="in_flight", model="gpt-4o", provider="openai",
             started_ago=35, task="Work out why the checkout e2e test fails only in CI",
             tokens=(12000, 800, 4000, 60000), cost=0.14)

    seed_comparison(db)
    db.commit()


def seed_comparison(db):
    """Three agents on one issue, for the Analytics comparison view.

    Deliberately arranged so cost alone gives the wrong answer twice over:
    `cursor` is by far the cheapest run and also the one that gave up, and
    `kilo` burned a failed attempt before the run that worked — so its cost
    per fix is higher than its winning run's price tag. Only the verdicts
    below separate "spent least" from "solved it for least".
    """
    experiment = experiment_id(db, EXPERIMENT_NAME)
    issue = "Fix #1284: session cookie is dropped on the OAuth callback"

    # A first attempt that went nowhere. It is still kilo's money, so the
    # per-agent roll-up charges it to kilo's eventual success.
    seed_run(db, experiment, issue, "kilo", "cmp-kilo-1st", "gpt-4o", "openai",
             started_ago=1200, turns=[
                 # (prompt, completion, cache_write, cache_read), cost, tools
                 ((8000, 600, 0, 15000), 0.022, {"read_file": 2, "grep": 1}),
             ])
    set_verdict(db, "kilo", "cmp-kilo-1st", "failed", "Chased a red herring in the CSRF filter")

    # Cheap and correct: small model, heavy cache reuse, few turns. The token
    # mix walks the arc the phase view exists to show — a cold, input-heavy
    # read-in, then a warm cache and progressively more generation.
    seed_run(db, experiment, issue, "kilo", "cmp-kilo", "gpt-4o", "openai",
             started_ago=900, turns=[
                 ((9000, 200, 6000, 0), 0.012, {"read_file": 3, "grep": 2}),
                 ((2000, 300, 0, 14000), 0.010, {"read_file": 2, "grep": 1}),
                 ((1500, 500, 0, 16000), 0.010, {"read_file": 1, "bash": 1}),
                 ((1200, 900, 0, 18000), 0.011, {"edit_file": 1}),
                 ((1000, 1400, 0, 20000), 0.012, {"edit_file": 1, "bash": 1}),
                 # No tools on the last turn: it answered. Turns like this are
                 # deliberately attributed to nothing in the tool breakdown.
                 ((800, 1800, 0, 21000), 0.013, {}),
             ])
    set_verdict(db, "kilo", "cmp-kilo", "solved", "Cookie set on the callback; e2e green")

    # Also correct, but it read far more of the repo to get there — the same
    # arc an order of magnitude wider.
    seed_run(db, experiment, issue, "claude-code", "cmp-claude", "claude-opus-5", "anthropic",
             started_ago=880, turns=[
                 ((30000, 400, 20000, 0), 0.20, {"read_file": 6, "grep": 4}),
                 ((5000, 600, 0, 60000), 0.14, {"read_file": 5, "grep": 3}),
                 ((4000, 800, 0, 80000), 0.15, {"read_file": 4, "bash": 1}),
                 ((3500, 1000, 0, 95000), 0.15, {"read_file": 2, "edit_file": 1}),
                 ((3000, 1500, 0, 110000), 0.16, {"edit_file": 2, "bash": 1}),
                 ((2500, 2200, 0, 120000), 0.16, {"edit_file": 1, "run_tests": 1}),
                 ((2000, 2800, 0, 130000), 0.15, {"run_tests": 2}),
                 ((1500, 3000, 0, 135000), 0.08, {}),
             ])
    set_verdict(db, "claude-code", "cmp-claude", "solved", "Same fix, plus a regression test")

    # The trap: cheapest of the three, because it stopped after two turns
    # without a working fix. Two calls means two phase slices, not five.
    seed_run(db, experiment, issue, "cursor", "cmp-cursor", "gpt-4o", "openai",
             started_ago=870, turns=[
                 ((6000, 400, 0, 9000), 0.008, {"read_file": 2}),
                 ((7000, 500, 0, 11000), 0.009, {"edit_file": 1}),
             ])
    set_verdict(db, "cursor", "cmp-cursor", "failed", "Edited the wrong middleware, gave up")


def seed_run(db, experiment, task, agent, session, model, provider, *, started_ago, turns):
    """Writes one run as a sequence of turns, 30s apart, newest last.

    Taking the turns as a list rather than one repeated call is what lets the
    seeded data have a *shape* — the phase breakdown has nothing to show if
    every turn of a run is identical.
    """
    anthropic = provider == "anthropic"
    for index, (tokens, cost, tools) in enumerate(turns):
        last = index == len(turns) - 1
        ago = started_ago - index * 30
        add_call(db, agent, session, status="ok", model=model, provider=provider,
                 task=task, started_ago=ago, finished_ago=ago - 20,
                 stop_reason=("end_turn" if anthropic else "stop") if last
                 else ("tool_use" if anthropic else "tool_calls"),
                 awaiting=1 if last else 0,
                 tokens=tokens, cost=cost, experiment=experiment,
                 tool_calls=sum(tools.values()), tools=tools)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", default="harnesswurm.db",
                        help="path to harnesswurm.db (default: ./harnesswurm.db)")
    parser.add_argument("--clear", action="store_true",
                        help="remove previously seeded demo sessions and exit")
    args = parser.parse_args()

    db = connect(args.db)
    removed = clear(db)
    if args.clear:
        print(f"Removed {removed} demo call(s) from {args.db}")
        return

    seed(db)
    print(
        f"Seeded demo sessions into {args.db}"
        + (f" (replacing {removed} previous demo call(s))" if removed else "")
        + "\nOpen the Agents view — every status should be visible."
        "\nRemove them again with: python3 demo_seed.py --clear"
    )


if __name__ == "__main__":
    main()
