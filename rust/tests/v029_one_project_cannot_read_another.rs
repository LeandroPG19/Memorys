use uuid::Uuid;

async fn as_app_role(pool: &sqlx::PgPool, project: Uuid, sql: &str) -> Vec<String> {
    let mut conn = pool.acquire().await.expect("a connection");
    sqlx::query("SET ROLE cuba_app")
        .execute(&mut *conn)
        .await
        .expect("cuba_app exists — `cuba-memorys secure` creates it");
    sqlx::query("SELECT set_config('app.current_project', $1, false)")
        .bind(project.to_string())
        .execute(&mut *conn)
        .await
        .expect("declare the scope");
    let rows: Vec<(String,)> = sqlx::query_as(sql)
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default();
    sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
    rows.into_iter().map(|(t,)| t).collect()
}

#[tokio::test]
#[ignore]
async fn the_text_of_one_project_never_comes_back_inside_another() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("leak_{}", &Uuid::new_v4().to_string()[..8]);
    let mine: Uuid =
        sqlx::query_scalar("INSERT INTO brain_projects (name) VALUES ($1) RETURNING id")
            .bind(format!("{marker}_mio"))
            .fetch_one(&pool)
            .await
            .expect("seed my project");
    let theirs: Uuid =
        sqlx::query_scalar("INSERT INTO brain_projects (name) VALUES ($1) RETURNING id")
            .bind(format!("{marker}_ajeno"))
            .fetch_one(&pool)
            .await
            .expect("seed the other project");

    let entity: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type, project_id) VALUES ($1, 'concept', $2)
         RETURNING id",
    )
    .bind(&marker)
    .bind(theirs)
    .fetch_one(&pool)
    .await
    .expect("seed their entity");
    let secret = format!("{marker} SECRETO del proyecto ajeno");
    let observation: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content, project_id) VALUES ($1, $2, $3)
         RETURNING id",
    )
    .bind(entity)
    .bind(&secret)
    .bind(theirs)
    .fetch_one(&pool)
    .await
    .expect("seed their observation");

    sqlx::query(
        "INSERT INTO brain_sync_conflicts (observation_id, local_content, incoming_content)
         VALUES ($1, $2, $3)",
    )
    .bind(observation)
    .bind(&secret)
    .bind(format!("{secret} corregido en la otra maquina"))
    .execute(&pool)
    .await
    .expect("seed the conflict that carries their text verbatim");

    sqlx::query(
        "INSERT INTO brain_compaction_snapshots (session_id, summary_md, project_id)
         VALUES (NULL, $1, $2)",
    )
    .bind(format!("# {secret}"))
    .bind(theirs)
    .execute(&pool)
    .await
    .expect("seed their session summary");

    let baseline = as_app_role(
        &pool,
        theirs,
        "SELECT local_content FROM brain_sync_conflicts WHERE local_content LIKE 'leak_%'",
    )
    .await;
    assert!(
        baseline.iter().any(|t| t.contains(&marker)),
        "the owning project has to see its own conflict, or this test proves nothing about \
         hiding it from anybody else — a policy that hides it from everyone would pass the \
         next assertion for the wrong reason"
    );

    let conflicts = as_app_role(
        &pool,
        mine,
        "SELECT local_content FROM brain_sync_conflicts WHERE local_content LIKE 'leak_%'",
    )
    .await;
    assert!(
        !conflicts.iter().any(|t| t.contains(&marker)),
        "brain_sync_conflicts stores observation text verbatim and had no policy, so a session \
         scoped to one project read another's text straight out of it — the same text the \
         policy on brain_observations was denying through the front door. Got: {conflicts:?}"
    );

    let snapshots = as_app_role(
        &pool,
        mine,
        "SELECT summary_md FROM brain_compaction_snapshots WHERE summary_md LIKE '# leak_%'",
    )
    .await;
    assert!(
        !snapshots.iter().any(|t| t.contains(&marker)),
        "and the compaction summaries were worse: protocol.rs listed the last twenty of every \
         project as MCP resources — handing out their ids — and served summary_md with no \
         filter at all. That one needs no sync to have happened. Got: {snapshots:?}"
    );

    sqlx::query("DELETE FROM brain_projects WHERE id = ANY($1)")
        .bind(vec![mine, theirs])
        .execute(&pool)
        .await
        .ok();
}
