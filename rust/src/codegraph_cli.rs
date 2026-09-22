use anyhow::{Context, Result};

use crate::codegraph::{self, EdgeKind, Symbol, SymbolKind};

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut path_arg: Option<String> = None;
    let mut langs: Vec<String> = Vec::new();
    let mut dry_run = false;
    let mut json = false;
    let mut project_arg: Option<String> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--path" | "-p" => path_arg = it.next().cloned(),
            "--lang" => {
                if let Some(v) = it.next() {
                    langs = v.split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--project" => project_arg = it.next().cloned(),
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry codegraph build [--path DIR] [--lang rust,python] [--project NAME] [--dry-run] [--json]\n\n\
                     Parses source with tree-sitter (deterministic, no LLM, nothing leaves\n\
                     this process) and folds it into the SAME knowledge graph cuba_puente\n\
                     and cuba_faro already use: functions/structs/classes become\n\
                     brain_entities (entity_type='code_symbol'), resolved calls and use/import\n\
                     statements become brain_relations with provenance='extracted'.\n\n\
                     A call only becomes an edge when its callee name matches exactly one\n\
                     symbol in the parsed batch — ambiguous names are dropped, not guessed.\n\n\
                     This runs as its own process — a manual invocation or the post-commit\n\
                     hook from `hook install --with-codegraph` — so it never inherits the\n\
                     active MCP session's project. --project NAME scopes the graph\n\
                     explicitly; omitted, it falls back to the current directory's name\n\
                     (the repo root when run from the hook), same convention as `recall`,\n\
                     so two different repos never collapse into one unscoped bucket.\n\n\
                     --dry-run prints counts without writing anything."
                );
                return Ok(());
            }
            "build" => {}
            other => anyhow::bail!("unknown codegraph flag: {other} (try --help)"),
        }
    }

    let root = codegraph::resolve_path(path_arg.as_deref());
    let extensions = codegraph::default_extensions_for(&langs)?;

    let result = codegraph::extract_dir(&root, &extensions)
        .with_context(|| format!("scanning {}", root.display()))?;
    let call_edges = codegraph::resolve_call_edges(&result.symbols);

    if dry_run {
        let report = serde_json::json!({
            "action": "codegraph_build",
            "dry_run": true,
            "files_parsed": result.files_parsed,
            "files_skipped": result.files_skipped,
            "symbols_found": result.symbols.len(),
            "call_edges_resolved": call_edges.len(),
            "import_statements": result.imports.iter().map(|m| m.paths.len()).sum::<usize>(),
        });
        if json {
            println!("{report}");
        } else {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        return Ok(());
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for codegraph build")?;

    let project_name = project_arg.or_else(|| {
        std::env::current_dir().ok().and_then(|d| {
            d.file_name()
                .and_then(|n| n.to_str())
                .map(std::string::ToString::to_string)
        })
    });
    if let Some(name) = project_name {
        let pid = crate::project::upsert_project(&pool, &name).await?;
        crate::session::set(uuid::Uuid::new_v4(), Some(pid));
    }

    let project_id = crate::project::current_project_id(&pool).await?;

    let mut tx = pool.begin().await?;

    let mut entities_written = 0u32;
    for symbol in &result.symbols {
        upsert_symbol(&mut tx, symbol, project_id).await?;
        entities_written += 1;
    }

    let mut edges_written = 0u32;
    for edge in &call_edges {
        if upsert_edge(&mut tx, &edge.from, &edge.to, edge.kind, project_id).await? {
            edges_written += 1;
        }
    }

    let mut import_edges_written = 0u32;
    for module in &result.imports {
        let from = format!("{}::<module>", module.file);
        upsert_placeholder_entity(&mut tx, &from, "module", project_id).await?;
        for path in &module.paths {
            upsert_placeholder_entity(&mut tx, path, "external_dependency", project_id).await?;
            if upsert_edge(&mut tx, &from, path, EdgeKind::Imports, project_id).await? {
                import_edges_written += 1;
            }
        }
    }

    tx.commit().await?;

    let report = serde_json::json!({
        "action": "codegraph_build",
        "dry_run": false,
        "files_parsed": result.files_parsed,
        "files_skipped": result.files_skipped,
        "symbols_written": entities_written,
        "call_edges_written": edges_written,
        "import_edges_attempted": import_edges_written,
    });

    if json {
        println!("{report}");
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn parsed_from(file: &str) -> String {
    format!("tree-sitter parse of {file}")
}

pub fn symbol_identity(kind_label: &str, simple_name: &str, file: &str) -> String {
    format!("{kind_label} `{simple_name}` in {file}:")
}

fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Struct => "struct",
        SymbolKind::Class => "class",
        SymbolKind::Module => "module",
    }
}

async fn upsert_symbol(
    conn: &mut sqlx::PgConnection,
    symbol: &Symbol,
    project_id: Option<uuid::Uuid>,
) -> Result<()> {
    let entity_id: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type, project_id)
         VALUES ($1, 'code_symbol', $2)
         ON CONFLICT ON CONSTRAINT uq_brain_entities_name_project
         DO UPDATE SET entity_type = 'code_symbol'
         RETURNING id",
    )
    .bind(&symbol.qualified_name)
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?;

    let identity = symbol_identity(kind_label(symbol.kind), &symbol.simple_name, &symbol.file);
    let content = format!(
        "{}{}-{}\n{}",
        identity, symbol.line_start, symbol.line_end, symbol.signature
    );

    let refreshed = sqlx::query(
        "UPDATE brain_observations
         SET content = $2, updated_at = NOW(), evidence = 'observed', verification = $5
         WHERE entity_id = $1
           AND observation_type = 'context'
           AND source = 'agent'
           AND left(content, $3) = $4
           AND content <> $2",
    )
    .bind(entity_id.0)
    .bind(&content)
    .bind(identity.chars().count() as i32)
    .bind(&identity)
    .bind(parsed_from(&symbol.file))
    .execute(&mut *conn)
    .await?;

    if refreshed.rows_affected() > 0 {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO brain_observations
            (entity_id, content, observation_type, source, project_id, evidence, verification)
         SELECT $1, $2, 'context', 'agent', $3, 'observed', $6
         WHERE NOT EXISTS (
             SELECT 1 FROM brain_observations
             WHERE entity_id = $1 AND left(content, $4) = $5
         )",
    )
    .bind(entity_id.0)
    .bind(&content)
    .bind(project_id)
    .bind(identity.chars().count() as i32)
    .bind(&identity)
    .bind(parsed_from(&symbol.file))
    .execute(&mut *conn)
    .await?;

    Ok(())
}

async fn upsert_placeholder_entity(
    conn: &mut sqlx::PgConnection,
    name: &str,
    entity_type: &str,
    project_id: Option<uuid::Uuid>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO brain_entities (name, entity_type, project_id)
         VALUES ($1, $2, $3)
         ON CONFLICT ON CONSTRAINT uq_brain_entities_name_project DO NOTHING",
    )
    .bind(name)
    .bind(entity_type)
    .bind(project_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn upsert_edge(
    conn: &mut sqlx::PgConnection,
    from_name: &str,
    to_name: &str,
    kind: EdgeKind,
    project_id: Option<uuid::Uuid>,
) -> Result<bool> {
    let from_id: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM brain_entities WHERE name = $1")
            .bind(from_name)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((from_id,)) = from_id else {
        return Ok(false);
    };

    let to_id: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM brain_entities WHERE name = $1")
            .bind(to_name)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((to_id,)) = to_id else {
        return Ok(false);
    };

    let result = sqlx::query(
        "INSERT INTO brain_relations (from_entity, to_entity, relation_type, project_id, provenance)
         VALUES ($1, $2, $3, $4, 'extracted')
         ON CONFLICT (from_entity, to_entity, relation_type)
         DO UPDATE SET strength = LEAST(brain_relations.strength + 0.1, 1.0),
                       last_traversed = NOW(),
                       provenance = 'extracted'",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(kind.as_relation_type())
    .bind(project_id)
    .execute(&mut *conn)
    .await?;

    Ok(result.rows_affected() > 0)
}
