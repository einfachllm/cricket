use sqlx::{sqlite::SqlitePool, Row};
use anyhow::Result;

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

        Ok(Self { pool })
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

    pub async fn create_task(&self, agent_id: i64, experiment_id: Option<i64>, description: Option<String>, session_id: Option<String>, model: Option<String>) -> Result<i64> {
        let res = sqlx::query(
            "INSERT INTO tasks (agent_id, experiment_id, task_description, session_id, model_name) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(agent_id)
        .bind(experiment_id)
        .bind(description)
        .bind(session_id)
        .bind(model)
        .execute(&self.pool)
        .await?;
        
        Ok(res.last_insert_rowid())
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
                m.prompt_tokens,
                m.completion_tokens,
                m.tool_calls_count,
                m.latency_ms
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
                "prompt_tokens": row.get::<i64, _>("prompt_tokens"),
                "completion_tokens": row.get::<i64, _>("completion_tokens"),
                "tool_calls_count": row.get::<i64, _>("tool_calls_count"),
                "latency_ms": row.get::<i64, _>("latency_ms"),
            })
        }).collect();

        Ok(metrics)
    }

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePool;

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
        
        let task_id = db.create_task(agent_id, None, None, Some("test task".to_string()), Some("gpt-3.5-turbo".to_string())).await?;
        assert_eq!(task_id, 1);
        
        Ok(())
    }

    #[tokio::test]
    async fn test_log_metric() -> Result<()> {
        let db = Database::new("sqlite::memory:").await?;
        let agent_id = db.get_or_create_agent("test_agent").await?;
        let task_id = db.create_task(agent_id, None, None, None, None).await?;
        
        let res = db.log_metric(task_id, 10, 20, 5, 100).await;
        assert!(res.is_ok());
        
        Ok(())
    }
}
