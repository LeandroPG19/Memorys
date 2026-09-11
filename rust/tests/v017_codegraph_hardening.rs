use std::fs;
use uuid::Uuid;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cuba_cg_{}_{}",
        name,
        &Uuid::new_v4().to_string()[..8]
    ));
    fs::create_dir_all(&dir).expect("creating scratch dir");
    dir
}

#[test]
fn a_symlink_loop_does_not_hang_the_walker() {
    let root = scratch("symlink");
    let nested = root.join("src").join("deep");
    fs::create_dir_all(&nested).expect("creating nested dirs");
    fs::write(nested.join("real.rs"), "pub fn only_real() {}\n").expect("writing a real file");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&root, nested.join("loop_back")).expect("creating the cycle");

        let result = memory_industry::codegraph::extract_dir(&root, &["rs"])
            .expect("walking a tree that links back to itself must terminate");

        assert_eq!(
            result.files_parsed, 1,
            "the one real file must be parsed exactly once, not once per loop iteration"
        );
        assert!(
            result.files_skipped.iter().any(|(_, why)| why == "symlink"),
            "the skipped symlink must be reported, not silently ignored: {:?}",
            result.files_skipped
        );
    }

    fs::remove_dir_all(&root).ok();
}

#[test]
fn the_symbol_identity_survives_line_number_changes() {
    let before =
        memory_industry::codegraph_cli::symbol_identity("function", "handle", "src/api.rs");
    let after = memory_industry::codegraph_cli::symbol_identity("function", "handle", "src/api.rs");
    assert_eq!(
        before, after,
        "identity must not depend on where the symbol currently sits"
    );
    assert!(
        !before.contains(char::is_numeric),
        "line numbers must stay out of the identity, otherwise editing anything above a symbol \
         orphans its old row: {before}"
    );

    let other_file =
        memory_industry::codegraph_cli::symbol_identity("function", "handle", "src/other.rs");
    assert_ne!(
        before, other_file,
        "the same name in another file is a different symbol"
    );
    let other_kind =
        memory_industry::codegraph_cli::symbol_identity("struct", "handle", "src/api.rs");
    assert_ne!(before, other_kind, "kind is part of the identity");
}

async fn build(root: &std::path::Path) {
    let args: Vec<String> = vec![
        "--path".to_string(),
        root.display().to_string(),
        "--lang".to_string(),
        "rust".to_string(),
        "--json".to_string(),
    ];
    memory_industry::codegraph_cli::run_cli(&args)
        .await
        .expect("codegraph build");
}

#[tokio::test]
#[ignore]
async fn rebuilding_after_an_edit_refreshes_the_row_instead_of_orphaning_it() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let root = scratch("rebuild");
    let src = root.join("src");
    fs::create_dir_all(&src).expect("creating src");
    let file = src.join("lib.rs");

    fs::write(&file, "pub fn target_symbol(a: u32) -> u32 { a }\n").expect("writing v1");
    build(&root).await;

    let after_first: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE e.entity_type = 'code_symbol' AND o.content LIKE '%target_symbol%'",
    )
    .fetch_one(&pool)
    .await
    .expect("counting after the first build");
    assert!(after_first >= 1, "the symbol must be recorded");

    fs::write(
        &file,
        "// a new comment line pushes everything down\n\
         // and another one\n\
         pub fn target_symbol(a: u32) -> u32 { a }\n",
    )
    .expect("writing v2");
    build(&root).await;

    let after_second: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE e.entity_type = 'code_symbol' AND o.content LIKE '%target_symbol%'",
    )
    .fetch_one(&pool)
    .await
    .expect("counting after the second build");

    assert_eq!(
        after_second, after_first,
        "moving a symbol down two lines must update its row, not leave the old one behind"
    );

    let content: String = sqlx::query_scalar(
        "SELECT o.content FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE e.entity_type = 'code_symbol' AND o.content LIKE '%target_symbol%'
         LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("reading the row back");
    assert!(
        content.contains(":3-3"),
        "the surviving row must carry the NEW line numbers, got: {content}"
    );

    sqlx::query(
        "DELETE FROM brain_entities WHERE entity_type = 'code_symbol' AND name LIKE '%target_symbol%'",
    )
    .execute(&pool)
    .await
    .ok();
    fs::remove_dir_all(&root).ok();
}
