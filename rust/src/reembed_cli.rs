use anyhow::{Context, Result};

use crate::embeddings::backfill::VectorTable;
use crate::handlers::zafra::StaleRow;

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut batch: i64 = 500;
    let mut all = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry reembed [--all] [--batch N]\n\n\
                     Re-encodes the observations and episodes that need it: missing a vector, or tagged\n\
                     with a model other than the one currently loaded. Run it after changing\n\
                     CUBA_EMBED_MODEL / CUBA_EMBEDDING_DIM / ONNX_MODEL_PATH — and after\n\
                     scripts/migrate-embedding-dim.sh has retyped the column.\n\n\
                     --all       re-encode every observation and episode, stale or not\n\
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

    if !crate::embeddings::onnx::model_loaded().await {
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

    let observations: Vec<StaleRow> = if all {
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

    let episodes: Vec<StaleRow> = if all {
        sqlx::query_as(
            "SELECT ep.id, ep.content, e.entity_type, e.name AS entity_name
             FROM brain_episodes ep
             JOIN brain_entities e ON e.id = ep.entity_id
             ORDER BY ep.id",
        )
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query_as(
            "SELECT ep.id, ep.content, e.entity_type, e.name AS entity_name
             FROM brain_episodes ep
             JOIN brain_entities e ON e.id = ep.entity_id
             WHERE ep.embedding IS NULL OR ep.embedding_model IS DISTINCT FROM $1
             ORDER BY ep.id",
        )
        .bind(&model)
        .fetch_all(&pool)
        .await
    }
    .context("reading episodes")?;

    let total_observations = observations.len();
    let total_episodes = episodes.len();
    let total = total_observations + total_episodes;
    if total == 0 {
        println!("Nada que reembeber: todo el corpus ya está en {model} ({dim}-d).");
        return Ok(());
    }

    let scope = if all { "todas" } else { "pendientes" };
    eprintln!("reembed: {total_observations} observaciones y {total_episodes} episodios ({scope})");

    let batch = batch.clamp(1, 5000) as usize;
    let mut done_observations = 0usize;
    let mut done_episodes = 0usize;
    let mut failed = 0usize;
    for (table, rows) in [
        (VectorTable::Observations, &observations),
        (VectorTable::Episodes, &episodes),
    ] {
        for chunk in rows.chunks(batch) {
            let (ok, bad) =
                crate::handlers::zafra::reembed_batch(&pool, table, chunk, &model).await;
            match table {
                VectorTable::Observations => done_observations += ok,
                VectorTable::Episodes => done_episodes += ok,
            }
            failed += bad;
            eprintln!("  {}/{total}", done_observations + done_episodes);
        }
    }

    println!(
        "Reembebidas {done_observations} de {total_observations} observaciones y \
         {done_episodes} de {total_episodes} episodios con {model} ({dim}-d)."
    );
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
