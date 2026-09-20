use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

use super::zafra::safe_truncate;

struct ErrorRow {
    id: uuid::Uuid,
    error_type: String,
    error_message: String,
    solution: Option<String>,
    resolved: bool,
    project: String,
    sim: f64,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for ErrorRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            error_type: row.try_get("error_type")?,
            error_message: row.try_get("error_message")?,
            solution: row.try_get("solution")?,
            resolved: row.try_get("resolved")?,
            project: row.try_get("project")?,
            sim: row.try_get("sim")?,
        })
    }
}

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let project = args.get("project").and_then(|v| v.as_str());
    let resolved_only = args
        .get("resolved_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let proposed_action = args.get("proposed_action").and_then(|v| v.as_str());

    if query.is_empty() {
        anyhow::bail!("query is required");
    }

    let project_id = crate::project::current_project_id(pool).await?;

    let errors = if resolved_only {
        sqlx::query_as::<_, ErrorRow>(
            "SELECT id, error_type, error_message, solution, resolved, project,
                    similarity(error_message, $1)::float8 AS sim
             FROM brain_errors
             WHERE (search_vector @@ cuba_or_tsquery($1) OR similarity(error_message, $1) > 0.3)
               AND resolved = true
               AND ($2::text IS NULL OR project = $2)
               AND ($3::uuid IS NULL OR project_id = $3)
               AND trust = 'trusted'
             ORDER BY sim DESC LIMIT 20",
        )
        .bind(query)
        .bind(project)
        .bind(project_id)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, ErrorRow>(
            "SELECT id, error_type, error_message, solution, resolved, project,
                    similarity(error_message, $1)::float8 AS sim
             FROM brain_errors
             WHERE (search_vector @@ cuba_or_tsquery($1) OR similarity(error_message, $1) > 0.3)
               AND ($2::text IS NULL OR project = $2)
               AND ($3::uuid IS NULL OR project_id = $3)
               AND trust = 'trusted'
             ORDER BY sim DESC LIMIT 20",
        )
        .bind(query)
        .bind(project)
        .bind(project_id)
        .fetch_all(pool)
        .await?
    };

    let results: Vec<Value> = errors
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.id.to_string(),
                "error_type": row.error_type,
                "error_message": safe_truncate(&row.error_message, 200),
                "solution": row.solution,
                "resolved": row.resolved,
                "project": row.project,
                "similarity": row.sim
            })
        })
        .collect();

    let mut response =
        serde_json::json!({"query": query, "results": results, "count": results.len()});

    if let Some(action) = proposed_action {
        let failed_similar: Vec<(String,)> = sqlx::query_as(
            "SELECT error_message FROM brain_errors
             WHERE resolved = false AND similarity(error_message, $1) > 0.5
               AND ($2::uuid IS NULL OR project_id = $2)
               AND trust = 'trusted'
             LIMIT 3",
        )
        .bind(action)
        .bind(project_id)
        .fetch_all(pool)
        .await?;

        if !failed_similar.is_empty() {
            response["anti_repetition_warning"] = serde_json::json!(format!(
                "⚠️ Similar approach failed {} time(s) before",
                failed_similar.len()
            ));
        }
    }

    Ok(response)
}
