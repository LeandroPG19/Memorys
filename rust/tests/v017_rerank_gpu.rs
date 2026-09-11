static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn own_the_environment() -> std::sync::MutexGuard<'static, ()> {
    ENV_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn fixed_shape_defaults_to_the_gpu_answer_and_is_overridable() {
    let _env = own_the_environment();
    unsafe { std::env::remove_var("CUBA_RERANK_FIXED_SHAPE") };
    assert_eq!(
        memory_industry::search::rerank::fixed_shape(),
        cfg!(any(feature = "cuda", feature = "directml")),
        "padding to a constant 512 pays off only on GPU, where a new tensor shape forces a \
         kernel recompile; on CPU it just makes every batch bigger"
    );

    unsafe { std::env::set_var("CUBA_RERANK_FIXED_SHAPE", "0") };
    assert!(!memory_industry::search::rerank::fixed_shape());

    unsafe { std::env::set_var("CUBA_RERANK_FIXED_SHAPE", "1") };
    assert!(memory_industry::search::rerank::fixed_shape());

    unsafe { std::env::remove_var("CUBA_RERANK_FIXED_SHAPE") };
}

#[test]
fn is_configured_reports_whether_a_model_is_on_disk_without_loading_it() {
    let _env = own_the_environment();
    let missing = std::env::temp_dir().join("cuba-no-reranker-here");
    unsafe { std::env::set_var("CUBA_RERANKER_PATH", &missing) };
    assert!(
        !memory_industry::search::rerank::is_configured(),
        "a path with no model.onnx must not claim to be configured, or startup would warm \
         up something that cannot load"
    );
    unsafe { std::env::remove_var("CUBA_RERANKER_PATH") };
}

#[tokio::test]
#[ignore]
async fn warming_up_leaves_the_reranker_ready() {
    assert!(
        memory_industry::search::rerank::is_configured(),
        "no reranker model on disk: this suite measures the model path and cannot report on it"
    );
    let started = std::time::Instant::now();
    let warm = memory_industry::search::rerank::warm_up().await;
    assert!(warm, "a configured reranker must warm up successfully");
    assert!(
        memory_industry::search::rerank::enabled(),
        "after warm-up the reranker must report enabled, so searches take the model path"
    );
    eprintln!("warm-up took {:.2}s", started.elapsed().as_secs_f32());
}

#[tokio::test]
#[ignore]
async fn reranking_reorders_candidates_by_relevance() {
    assert!(
        memory_industry::search::rerank::is_configured(),
        "no reranker model on disk: this suite measures the model path and cannot report on it"
    );
    memory_industry::search::rerank::warm_up().await;
    assert!(
        memory_industry::search::rerank::enabled(),
        "the reranker is configured but did not load. That is the failure this test exists to \
         catch — the identity fallback returns the RRF order unchanged and every search looks \
         like it worked"
    );

    let query = "how does the REM consolidation cycle decay old memories";
    let candidates = vec![
        "Bees overwinter best when the hive is insulated and the entrance reduced.",
        "The REM consolidation cycle applies stratified exponential decay to observation \
         importance, with a half-life that depends on the observation type.",
        "Docker multi-stage builds keep the final image small by discarding build tooling.",
    ];

    let scored = memory_industry::search::rerank::rerank(query, &candidates)
        .await
        .expect("reranking");
    assert_eq!(scored.len(), candidates.len());
    assert_eq!(
        scored[0].0,
        1,
        "the passage that actually describes REM decay must rank first, got order {:?}",
        scored.iter().map(|(i, _)| *i).collect::<Vec<_>>()
    );
    assert!(
        scored[0].1 > scored[2].1,
        "the cross-encoder must separate the relevant passage from the irrelevant ones"
    );
}
