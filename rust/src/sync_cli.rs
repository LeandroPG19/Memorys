use anyhow::{Context, Result};

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut action = String::new();
    let mut dir: Option<String> = None;
    let mut scope = "project".to_string();
    let mut conflict = "merge".to_string();
    let mut with_embeddings = false;
    let mut confirm = false;
    let mut json = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "export" | "import" | "diff" | "status" if action.is_empty() => {
                action = a.clone();
            }
            "--dir" | "-d" => dir = it.next().cloned(),
            "--scope" => scope = it.next().cloned().context("--scope needs project|all")?,
            "--conflict" => {
                conflict = it
                    .next()
                    .cloned()
                    .context("--conflict needs merge|skip|overwrite")?
            }
            "--with-embeddings" => with_embeddings = true,
            "--confirm" => confirm = true,
            "--json" => json = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry sync <export|import|diff|status> [--dir PATH]\n\
                     \x20                          [--scope project|all] [--conflict merge|skip|overwrite]\n\
                     \x20                          [--confirm]\n\n\
                     --confirm  a bundle whose tombstones would delete more than 10% of the\n\
                     \x20          observations here, and at least 25 rows, is refused without\n\
                     \x20          this. Read what the refusal says before passing it.\n\
                     \x20                          [--with-embeddings] [--json]\n\n\
                     Git-friendly export/import of the knowledge graph — same engine as the\n\
                     cuba_sync MCP tool. Meant to be driven by `memory-industry hook install`."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown sync flag or duplicate action: {other} (try --help)"),
        }
    }
    if action.is_empty() {
        anyhow::bail!("missing action: export|import|diff|status (try --help)");
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for sync")?;

    let args_json = serde_json::json!({
        "action": action,
        "dir": dir,
        "scope": scope,
        "conflict": conflict,
        "with_embeddings": with_embeddings,
        "confirm": confirm,
    });
    let report = crate::handlers::sync::handle(&pool, args_json).await?;

    if json {
        println!("{report}");
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}
