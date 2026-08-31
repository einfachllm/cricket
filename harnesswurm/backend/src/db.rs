use sqlx::{sqlite::SqlitePool, Row};
use anyhow::Result;
use serde_json::{json, Value};

use crate::rate_limits::RateLimitSnapshot;
use crate::session_state::{self, LastCall, STATUS_IN_FLIGHT, STATUS_INTERRUPTED};

/// Everything learned about a call once its response is complete. Grouped
/// into one struct rather than passed as a dozen positional arguments, where
/// two adjacent `Option<String>`s would be trivial to swap by accident.
#[derive(Debug, Clone, Default)]
pub struct TaskOutcome {
    pub status: String,
    pub http_status: Option<i64>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub stop_reason: Option<String>,
    pub awaiting_input: bool,
    /// Time to the response headers. For a stream this is time-to-first-byte;
    /// for a unary call it's effectively the whole call.
    pub ttfb_ms: Option<i64>,
    /// Wall-clock time until the response was fully consumed. Only this
    /// number answers "how long was the agent actually blocked" on a stream.
    pub duration_ms: Option<i64>,
}

/// How many recent quota readings to fold when answering "what are my
/// limits". Bounded so the scan stays cheap; far more than enough to find a
/// populated value for each field, since providers send the headers on
/// nearly every successful call.
const RATE_LIMIT_SCAN_ROWS: i64 = 200;

/// The run did what it was asked: the change works, the issue is fixed.
pub const VERDICT_SOLVED: &str = "solved";
/// The run did not — it gave up, went in circles, or produced something wrong.
pub const VERDICT_FAILED: &str = "failed";

/// Whether a string is a verdict the comparison understands. Checked at the
/// edge so an unrecognised value is a 400 rather than a row that quietly
/// matches nothing when the comparison later filters on it.
pub fn is_valid_verdict(verdict: &str) -> bool {
    verdict == VERDICT_SOLVED || verdict == VERDICT_FAILED
}

/// What counts as one *run* — the unit the comparison ranks and the phase
/// breakdown slices.
///
/// There is no single right answer, because `X-Session-ID` means different
/// things to different agents:
///
/// - `Session` trusts it to mark one attempt at the task. Right when it is
///   stable for the length of a task, and the only way to sit several
///   deliberate repeats of the same task side by side under one experiment —
///   which is the whole point of taking more than one sample.
/// - `Agent` treats everything one agent did under the experiment as a single
///   run. Some agents mint a fresh session id per *session* rather than per
///   task — a restart, a context compaction, reopening the editor — and under
///   `Session` that shatters one attempt into a row per fragment, then
///   crowns the cheapest fragment as the winner.
///
/// So it is a choice the caller makes, not something to infer: only the
/// person running the agent knows which their session ids are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunGrouping {
    #[default]
    Session,
    Agent,
}

impl RunGrouping {
    /// Anything unrecognised falls back to per-session, the narrower reading:
    /// it over-reports runs rather than silently merging attempts that were
    /// meant to stay apart.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("agent") => RunGrouping::Agent,
            _ => RunGrouping::Session,
        }
    }

    /// The SQL expression that identifies a run within an agent. Not caller
    /// input — it comes from this enum — so interpolating it is safe.
    fn run_key_sql(self) -> &'static str {
        match self {
            RunGrouping::Session => "COALESCE(t.session_id, '')",
            RunGrouping::Agent => "''",
        }
    }
}

/// One stored quota reading, with when it was taken.
#[derive(Debug, Clone)]
struct RateLimitReading {
    provider: String,
    observed_at: Option<String>,
    observed_seconds_ago: i64,
    snapshot: RateLimitSnapshot,
}

/// Collapses readings (newest first) into one current view per provider,
/// taking the newest *non-null* value for each field independently.
///
/// Per-field rather than newest-row-wins for a specific reason: a 429
/// response carries `retry-after` but usually no budget headers at all, so
/// letting the newest row win outright would blank the remaining-quota
/// numbers at exactly the moment the human wants to look at them.
///
/// `retry_after` is the one time-sensitive field, so it is converted here
/// into the wait that is actually *left* and dropped once that has elapsed —
/// a stored "retry in 60s" is meaningless an hour later.
fn fold_rate_limits(readings: Vec<RateLimitReading>) -> Vec<Value> {
    let mut providers: Vec<String> = Vec::new();
    let mut folded: Vec<(RateLimitSnapshot, Option<String>, i64)> = Vec::new();

    for reading in readings {
        let index = match providers.iter().position(|p| *p == reading.provider) {
            Some(index) => index,
            None => {
                providers.push(reading.provider.clone());
                // Readings arrive newest-first, so the first one seen for a
                // provider supplies the "as of" timestamp.
                folded.push((RateLimitSnapshot::default(), reading.observed_at.clone(), reading.observed_seconds_ago));
                providers.len() - 1
            }
        };

        let (snapshot, _, _) = &mut folded[index];
        snapshot.requests_limit = snapshot.requests_limit.or(reading.snapshot.requests_limit);
        snapshot.requests_remaining = snapshot.requests_remaining.or(reading.snapshot.requests_remaining);
        snapshot.requests_reset = snapshot.requests_reset.take().or(reading.snapshot.requests_reset);
        snapshot.tokens_limit = snapshot.tokens_limit.or(reading.snapshot.tokens_limit);
        snapshot.tokens_remaining = snapshot.tokens_remaining.or(reading.snapshot.tokens_remaining);
        snapshot.tokens_reset = snapshot.tokens_reset.take().or(reading.snapshot.tokens_reset);
        snapshot.retry_after_s = snapshot.retry_after_s.or_else(|| {
            reading.snapshot.retry_after_s
                .map(|secs| secs - reading.observed_seconds_ago)
                .filter(|remaining| *remaining > 0)
        });
    }

    providers.into_iter().zip(folded).map(|(provider, (snapshot, observed_at, age))| {
        json!({
            "provider": provider,
            "requests_limit": snapshot.requests_limit,
            "requests_remaining": snapshot.requests_remaining,
            "requests_reset": snapshot.requests_reset,
            "tokens_limit": snapshot.tokens_limit,
            "tokens_remaining": snapshot.tokens_remaining,
            "tokens_reset": snapshot.tokens_reset,
            "retry_after_remaining_s": snapshot.retry_after_s,
            "observed_at": observed_at,
            "observed_seconds_ago": age,
        })
    }).collect()
}

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE
            );"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS experiments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                description TEXT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
            );"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id INTEGER NOT NULL,
                experiment_id INTEGER,
                task_description TEXT,
                session_id TEXT,
                model_name TEXT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(agent_id) REFERENCES agents(id),
                FOREIGN KEY(experiment_id) REFERENCES experiments(id)
            );"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id INTEGER NOT NULL,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                tool_calls_count INTEGER,
                latency_ms INTEGER,
                cost_estimate REAL,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS traffic (
                task_id INTEGER PRIMARY KEY,
                request_body TEXT,
                response_body TEXT,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS rate_limits (
                task_id INTEGER PRIMARY KEY,
                provider TEXT,
                requests_limit INTEGER,
                requests_remaining INTEGER,
                requests_reset TEXT,
                tokens_limit INTEGER,
                tokens_remaining INTEGER,
                tokens_reset TEXT,
                retry_after_s INTEGER,
                observed_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );"
        ).execute(&pool).await?;

        // Which tools a turn actually called. `metrics.tool_calls_count`
        // knows how many; this knows which, so a run's spend can be
        // attributed to the tools that drove it.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS task_tools (
                task_id INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                call_count INTEGER NOT NULL,
                PRIMARY KEY (task_id, tool_name),
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );"
        ).execute(&pool).await?;

        // Whether a run actually solved what it was given. Keyed by the run
        // (agent + session), not by call, because that is the grain a human
        // can actually judge: individual calls have no notion of success.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session_verdicts (
                agent_id INTEGER NOT NULL,
                session_id TEXT NOT NULL,
                verdict TEXT NOT NULL,
                note TEXT,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (agent_id, session_id),
                FOREIGN KEY(agent_id) REFERENCES agents(id)
            );"
        ).execute(&pool).await?;

        // Older on-disk databases predate these columns; add them in place so
        // existing installs upgrade without losing history.
        Self::add_column_if_missing(&pool, "tasks", "provider", "TEXT").await?;
        Self::add_column_if_missing(&pool, "metrics", "cache_creation_tokens", "INTEGER DEFAULT 0").await?;
        Self::add_column_if_missing(&pool, "metrics", "cache_read_tokens", "INTEGER DEFAULT 0").await?;
        Self::add_column_if_missing(&pool, "traffic", "agent_question_tool", "TEXT").await?;
        Self::add_column_if_missing(&pool, "traffic", "agent_question_text", "TEXT").await?;
        // Call outcome, so a 429 or a dropped stream is distinguishable from
        // a healthy call that happened to report no tokens.
        Self::add_column_if_missing(&pool, "tasks", "status", "TEXT").await?;
        Self::add_column_if_missing(&pool, "tasks", "http_status", "INTEGER").await?;
        Self::add_column_if_missing(&pool, "tasks", "error_type", "TEXT").await?;
        Self::add_column_if_missing(&pool, "tasks", "error_message", "TEXT").await?;
        Self::add_column_if_missing(&pool, "tasks", "stop_reason", "TEXT").await?;
        Self::add_column_if_missing(&pool, "tasks", "awaiting_input", "INTEGER DEFAULT 0").await?;
        Self::add_column_if_missing(&pool, "tasks", "ttfb_ms", "INTEGER").await?;
        Self::add_column_if_missing(&pool, "tasks", "duration_ms", "INTEGER").await?;
        Self::add_column_if_missing(&pool, "tasks", "finished_at", "DATETIME").await?;

        // The session rollup filters and orders by these on every poll, and
        // the traffic list is always "newest first".
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(agent_id, session_id, id)")
            .execute(&pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_timestamp ON tasks(timestamp DESC)")
            .execute(&pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_experiment ON tasks(experiment_id, agent_id, session_id)")
            .execute(&pool).await?;

        Ok(Self { pool })
    }

    /// Closes out calls left open by a previous process. Nothing survives a
    /// restart, so any row still `in_flight` at startup is by definition a
    /// response that will never arrive — without this sweep those rows would
    /// show as permanently "Thinking" in the dashboard. Returns how many
    /// were closed, which is worth logging: a nonzero count on every boot
    /// means the proxy is being killed mid-call.
    pub async fn reap_in_flight_tasks(&self) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE tasks SET status = ?, finished_at = CURRENT_TIMESTAMP WHERE status = ?"
        )
        .bind(STATUS_INTERRUPTED)
        .bind(STATUS_IN_FLIGHT)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    async fn add_column_if_missing(pool: &SqlitePool, table: &str, column: &str, ddl_type: &str) -> Result<()> {
        let existing: Vec<String> = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await?
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect();

        if !existing.iter().any(|name| name == column) {
            sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"))
                .execute(pool)
                .await?;
        }
        Ok(())
    }

    pub async fn get_or_create_agent(&self, name: &str) -> Result<i64> {
        let row = sqlx::query("SELECT id FROM agents WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(r) = row {
            Ok(r.get(0))
        } else {
            let res = sqlx::query("INSERT INTO agents (name) VALUES (?)")
                .bind(name)
                .execute(&self.pool)
                .await?;
            Ok(res.last_insert_rowid())
        }
    }

    pub async fn get_or_create_experiment(&self, name: &str, description: Option<String>) -> Result<i64> {
        let row = sqlx::query("SELECT id FROM experiments WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;

        if let Some(r) = row {
            Ok(r.get(0))
        } else {
            let res = sqlx::query("INSERT INTO experiments (name, description) VALUES (?, ?)")
                .bind(name)
                .bind(description)
                .execute(&self.pool)
            .await?;
            Ok(res.last_insert_rowid())
        }
    }

    /// Opens a call as `in_flight`. The row exists from the moment the
    /// request is forwarded, not once it comes back — that's what makes a
    /// still-running call visible as "Thinking" instead of invisible.
    pub async fn create_task(&self, agent_id: i64, experiment_id: Option<i64>, description: Option<String>, session_id: Option<String>, model: Option<String>, provider: Option<String>) -> Result<i64> {
        let res = sqlx::query(
            "INSERT INTO tasks (agent_id, experiment_id, task_description, session_id, model_name, provider, status) VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(agent_id)
        .bind(experiment_id)
        .bind(description)
        .bind(session_id)
        .bind(model)
        .bind(provider)
        .bind(STATUS_IN_FLIGHT)
        .execute(&self.pool)
        .await?;

        Ok(res.last_insert_rowid())
    }

    /// Closes out a call with everything the response revealed.
    pub async fn finish_task(&self, task_id: i64, outcome: &TaskOutcome) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET
                status = ?,
                http_status = ?,
                error_type = ?,
                error_message = ?,
                stop_reason = ?,
                awaiting_input = ?,
                ttfb_ms = ?,
                duration_ms = ?,
                finished_at = CURRENT_TIMESTAMP
             WHERE id = ?"
        )
        .bind(&outcome.status)
        .bind(outcome.http_status)
        .bind(outcome.error_type.as_deref())
        .bind(outcome.error_message.as_deref())
        .bind(outcome.stop_reason.as_deref())
        .bind(i64::from(outcome.awaiting_input))
        .bind(outcome.ttfb_ms)
        .bind(outcome.duration_ms)
        .bind(task_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn save_rate_limits(&self, task_id: i64, provider: &str, snapshot: &RateLimitSnapshot) -> Result<()> {
        sqlx::query(
            "INSERT INTO rate_limits (task_id, provider, requests_limit, requests_remaining, requests_reset, tokens_limit, tokens_remaining, tokens_reset, retry_after_s)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(task_id) DO UPDATE SET
                provider = excluded.provider,
                requests_limit = excluded.requests_limit,
                requests_remaining = excluded.requests_remaining,
                requests_reset = excluded.requests_reset,
                tokens_limit = excluded.tokens_limit,
                tokens_remaining = excluded.tokens_remaining,
                tokens_reset = excluded.tokens_reset,
                retry_after_s = excluded.retry_after_s"
        )
        .bind(task_id)
        .bind(provider)
        .bind(snapshot.requests_limit)
        .bind(snapshot.requests_remaining)
        .bind(snapshot.requests_reset.as_deref())
        .bind(snapshot.tokens_limit)
        .bind(snapshot.tokens_remaining)
        .bind(snapshot.tokens_reset.as_deref())
        .bind(snapshot.retry_after_s)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn log_metric(
        &self,
        task_id: i64,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_creation_tokens: i64,
        cache_read_tokens: i64,
        tool_calls_count: i64,
        latency_ms: i64,
        cost_estimate: Option<f64>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO metrics (task_id, prompt_tokens, completion_tokens, cache_creation_tokens, cache_read_tokens, tool_calls_count, latency_ms, cost_estimate) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(task_id)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(cache_creation_tokens)
        .bind(cache_read_tokens)
        .bind(tool_calls_count)
        .bind(latency_ms)
        .bind(cost_estimate)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn save_traffic_request(&self, task_id: i64, request_body: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO traffic (task_id, request_body) VALUES (?, ?)
             ON CONFLICT(task_id) DO UPDATE SET request_body = excluded.request_body"
        )
        .bind(task_id)
        .bind(request_body)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_traffic_response(&self, task_id: i64, response_body: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO traffic (task_id, response_body) VALUES (?, ?)
             ON CONFLICT(task_id) DO UPDATE SET response_body = excluded.response_body"
        )
        .bind(task_id)
        .bind(response_body)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records which tools a turn called, deduplicated to one row per name
    /// with a count. Called with every name the response contained, in the
    /// order they appeared, so repeats of the same tool in one turn add up.
    pub async fn save_tool_calls(&self, task_id: i64, tool_names: &[String]) -> Result<()> {
        let mut counts: Vec<(&str, i64)> = Vec::new();
        for name in tool_names {
            match counts.iter_mut().find(|(seen, _)| *seen == name.as_str()) {
                Some((_, count)) => *count += 1,
                None => counts.push((name.as_str(), 1)),
            }
        }

        for (name, count) in counts {
            sqlx::query(
                "INSERT INTO task_tools (task_id, tool_name, call_count) VALUES (?, ?, ?)
                 ON CONFLICT(task_id, tool_name) DO UPDATE SET call_count = excluded.call_count"
            )
            .bind(task_id)
            .bind(name)
            .bind(count)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    pub async fn save_agent_question(&self, task_id: i64, tool_name: &str, question_text: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO traffic (task_id, agent_question_tool, agent_question_text) VALUES (?, ?, ?)
             ON CONFLICT(task_id) DO UPDATE SET
                agent_question_tool = excluded.agent_question_tool,
                agent_question_text = excluded.agent_question_text"
        )
        .bind(task_id)
        .bind(tool_name)
        .bind(question_text)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_all_experiments(&self) -> Result<Vec<(i64, String, Option<String>)>> {
        let rows = sqlx::query("SELECT id, name, description FROM experiments")
            .fetch_all(&self.pool)
            .await?;

        let experiments = rows.iter().map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get(2),
            )
        }).collect();

        Ok(experiments)
    }

    pub async fn get_experiment_metrics(&self, experiment_id: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT
                t.timestamp,
                t.model_name,
                t.provider,
                m.prompt_tokens,
                m.completion_tokens,
                m.cache_creation_tokens,
                m.cache_read_tokens,
                m.tool_calls_count,
                m.latency_ms,
                m.cost_estimate
             FROM tasks t
             JOIN metrics m ON t.id = m.task_id
             WHERE t.experiment_id = ?
             ORDER BY t.timestamp ASC"
        )
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;

        let metrics = rows.iter().map(|row| {
            json!({
                "timestamp": row.get::<String, _>("timestamp"),
                "model_name": row.get::<Option<String>, _>("model_name"),
                "provider": row.get::<Option<String>, _>("provider"),
                "prompt_tokens": row.get::<i64, _>("prompt_tokens"),
                "completion_tokens": row.get::<i64, _>("completion_tokens"),
                "cache_creation_tokens": row.get::<i64, _>("cache_creation_tokens"),
                "cache_read_tokens": row.get::<i64, _>("cache_read_tokens"),
                "tool_calls_count": row.get::<i64, _>("tool_calls_count"),
                "latency_ms": row.get::<i64, _>("latency_ms"),
                "cost_estimate": row.get::<Option<f64>, _>("cost_estimate"),
            })
        }).collect();

        Ok(metrics)
    }

    /// One row per *run* in an experiment — see `RunGrouping` for what a run
    /// is, which depends on whether the agent's session ids mark attempts or
    /// merely sessions.
    ///
    /// This is the grain that answers "which agent solved it cheaper", and
    /// the reason `get_experiment_metrics` cannot: that returns a per-call
    /// time series with no agent on it, so two agents pointed at the same
    /// experiment are indistinguishable once their calls interleave.
    ///
    /// Cost is summed with unpriced calls counted separately rather than
    /// treated as free — a run on a model missing from `pricing.yaml` would
    /// otherwise total $0.00 and win the comparison outright.
    pub async fn get_experiment_comparison(&self, experiment_id: i64, grouping: RunGrouping) -> Result<Vec<Value>> {
        let rows = sqlx::query(&format!(
            "WITH call AS (
                SELECT
                    t.agent_id,
                    -- What identifies a run within this agent, per the caller's
                    -- grouping. Verdicts stay keyed to the real session below,
                    -- so they still aggregate correctly when runs are merged.
                    {run_key} AS run_key,
                    -- session_id is nullable on tasks, but a verdict needs a
                    -- stable key; '' is that key for calls sent without one.
                    COALESCE(t.session_id, '') AS session_key,
                    t.session_id,
                    t.timestamp,
                    t.status,
                    t.model_name,
                    t.provider,
                    t.duration_ms,
                    COALESCE(mm.prompt_tokens, 0) + COALESCE(mm.cache_creation_tokens, 0)
                        + COALESCE(mm.cache_read_tokens, 0) AS input_tokens,
                    COALESCE(mm.completion_tokens, 0) AS output_tokens,
                    COALESCE(mm.cache_read_tokens, 0) AS cache_read_tokens,
                    COALESCE(mm.tool_calls_count, 0) AS tool_calls,
                    mm.cost_estimate
                FROM tasks t
                LEFT JOIN (
                    SELECT task_id,
                           MAX(prompt_tokens) AS prompt_tokens,
                           MAX(completion_tokens) AS completion_tokens,
                           MAX(cache_creation_tokens) AS cache_creation_tokens,
                           MAX(cache_read_tokens) AS cache_read_tokens,
                           MAX(tool_calls_count) AS tool_calls_count,
                           MAX(cost_estimate) AS cost_estimate
                    FROM metrics GROUP BY task_id
                ) mm ON mm.task_id = t.id
                WHERE t.experiment_id = ?
             )
             SELECT
                a.name AS agent_name,
                c.run_key AS session_key,
                -- A run covering exactly one session still has an id worth
                -- showing, however the caller grouped. Above one it has none:
                -- naming an arbitrary member would read as the whole.
                CASE WHEN COUNT(DISTINCT c.session_key) = 1 THEN MAX(c.session_id) END AS session_id,
                COUNT(DISTINCT c.session_key) AS sessions,
                COUNT(*) AS call_count,
                MIN(c.timestamp) AS first_seen,
                MAX(c.timestamp) AS last_seen,
                CAST(strftime('%s', MAX(c.timestamp)) AS INTEGER)
                    - CAST(strftime('%s', MIN(c.timestamp)) AS INTEGER) AS wall_clock_seconds,
                SUM(c.input_tokens) AS input_tokens,
                SUM(c.output_tokens) AS output_tokens,
                SUM(c.cache_read_tokens) AS cache_read_tokens,
                SUM(c.tool_calls) AS tool_calls,
                -- CAST because SQLite infers INTEGER for this SUM when every
                -- call is unpriced, which then fails to decode as an f64.
                CAST(SUM(COALESCE(c.cost_estimate, 0)) AS REAL) AS total_cost,
                SUM(CASE WHEN c.cost_estimate IS NULL THEN 1 ELSE 0 END) AS unpriced_calls,
                SUM(COALESCE(c.duration_ms, 0)) AS busy_ms,
                SUM(CASE WHEN c.status = 'rate_limited' THEN 1 ELSE 0 END) AS rate_limited_calls,
                SUM(CASE WHEN c.status IN ('error', 'overloaded') THEN 1 ELSE 0 END) AS error_calls,
                -- A run with a call still open has not finished spending, so
                -- its total is a running tally rather than a result.
                SUM(CASE WHEN c.status = 'in_flight' THEN 1 ELSE 0 END) AS in_flight_calls,
                GROUP_CONCAT(DISTINCT c.model_name) AS models,
                GROUP_CONCAT(DISTINCT c.provider) AS providers,
                -- Ranked rather than alphabetical: when a merged run covers
                -- several judged sessions, one success makes the attempt a
                -- success. Both notes are carried so the one belonging to the
                -- verdict that won can be shown beside it.
                MAX(CASE v.verdict WHEN 'solved' THEN 2 WHEN 'failed' THEN 1 ELSE 0 END) AS verdict_rank,
                MAX(CASE WHEN v.verdict = 'solved' THEN v.note END) AS solved_note,
                MAX(CASE WHEN v.verdict = 'failed' THEN v.note END) AS failed_note
             FROM call c
             JOIN agents a ON a.id = c.agent_id
             LEFT JOIN session_verdicts v
                    ON v.agent_id = c.agent_id AND v.session_id = c.session_key
             GROUP BY c.agent_id, c.run_key
             ORDER BY total_cost ASC, agent_name ASC",
            run_key = grouping.run_key_sql(),
        ))
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;

        let runs = rows.iter().map(|row| {
            let verdict = match row.get::<Option<i64>, _>("verdict_rank").unwrap_or(0) {
                2 => Some(VERDICT_SOLVED),
                1 => Some(VERDICT_FAILED),
                _ => None,
            };
            json!({
                "agent_name": row.get::<String, _>("agent_name"),
                "session_key": row.get::<String, _>("session_key"),
                "session_id": row.get::<Option<String>, _>("session_id"),
                "sessions": row.get::<i64, _>("sessions"),
                "call_count": row.get::<i64, _>("call_count"),
                "first_seen": row.get::<Option<String>, _>("first_seen"),
                "last_seen": row.get::<Option<String>, _>("last_seen"),
                "wall_clock_seconds": row.get::<Option<i64>, _>("wall_clock_seconds").unwrap_or(0),
                "input_tokens": row.get::<Option<i64>, _>("input_tokens").unwrap_or(0),
                "output_tokens": row.get::<Option<i64>, _>("output_tokens").unwrap_or(0),
                "cache_read_tokens": row.get::<Option<i64>, _>("cache_read_tokens").unwrap_or(0),
                "tool_calls": row.get::<Option<i64>, _>("tool_calls").unwrap_or(0),
                "total_cost": row.get::<Option<f64>, _>("total_cost").unwrap_or(0.0),
                "unpriced_calls": row.get::<Option<i64>, _>("unpriced_calls").unwrap_or(0),
                "busy_ms": row.get::<Option<i64>, _>("busy_ms").unwrap_or(0),
                "rate_limited_calls": row.get::<Option<i64>, _>("rate_limited_calls").unwrap_or(0),
                "error_calls": row.get::<Option<i64>, _>("error_calls").unwrap_or(0),
                "in_flight_calls": row.get::<Option<i64>, _>("in_flight_calls").unwrap_or(0),
                "models": row.get::<Option<String>, _>("models"),
                "providers": row.get::<Option<String>, _>("providers"),
                "verdict": verdict,
                "verdict_note": match verdict {
                    Some(VERDICT_SOLVED) => row.get::<Option<String>, _>("solved_note"),
                    Some(VERDICT_FAILED) => row.get::<Option<String>, _>("failed_note"),
                    _ => None,
                },
            })
        }).collect();

        Ok(runs)
    }

    /// How each run's spend was distributed across the arc of the task,
    /// in five equal slices of its calls.
    ///
    /// Worth its own view because the shape differs from the total: early
    /// calls are context construction (input-heavy, the agent reading its
    /// way in) and later ones are generation. Two runs that cost the same
    /// can have entirely different shapes, and the shape is what says
    /// *where* the money went.
    ///
    /// Five slices rather than raw calls so runs of different lengths line
    /// up against each other. A run of fewer than five calls simply fills
    /// fewer slices — NTILE gives it 1..n — rather than inventing empty ones.
    pub async fn get_experiment_phases(&self, experiment_id: i64, grouping: RunGrouping) -> Result<Vec<Value>> {
        let rows = sqlx::query(&format!(
            "WITH call AS (
                SELECT
                    t.id,
                    t.agent_id,
                    {run_key} AS run_key,
                    COALESCE(mm.prompt_tokens, 0) + COALESCE(mm.cache_creation_tokens, 0)
                        + COALESCE(mm.cache_read_tokens, 0) AS input_tokens,
                    COALESCE(mm.completion_tokens, 0) AS output_tokens,
                    COALESCE(mm.cache_read_tokens, 0) AS cache_read_tokens,
                    COALESCE(mm.tool_calls_count, 0) AS tool_calls,
                    COALESCE(mm.cost_estimate, 0) AS cost
                FROM tasks t
                LEFT JOIN (
                    SELECT task_id,
                           MAX(prompt_tokens) AS prompt_tokens,
                           MAX(completion_tokens) AS completion_tokens,
                           MAX(cache_creation_tokens) AS cache_creation_tokens,
                           MAX(cache_read_tokens) AS cache_read_tokens,
                           MAX(tool_calls_count) AS tool_calls_count,
                           MAX(cost_estimate) AS cost_estimate
                    FROM metrics GROUP BY task_id
                ) mm ON mm.task_id = t.id
                WHERE t.experiment_id = ?
             ),
             phased AS (
                SELECT
                    call.*,
                    NTILE(5) OVER (PARTITION BY agent_id, run_key ORDER BY id) AS phase
                FROM call
             )
             SELECT
                a.name AS agent_name,
                p.run_key AS session_key,
                p.phase AS phase,
                COUNT(*) AS calls,
                SUM(p.input_tokens) AS input_tokens,
                SUM(p.output_tokens) AS output_tokens,
                SUM(p.cache_read_tokens) AS cache_read_tokens,
                SUM(p.tool_calls) AS tool_calls,
                CAST(SUM(p.cost) AS REAL) AS cost
             FROM phased p
             JOIN agents a ON a.id = p.agent_id
             GROUP BY p.agent_id, p.run_key, p.phase
             ORDER BY agent_name, session_key, phase",
            run_key = grouping.run_key_sql(),
        ))
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;

        let phases = rows.iter().map(|row| {
            json!({
                "agent_name": row.get::<String, _>("agent_name"),
                "session_key": row.get::<String, _>("session_key"),
                "phase": row.get::<i64, _>("phase"),
                "calls": row.get::<i64, _>("calls"),
                "input_tokens": row.get::<Option<i64>, _>("input_tokens").unwrap_or(0),
                "output_tokens": row.get::<Option<i64>, _>("output_tokens").unwrap_or(0),
                "cache_read_tokens": row.get::<Option<i64>, _>("cache_read_tokens").unwrap_or(0),
                "tool_calls": row.get::<Option<i64>, _>("tool_calls").unwrap_or(0),
                "cost": row.get::<Option<f64>, _>("cost").unwrap_or(0.0),
            })
        }).collect();

        Ok(phases)
    }

    /// Which tools a run's spend went through, per run.
    ///
    /// A turn's tokens are split across the tool calls that turn made — a
    /// turn calling `read_file` twice and `bash` once gives `read_file` two
    /// thirds of it. That keeps the attributed total equal to what the turns
    /// actually cost instead of counting a multi-tool turn once per tool.
    ///
    /// It is a simplification: the real price of a tool result is paid by
    /// the *next* turn, which carries that result in its context. So read
    /// these as "which tools this run leaned on", not as an exact ledger.
    /// Turns that called no tool at all are not attributed to anything and
    /// simply do not appear here.
    pub async fn get_experiment_tool_usage(&self, experiment_id: i64, grouping: RunGrouping) -> Result<Vec<Value>> {
        let rows = sqlx::query(&format!(
            "WITH call AS (
                SELECT
                    t.id,
                    t.agent_id,
                    {run_key} AS run_key,
                    COALESCE(mm.prompt_tokens, 0) + COALESCE(mm.cache_creation_tokens, 0)
                        + COALESCE(mm.cache_read_tokens, 0) AS input_tokens,
                    COALESCE(mm.completion_tokens, 0) AS output_tokens,
                    COALESCE(mm.cost_estimate, 0) AS cost
                FROM tasks t
                LEFT JOIN (
                    SELECT task_id,
                           MAX(prompt_tokens) AS prompt_tokens,
                           MAX(completion_tokens) AS completion_tokens,
                           MAX(cache_creation_tokens) AS cache_creation_tokens,
                           MAX(cache_read_tokens) AS cache_read_tokens,
                           MAX(cost_estimate) AS cost_estimate
                    FROM metrics GROUP BY task_id
                ) mm ON mm.task_id = t.id
                WHERE t.experiment_id = ?
             ),
             turn_total AS (
                SELECT task_id, SUM(call_count) AS tools_in_turn
                FROM task_tools GROUP BY task_id
             )
             SELECT
                a.name AS agent_name,
                c.run_key AS session_key,
                tt.tool_name AS tool_name,
                SUM(tt.call_count) AS call_count,
                SUM(c.input_tokens * tt.call_count / CAST(turn_total.tools_in_turn AS REAL)) AS input_tokens,
                SUM(c.output_tokens * tt.call_count / CAST(turn_total.tools_in_turn AS REAL)) AS output_tokens,
                CAST(SUM(c.cost * tt.call_count / CAST(turn_total.tools_in_turn AS REAL)) AS REAL) AS cost
             FROM call c
             JOIN task_tools tt ON tt.task_id = c.id
             JOIN turn_total ON turn_total.task_id = c.id
             JOIN agents a ON a.id = c.agent_id
             GROUP BY c.agent_id, c.run_key, tt.tool_name
             ORDER BY agent_name, session_key, input_tokens DESC",
            run_key = grouping.run_key_sql(),
        ))
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;

        let tools = rows.iter().map(|row| {
            json!({
                "agent_name": row.get::<String, _>("agent_name"),
                "session_key": row.get::<String, _>("session_key"),
                "tool_name": row.get::<String, _>("tool_name"),
                "call_count": row.get::<i64, _>("call_count"),
                "input_tokens": row.get::<Option<f64>, _>("input_tokens").unwrap_or(0.0).round() as i64,
                "output_tokens": row.get::<Option<f64>, _>("output_tokens").unwrap_or(0.0).round() as i64,
                "cost": row.get::<Option<f64>, _>("cost").unwrap_or(0.0),
            })
        }).collect();

        Ok(tools)
    }

    /// The same judgement as `set_session_verdict`, applied to every session
    /// one agent has under an experiment.
    ///
    /// This is what marking a merged run means: with `RunGrouping::Agent` the
    /// row on screen has no single session behind it, so the verdict lands on
    /// all of them. Reading it back the other way round still agrees —
    /// `get_experiment_comparison` resolves a merged run to the best verdict
    /// among its sessions.
    ///
    /// Returns false when the agent has no calls in that experiment, so a
    /// stale row can't silently write nothing.
    pub async fn set_experiment_verdict(
        &self,
        agent_name: &str,
        experiment_id: i64,
        verdict: Option<&str>,
        note: Option<&str>,
    ) -> Result<bool> {
        let sessions: Vec<String> = sqlx::query(
            "SELECT DISTINCT COALESCE(t.session_id, '') AS session_key
             FROM tasks t
             JOIN agents a ON a.id = t.agent_id
             WHERE a.name = ? AND t.experiment_id = ?"
        )
        .bind(agent_name)
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| row.get::<String, _>("session_key"))
        .collect();

        if sessions.is_empty() {
            return Ok(false);
        }

        for session in sessions {
            self.set_session_verdict(agent_name, Some(&session), verdict, note).await?;
        }

        Ok(true)
    }

    /// Records whether a run actually solved the task it was given.
    ///
    /// The proxy can measure what a run *cost* but not whether it was any
    /// good — nothing in the traffic says the patch compiles. That judgement
    /// is a human's, and without it "cheapest" would crown whichever agent
    /// gave up first.
    ///
    /// A `verdict` of `None` clears a previous call, so a misclick is
    /// undoable. Returns `false` if no agent by that name exists, which the
    /// caller turns into a 404 rather than inventing an agent from a typo.
    pub async fn set_session_verdict(
        &self,
        agent_name: &str,
        session_id: Option<&str>,
        verdict: Option<&str>,
        note: Option<&str>,
    ) -> Result<bool> {
        let agent_id: Option<i64> = sqlx::query("SELECT id FROM agents WHERE name = ?")
            .bind(agent_name)
            .fetch_optional(&self.pool)
            .await?
            .map(|row| row.get(0));

        let agent_id = match agent_id {
            Some(id) => id,
            None => return Ok(false),
        };
        let session_key = session_id.unwrap_or("");

        match verdict {
            Some(verdict) => {
                sqlx::query(
                    "INSERT INTO session_verdicts (agent_id, session_id, verdict, note)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(agent_id, session_id) DO UPDATE SET
                        verdict = excluded.verdict,
                        note = excluded.note,
                        updated_at = CURRENT_TIMESTAMP"
                )
                .bind(agent_id)
                .bind(session_key)
                .bind(verdict)
                .bind(note)
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM session_verdicts WHERE agent_id = ? AND session_id = ?")
                    .bind(agent_id)
                    .bind(session_key)
                    .execute(&self.pool)
                    .await?;
            }
        }

        Ok(true)
    }

    /// One row per task, aggregated across its metric rows (a streaming task
    /// logs a single final row today, but MAX() keeps this correct even for
    /// older multi-row history from before that change).
    pub async fn get_recent_tasks(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT
                t.id as task_id,
                a.name as agent_name,
                t.model_name,
                t.provider,
                t.session_id,
                t.timestamp,
                t.task_description,
                t.status,
                t.http_status,
                t.error_type,
                t.error_message,
                t.stop_reason,
                t.awaiting_input,
                t.ttfb_ms,
                t.duration_ms,
                e.name as experiment_name,
                MAX(m.prompt_tokens) as prompt_tokens,
                MAX(m.completion_tokens) as completion_tokens,
                MAX(m.cache_creation_tokens) as cache_creation_tokens,
                MAX(m.cache_read_tokens) as cache_read_tokens,
                MAX(m.tool_calls_count) as tool_calls_count,
                MAX(m.latency_ms) as latency_ms,
                MAX(m.cost_estimate) as cost_estimate,
                MAX(tr.agent_question_tool) as agent_question_tool,
                MAX(tr.agent_question_text) as agent_question_text
             FROM tasks t
             JOIN agents a ON t.agent_id = a.id
             LEFT JOIN experiments e ON t.experiment_id = e.id
             LEFT JOIN metrics m ON m.task_id = t.id
             LEFT JOIN traffic tr ON tr.task_id = t.id
             GROUP BY t.id
             ORDER BY t.timestamp DESC
             LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let tasks = rows.iter().map(|row| {
            json!({
                "task_id": row.get::<i64, _>("task_id"),
                "agent_name": row.get::<String, _>("agent_name"),
                "model_name": row.get::<Option<String>, _>("model_name"),
                "provider": row.get::<Option<String>, _>("provider"),
                "session_id": row.get::<Option<String>, _>("session_id"),
                "timestamp": row.get::<String, _>("timestamp"),
                "task_description": row.get::<Option<String>, _>("task_description"),
                "status": row.get::<Option<String>, _>("status"),
                "http_status": row.get::<Option<i64>, _>("http_status"),
                "error_type": row.get::<Option<String>, _>("error_type"),
                "error_message": row.get::<Option<String>, _>("error_message"),
                "stop_reason": row.get::<Option<String>, _>("stop_reason"),
                "awaiting_input": row.get::<Option<i64>, _>("awaiting_input").unwrap_or(0) != 0,
                "ttfb_ms": row.get::<Option<i64>, _>("ttfb_ms"),
                "duration_ms": row.get::<Option<i64>, _>("duration_ms"),
                "experiment_name": row.get::<Option<String>, _>("experiment_name"),
                "prompt_tokens": row.get::<Option<i64>, _>("prompt_tokens"),
                "completion_tokens": row.get::<Option<i64>, _>("completion_tokens"),
                "cache_creation_tokens": row.get::<Option<i64>, _>("cache_creation_tokens"),
                "cache_read_tokens": row.get::<Option<i64>, _>("cache_read_tokens"),
                "tool_calls_count": row.get::<Option<i64>, _>("tool_calls_count"),
                "latency_ms": row.get::<Option<i64>, _>("latency_ms"),
                "cost_estimate": row.get::<Option<f64>, _>("cost_estimate"),
                "agent_question_tool": row.get::<Option<String>, _>("agent_question_tool"),
                "agent_question_text": row.get::<Option<String>, _>("agent_question_text"),
            })
        }).collect();

        Ok(tasks)
    }

    /// One row per agent+session: the totals for the whole session, plus the
    /// last call on it, mapped through `session_state::derive_state` into a
    /// human-facing status. This is the query behind the Agents view.
    ///
    /// The two ages are both needed and are not the same thing: `age_seconds`
    /// (since the call started) is how long an open call has been running,
    /// while `idle_seconds` (since it finished) is how long a closed session
    /// has been quiet. Both are computed in SQL against `now`, so the derived
    /// state doesn't depend on the caller's clock or timezone handling.
    pub async fn get_sessions(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "WITH call AS (
                SELECT
                    t.id, t.agent_id, t.session_id, t.timestamp, t.status, t.model_name, t.provider,
                    t.stop_reason, t.awaiting_input, t.error_type, t.error_message, t.task_description,
                    t.experiment_id, t.duration_ms,
                    COALESCE(mm.prompt_tokens, 0) + COALESCE(mm.cache_creation_tokens, 0)
                        + COALESCE(mm.cache_read_tokens, 0) AS input_tokens,
                    COALESCE(mm.completion_tokens, 0) AS output_tokens,
                    mm.cost_estimate,
                    tr.agent_question_text,
                    rl.retry_after_s, rl.requests_remaining, rl.requests_limit,
                    rl.tokens_remaining, rl.tokens_limit,
                    CAST(strftime('%s', 'now') AS INTEGER) - CAST(strftime('%s', t.timestamp) AS INTEGER) AS age_seconds,
                    CAST(strftime('%s', 'now') AS INTEGER)
                        - CAST(strftime('%s', COALESCE(t.finished_at, t.timestamp)) AS INTEGER) AS idle_seconds
                FROM tasks t
                LEFT JOIN (
                    SELECT task_id,
                           MAX(prompt_tokens) AS prompt_tokens,
                           MAX(completion_tokens) AS completion_tokens,
                           MAX(cache_creation_tokens) AS cache_creation_tokens,
                           MAX(cache_read_tokens) AS cache_read_tokens,
                           MAX(cost_estimate) AS cost_estimate
                    FROM metrics GROUP BY task_id
                ) mm ON mm.task_id = t.id
                LEFT JOIN traffic tr ON tr.task_id = t.id
                LEFT JOIN rate_limits rl ON rl.task_id = t.id
             ),
             agg AS (
                SELECT
                    agent_id,
                    session_id,
                    COUNT(*) AS call_count,
                    MIN(timestamp) AS first_seen,
                    MAX(timestamp) AS last_seen,
                    MAX(id) AS last_task_id,
                    SUM(input_tokens) AS input_tokens,
                    SUM(output_tokens) AS output_tokens,
                    -- CAST because SQLite infers INTEGER for this SUM when every
                    -- call is unpriced, which then fails to decode as an f64.
                    CAST(SUM(COALESCE(cost_estimate, 0)) AS REAL) AS total_cost,
                    SUM(CASE WHEN cost_estimate IS NULL THEN 1 ELSE 0 END) AS unpriced_calls,
                    SUM(COALESCE(duration_ms, 0)) AS busy_ms,
                    SUM(CASE WHEN status = 'rate_limited' THEN 1 ELSE 0 END) AS rate_limited_calls,
                    SUM(CASE WHEN status IN ('error', 'overloaded') THEN 1 ELSE 0 END) AS error_calls
                FROM call
                GROUP BY agent_id, session_id
             )
             SELECT
                a.name AS agent_name,
                agg.session_id AS session_id,
                agg.call_count AS call_count,
                agg.first_seen AS first_seen,
                agg.last_seen AS last_seen,
                agg.input_tokens AS input_tokens,
                agg.output_tokens AS output_tokens,
                agg.total_cost AS total_cost,
                agg.unpriced_calls AS unpriced_calls,
                agg.busy_ms AS busy_ms,
                agg.rate_limited_calls AS rate_limited_calls,
                agg.error_calls AS error_calls,
                c.id AS last_task_id,
                c.status AS last_status,
                c.model_name AS model_name,
                c.provider AS provider,
                c.stop_reason AS stop_reason,
                c.awaiting_input AS awaiting_input,
                c.error_type AS error_type,
                c.error_message AS error_message,
                c.task_description AS last_task_description,
                c.agent_question_text AS question_text,
                c.retry_after_s AS retry_after_s,
                c.requests_remaining AS requests_remaining,
                c.requests_limit AS requests_limit,
                c.tokens_remaining AS tokens_remaining,
                c.tokens_limit AS tokens_limit,
                c.age_seconds AS age_seconds,
                c.idle_seconds AS idle_seconds,
                e.name AS experiment_name
             FROM agg
             JOIN agents a ON a.id = agg.agent_id
             JOIN call c ON c.id = agg.last_task_id
             LEFT JOIN experiments e ON e.id = c.experiment_id
             ORDER BY agg.last_seen DESC, agg.last_task_id DESC
             LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let sessions = rows.iter().map(|row| {
            let last_call = LastCall {
                status: row.get::<Option<String>, _>("last_status"),
                awaiting_input: row.get::<Option<i64>, _>("awaiting_input").unwrap_or(0) != 0,
                stop_reason: row.get::<Option<String>, _>("stop_reason"),
                question_text: row.get::<Option<String>, _>("question_text"),
                error_type: row.get::<Option<String>, _>("error_type"),
                error_message: row.get::<Option<String>, _>("error_message"),
                age_seconds: row.get::<Option<i64>, _>("age_seconds").unwrap_or(0),
                idle_seconds: row.get::<Option<i64>, _>("idle_seconds").unwrap_or(0),
                retry_after_s: row.get::<Option<i64>, _>("retry_after_s"),
            };
            let state = session_state::derive_state(&last_call);

            json!({
                "agent_name": row.get::<String, _>("agent_name"),
                "session_id": row.get::<Option<String>, _>("session_id"),
                "state": state.state,
                "state_label": state.label,
                "state_detail": state.detail,
                "needs_attention": state.needs_attention,
                "call_count": row.get::<i64, _>("call_count"),
                "first_seen": row.get::<Option<String>, _>("first_seen"),
                "last_seen": row.get::<Option<String>, _>("last_seen"),
                "age_seconds": last_call.age_seconds,
                "idle_seconds": last_call.idle_seconds,
                "input_tokens": row.get::<Option<i64>, _>("input_tokens").unwrap_or(0),
                "output_tokens": row.get::<Option<i64>, _>("output_tokens").unwrap_or(0),
                "total_cost": row.get::<Option<f64>, _>("total_cost").unwrap_or(0.0),
                "unpriced_calls": row.get::<Option<i64>, _>("unpriced_calls").unwrap_or(0),
                "busy_ms": row.get::<Option<i64>, _>("busy_ms").unwrap_or(0),
                "rate_limited_calls": row.get::<Option<i64>, _>("rate_limited_calls").unwrap_or(0),
                "error_calls": row.get::<Option<i64>, _>("error_calls").unwrap_or(0),
                "last_task_id": row.get::<i64, _>("last_task_id"),
                "last_task_description": row.get::<Option<String>, _>("last_task_description"),
                "model_name": row.get::<Option<String>, _>("model_name"),
                "provider": row.get::<Option<String>, _>("provider"),
                "experiment_name": row.get::<Option<String>, _>("experiment_name"),
                "question_text": last_call.question_text,
                "error_message": last_call.error_message,
                "requests_remaining": row.get::<Option<i64>, _>("requests_remaining"),
                "requests_limit": row.get::<Option<i64>, _>("requests_limit"),
                "tokens_remaining": row.get::<Option<i64>, _>("tokens_remaining"),
                "tokens_limit": row.get::<Option<i64>, _>("tokens_limit"),
            })
        }).collect();

        Ok(sessions)
    }

    /// The current quota picture per provider. Rate-limit headers arrive on
    /// individual calls, but the budget they describe is account-wide, so the
    /// newest reading for a provider answers for every agent using it.
    pub async fn get_latest_rate_limits(&self) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT
                provider,
                requests_limit,
                requests_remaining,
                requests_reset,
                tokens_limit,
                tokens_remaining,
                tokens_reset,
                retry_after_s,
                observed_at,
                CAST(strftime('%s', 'now') AS INTEGER)
                    - CAST(strftime('%s', observed_at) AS INTEGER) AS observed_seconds_ago
             FROM rate_limits
             WHERE provider IS NOT NULL
             ORDER BY task_id DESC
             LIMIT ?"
        )
        .bind(RATE_LIMIT_SCAN_ROWS)
        .fetch_all(&self.pool)
        .await?;

        let readings = rows.iter().map(|row| RateLimitReading {
            provider: row.get::<String, _>("provider"),
            observed_at: row.get::<Option<String>, _>("observed_at"),
            observed_seconds_ago: row.get::<Option<i64>, _>("observed_seconds_ago").unwrap_or(0),
            snapshot: RateLimitSnapshot {
                requests_limit: row.get("requests_limit"),
                requests_remaining: row.get("requests_remaining"),
                requests_reset: row.get("requests_reset"),
                tokens_limit: row.get("tokens_limit"),
                tokens_remaining: row.get("tokens_remaining"),
                tokens_reset: row.get("tokens_reset"),
                retry_after_s: row.get("retry_after_s"),
            },
        }).collect();

        Ok(fold_rate_limits(readings))
    }

    pub async fn get_task_traffic(&self, task_id: i64) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT
                t.id as task_id,
                a.name as agent_name,
                t.model_name,
                t.provider,
                t.timestamp,
                t.task_description,
                tr.request_body,
                tr.response_body,
                tr.agent_question_tool,
                tr.agent_question_text
             FROM tasks t
             JOIN agents a ON t.agent_id = a.id
             LEFT JOIN traffic tr ON tr.task_id = t.id
             WHERE t.id = ?"
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| json!({
            "task_id": row.get::<i64, _>("task_id"),
            "agent_name": row.get::<String, _>("agent_name"),
            "model_name": row.get::<Option<String>, _>("model_name"),
            "provider": row.get::<Option<String>, _>("provider"),
            "timestamp": row.get::<String, _>("timestamp"),
            "task_description": row.get::<Option<String>, _>("task_description"),
            "request_body": row.get::<Option<String>, _>("request_body"),
            "response_body": row.get::<Option<String>, _>("response_body"),
            "agent_question_tool": row.get::<Option<String>, _>("agent_question_tool"),
            "agent_question_text": row.get::<Option<String>, _>("agent_question_text"),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_or_create_agent() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;

        let agent_id1 = db.get_or_create_agent("test_agent").await?;
        assert_eq!(agent_id1, 1);

        let agent_id2 = db.get_or_create_agent("test_agent").await?;
        assert_eq!(agent_id1, agent_id2);

        Ok(())
    }

    #[tokio::test]
    async fn test_create_task() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;

        let task_id = db.create_task(agent_id, None, None, Some("test task".to_string()), Some("gpt-3.5-turbo".to_string()), Some("openai".to_string())).await?;
        assert_eq!(task_id, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_log_metric() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;
        let task_id = db.create_task(agent_id, None, None, None, None, None).await?;

        let res = db.log_metric(task_id, 10, 20, 0, 0, 5, 100, Some(0.0042)).await;
        assert!(res.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_traffic_roundtrip() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;
        let task_id = db.create_task(agent_id, None, None, None, Some("gpt-4o".to_string()), Some("openai".to_string())).await?;

        db.save_traffic_request(task_id, "{\"model\":\"gpt-4o\"}").await?;
        db.save_traffic_response(task_id, "{\"id\":\"resp_1\"}").await?;

        let traffic = db.get_task_traffic(task_id).await?.expect("traffic row should exist");
        assert_eq!(traffic["request_body"], "{\"model\":\"gpt-4o\"}");
        assert_eq!(traffic["response_body"], "{\"id\":\"resp_1\"}");

        Ok(())
    }

    #[tokio::test]
    async fn test_get_recent_tasks() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;
        let task_id = db.create_task(agent_id, None, None, None, Some("gpt-4o".to_string()), Some("openai".to_string())).await?;
        db.log_metric(task_id, 10, 20, 0, 5, 1, 100, Some(0.001)).await?;
        db.save_agent_question(task_id, "ask_followup_question", "Use TypeScript?").await?;

        let tasks = db.get_recent_tasks(10).await?;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["prompt_tokens"], 10);
        assert_eq!(tasks[0]["cache_read_tokens"], 5);
        assert_eq!(tasks[0]["agent_question_tool"], "ask_followup_question");
        assert_eq!(tasks[0]["agent_question_text"], "Use TypeScript?");

        Ok(())
    }

    fn ok_outcome(stop_reason: &str, awaiting_input: bool) -> TaskOutcome {
        TaskOutcome {
            status: session_state::STATUS_OK.to_string(),
            http_status: Some(200),
            stop_reason: Some(stop_reason.to_string()),
            awaiting_input,
            ttfb_ms: Some(300),
            duration_ms: Some(1200),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_new_task_starts_in_flight_and_reads_back_as_thinking() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["state"], "working");
        assert_eq!(sessions[0]["state_label"], "Thinking");

        Ok(())
    }

    #[tokio::test]
    async fn a_finished_turn_that_asked_a_question_reads_as_waiting_for_you() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.save_agent_question(task_id, "ask_followup_question", "Which file?").await?;
        db.finish_task(task_id, &ok_outcome("tool_use", true)).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions[0]["state"], "waiting_for_you");
        assert_eq!(sessions[0]["needs_attention"], true);
        assert_eq!(sessions[0]["state_detail"], "Which file?");

        Ok(())
    }

    #[tokio::test]
    async fn a_rate_limited_call_reports_the_retry_window() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, None, None, Some("s1".to_string()), None, Some("anthropic".to_string())).await?;
        db.save_rate_limits(task_id, "anthropic", &RateLimitSnapshot {
            retry_after_s: Some(60),
            tokens_remaining: Some(0),
            tokens_limit: Some(80_000),
            ..Default::default()
        }).await?;
        db.finish_task(task_id, &TaskOutcome {
            status: session_state::STATUS_RATE_LIMITED.to_string(),
            http_status: Some(429),
            error_type: Some("rate_limit_error".to_string()),
            ..Default::default()
        }).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions[0]["state"], "rate_limited");
        assert_eq!(sessions[0]["state_detail"], "Provider says retry in 1m");
        assert_eq!(sessions[0]["tokens_remaining"], 0);
        assert_eq!(sessions[0]["rate_limited_calls"], 1);

        let limits = db.get_latest_rate_limits().await?;
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0]["provider"], "anthropic");
        assert_eq!(limits[0]["tokens_limit"], 80_000);

        Ok(())
    }

    /// The reading that reports a 429 usually carries only `retry-after`, so
    /// the budget numbers have to survive from the last call that did report
    /// them — otherwise the quota display empties out precisely when the
    /// human goes looking at it.
    #[tokio::test]
    async fn a_429_reading_does_not_erase_the_budget_numbers_before_it() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        let healthy = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.save_rate_limits(healthy, "anthropic", &RateLimitSnapshot {
            requests_limit: Some(1000),
            requests_remaining: Some(4),
            tokens_limit: Some(80_000),
            tokens_remaining: Some(1_200),
            ..Default::default()
        }).await?;

        let blocked = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.save_rate_limits(blocked, "anthropic", &RateLimitSnapshot {
            retry_after_s: Some(30),
            ..Default::default()
        }).await?;

        let limits = db.get_latest_rate_limits().await?;
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0]["retry_after_remaining_s"], 30, "from the newest reading");
        assert_eq!(limits[0]["requests_remaining"], 4, "carried over from the last reading that had it");
        assert_eq!(limits[0]["tokens_remaining"], 1_200);

        Ok(())
    }

    #[test]
    fn folding_keeps_providers_apart_and_prefers_the_newest_value_per_field() {
        let limits = fold_rate_limits(vec![
            RateLimitReading {
                provider: "openai".to_string(),
                observed_at: Some("2026-08-31 12:00:10".to_string()),
                observed_seconds_ago: 10,
                snapshot: RateLimitSnapshot { requests_remaining: Some(7), ..Default::default() },
            },
            RateLimitReading {
                provider: "anthropic".to_string(),
                observed_at: Some("2026-08-31 12:00:05".to_string()),
                observed_seconds_ago: 15,
                snapshot: RateLimitSnapshot { requests_remaining: Some(500), ..Default::default() },
            },
            RateLimitReading {
                provider: "openai".to_string(),
                observed_at: Some("2026-08-31 11:59:00".to_string()),
                observed_seconds_ago: 80,
                snapshot: RateLimitSnapshot {
                    requests_remaining: Some(99),
                    tokens_remaining: Some(4_000),
                    ..Default::default()
                },
            },
        ]);

        assert_eq!(limits.len(), 2);
        let openai = limits.iter().find(|l| l["provider"] == "openai").unwrap();
        assert_eq!(openai["requests_remaining"], 7, "newest wins for a field both rows have");
        assert_eq!(openai["tokens_remaining"], 4_000, "older row still fills a field the newest lacks");
        assert_eq!(openai["observed_seconds_ago"], 10, "age comes from the newest reading");
        assert_eq!(limits.iter().find(|l| l["provider"] == "anthropic").unwrap()["requests_remaining"], 500);
    }

    #[test]
    fn a_retry_window_that_already_elapsed_is_dropped_rather_than_shown_stale() {
        let limits = fold_rate_limits(vec![RateLimitReading {
            provider: "anthropic".to_string(),
            observed_at: Some("2026-08-31 11:00:00".to_string()),
            observed_seconds_ago: 3600,
            snapshot: RateLimitSnapshot { retry_after_s: Some(60), ..Default::default() },
        }]);

        assert!(limits[0]["retry_after_remaining_s"].is_null());
    }

    #[test]
    fn a_live_retry_window_is_reported_as_the_time_still_left() {
        let limits = fold_rate_limits(vec![RateLimitReading {
            provider: "anthropic".to_string(),
            observed_at: Some("2026-08-31 12:00:00".to_string()),
            observed_seconds_ago: 20,
            snapshot: RateLimitSnapshot { retry_after_s: Some(60), ..Default::default() },
        }]);

        assert_eq!(limits[0]["retry_after_remaining_s"], 40);
    }

    /// The rollup must reflect the *last* call, not an arbitrary one: a
    /// session that hit a limit and then recovered is no longer blocked.
    #[tokio::test]
    async fn session_state_follows_the_most_recent_call() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        let first = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.finish_task(first, &TaskOutcome {
            status: session_state::STATUS_RATE_LIMITED.to_string(),
            http_status: Some(429),
            ..Default::default()
        }).await?;

        let second = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.finish_task(second, &ok_outcome("tool_use", false)).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions.len(), 1, "both calls belong to one session");
        assert_eq!(sessions[0]["state"], "working");
        assert_eq!(sessions[0]["call_count"], 2);
        assert_eq!(sessions[0]["rate_limited_calls"], 1, "the earlier 429 still counts in the session's history");

        Ok(())
    }

    /// Two metric rows for one task (possible in history from before a task
    /// logged a single final row) must not double-count into session totals.
    #[tokio::test]
    async fn session_totals_do_not_double_count_repeated_metric_rows() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.log_metric(task_id, 100, 20, 0, 0, 1, 500, Some(0.01)).await?;
        db.log_metric(task_id, 100, 20, 0, 0, 1, 500, Some(0.01)).await?;
        db.finish_task(task_id, &ok_outcome("end_turn", true)).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions[0]["call_count"], 1);
        assert_eq!(sessions[0]["input_tokens"], 100);
        assert_eq!(sessions[0]["output_tokens"], 20);

        Ok(())
    }

    #[tokio::test]
    async fn sessions_are_split_per_agent_even_on_the_same_session_id() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let kilo = db.get_or_create_agent("kilo").await?;
        let opencode = db.get_or_create_agent("opencode").await?;
        db.create_task(kilo, None, None, Some("shared".to_string()), None, None).await?;
        db.create_task(opencode, None, None, Some("shared".to_string()), None, None).await?;

        let sessions = db.get_sessions(10).await?;
        assert_eq!(sessions.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn restarting_closes_out_calls_that_can_never_complete() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        let finished = db.create_task(agent_id, None, None, Some("s2".to_string()), None, None).await?;
        db.finish_task(finished, &ok_outcome("end_turn", true)).await?;

        assert_eq!(db.reap_in_flight_tasks().await?, 1, "only the still-open call is reaped");

        let sessions = db.get_sessions(10).await?;
        let s1 = sessions.iter().find(|s| s["session_id"] == "s1").unwrap();
        assert_eq!(s1["state"], "interrupted");
        let s2 = sessions.iter().find(|s| s["session_id"] == "s2").unwrap();
        assert_eq!(s2["state"], "waiting_for_you");

        Ok(())
    }

    #[tokio::test]
    async fn recent_tasks_expose_the_call_outcome() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, None, None, Some("s1".to_string()), None, None).await?;
        db.finish_task(task_id, &TaskOutcome {
            status: session_state::STATUS_ERROR.to_string(),
            http_status: Some(401),
            error_type: Some("authentication_error".to_string()),
            error_message: Some("invalid x-api-key".to_string()),
            ttfb_ms: Some(90),
            duration_ms: Some(95),
            ..Default::default()
        }).await?;

        let tasks = db.get_recent_tasks(10).await?;
        assert_eq!(tasks[0]["status"], "error");
        assert_eq!(tasks[0]["http_status"], 401);
        assert_eq!(tasks[0]["error_type"], "authentication_error");
        assert_eq!(tasks[0]["duration_ms"], 95);

        Ok(())
    }

    #[tokio::test]
    async fn test_save_agent_question_roundtrip() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;
        let task_id = db.create_task(agent_id, None, None, None, Some("gpt-4o".to_string()), Some("openai".to_string())).await?;

        db.save_traffic_request(task_id, "{\"model\":\"gpt-4o\"}").await?;
        db.save_agent_question(task_id, "ask_followup_question", "Which file?").await?;

        let traffic = db.get_task_traffic(task_id).await?.expect("traffic row should exist");
        assert_eq!(traffic["request_body"], "{\"model\":\"gpt-4o\"}");
        assert_eq!(traffic["agent_question_tool"], "ask_followup_question");
        assert_eq!(traffic["agent_question_text"], "Which file?");

        Ok(())
    }

    /// One completed call by `agent` in `session`, attributed to `experiment`.
    async fn seed_run_call(
        db: &Database,
        experiment_id: i64,
        agent: &str,
        session: &str,
        cost: Option<f64>,
    ) -> Result<i64> {
        let agent_id = db.get_or_create_agent(agent).await?;
        let task_id = db.create_task(
            agent_id,
            Some(experiment_id),
            None,
            Some(session.to_string()),
            Some("gpt-4o".to_string()),
            Some("openai".to_string()),
        ).await?;
        db.log_metric(task_id, 1000, 200, 0, 500, 2, 300, cost).await?;
        db.finish_task(task_id, &ok_outcome("stop", false)).await?;
        Ok(task_id)
    }

    #[tokio::test]
    async fn comparison_folds_an_experiment_into_one_row_per_agent_and_session() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;

        // Two calls from one agent, one from the other: the run is the unit,
        // not the call, so this must come back as two rows.
        seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.10)).await?;
        seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.05)).await?;
        seed_run_call(&db, experiment_id, "opencode", "run-b", Some(0.40)).await?;

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs.len(), 2);

        // Cheapest first.
        assert_eq!(runs[0]["agent_name"], "kilo");
        assert_eq!(runs[0]["call_count"], 2);
        assert!((runs[0]["total_cost"].as_f64().unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(runs[0]["input_tokens"], 3000);
        assert_eq!(runs[0]["output_tokens"], 400);
        assert_eq!(runs[0]["cache_read_tokens"], 1000);
        assert_eq!(runs[0]["tool_calls"], 4);
        assert_eq!(runs[0]["unpriced_calls"], 0);
        assert_eq!(runs[0]["models"], "gpt-4o");

        assert_eq!(runs[1]["agent_name"], "opencode");
        assert_eq!(runs[1]["total_cost"], 0.4);

        Ok(())
    }

    #[tokio::test]
    async fn comparison_ignores_calls_from_other_experiments() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let mine = db.get_or_create_experiment("issue-1284", None).await?;
        let theirs = db.get_or_create_experiment("issue-9000", None).await?;

        seed_run_call(&db, mine, "kilo", "run-a", Some(0.10)).await?;
        seed_run_call(&db, theirs, "kilo", "run-c", Some(0.90)).await?;

        let runs = db.get_experiment_comparison(mine, RunGrouping::Session).await?;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["session_id"], "run-a");

        Ok(())
    }

    /// A run on a model missing from `pricing.yaml` totals $0.00, which would
    /// otherwise read as "free" and win. The count of unpriced calls is what
    /// lets the UI say "unknown" instead.
    #[tokio::test]
    async fn comparison_counts_unpriced_calls_rather_than_treating_them_as_free() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;

        seed_run_call(&db, experiment_id, "kilo", "run-a", None).await?;
        seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.20)).await?;

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["unpriced_calls"], 1);
        assert_eq!(runs[0]["total_cost"], 0.2);

        Ok(())
    }

    #[tokio::test]
    async fn comparison_surfaces_failed_calls_per_run() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("cursor").await?;

        let task_id = db.create_task(agent_id, Some(experiment_id), None,
            Some("run-a".to_string()), None, Some("openai".to_string())).await?;
        db.finish_task(task_id, &TaskOutcome {
            status: session_state::STATUS_RATE_LIMITED.to_string(),
            http_status: Some(429),
            ..Default::default()
        }).await?;

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["rate_limited_calls"], 1);
        assert_eq!(runs[0]["error_calls"], 0);
        // No metric row at all, so nothing was priced.
        assert_eq!(runs[0]["unpriced_calls"], 1);

        Ok(())
    }

    /// A run still mid-call has not finished spending; its total is a running
    /// tally, and the comparison has to be able to say so.
    #[tokio::test]
    async fn comparison_flags_a_run_that_is_still_in_flight() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.10)).await?;
        db.create_task(agent_id, Some(experiment_id), None, Some("run-a".to_string()), None, None).await?;

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["in_flight_calls"], 1);
        assert_eq!(runs[0]["call_count"], 2);

        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_attaches_to_its_run_and_can_be_cleared() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.10)).await?;
        seed_run_call(&db, experiment_id, "opencode", "run-b", Some(0.40)).await?;

        assert!(db.set_session_verdict("kilo", Some("run-a"), Some(VERDICT_SOLVED), Some("tests pass")).await?);

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["verdict"], "solved");
        assert_eq!(runs[0]["verdict_note"], "tests pass");
        // The other run is untouched — a verdict is per run, not per agent.
        assert_eq!(runs[1]["verdict"], Value::Null);

        // Re-marking overwrites rather than erroring on the primary key.
        assert!(db.set_session_verdict("kilo", Some("run-a"), Some(VERDICT_FAILED), None).await?);
        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["verdict"], "failed");
        assert_eq!(runs[0]["verdict_note"], Value::Null);

        assert!(db.set_session_verdict("kilo", Some("run-a"), None, None).await?);
        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["verdict"], Value::Null);

        Ok(())
    }

    #[tokio::test]
    async fn a_verdict_for_an_unknown_agent_is_rejected_rather_than_inventing_one() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;

        assert!(!db.set_session_verdict("typo", Some("run-a"), Some(VERDICT_SOLVED), None).await?);
        let agents: i64 = sqlx::query("SELECT COUNT(*) FROM agents")
            .fetch_one(&db.pool).await?.get(0);
        assert_eq!(agents, 0);

        Ok(())
    }

    /// Calls sent without an `X-Session-ID` land in one bucket keyed by '',
    /// and a verdict has to be able to name that bucket too.
    #[tokio::test]
    async fn a_run_with_no_session_id_still_takes_a_verdict() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, Some(experiment_id), None, None, None, None).await?;
        db.log_metric(task_id, 10, 20, 0, 0, 0, 100, Some(0.01)).await?;

        assert!(db.set_session_verdict("kilo", None, Some(VERDICT_SOLVED), None).await?);

        let runs = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(runs[0]["session_id"], Value::Null);
        assert_eq!(runs[0]["session_key"], "");
        assert_eq!(runs[0]["verdict"], "solved");

        Ok(())
    }

    #[tokio::test]
    async fn phases_split_a_run_into_five_slices_of_its_calls() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;

        // Ten calls: two per slice. Output climbs while input falls, the
        // shape the phase view exists to show.
        let agent_id = db.get_or_create_agent("kilo").await?;
        for i in 0..10 {
            let task_id = db.create_task(agent_id, Some(experiment_id), None,
                Some("run-a".to_string()), None, None).await?;
            db.log_metric(task_id, 1000 - i * 100, 100 + i * 100, 0, 0, 1, 100, Some(0.01)).await?;
        }

        let phases = db.get_experiment_phases(experiment_id, RunGrouping::Session).await?;
        assert_eq!(phases.len(), 5);
        assert_eq!(phases[0]["phase"], 1);
        assert_eq!(phases[0]["calls"], 2);
        assert_eq!(phases[4]["phase"], 5);

        // Input-heavy at the start, output-heavy at the end.
        let first_in = phases[0]["input_tokens"].as_i64().unwrap();
        let first_out = phases[0]["output_tokens"].as_i64().unwrap();
        let last_in = phases[4]["input_tokens"].as_i64().unwrap();
        let last_out = phases[4]["output_tokens"].as_i64().unwrap();
        assert!(first_in > first_out);
        assert!(last_out > last_in);

        Ok(())
    }

    /// A short run must fill fewer slices rather than have empty ones
    /// invented for it — three calls is three phases, not five.
    #[tokio::test]
    async fn a_run_shorter_than_five_calls_fills_fewer_phases() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        for _ in 0..3 {
            seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.01)).await?;
        }

        let phases = db.get_experiment_phases(experiment_id, RunGrouping::Session).await?;
        assert_eq!(phases.len(), 3);
        assert_eq!(phases.iter().map(|p| p["phase"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 3]);

        Ok(())
    }

    #[tokio::test]
    async fn phases_are_computed_per_run_not_across_the_experiment() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        for _ in 0..5 {
            seed_run_call(&db, experiment_id, "kilo", "run-a", Some(0.01)).await?;
            seed_run_call(&db, experiment_id, "opencode", "run-b", Some(0.01)).await?;
        }

        let phases = db.get_experiment_phases(experiment_id, RunGrouping::Session).await?;
        // Five slices each, not five shared between them.
        assert_eq!(phases.len(), 10);
        assert_eq!(phases.iter().filter(|p| p["agent_name"] == "kilo").count(), 5);
        assert_eq!(phases.iter().filter(|p| p["agent_name"] == "opencode").count(), 5);

        Ok(())
    }

    #[tokio::test]
    async fn tool_calls_are_deduplicated_into_counts() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("kilo").await?;
        let task_id = db.create_task(agent_id, None, None, None, None, None).await?;

        db.save_tool_calls(task_id, &[
            "read_file".to_string(), "bash".to_string(), "read_file".to_string(),
        ]).await?;

        let rows: Vec<(String, i64)> = sqlx::query(
            "SELECT tool_name, call_count FROM task_tools WHERE task_id = ? ORDER BY tool_name"
        )
        .bind(task_id)
        .fetch_all(&db.pool).await?
        .iter().map(|r| (r.get("tool_name"), r.get("call_count"))).collect();

        assert_eq!(rows, vec![("bash".to_string(), 1), ("read_file".to_string(), 2)]);

        // Re-saving the same turn replaces rather than doubling — a retry of
        // the write must not inflate the attribution.
        db.save_tool_calls(task_id, &["read_file".to_string(), "read_file".to_string()]).await?;
        let count: i64 = sqlx::query("SELECT call_count FROM task_tools WHERE task_id = ? AND tool_name = 'read_file'")
            .bind(task_id).fetch_one(&db.pool).await?.get(0);
        assert_eq!(count, 2);

        Ok(())
    }

    #[tokio::test]
    async fn tool_usage_splits_a_turn_across_the_tools_it_called() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        // One turn, 900 input tokens, three tool calls: two read_file, one bash.
        let task_id = db.create_task(agent_id, Some(experiment_id), None,
            Some("run-a".to_string()), None, None).await?;
        db.log_metric(task_id, 900, 90, 0, 0, 3, 100, Some(0.09)).await?;
        db.save_tool_calls(task_id, &[
            "read_file".to_string(), "read_file".to_string(), "bash".to_string(),
        ]).await?;

        let tools = db.get_experiment_tool_usage(experiment_id, RunGrouping::Session).await?;
        assert_eq!(tools.len(), 2);

        // Ordered by input tokens, so the two-thirds share comes first.
        assert_eq!(tools[0]["tool_name"], "read_file");
        assert_eq!(tools[0]["call_count"], 2);
        assert_eq!(tools[0]["input_tokens"], 600);
        assert_eq!(tools[1]["tool_name"], "bash");
        assert_eq!(tools[1]["input_tokens"], 300);

        // The split is exhaustive: nothing is counted twice, nothing lost.
        let attributed: i64 = tools.iter().map(|t| t["input_tokens"].as_i64().unwrap()).sum();
        assert_eq!(attributed, 900);

        Ok(())
    }

    /// A turn that called no tool is attributed to nothing rather than
    /// smeared over whichever tools the run used elsewhere.
    #[tokio::test]
    async fn tool_usage_ignores_turns_with_no_tool_calls() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        let with_tool = db.create_task(agent_id, Some(experiment_id), None,
            Some("run-a".to_string()), None, None).await?;
        db.log_metric(with_tool, 100, 10, 0, 0, 1, 100, Some(0.01)).await?;
        db.save_tool_calls(with_tool, &["bash".to_string()]).await?;

        let no_tool = db.create_task(agent_id, Some(experiment_id), None,
            Some("run-a".to_string()), None, None).await?;
        db.log_metric(no_tool, 9999, 999, 0, 0, 0, 100, Some(9.99)).await?;

        let tools = db.get_experiment_tool_usage(experiment_id, RunGrouping::Session).await?;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["input_tokens"], 100);

        Ok(())
    }

    /// The case that motivates `RunGrouping::Agent`: an agent that mints a
    /// fresh session id per session turns one attempt into a row per
    /// fragment, and per-session the cheapest fragment reads as the whole run.
    #[tokio::test]
    async fn agent_grouping_merges_sessions_the_agent_split_by_itself() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;

        // One attempt, three session ids, because the agent restarted twice.
        for session in ["kilo-s1", "kilo-s2", "kilo-s3"] {
            seed_run_call(&db, experiment_id, "kilo", session, Some(0.10)).await?;
        }
        seed_run_call(&db, experiment_id, "opencode", "oc-1", Some(0.25)).await?;

        let per_session = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(per_session.len(), 4);
        // Each fragment reads as a $0.10 run, none of which is the attempt.
        assert!((per_session[0]["total_cost"].as_f64().unwrap() - 0.10).abs() < 1e-9);

        let per_agent = db.get_experiment_comparison(experiment_id, RunGrouping::Agent).await?;
        assert_eq!(per_agent.len(), 2);

        let kilo = per_agent.iter().find(|r| r["agent_name"] == "kilo").unwrap();
        assert_eq!(kilo["sessions"], 3);
        assert_eq!(kilo["call_count"], 3);
        assert!((kilo["total_cost"].as_f64().unwrap() - 0.30).abs() < 1e-9);
        // A merged run spans many session ids, so it reports none as its own.
        assert_eq!(kilo["session_id"], Value::Null);
        assert_eq!(kilo["session_key"], "");

        // …but an agent with only one session keeps the id it does have.
        let opencode = per_agent.iter().find(|r| r["agent_name"] == "opencode").unwrap();
        assert_eq!(opencode["sessions"], 1);
        assert_eq!(opencode["session_id"], "oc-1");

        // Merged, kilo is the dearer of the two — the opposite of what the
        // per-session view said.
        assert_eq!(per_agent[0]["agent_name"], "opencode");

        Ok(())
    }

    #[tokio::test]
    async fn a_merged_run_counts_as_solved_when_any_of_its_sessions_did() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        seed_run_call(&db, experiment_id, "kilo", "kilo-s1", Some(0.10)).await?;
        seed_run_call(&db, experiment_id, "kilo", "kilo-s2", Some(0.10)).await?;

        // The agent gave up in its first session and got there in the second.
        db.set_session_verdict("kilo", Some("kilo-s1"), Some(VERDICT_FAILED), Some("wrong file")).await?;
        db.set_session_verdict("kilo", Some("kilo-s2"), Some(VERDICT_SOLVED), Some("tests pass")).await?;

        let merged = db.get_experiment_comparison(experiment_id, RunGrouping::Agent).await?;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0]["verdict"], "solved");
        // The note shown is the one belonging to the verdict that won.
        assert_eq!(merged[0]["verdict_note"], "tests pass");
        // …and the attempt is charged for both sessions.
        assert!((merged[0]["total_cost"].as_f64().unwrap() - 0.20).abs() < 1e-9);

        Ok(())
    }

    #[tokio::test]
    async fn judging_a_merged_run_marks_every_session_under_it() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        for session in ["kilo-s1", "kilo-s2"] {
            seed_run_call(&db, experiment_id, "kilo", session, Some(0.10)).await?;
        }
        // Another experiment's session by the same agent must be left alone.
        let other = db.get_or_create_experiment("issue-9000", None).await?;
        seed_run_call(&db, other, "kilo", "kilo-elsewhere", Some(0.10)).await?;

        assert!(db.set_experiment_verdict("kilo", experiment_id, Some(VERDICT_SOLVED), None).await?);

        let per_session = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert_eq!(per_session.len(), 2);
        assert!(per_session.iter().all(|r| r["verdict"] == "solved"));

        let elsewhere = db.get_experiment_comparison(other, RunGrouping::Session).await?;
        assert_eq!(elsewhere[0]["verdict"], Value::Null);

        // Clearing works the same way round.
        assert!(db.set_experiment_verdict("kilo", experiment_id, None, None).await?);
        let cleared = db.get_experiment_comparison(experiment_id, RunGrouping::Session).await?;
        assert!(cleared.iter().all(|r| r["verdict"] == Value::Null));

        Ok(())
    }

    #[tokio::test]
    async fn judging_a_merged_run_for_an_agent_with_no_calls_here_is_rejected() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        db.get_or_create_agent("kilo").await?;

        assert!(!db.set_experiment_verdict("kilo", experiment_id, Some(VERDICT_SOLVED), None).await?);

        Ok(())
    }

    /// The phase arc must be cut across the whole merged attempt, not
    /// restarted per fragment — five slices of the run, however many session
    /// ids the agent happened to mint along the way.
    #[tokio::test]
    async fn agent_grouping_slices_phases_across_the_whole_attempt() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        for session in ["s1", "s2", "s3"] {
            for _ in 0..4 {
                seed_run_call(&db, experiment_id, "kilo", session, Some(0.01)).await?;
            }
        }

        let per_session = db.get_experiment_phases(experiment_id, RunGrouping::Session).await?;
        // Three fragments, four calls each: 4 slices apiece.
        assert_eq!(per_session.len(), 12);

        let merged = db.get_experiment_phases(experiment_id, RunGrouping::Agent).await?;
        assert_eq!(merged.len(), 5);
        assert_eq!(merged.iter().map(|p| p["calls"].as_i64().unwrap()).sum::<i64>(), 12);

        Ok(())
    }

    #[tokio::test]
    async fn agent_grouping_pools_tool_usage_across_the_attempt() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let experiment_id = db.get_or_create_experiment("issue-1284", None).await?;
        let agent_id = db.get_or_create_agent("kilo").await?;

        for session in ["s1", "s2"] {
            let task_id = db.create_task(agent_id, Some(experiment_id), None,
                Some(session.to_string()), None, None).await?;
            db.log_metric(task_id, 100, 10, 0, 0, 1, 100, Some(0.01)).await?;
            db.save_tool_calls(task_id, &["read_file".to_string()]).await?;
        }

        let merged = db.get_experiment_tool_usage(experiment_id, RunGrouping::Agent).await?;
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0]["call_count"], 2);
        assert_eq!(merged[0]["input_tokens"], 200);

        Ok(())
    }

    #[test]
    fn run_grouping_defaults_to_the_narrower_per_session_reading() {
        assert_eq!(RunGrouping::parse(Some("agent")), RunGrouping::Agent);
        assert_eq!(RunGrouping::parse(Some("session")), RunGrouping::Session);
        assert_eq!(RunGrouping::parse(None), RunGrouping::Session);
        // Never silently merges attempts on a value it doesn't recognise.
        assert_eq!(RunGrouping::parse(Some("nonsense")), RunGrouping::Session);
    }

    #[test]
    fn only_the_two_known_verdicts_are_accepted() {
        assert!(is_valid_verdict(VERDICT_SOLVED));
        assert!(is_valid_verdict(VERDICT_FAILED));
        assert!(!is_valid_verdict("maybe"));
        assert!(!is_valid_verdict(""));
    }
}
