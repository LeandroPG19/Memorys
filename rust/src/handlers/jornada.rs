use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "start" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            let goals = args.get("goals").cloned().unwrap_or(Value::Array(vec![]));

            crate::redact::refuse_secrets(&args, "name", name)?;
            crate::redact::refuse_secrets(
                &args,
                "goals",
                &serde_json::to_string_pretty(&goals).unwrap_or_default(),
            )?;

            let project_arg = args.get("project").and_then(|v| v.as_str());
            let project_id = match project_arg {
                Some(p) if !p.is_empty() => Some(crate::project::upsert_project(pool, p).await?),
                _ => None,
            };

            let mut tx = crate::project::begin_write_scope(pool)
                .await
                .context("failed to open a write scope for the new session")?;

            let replaced = if let Some(prev) = crate::session::session_id() {
                sqlx::query(
                    "UPDATE brain_sessions
                     SET ended_at = NOW(),
                         outcome = 'abandoned',
                         summary = 'replaced by a new jornada start on this client'
                     WHERE id = $1 AND ended_at IS NULL",
                )
                .bind(prev)
                .execute(&mut *tx)
                .await
                .context("failed to close the previous session on this client")?
                .rows_affected()
            } else {
                0
            };

            let row: (uuid::Uuid,) = sqlx::query_as(
                "INSERT INTO brain_sessions (session_name, goals, project_id)
                 VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(name)
            .bind(&goals)
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to start session")?;

            tx.commit()
                .await
                .context("failed to commit the new session")?;

            crate::session::set(row.0, project_id);

            let mut response = serde_json::json!({
                "action": "started",
                "session": {
                    "id": row.0.to_string(),
                    "session_name": name,
                    "started_at": chrono::Utc::now().to_rfc3339(),
                    "project_id": project_id.map(|p| p.to_string()),
                    "project_name": project_arg,
                },
                "replaced_previous": replaced > 0,
            });

            let prev_session: Option<(Option<String>, Option<String>, Option<String>)> =
                sqlx::query_as(
                    "SELECT session_name, summary, outcome FROM brain_sessions
                     WHERE ended_at IS NOT NULL ORDER BY ended_at DESC LIMIT 1",
                )
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();

            if let Some((prev_name, prev_summary, prev_outcome)) = prev_session {
                response["previous_session"] = serde_json::json!({
                    "name": prev_name,
                    "summary": prev_summary,
                    "outcome": prev_outcome
                });
            }

            let triggered =
                crate::handlers::centinela::check_triggers(pool, name, "on_session_start")
                    .await
                    .unwrap_or_default();
            if !triggered.is_empty() {
                response["triggered_reminders"] = serde_json::json!(triggered);
            }

            let notices = crate::handlers::sync::pending_notices(pool)
                .await
                .unwrap_or_default();
            if !notices.is_empty() {
                response["peer_notices"] = serde_json::json!(notices);
                response["peer_notices_note"] = serde_json::json!(
                    "the other machine says it learned something you do not have. Nothing of \
                     it is in this database: a peer can only ring the bell. Take it with \
                     cuba_sync action=pull against that node and import what comes back, or \
                     ignore it — the notice closes itself when a bundle with its hash lands."
                );
            }

            let pending_review: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM brain_observations
                 WHERE trust = 'quarantined'
                   AND ($1::uuid IS NULL OR project_id = $1)",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            if pending_review > 0 {
                response["observations_pending_review"] = serde_json::json!(pending_review);
                response["observations_pending_review_note"] = serde_json::json!(
                    "written by the REM cycle's auto-extraction, or imported from a peer, and \
                     held back from cuba_faro until a human looks at them — cuba_eco \
                     action=pending lists them, action=promote clears one."
                );
            }

            Ok(response)
        }
        "end" => {
            let outcome = args
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("success");
            let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");

            crate::redact::refuse_secrets(&args, "summary", summary)?;

            let Some(own_session) = crate::session::session_id() else {
                return Ok(serde_json::json!({
                    "action": "ended",
                    "updated": false,
                    "reason": "no session was started by this process — nothing to end",
                }));
            };

            let active_session: Option<(uuid::Uuid,)> = sqlx::query_as(
                "UPDATE brain_sessions SET ended_at = NOW(), outcome = $1, summary = $2
                 WHERE id = $3 AND ended_at IS NULL
                 RETURNING id",
            )
            .bind(outcome)
            .bind(summary)
            .bind(own_session)
            .fetch_optional(pool)
            .await?;

            let updated = active_session.is_some();
            crate::session::clear();

            let mut response = serde_json::json!({
                "action": "ended",
                "outcome": outcome,
                "updated": updated
            });

            if let Some((session_id,)) = active_session {
                let session_diff: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT observation_type, COUNT(*) FROM brain_observations
                     WHERE session_id = $1 GROUP BY observation_type",
                )
                .bind(session_id)
                .fetch_all(pool)
                .await
                .unwrap_or_default();

                let episode_count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM brain_episodes WHERE session_id = $1")
                        .bind(session_id)
                        .fetch_one(pool)
                        .await
                        .unwrap_or(0);

                let mut diff = serde_json::Map::new();
                for (obs_type, count) in &session_diff {
                    diff.insert(obs_type.clone(), serde_json::json!(count));
                }
                diff.insert("episodes".to_string(), serde_json::json!(episode_count));

                response["session_diff"] = Value::Object(diff);
            }

            Ok(response)
        }
        "current" => {
            type SessionRow = (
                uuid::Uuid,
                Option<String>,
                Value,
                chrono::DateTime<chrono::Utc>,
            );
            let session: Option<SessionRow> = match crate::session::session_id() {
                Some(sid) => {
                    sqlx::query_as(
                        "SELECT id, session_name, goals, started_at FROM brain_sessions
                         WHERE id = $1 AND ended_at IS NULL",
                    )
                    .bind(sid)
                    .fetch_optional(pool)
                    .await?
                }
                None => None,
            };
            match session {
                Some((id, name, goals, started_at)) => {
                    let obs_count: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM brain_observations WHERE session_id = $1",
                    )
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap_or(0);

                    let elapsed = chrono::Utc::now() - started_at;
                    let compaction_hint = elapsed
                        > chrono::Duration::seconds(crate::constants::COMPACTION_HINT_HOURS * 3600)
                        || obs_count >= crate::constants::COMPACTION_HINT_OBS_COUNT;

                    let last_snapshot: Option<(uuid::Uuid, chrono::DateTime<chrono::Utc>)> =
                        sqlx::query_as(
                            "SELECT id, created_at FROM brain_compaction_snapshots
                             WHERE session_id = $1
                             ORDER BY created_at DESC LIMIT 1",
                        )
                        .bind(id)
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten();

                    Ok(serde_json::json!({
                        "action": "current",
                        "session": {
                            "id": id.to_string(),
                            "name": name,
                            "goals": goals,
                            "started_at": started_at.to_rfc3339(),
                            "obs_count_in_session": obs_count,
                        },
                        "compaction_hint": compaction_hint,
                        "last_snapshot": last_snapshot.map(|(sid, ts)| {
                            serde_json::json!({
                                "id": sid.to_string(),
                                "created_at": ts.to_rfc3339(),
                            })
                        }),
                    }))
                }
                None => Ok(serde_json::json!({
                    "action": "current",
                    "session": null,
                    "compaction_hint": false,
                    "last_snapshot": null,
                })),
            }
        }
        "list" => {
            type SessionRow = (uuid::Uuid, Option<String>, Option<String>, Option<String>);
            let sessions: Vec<SessionRow> = sqlx::query_as(
                "SELECT id, session_name, outcome, summary FROM brain_sessions ORDER BY started_at DESC LIMIT 20"
            ).fetch_all(pool).await?;
            let list: Vec<Value> = sessions.iter().map(|(id, name, outcome, summary)| {
                serde_json::json!({"id": id.to_string(), "name": name, "outcome": outcome, "summary": summary})
            }).collect();
            Ok(serde_json::json!({"action": "list", "sessions": list, "count": list.len()}))
        }
        _ => anyhow::bail!("Invalid action: {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://jornada-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn closing_a_session_with_a_credential_in_the_summary_is_refused() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "end",
            "outcome": "success",
            "summary": "quedó desplegado usando ghp_abcdefghijklmnop"
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "end answered Ok without a live session, which is the path that returns before \
                 touching the database: the gate has to run before that shortcut, or a summary \
                 written by a process with no session of its own escapes it"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token") && chain.contains("summary"),
            "the end-of-session summary is read back verbatim at the start of the next session \
             as previous_session.summary, so a credential stored here is re-served to the next \
             agent without anyone asking for it. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn a_goal_that_carries_a_credential_is_refused_at_the_start_of_the_session() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "start",
            "name": "deploy",
            "goals": ["rotar la clave del bucket", "ghp_abcdefghijklmnop"]
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "start persists goals into brain_sessions and they are read back by every later \
                 session, so the gate belongs here too. It also has to run before upsert_project, \
                 or a write that is going to be refused creates a project row on its way out"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("goals"),
            "goals is a JSON array, so it only reaches the scanner if it is serialised first — \
             and pretty-printed, because a compact array hides its strings from a scanner that \
             works on text. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret: {chain}"
        );
    }

    #[tokio::test]
    async fn a_session_named_after_the_work_is_not_refused() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "start",
            "name": "rotación de credenciales y endurecimiento del login",
            "goals": ["revisar que ninguna password quede en los logs", "subir el rate limit"]
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!("this pool cannot connect, so reaching the database is the pass condition")
        };

        let chain = format!("{failure:#}");
        assert!(
            !chain.contains("refusing to write"),
            "naming a credential is not carrying one. A gate that refuses the words operators \
             actually use to describe security work gets switched off, and then it protects \
             nothing. Got: {chain}"
        );
    }
}
