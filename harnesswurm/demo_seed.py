#!/usr/bin/env python3
"""Fill the Harnesswurm database with one agent session per status, so the
Agents view can be seen working without wiring up a real coding agent (or
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
    for table in ("metrics", "traffic", "rate_limits"):
        db.executemany(f"DELETE FROM {table} WHERE task_id = ?", [(i,) for i in ids])
    db.execute("DELETE FROM tasks WHERE session_id LIKE ?", (SESSION_PREFIX + "%",))
    db.commit()
    return len(ids)


def agent_id(db, name):
    db.execute("INSERT OR IGNORE INTO agents (name) VALUES (?)", (name,))
    return db.execute("SELECT id FROM agents WHERE name = ?", (name,)).fetchone()[0]


def add_call(db, agent, session, *, status, model, provider, task, started_ago,
             finished_ago=None, http_status=200, stop_reason=None, awaiting=0,
             question=None, error_type=None, error_message=None,
             tokens=(0, 0, 0, 0), cost=None, quota=None, duration_ms=3800):
    """Writes one completed (or still-running) call, exactly as the proxy would."""
    finished = "NULL" if finished_ago is None else f"datetime('now', '-{finished_ago} seconds')"
    db.execute(
        "INSERT INTO tasks (agent_id, task_description, session_id, model_name, provider,"
        " timestamp, status, http_status, error_type, error_message, stop_reason,"
        f" awaiting_input, ttfb_ms, duration_ms, finished_at)"
        f" VALUES (?, ?, ?, ?, ?, datetime('now', '-{started_ago} seconds'),"
        f" ?, ?, ?, ?, ?, ?, ?, ?, {finished})",
        (agent_id(db, agent), task, SESSION_PREFIX + session, model, provider,
         status, http_status, error_type, error_message, stop_reason, awaiting, 380, duration_ms),
    )
    task_id = db.execute("SELECT last_insert_rowid()").fetchone()[0]

    prompt, completion, cache_write, cache_read = tokens
    db.execute(
        "INSERT INTO metrics (task_id, prompt_tokens, completion_tokens, cache_creation_tokens,"
        " cache_read_tokens, tool_calls_count, latency_ms, cost_estimate)"
        " VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        (task_id, prompt, completion, cache_write, cache_read, 3, 380, cost),
    )
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

    db.commit()


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
