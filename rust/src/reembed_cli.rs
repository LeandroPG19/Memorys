use anyhow::{Context, Result};

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut batch: i64 = 500;
    let mut all = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry reembed [--all] [--batch N]\n\n\
                     Re-encodes the observations that need it: missing a vector, or tagged\n\
                     with a model other than the one currently loaded. Run it after changing\n\
                     CUBA_EMBED_MODEL / CUBA_EMBEDDING_DIM / ONNX_MODEL_PATH — and after\n\
                     scripts/migrate-embedding-dim.sh has retyped the column.\n\n\
                     --all       re-encode every observation, stale or not\n\
                     --batch N   rows per progress report (default 500)\n\n\
                     Refuses to run without a real ONNX model: re-embedding with the hash\n\
                     fallback would overwrite meaningful vectors with noise."
                );
                return Ok(());
            }
            "--all" => all = true,
            "--batch" => {
                let n = it.next().context("--batch needs a number: --batch 500")?;
                batch = n.parse().context("--batch needs an integer")?;
            }
            other => {
                if let Some(n) = other.strip_prefix("--batch=") {
                    batch = n.parse().context("--batch needs an integer")?;
                } else {
                    anyhow::bail!("reembed: argumento desconocido `{other}` (probá --help)");
                }
            }
        }
    }

    if !crate::embeddings::onnx::is_model_loaded() {
        anyhow::bail!(
            "no hay modelo ONNX cargado. Reembeber con el fallback de hash reemplazaría \
             los vectores por ruido sin que nada lo avise.\n\
             Comprobá ONNX_MODEL_PATH y ORT_DYLIB_PATH (memory-industry doctor lo diagnostica)."
        );
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for reembed")?;

    let model = crate::embeddings::onnx::current_model();
    let dim = crate::embeddings::onnx::embedding_dim();
    eprintln!("reembed: modelo={model} dim={dim}");

    let col: String = sqlx::query_scalar(
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute
         WHERE attrelid = 'brain_observations'::regclass AND attname = 'embedding'",
    )
    .fetch_one(&pool)
    .await
    .context("reading the embedding column type")?;
    let expected = format!("vector({dim})");
    if col != expected {
        anyhow::bail!(
            "la columna es {col} y el modelo produce {expected}.\n\
             Corré primero:  DATABASE_URL=… scripts/migrate-embedding-dim.sh {dim}"
        );
    }

    let rows: Vec<crate::handlers::zafra::StaleObservation> = if all {
        sqlx::query_as(
            "SELECT o.id, o.content, e.entity_type, e.name AS entity_name
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             ORDER BY o.id",
        )
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query_as(
            "SELECT o.id, o.content, e.entity_type, e.name AS entity_name
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE o.embedding IS NULL OR o.embedding_model IS DISTINCT FROM $1
             ORDER BY o.id",
        )
        .bind(&model)
        .fetch_all(&pool)
        .await
    }
    .context("reading observations")?;

    let total = rows.len();
    if total == 0 {
        println!("Nada que reembeber: todo el corpus ya está en {model} ({dim}-d).");
        return Ok(());
    }

    let scope = if all { "todas" } else { "pendientes" };
    eprintln!("reembed: {total} observaciones ({scope})");

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in rows.chunks(batch.clamp(1, 5000) as usize) {
        let (ok, bad) = crate::handlers::zafra::reembed_batch(&pool, chunk, &model).await;
        done += ok;
        failed += bad;
        eprintln!("  {done}/{total}");
    }

    println!("Reembebidas {done} de {total} observaciones con {model} ({dim}-d).");
    if failed > 0 {
        println!("{failed} fallaron (ver los warnings).");
    }
    if all || total > 100 {
        println!(
            "\nEl umbral de abstención estaba calibrado para el modelo anterior y ya no vale:"
        );
        println!("  memory-industry calibrate --dataset <dataset.jsonl> --apply");
    }
    Ok(())
}
