//! The saved configuration reaches the environment before the tokio runtime exists.
//!
//! `llm_cli::load_saved_config_into_env` and `graph_db::load_saved_config_into_env` call
//! `std::env::set_var`, which Rust 2024 makes `unsafe` because changing the environment
//! while another thread may read it is a data race. Their `SAFETY` comment says
//! «single-threaded startup before tokio workers spawn MCP handlers», and for a long time
//! that was false: both were called at the top of `async_main`, which runs inside
//! `runtime.block_on`, after `Builder::new_multi_thread().build()` had already created the
//! worker threads (found by the 0.32 audit, 2026-10-21). The workers were parked, so nothing
//! was ever observed to go wrong; the contract was broken all the same.
//!
//! `resources::apply`, the other writer of the environment at startup, already sat before
//! the runtime. This holds the two loaders to the same place.

fn main_rs() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn offsets(haystack: &str, needle: &str) -> Vec<usize> {
    haystack.match_indices(needle).map(|(i, _)| i).collect()
}

#[test]
fn the_saved_configuration_is_loaded_before_the_runtime_is_built() {
    let src = main_rs();
    let runtime = *offsets(&src, "Builder::new_multi_thread()")
        .first()
        .expect("main.rs builds the multi-thread runtime");
    let async_main = src
        .find("async fn async_main")
        .expect("main.rs has async_main");

    for loader in [
        "memory_industry::llm_cli::load_saved_config_into_env()",
        "memory_industry::graph_db::load_saved_config_into_env()",
    ] {
        let at = offsets(&src, loader);
        assert!(
            !at.is_empty(),
            "main.rs no longer loads the saved configuration with {loader}: `llm set` and \
             `graph` would stop reaching the daemon"
        );
        for position in at {
            assert!(
                position < runtime,
                "{loader} is called after the tokio runtime is built, so the set_var inside \
                 it runs with worker threads alive. Call it in main(), next to \
                 resources::apply, before Builder::new_multi_thread()"
            );
            assert!(
                position < async_main,
                "{loader} is called inside async_main, which runs under block_on"
            );
        }
    }
}

#[test]
fn the_saved_configuration_is_loaded_after_the_resource_plan_it_must_not_change() {
    // Order held on purpose: the plan reads the environment as the operator left it, and the
    // saved LLM and graph settings are applied after it, exactly as before they moved.
    let src = main_rs();
    let apply = src
        .find("memory_industry::resources::apply(&plan)")
        .expect("main.rs applies the resource plan");
    let load = src
        .find("memory_industry::llm_cli::load_saved_config_into_env()")
        .expect("main.rs loads the saved LLM configuration");
    assert!(
        apply < load,
        "the saved configuration is applied before the resource plan: the plan would now \
         read variables it never saw"
    );
}
