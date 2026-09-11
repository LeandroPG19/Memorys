use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::handlers;

pub const COMMANDS: [&str; 23] = [
    "serve",
    "tunnel",
    "search",
    "save",
    "delete",
    "export",
    "dashboard",
    "doctor",
    "recall",
    "reembed",
    "calibrate",
    "link",
    "dedupe",
    "skills",
    "eval",
    "sync",
    "hook",
    "codegraph",
    "rem",
    "models",
    "llm",
    "secure",
    "setup",
];

fn undo_dir() -> std::path::PathBuf {
    std::env::var("CUBA_UNDO_DIR").map_or_else(
        |_| {
            let cache = dirs_home().join(".cache");
            let preferred = cache.join("memory-industry").join("undo");
            let legacy = cache.join("cuba-memorys").join("undo");
            if preferred.exists() || !legacy.exists() {
                preferred
            } else {
                legacy
            }
        },
        std::path::PathBuf::from,
    )
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME").map_or_else(|_| std::path::PathBuf::from("."), std::path::PathBuf::from)
}

async fn pool() -> Result<PgPool> {
    let url = crate::setup::resolve_database_url().await;
    crate::db::create_pool(&url)
        .await
        .context("connecting to database")
}

fn ellipsize(s: &str, max: usize) -> String {
    let clean = s.replace('\n', " ");
    if clean.chars().count() <= max {
        return clean;
    }
    let cut: String = clean.chars().take(max).collect();
    format!("{cut}…")
}

pub async fn run_search(args: &[String]) -> Result<()> {
    let mut query: Option<String> = None;
    let mut limit: i64 = 10;
    let mut json_out = false;
    let mut associative = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" | "-n" => {
                limit = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--limit needs an integer")?;
            }
            "--json" => json_out = true,
            "--associative" => associative = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: cuba-memorys search <query> [--limit N] [--associative] [--json]\n\n\
                     Hybrid retrieval (text + vector + BM25, RRF-fused) — the same engine\n\
                     cuba_faro serves to the agent.\n\n\
                     Nota: `--format` no existe aquí. El CLI renderiza para un humano;\n\
                     el formato compact/verbose es cosa de la tool MCP."
                );
                return Ok(());
            }
            flag if flag.starts_with("--") => {
                bail!(
                    "search: opción desconocida `{flag}`.\n\
                     Si es parte de lo que buscás, entrecomillalo: search \"{flag}\".\n\
                     Opciones válidas: --limit N · --associative · --json"
                );
            }
            other => {
                if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    let q = query.take().unwrap_or_default();
                    query = Some(format!("{q} {other}"));
                }
            }
        }
    }

    let Some(q) = query else {
        bail!("falta la query — uso: cuba-memorys search \"texto a buscar\"");
    };

    let pool = pool().await?;
    let result = handlers::faro::handle(
        &pool,
        json!({
            "query": q,
            "limit": limit,
            "associative": associative,
            "format": "verbose",
        }),
    )
    .await
    .context("search failed")?;

    if json_out {
        println!("{result}");
        return Ok(());
    }

    let empty = vec![];
    let results = result
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    if results.is_empty() {
        println!("Sin resultados para «{q}».");
        println!("\nSi esperabas resultados, corré `cuba-memorys doctor`: el recall puede estar");
        println!("degradado en silencio (modelo ONNX no cargado, o falta cuba_or_tsquery).");
        return Ok(());
    }

    println!("{} resultado(s) para «{q}»\n", results.len());
    for (i, r) in results.iter().enumerate() {
        let score = r.get("fused_score").and_then(Value::as_f64).unwrap_or(0.0);
        let kind = r.get("type").and_then(Value::as_str).unwrap_or("");
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");

        let (head, body) = if kind == "error" {
            let etype = r
                .get("error_type")
                .and_then(Value::as_str)
                .unwrap_or("error");
            let resolved = r.get("resolved").and_then(Value::as_bool).unwrap_or(false);
            let state = if resolved { "resuelto" } else { "SIN RESOLVER" };
            (
                format!("{etype}  (error, {state})"),
                r.get("error_message").and_then(Value::as_str).unwrap_or(""),
            )
        } else {
            let entity = r
                .get("entity_name")
                .and_then(Value::as_str)
                .unwrap_or("(sin entidad)");
            (
                format!("{entity}  ({kind})"),
                r.get("content").and_then(Value::as_str).unwrap_or(""),
            )
        };

        println!("{:>2}. [{score:.4}] {head}", i + 1);
        if !body.is_empty() {
            println!("    {}", ellipsize(body, 140));
        }
        if !id.is_empty() {
            println!("    id: {id}");
        }
        println!();
    }
    Ok(())
}

pub async fn run_save(args: &[String]) -> Result<()> {
    let mut positional: Vec<String> = Vec::new();
    let mut obs_type = "fact".to_string();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--type" | "-t" => {
                obs_type = it.next().cloned().context("--type needs a value")?;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: cuba-memorys save <entidad> <contenido> [--type TIPO]\n\n\
                     TIPO: fact (default) | decision | lesson | preference | context |\n\
                           tool_usage | error | solution\n\n\
                     Pasa por el mismo pipeline que cuba_cronica: dedup, embedding y tags\n\
                     automáticos. La entidad se crea si no existe."
                );
                return Ok(());
            }
            flag if flag.starts_with("--") => {
                bail!("save: opción desconocida `{flag}` (probá --help)");
            }
            other => positional.push(other.to_string()),
        }
    }

    if positional.len() < 2 {
        bail!("uso: cuba-memorys save <entidad> \"<contenido>\" [--type TIPO]");
    }
    let entity = positional.remove(0);
    let content = positional.join(" ");

    let pool = pool().await?;
    let result = handlers::cronica::handle(
        &pool,
        json!({
            "action": "add",
            "entity_name": entity,
            "content": content,
            "observation_type": obs_type,
            "source": "user",
        }),
    )
    .await
    .context("save failed")?;

    let id = result.get("id").and_then(Value::as_str).unwrap_or("?");
    let tags = result
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    println!("Guardado en «{entity}» como {obs_type}.");
    println!("  id:   {id}");
    if !tags.is_empty() {
        println!("  tags: {tags}");
    }
    if result.get("duplicate").and_then(Value::as_bool) == Some(true) {
        println!("  nota: era casi idéntica a una observación existente — no se duplicó.");
    }
    Ok(())
}

pub async fn run_delete(args: &[String]) -> Result<()> {
    let mut id: Option<String> = None;
    let mut apply = false;

    for a in args {
        match a.as_str() {
            "--apply" => apply = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: cuba-memorys delete <observation-id> [--apply]\n\n\
                     Sin --apply solo muestra qué borraría (plan). Con --apply escribe primero\n\
                     un archivo de undo con la fila completa, y después borra."
                );
                return Ok(());
            }
            flag if flag.starts_with("--") => {
                bail!("delete: opción desconocida `{flag}` (probá --help)");
            }
            other => id = Some(other.to_string()),
        }
    }

    let Some(id) = id else {
        bail!("falta el id — uso: cuba-memorys delete <observation-id> [--apply]");
    };
    let uuid = uuid::Uuid::parse_str(&id).context("el id no es un UUID válido")?;

    let pool = pool().await?;

    let row = sqlx::query(
        "SELECT o.id::text AS id, o.content, o.observation_type, o.created_at::text AS created_at,
                o.importance, e.name AS entity
         FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE o.id = $1",
    )
    .bind(uuid)
    .fetch_optional(&pool)
    .await
    .context("looking up the observation")?;

    let Some(row) = row else {
        bail!("no existe ninguna observación con id {id}");
    };

    let entity: String = row.try_get("entity").unwrap_or_default();
    let content: String = row.try_get("content").unwrap_or_default();
    let kind: String = row.try_get("observation_type").unwrap_or_default();
    let created: String = row.try_get("created_at").unwrap_or_default();
    let importance: f64 = row.try_get("importance").unwrap_or(0.0);

    println!("Se borraría 1 observación:\n");
    println!("  entidad:    {entity}");
    println!("  tipo:       {kind}");
    println!("  creada:     {created}");
    println!("  importancia:{importance:.3}");
    println!("  contenido:  {}", ellipsize(&content, 160));
    println!();

    if !apply {
        println!("Esto fue un plan — no se borró nada.");
        println!("Para aplicarlo de verdad:  cuba-memorys delete {id} --apply");
        return Ok(());
    }

    let dir = undo_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("no se pudo crear el directorio de undo {}", dir.display()))?;

    let undo = json!({
        "deleted_at": chrono::Utc::now().to_rfc3339(),
        "observation": {
            "id": id,
            "entity": entity,
            "content": content,
            "observation_type": kind,
            "created_at": created,
            "importance": importance,
        },
    });
    let path = dir.join(format!("obs-{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&undo)?)
        .with_context(|| format!("no se pudo escribir el undo en {}", path.display()))?;

    handlers::cronica::handle(&pool, json!({ "action": "delete", "observation_id": id }))
        .await
        .context("delete failed")?;

    println!("Borrada.");
    println!("  undo: {}", path.display());
    Ok(())
}
