// ONE TEST IN THIS FILE, AND IT HAS TO STAY ONE.
//
// `dashboard::render` is private and the only way in is `dashboard::run_cli`,
// which finds its database through `DATABASE_URL` like the binary does. To
// point it at a scratch database this test rewrites that variable, and the
// environment belongs to the whole process. Cargo builds every tests/*.rs into
// its own process, so with a single test here nothing else can read the
// variable while it moves. A second test in this file would share it: it could
// be handed the scratch URL of this one, or, worse, this one could render the
// base the variable named before, which on a developer's machine is their real
// memory. `every_test_that_moves_a_process_wide_variable_serialises` in
// smoke_test.rs goes red when a second test lands in a file that writes the
// environment without taking a guard. Put the new test in a file of its own.
//
// Before this file the test lived with the calibration and export ones and
// seeded whatever `DATABASE_URL` named, so a plain run on a machine that
// exports it towards the live base wrote an entity into it.

mod common;

use common::in_a_scratch_database;
use serde_json::json;
use sqlx::PgPool;

async fn remember(pool: &PgPool, entity: &str, content: &str) {
    memory_industry::handlers::dispatch(
        pool,
        "cuba_cronica",
        json!({
            "action": "add",
            "entity_name": entity,
            "content": content,
            "observation_type": "fact",
            "source": "agent"
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("seeding {entity} through cuba_cronica: {e:#}"));
}

#[tokio::test]
async fn the_dashboard_shows_an_observation_it_read_from_the_base() {
    in_a_scratch_database("brain_dashboard", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let t = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let entity = format!("tablero_{t}");
        let content = format!("el tablero tiene que mostrar esta nota {t}");
        remember(&pool, &entity, &content).await;
        pool.close().await;

        // SAFETY: set_var is unsound only while another thread reads the
        // environment. This process runs this one test (see the top of the
        // file), on a current-thread runtime, and the pool that could have a
        // resolver thread in flight was closed on the line above.
        unsafe { std::env::set_var("DATABASE_URL", &url) };

        let out = std::env::temp_dir().join(format!("cuba-dashboard-{t}.html"));
        let written = memory_industry::dashboard::run_cli(&[out.display().to_string()]).await;
        let html = std::fs::read_to_string(&out).ok();
        std::fs::remove_file(&out).ok();

        written.expect("the dashboard command failed");
        let html = html.expect("the dashboard command returned Ok and wrote no file");
        assert!(
            html.contains(&content),
            "the observation just saved is the only one in this scratch base and has to be \
             among the recent ones the page carries; a page without it was not rendered from \
             the base DATABASE_URL points at"
        );
        assert!(
            html.contains(&entity),
            "the page carries the observation but not the entity it belongs to"
        );
    })
    .await;
}
