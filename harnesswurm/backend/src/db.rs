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
}
