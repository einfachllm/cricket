use sqlx::{sqlite::SqlitePool, Row};
use anyhow::Result;
use serde_json::{json, Value};

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

        // Older on-disk databases predate these columns; add them in place so
        // existing installs upgrade without losing history.
        Self::add_column_if_missing(&pool, "tasks", "provider", "TEXT").await?;
        Self::add_column_if_missing(&pool, "metrics", "cache_creation_tokens", "INTEGER DEFAULT 0").await?;
        Self::add_column_if_missing(&pool, "metrics", "cache_read_tokens", "INTEGER DEFAULT 0").await?;
        Self::add_column_if_missing(&pool, "traffic", "agent_question_tool", "TEXT").await?;
        Self::add_column_if_missing(&pool, "traffic", "agent_question_text", "TEXT").await?;

        Ok(Self { pool })
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

    pub async fn create_task(&self, agent_id: i64, experiment_id: Option<i64>, description: Option<String>, session_id: Option<String>, model: Option<String>, provider: Option<String>) -> Result<i64> {
        let res = sqlx::query(
            "INSERT INTO tasks (agent_id, experiment_id, task_description, session_id, model_name, provider) VALUES (?, ?, ?, ?, ?, ?)"
        )
        .bind(agent_id)
        .bind(experiment_id)
        .bind(description)
        .bind(session_id)
        .bind(model)
        .bind(provider)
        .execute(&self.pool)
        .await?;

        Ok(res.last_insert_rowid())
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
