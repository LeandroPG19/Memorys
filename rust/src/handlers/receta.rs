use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

const Z_95: f64 = 1.96;

pub fn wilson_lower_bound(successes: i64, failures: i64) -> f64 {
    let n = (successes + failures) as f64;
    if n <= 0.0 {
        return 0.0;
    }
    let p = successes as f64 / n;
    let z2 = Z_95 * Z_95;
    let numerator = p + z2 / (2.0 * n) - Z_95 * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();
    let denominator = 1.0 + z2 / n;
    (numerator / denominator).clamp(0.0, 1.0)
}

fn steps_to_markdown(steps: &Value) -> String {
    let Some(arr) = steps.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for (i, s) in arr.iter().enumerate() {
        let doing = s.get("do").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("{}. {doing}\n", i + 1));
        if let Some(cmd) = s.get("run").and_then(Value::as_str)
            && !cmd.is_empty()
        {
            out.push_str(&format!("   ```\n   {cmd}\n   ```\n"));
        }
        if let Some(exp) = s.get("expect").and_then(Value::as_str)
            && !exp.is_empty()
        {
            out.push_str(&format!("   → esperado: {exp}\n"));
        }
    }
    out
}

fn row_to_json(r: &sqlx::postgres::PgRow) -> Value {
    let successes: i32 = r.try_get("success_count").unwrap_or(0);
    let failures: i32 = r.try_get("failure_count").unwrap_or(0);
    let steps: Value = r.try_get("steps").unwrap_or_else(|_| json!([]));
    json!({
        "id": r.try_get::<uuid::Uuid, _>("id").map(|v| v.to_string()).unwrap_or_default(),
        "name": r.try_get::<String, _>("name").unwrap_or_default(),
        "trigger": r.try_get::<String, _>("trigger_context").unwrap_or_default(),
        "steps": steps,
        "preconditions": r.try_get::<String, _>("preconditions").unwrap_or_default(),
        "verification": r.try_get::<String, _>("verification").unwrap_or_default(),
        "successes": successes,
        "failures": failures,
        "reliability": wilson_lower_bound(i64::from(successes), i64::from(failures)),
        "last_outcome": r.try_get::<Option<String>, _>("last_outcome").unwrap_or(None),
    })
}

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    match action {
        "add" => add(pool, &args).await,
        "get" => get(pool, &args).await,
        "search" => search(pool, &args).await,
        "outcome" => outcome(pool, &args).await,
        "list" => list(pool, &args).await,
        "delete" => delete(pool, &args).await,
        other => anyhow::bail!(
            "acción desconocida: '{other}'. Usá add | get | search | outcome | list | delete"
        ),
    }
}

async fn add(pool: &PgPool, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("falta 'name'")?;
    let trigger = args
        .get("trigger")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let steps = args.get("steps").cloned().unwrap_or_else(|| json!([]));
    if !steps.is_array() {
        anyhow::bail!("'steps' debe ser un array de {{do, run?, expect?}}");
    }
    if steps.as_array().is_some_and(std::vec::Vec::is_empty) {
        anyhow::bail!("un procedimiento sin pasos no es un procedimiento");
    }
    let preconditions = args
        .get("preconditions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let verification = args
        .get("verification")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let steps_text = serde_json::to_string_pretty(&steps)
        .context("serialising the steps for the secret scan")?;
    let stored_text = format!("{name}\n{trigger}\n{steps_text}\n{preconditions}\n{verification}");
    crate::redact::refuse_secrets(args, "the procedure", &stored_text)?;

    let project_id = crate::project::current_project_id(pool).await?;

    let embed_text = format!("{name}. {trigger}");
    let embedding = crate::embeddings::onnx::embed_passage(&embed_text)
        .await
        .ok();
    let model = crate::embeddings::onnx::current_model();

    let row = sqlx::query(
        "INSERT INTO brain_procedures
            (name, trigger_context, steps, preconditions, verification,
             project_id, embedding, embedding_model, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
         ON CONFLICT (name, project_id) DO UPDATE SET
            trigger_context = EXCLUDED.trigger_context,
            steps           = EXCLUDED.steps,
            preconditions   = EXCLUDED.preconditions,
            verification    = EXCLUDED.verification,
            embedding       = EXCLUDED.embedding,
            embedding_model = EXCLUDED.embedding_model,
            updated_at      = now()
         RETURNING *",
    )
    .bind(name)
    .bind(trigger)
    .bind(&steps)
    .bind(preconditions)
    .bind(verification)
    .bind(project_id)
    .bind(embedding.map(pgvector::Vector::from))
    .bind(&model)
    .fetch_one(pool)
    .await
    .context("guardando el procedimiento")?;

    let mut out = row_to_json(&row);
    out["action"] = json!("add");
    Ok(out)
}

async fn get(pool: &PgPool, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("falta 'name'")?;
    let project_id = crate::project::current_project_id(pool).await?;

    let row = sqlx::query(
        "SELECT * FROM brain_procedures
         WHERE name = $1 AND ($2::uuid IS NULL OR project_id = $2)
         LIMIT 1",
    )
    .bind(name)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .with_context(|| format!("no existe el procedimiento «{name}»"))?;

    let mut out = row_to_json(&row);
    out["markdown"] = json!(steps_to_markdown(&out["steps"]));
    out["action"] = json!("get");
    Ok(out)
}

async fn search(pool: &PgPool, args: &Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .context("falta 'query'")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(5)
        .clamp(1, 25);
    let project_id = crate::project::current_project_id(pool).await?;

    let embedding = crate::embeddings::onnx::embed(query).await.ok();

    let rows = if let Some(e) = embedding {
        sqlx::query(
            "SELECT *, 1 - (embedding <=> $1) AS score
             FROM brain_procedures
             WHERE embedding IS NOT NULL
               AND ($2::uuid IS NULL OR project_id = $2)
             ORDER BY embedding <=> $1
             LIMIT $3",
        )
        .bind(pgvector::Vector::from(e))
        .bind(project_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT *, similarity(name || ' ' || trigger_context, $1) AS score
             FROM brain_procedures
             WHERE ($2::uuid IS NULL OR project_id = $2)
               AND (name || ' ' || trigger_context) % $1
             ORDER BY score DESC
             LIMIT $3",
        )
        .bind(query)
        .bind(project_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    let mut results: Vec<Value> = rows.iter().map(row_to_json).collect();
    results.sort_by(|a, b| {
        let ra = a["reliability"].as_f64().unwrap_or(0.0);
        let rb = b["reliability"].as_f64().unwrap_or(0.0);
        rb.partial_cmp(&ra)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });

    Ok(json!({
        "action": "search",
        "query": query,
        "count": results.len(),
        "procedures": results,
    }))
}

async fn outcome(pool: &PgPool, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("falta 'name'")?;
    let ok = args
        .get("success")
        .and_then(Value::as_bool)
        .context("falta 'success' (true | false)")?;
    let project_id = crate::project::current_project_id(pool).await?;

    let row = sqlx::query(
        "UPDATE brain_procedures SET
            success_count = success_count + CASE WHEN $1 THEN 1 ELSE 0 END,
            failure_count = failure_count + CASE WHEN $1 THEN 0 ELSE 1 END,
            last_outcome  = CASE WHEN $1 THEN 'success' ELSE 'failure' END,
            last_used_at  = now(),
            updated_at    = now()
         WHERE name = $2 AND ($3::uuid IS NULL OR project_id = $3)
         RETURNING *",
    )
    .bind(ok)
    .bind(name)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .with_context(|| format!("no existe el procedimiento «{name}»"))?;

    let mut out = row_to_json(&row);
    out["action"] = json!("outcome");
    if !ok {
        out["hint"] = json!(
            "Falló. Si el procedimiento está desactualizado, corregilo con action=add \
             (mismo nombre) — se actualiza sin perder el historial."
        );
    }
    Ok(out)
}

async fn list(pool: &PgPool, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 100);
    let project_id = crate::project::current_project_id(pool).await?;

    let rows = sqlx::query(
        "SELECT * FROM brain_procedures
         WHERE ($1::uuid IS NULL OR project_id = $1)
         ORDER BY last_used_at DESC NULLS LAST, name
         LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "action": "list",
        "count": rows.len(),
        "procedures": rows.iter().map(row_to_json).collect::<Vec<_>>(),
    }))
}

async fn delete(pool: &PgPool, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("falta 'name'")?;
    let project_id = crate::project::current_project_id(pool).await?;

    let result = sqlx::query(
        "DELETE FROM brain_procedures
         WHERE name = $1 AND ($2::uuid IS NULL OR project_id = $2)",
    )
    .bind(name)
    .bind(project_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("no existe el procedimiento «{name}»");
    }
    Ok(json!({ "action": "delete", "name": name, "deleted": true }))
}

pub async fn all_for_export(pool: &PgPool) -> Result<Vec<Value>> {
    let rows = sqlx::query("SELECT * FROM brain_procedures ORDER BY name")
        .fetch_all(pool)
        .await?;
    let mut out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = row_to_json(r);
            v["markdown"] = json!(steps_to_markdown(&v["steps"]));
            v
        })
        .collect();
    out.sort_by(|a, b| {
        b["reliability"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["reliability"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_record_beats_a_lucky_first_try() {
        let lucky = wilson_lower_bound(1, 0);
        let proven = wilson_lower_bound(47, 3);
        assert!(
            proven > lucky,
            "47/50 ({proven:.3}) debe ganarle a 1/1 ({lucky:.3})"
        );
        assert!(lucky < 0.3, "1/1 no puede reclamar fiabilidad: {lucky:.3}");
        assert!(proven > 0.8, "47/50 sí la tiene: {proven:.3}");
    }

    #[test]
    fn never_run_means_no_evidence_not_perfect() {
        assert_eq!(wilson_lower_bound(0, 0), 0.0);
        assert!(wilson_lower_bound(0, 0) < wilson_lower_bound(1, 0));
    }

    #[test]
    fn failure_is_punished_and_more_evidence_sharpens_it() {
        assert!(wilson_lower_bound(0, 5) < 0.05, "0 de 5 no vale nada");
        let few = wilson_lower_bound(8, 2);
        let many = wilson_lower_bound(80, 20);
        assert!(
            many > few,
            "más evidencia con la misma tasa debe subir: {many:.3} vs {few:.3}"
        );
        assert!(many < 0.8);
    }

    #[test]
    fn bounds_hold() {
        assert!((wilson_lower_bound(1000, 0) - 1.0).abs() < 0.01);
        for (s, f) in [(0, 0), (1, 0), (0, 1), (10, 10), (999, 1)] {
            let w = wilson_lower_bound(s, f);
            assert!((0.0..=1.0).contains(&w), "fuera de rango en {s}/{f}: {w}");
        }
    }

    #[test]
    fn steps_render_to_markdown() {
        let steps = json!([
            {"do": "Levantar Postgres", "run": "docker compose up -d db", "expect": "healthy"},
            {"do": "Correr las migraciones"}
        ]);
        let md = steps_to_markdown(&steps);
        assert!(md.contains("1. Levantar Postgres"));
        assert!(md.contains("docker compose up -d db"));
        assert!(md.contains("→ esperado: healthy"));
        assert!(md.contains("2. Correr las migraciones"));
        assert_eq!(md.matches("```").count(), 2);
    }

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://receta-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn a_step_whose_command_carries_a_credential_is_refused() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "add",
            "name": "publicar la imagen",
            "trigger": "cuando hay que subir el contenedor al registry",
            "steps": [{"do": "autenticarse", "run": "docker login -u cuba -p ghp_abcdefghijklmnop"}]
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "receta add answered Ok on a pool that cannot connect: nothing but the secret \
                 gate could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token"),
            "a procedure is a list of commands to paste, so the credential lands inside `run` \
             far more often than in the prose around it. Scanning only the top-level strings \
             would leave the likeliest field open. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn an_ordinary_recipe_full_of_shell_is_not_mistaken_for_a_credential() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "add",
            "name": "levantar el entorno de desarrollo",
            "trigger": "cuando hay que correr los tests de integración",
            "preconditions": "el secret del webhook ya está cargado en el entorno",
            "verification": "el healthcheck responde 200 y no pide password",
            "steps": [
                {"do": "levantar Postgres", "run": "docker compose up -d db", "expect": "healthy"},
                {"do": "exportar la contraseña desde el entorno", "run": "export PGPASSWORD=$PGPASS"},
                {"do": "migrar", "run": "cargo sqlx migrate run", "expect": "0 errores"}
            ]
        });

        let chain = format!(
            "{:#}",
            handle(&pool, args)
                .await
                .expect_err("the pool cannot connect, so this call always ends in an error")
        );
        assert!(
            !chain.contains("Remove it and store a pointer"),
            "recipes are made of shell: docker commands, a password read from the environment, \
             prose that names a secret. Refusing this one would make the tool unusable for what \
             it exists to store, and the agent would go back to rediscovering the procedure \
             every session. Got: {chain}"
        );
    }
}
