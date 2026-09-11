#[tokio::test]
#[ignore]
async fn resources_starving_it_of_ram_reads_differently_from_no_model_installed() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let original = std::env::var("CUBA_RERANKER_PATH").ok();

    unsafe {
        std::env::set_var(
            "CUBA_RERANKER_PATH",
            memory_industry::resources::disabled_model_path(),
        )
    };
    let starved = memory_industry::doctor::run_checks_with(&pool, &url, false).await;
    let starved_reranker = starved
        .iter()
        .find(|c| c.name == "reranker")
        .expect("doctor always reports a reranker check");
    assert!(
        !starved_reranker.detail.contains("no hay modelo en disco"),
        "the resource plan pointed CUBA_RERANKER_PATH at a directory it invented for lack \
         of RAM at startup, and that must not read the same as never having installed a \
         model at all: {}",
        starved_reranker.detail
    );
    assert!(
        starved_reranker
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("reiniciá")),
        "this cause is fixed for the life of the process — the hint has to say a restart \
         is required, or raising the RAM limit later looks like it should have worked: {:?}",
        starved_reranker.hint
    );

    unsafe {
        std::env::set_var(
            "CUBA_RERANKER_PATH",
            std::env::temp_dir().join("cuba-v032-nothing-here-at-all"),
        )
    };
    let never_installed = memory_industry::doctor::run_checks_with(&pool, &url, false).await;
    let never_installed_reranker = never_installed
        .iter()
        .find(|c| c.name == "reranker")
        .expect("doctor always reports a reranker check");
    assert!(
        never_installed_reranker
            .detail
            .contains("no hay modelo en disco"),
        "a path the resource plan never touched has to fall back to the plain 'no model' \
         message, not the RAM one: {}",
        never_installed_reranker.detail
    );

    assert_ne!(
        starved_reranker.detail, never_installed_reranker.detail,
        "two different root causes producing the exact same sentence through the real \
         doctor code path is the bug this test exists to catch"
    );

    match original {
        Some(v) => unsafe { std::env::set_var("CUBA_RERANKER_PATH", v) },
        None => unsafe { std::env::remove_var("CUBA_RERANKER_PATH") },
    }
}
