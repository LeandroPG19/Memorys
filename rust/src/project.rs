use anyhow::Result;
use sqlx::PgPool;
use std::env;
use uuid::Uuid;

use crate::constants::KILL_SWITCH_ENV;

pub fn filter_disabled() -> bool {
    env::var(KILL_SWITCH_ENV)
        .ok()
        .is_some_and(|v| v.eq_ignore_ascii_case("off"))
}

pub fn rls_scope() -> String {
    if filter_disabled() {
        return "*".to_string();
    }
    crate::session::project_id()
        .map(|u| u.to_string())
        .unwrap_or_default()
}

pub async fn current_project_id(_pool: &PgPool) -> Result<Option<Uuid>> {
    if filter_disabled() {
        return Ok(None);
    }
    Ok(crate::session::project_id().or_else(crate::session::client_root_project))
}

pub async fn resolve_project_name(pool: &PgPool, name: &str) -> Result<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM brain_projects WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(id,)| id))
}

pub async fn upsert_project(pool: &PgPool, name: &str) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_projects (name) VALUES ($1)
         ON CONFLICT (name) DO UPDATE SET last_active_at = NOW()
         RETURNING id",
    )
    .bind(name)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// SQL predicate: no active project → unfiltered; active project → that id only.
/// `include_unscoped` is the escape hatch for rows that still have project_id NULL.
pub fn project_match_sql(column: &str, bind: u32, include_unscoped: bool) -> String {
    if include_unscoped {
        format!("(${bind}::uuid IS NULL OR {column} = ${bind} OR {column} IS NULL)")
    } else {
        format!("(${bind}::uuid IS NULL OR {column} = ${bind})")
    }
}

/// Open a transaction whose RLS setting cannot lock the writer into the previous tenant.
/// Used by jornada/proyecto when they write brain_sessions (control plane).
pub async fn begin_write_scope(pool: &PgPool) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.current_project', '*', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

const BACKFILL_TABLES: [&str; 5] = [
    "brain_entities",
    "brain_observations",
    "brain_episodes",
    "brain_errors",
    "brain_sessions",
];

pub async fn backfill_unscoped(
    pool: &PgPool,
    project_id: Uuid,
    apply: bool,
) -> Result<serde_json::Value> {
    let mut counts = serde_json::Map::new();
    for table in BACKFILL_TABLES {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE project_id IS NULL"
        ))
        .fetch_one(pool)
        .await?;
        counts.insert(table.to_string(), serde_json::json!(n));
    }
    if !apply {
        return Ok(serde_json::json!({
            "dry_run": true,
            "would_assign": counts,
            "note": "pass confirm=true to write. This assigns every NULL project_id row \
                     to the named project — run dry_run first on a live corpus.",
        }));
    }
    let mut moved = serde_json::Map::new();
    let mut tx = begin_write_scope(pool).await?;
    for table in BACKFILL_TABLES {
        let q = format!("UPDATE {table} SET project_id = $1 WHERE project_id IS NULL");
        let rows = sqlx::query(&q).bind(project_id).execute(&mut *tx).await?;
        moved.insert(table.to_string(), serde_json::json!(rows.rows_affected()));
    }
    tx.commit().await?;
    Ok(serde_json::json!({
        "dry_run": false,
        "assigned": moved,
    }))
}

pub async fn observation_in_scope(
    pool: &PgPool,
    observation_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<bool> {
    if filter_disabled() {
        return Ok(true);
    }
    let Some(pid) = project_id else {
        return Ok(true);
    };
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM brain_observations
         WHERE id = $1 AND project_id = $2
         LIMIT 1",
    )
    .bind(observation_id)
    .bind(pid)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_is_never_queried() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://nobody@127.0.0.1:1/nothing")
            .expect("a lazy pool never dials until something queries it")
    }

    #[tokio::test]
    async fn a_write_with_no_session_lands_in_the_project_the_client_is_working_in() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_is_never_queried();
        let from_root = Uuid::new_v4();

        crate::session::with_root_project(Some(from_root), async {
            crate::session::clear();

            assert_eq!(
                current_project_id(&pool).await.unwrap(),
                Some(from_root),
                "with no jornada open this used to return None, and the row was written with \
                 project_id NULL — 409 of 1907 observations ended up that way by 16-ago-2026, \
                 every one of them visible from every project because tenant_isolation lets \
                 NULL through. The client told us its working directory in the handshake"
            );

            let from_session = Uuid::new_v4();
            crate::session::set(Uuid::new_v4(), Some(from_session));
            assert_eq!(
                current_project_id(&pool).await.unwrap(),
                Some(from_session),
                "an explicit jornada must always beat the directory we guessed from: the root \
                 is a fallback for writes that would otherwise be homeless, not an override"
            );

            crate::session::clear();
        })
        .await;
    }

    #[tokio::test]
    async fn the_root_reaches_the_write_even_when_the_client_declared_no_identity() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_is_never_queried();
        let from_root = Uuid::new_v4();
        crate::session::clear();
        crate::session::remember_client_root("anonymous", from_root);

        let seen = crate::session::with_root_project(
            crate::session::client_root_project_for("anonymous"),
            async { current_project_id(&pool).await.unwrap() },
        )
        .await;

        crate::session::forget_client("anonymous");

        assert_eq!(
            seen,
            Some(from_root),
            "the daemon only enters with_client when the client declared an identity through \
             _meta, on purpose: every Claude Code instance sends the same clientInfo.name and \
             sharing a session between them was a real bug. Reading the root through \
             current_client() therefore found nothing for exactly the clients this feature \
             exists for — measured end to end on 2026-08-16, the daemon logged 'adopted the \
             client's root as its project' and the observation still landed with project_id \
             NULL. The root belongs to the connection, not to a declared identity"
        );
    }

    #[tokio::test]
    async fn the_scope_the_pool_stamps_is_the_active_project() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        crate::session::clear();
        assert_eq!(
            rls_scope(),
            "",
            "with no session the scope is empty, which the tenant_isolation policy reads \
             as unfiltered — the WHERE clause in each handler is what narrows it there"
        );

        let project = Uuid::new_v4();
        crate::session::set(Uuid::new_v4(), Some(project));
        assert_eq!(
            rls_scope(),
            project.to_string(),
            "before_acquire stamps this onto every connection the pool hands out; if it \
             disagreed with what the handler binds, RLS would clamp to a different project"
        );

        crate::session::clear();
    }

    #[test]
    fn an_active_project_does_not_see_null_rows_unless_asked() {
        assert_eq!(
            project_match_sql("o.project_id", 5, false),
            "($5::uuid IS NULL OR o.project_id = $5)",
            "the silent OR project_id IS NULL is what mixed every imported memory into \
             every scoped faro call"
        );
        assert!(
            project_match_sql("project_id", 1, true).contains("OR project_id IS NULL"),
            "include_unscoped is the explicit escape, not the default"
        );
    }

    #[tokio::test]
    async fn the_kill_switch_widens_the_scope_instead_of_emptying_it() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        crate::session::set(Uuid::new_v4(), Some(Uuid::new_v4()));
        unsafe { std::env::set_var(KILL_SWITCH_ENV, "off") };

        assert_eq!(
            rls_scope(),
            "*",
            "the kill switch has to say `*` explicitly: an empty string means the same \
             thing to the policy today, but only `*` survives a policy that stops \
             treating empty as unfiltered"
        );

        unsafe { std::env::remove_var(KILL_SWITCH_ENV) };
        crate::session::clear();
    }
}
