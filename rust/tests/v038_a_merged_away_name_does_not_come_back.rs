use uuid::Uuid;

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

#[tokio::test]
#[ignore]
async fn writing_under_a_name_that_was_merged_away_lands_on_the_winner() {
    let pool = pool().await;
    let suffix = &Uuid::new_v4().to_string()[..8];
    let winner_name = format!("MergeWinner_{suffix}");
    let losing_name = format!("MergeLoser_{suffix}");

    let winner: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'project') RETURNING id",
    )
    .bind(&winner_name)
    .fetch_one(&pool)
    .await
    .expect("creating the surviving entity");

    sqlx::query("INSERT INTO brain_entity_aliases (entity_id, alias_text) VALUES ($1, $2)")
        .bind(winner.0)
        .bind(&losing_name)
        .execute(&pool)
        .await
        .expect("recording the merged-away name as an alias, the way dedupe --apply does");

    let args = serde_json::json!({
        "action": "add",
        "entity_name": losing_name,
        "content": "a note written under the name that no longer has an entity of its own",
        "observation_type": "fact",
    });
    memory_industry::handlers::cronica::handle(&pool, args)
        .await
        .expect("writing an observation under the merged-away name");

    let landed_on: Vec<(String,)> = sqlx::query_as(
        "SELECT e.name FROM brain_observations o JOIN brain_entities e ON e.id = o.entity_id
         WHERE o.content LIKE 'a note written under the name that no longer%'",
    )
    .fetch_all(&pool)
    .await
    .expect("reading back where the observation landed");

    let resurrected: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM brain_entities WHERE name = $1")
        .bind(&losing_name)
        .fetch_all(&pool)
        .await
        .expect("checking whether the losing name came back as its own entity");

    sqlx::query("DELETE FROM brain_entities WHERE name IN ($1, $2)")
        .bind(&winner_name)
        .bind(&losing_name)
        .execute(&pool)
        .await
        .ok();

    assert!(
        resurrected.is_empty(),
        "the merged-away name came back as a fresh entity, so the merge silently undid itself. \
         dedupe --apply moves every observation, episode, fact and edge onto the winner and \
         records the loser's name in brain_entity_aliases — a table that until 2026-08-16 was \
         written and never read by anything"
    );
    assert_eq!(
        landed_on.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec![winner_name.as_str()],
        "the observation had to land on the entity that absorbed that name"
    );
}
