use std::time::Instant;

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::embeddings::onnx;

const EXPECTED_TABLES: i64 = 24;

const REQUIRED_EXTENSIONS: [&str; 2] = ["vector", "pg_trgm"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn glyph(self) -> &'static str {
        match self {
            Status::Ok => "  ok  ",
            Status::Warn => " warn ",
            Status::Fail => " FAIL ",
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Ok,
            detail: detail.into(),
            hint: None,
        }
    }
    fn warn(name: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Warn,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
    fn fail(name: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "<unparseable>".to_string();
    };
    let Some((creds, host)) = rest.split_once('@') else {
        return format!("{scheme}://{rest}");
    };
    let user = creds.split_once(':').map_or(creds, |(u, _)| u);
    format!("{scheme}://{user}:***@{host}")
}

fn parse_vector_dim(col_type: &str) -> Option<i64> {
    col_type
        .trim()
        .strip_prefix("vector(")?
        .strip_suffix(')')?
        .parse()
        .ok()
}

fn stale_processes() -> Vec<u32> {
    let mut stale = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return stale;
    };

    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let exe_link = entry.path().join("exe");
        let Ok(exe) = std::fs::read_link(&exe_link) else {
            continue;
        };

        let raw = exe.to_string_lossy();
        let (path, deleted) = match raw.strip_suffix(" (deleted)") {
            Some(p) => (std::path::PathBuf::from(p), true),
            None => (exe.clone(), false),
        };
        if path.file_name().and_then(|n| n.to_str()) != Some("cuba-memorys") {
            continue;
        }

        if deleted {
            stale.push(pid);
            continue;
        }
        let exe = path;

        let (Ok(bin_meta), Ok(proc_meta)) =
            (std::fs::metadata(&exe), std::fs::metadata(entry.path()))
        else {
            continue;
        };
        let (Ok(built), Ok(started)) = (bin_meta.modified(), proc_meta.modified()) else {
            continue;
        };
        if built > started {
            stale.push(pid);
        }
    }
    stale
}

fn latest_published_version() -> Option<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "5",
            "https://registry.npmjs.org/cuba-memorys/latest",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json.get("version")?.as_str().map(String::from)
}

fn audit_chain_check(keyed: bool, rows: i64) -> Check {
    if keyed {
        return Check::ok(
            "audit_chain",
            format!(
                "clave resuelta: las filas nuevas se sellan con HMAC y una SHA-256 posterior \
                 se rechaza. De las {rows} ya escritas, `cuba_archivo verify` dice cuántas \
                 son anteriores a la clave"
            ),
        );
    }
    Check::warn(
        "audit_chain",
        format!("{rows} filas encadenadas con SHA-256 sin clave"),
        "sin CUBA_AUDIT_KEY la cadena es recomputable: cualquiera con INSERT sobre \
         brain_audit_log fabrica filas cuyo hash cuadra, y `cuba_archivo verify` las da \
         por buenas. La evidencia de manipulación es cero, que es justo lo que la tabla \
         existe para dar. Poné una clave y guardala fuera de ~/.cache.",
    )
}

fn peer_token_check(admin: Option<&str>, peer: Option<&str>) -> Check {
    match (admin, peer) {
        (_, None) => Check::ok(
            "peer_token",
            "CUBA_PEER_TOKEN sin fijar — no hay par que pueda llamar a este demonio".to_string(),
        ),
        (Some(a), Some(p)) if a == p => Check::fail(
            "peer_token",
            "CUBA_PEER_TOKEN es la misma cadena que CUBA_HTTP_TOKEN".to_string(),
            "así el token restringido no restringe nada: casa primero con la rama de admin y \
             se lleva las 28 herramientas, cuba_forget incluida. Y es el mismo que va por el \
             túnel de Cloudflare. Poné dos cadenas distintas; el demonio ya se niega a \
             arrancar con esta configuración.",
        ),
        (_, Some(_)) => Check::ok(
            "peer_token",
            format!(
                "CUBA_PEER_TOKEN fijado y distinto — el par solo puede llamar a {}",
                crate::session::PEER_VERBS
                    .iter()
                    .map(|(t, v)| format!("{t} action={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    }
}

fn rbac_check(principals: i64) -> Check {
    if principals > 0 {
        return Check::ok(
            "rbac",
            format!("{principals} principal(es) — la política restrictiva decide de verdad"),
        );
    }
    // Empty table is the designed single-user mode (fail-open). It is healthy, not a warn.
    Check::ok(
        "rbac",
        "single-user mode — brain_principals vacío (fail-open by design; insert principals to lock down)",
    )
}

fn runtime_role_check(user: &str, is_super: bool, app_role_ready: bool) -> Check {
    if !is_super {
        return Check::ok(
            "runtime_role",
            format!("'{user}' sin superuser — RLS y audit efectivos"),
        );
    }
    if app_role_ready {
        return Check::fail(
            "runtime_role",
            format!(
                "la app corre como '{user}' (SUPERUSER) teniendo {} listo y sin privilegios",
                crate::db::APP_ROLE
            ),
            "la mitigación está construida y desconectada: un superuser ignora RLS y puede \
             alterar el audit_log, así que el aislamiento por proyecto y la trazabilidad \
             append-only no existen. Suele ser un binario viejo — reinstalá el daemon, o borrá \
             ~/.cache/cuba-memorys/pgpass_app si de verdad querés correr como admin.",
        );
    }
    Check::warn(
        "runtime_role",
        format!("la app corre como '{user}' (SUPERUSER)"),
        "un superuser ignora RLS y puede alterar el audit_log: el aislamiento por proyecto y la \
         trazabilidad append-only son decorativos. Ejecutá `cuba-memorys secure` y apuntá el \
         runtime a cuba_app.",
    )
}

fn reranker_check(
    configured: bool,
    resolved: bool,
    enabled: bool,
    disabled_by_resources: bool,
    ram_available_mb: u64,
    tier: &str,
    on_by_default: bool,
) -> Check {
    if disabled_by_resources {
        return Check::warn(
            "reranker",
            format!(
                "el modelo NO se cargó: al arrancar había {ram_available_mb} MiB de RAM \
                 disponible y el plan de recursos salió en nivel «{tier}» — resources.rs \
                 redirigió CUBA_RERANKER_PATH a un directorio que no existe, para toda la \
                 vida de este proceso."
            ),
            "esta decisión se toma UNA sola vez, al arrancar: subí el límite de memoria del \
             contenedor o de la unidad de systemd y reiniciá el proceso — más RAM disponible \
             ahora mismo no lo revive.",
        );
    }
    match (configured, resolved) {
        (true, true) if enabled => Check::ok(
            "reranker",
            format!(
                "cargado (bge-reranker-v2-m3) — reordena los candidatos por cross-encoder · \
                 activo por defecto: {on_by_default} ({})",
                if on_by_default {
                    "modo completo, o GPU real detectada — ver el check «gpu»"
                } else {
                    "sin GPU real, ver el check «gpu» arriba — en CPU cuesta 60-110 s y ese \
                     trabajo se tira; pedilo con rerank:true si igual lo querés"
                }
            ),
        ),
        (true, true) => Check::fail(
            "reranker",
            "hay un modelo en disco pero NO carga",
            "el ranking se devuelve tal cual. Suele ser libonnxruntime.so: comprobá \
             ORT_DYLIB_PATH.",
        ),
        (true, false) => Check::ok(
            "reranker",
            "configurado — carga en su primer lote (no lo cargo aquí: son ~1,1 GB)",
        ),
        (false, _) => Check::warn(
            "reranker",
            "no hay modelo en disco — el ranking se devuelve tal cual (RRF, sin reordenar)",
            "es opcional, pero si querías reordenar y no pusiste el modelo, no está \
             pasando nada. Instalalo: cuba-memorys models reranker",
        ),
    }
}

pub async fn run_checks(pool: &PgPool, url: &str) -> Vec<Check> {
    run_checks_with(pool, url, false).await
}

pub async fn run_checks_with(pool: &PgPool, url: &str, deep: bool) -> Vec<Check> {
    let mut checks = Vec::new();

    let mode = crate::mode::active();
    checks.push(Check::ok(
        "mode",
        format!("{} — {}", mode.as_str(), mode.describe()),
    ));

    checks.push(Check::ok("database_url", redact_url(url)));

    let machine = crate::resources::probe();
    let resource_plan = crate::resources::plan(&machine);
    let footprint = format!(
        "RAM {} MiB total, {} MiB disponible{} · swap {} MiB · {} núcleos ({} físicos){}",
        machine.ram_total_mb,
        machine.ram_available_mb,
        machine
            .cgroup_limit_mb
            .map(|l| format!(", cgroup {l} MiB"))
            .unwrap_or_default(),
        machine.swap_total_mb,
        machine.cores_logical,
        machine.cores_physical,
        machine
            .vram_free_mb
            .map(|v| format!(" · VRAM libre {v} MiB"))
            .unwrap_or_default(),
    );
    checks.push(Check::ok("machine", footprint));
    checks.push(match resource_plan.tier {
        crate::resources::Tier::Minimal => Check::warn(
            "resource_plan",
            resource_plan.describe(),
            "sin presupuesto para los pesos: la búsqueda cae a léxica. Subí el límite de \
             memoria del contenedor o de la unidad de systemd para recuperar la semántica",
        ),
        _ => Check::ok("resource_plan", resource_plan.describe()),
    });

    let t0 = Instant::now();
    match sqlx::query("SELECT 1").fetch_one(pool).await {
        Ok(_) => checks.push(Check::ok(
            "connection",
            format!("responde en {} ms", t0.elapsed().as_millis()),
        )),
        Err(e) => {
            checks.push(Check::fail(
                "connection",
                format!("no se pudo consultar: {e}"),
                "verificá que el contenedor de Postgres esté arriba y que DATABASE_URL apunte a él",
            ));
            return checks;
        }
    }

    match sqlx::query("SELECT extname::text FROM pg_extension")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => {
            let have: Vec<String> = rows.iter().filter_map(|r| r.try_get(0).ok()).collect();
            let missing: Vec<&str> = REQUIRED_EXTENSIONS
                .iter()
                .filter(|e| !have.iter().any(|h| h == *e))
                .copied()
                .collect();
            if missing.is_empty() {
                checks.push(Check::ok("extensions", have.join(", ")));
            } else {
                checks.push(Check::fail(
                    "extensions",
                    format!("faltan: {}", missing.join(", ")),
                    "CREATE EXTENSION vector; CREATE EXTENSION pg_trgm;",
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "extensions",
            format!("no se pudo leer: {e}"),
            "revisar permisos",
        )),
    }

    match sqlx::query(
        "SELECT count(*)::bigint AS total, count(*) FILTER (WHERE NOT success)::bigint AS dirty
         FROM _sqlx_migrations",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let total: i64 = row.try_get("total").unwrap_or(0);
            let dirty: i64 = row.try_get("dirty").unwrap_or(0);
            if dirty > 0 {
                checks.push(Check::fail(
                    "migrations",
                    format!("{total} aplicadas, {dirty} en estado dirty"),
                    "una migración quedó a medias: restaurá desde backup antes de seguir",
                ));
            } else {
                checks.push(Check::ok(
                    "migrations",
                    format!("{total} aplicadas, ninguna dirty"),
                ));
            }
        }
        Err(e) => checks.push(Check::fail(
            "migrations",
            format!("no existe _sqlx_migrations: {e}"),
            "la base no está inicializada — arrancá el servidor una vez para que migre",
        )),
    }

    match sqlx::query(
        "SELECT count(*)::bigint FROM information_schema.tables
         WHERE table_schema='public' AND table_name LIKE 'brain%'",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let n: i64 = row.try_get(0).unwrap_or(0);
            if n >= EXPECTED_TABLES {
                checks.push(Check::ok("schema", format!("{n} tablas brain_*")));
            } else {
                checks.push(Check::fail(
                    "schema",
                    format!("{n} tablas brain_* (se esperaban {EXPECTED_TABLES})"),
                    "esquema incompleto — corré las migraciones",
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "schema",
            format!("no se pudo contar: {e}"),
            "revisar permisos",
        )),
    }

    match sqlx::query("SELECT count(*)::bigint FROM pg_proc WHERE proname='cuba_or_tsquery'")
        .fetch_one(pool)
        .await
    {
        Ok(row) => {
            let n: i64 = row.try_get(0).unwrap_or(0);
            if n > 0 {
                checks.push(Check::ok("recall/or_tsquery", "presente (migración 0026)"));
            } else {
                checks.push(Check::fail(
                    "recall/or_tsquery",
                    "falta la función cuba_or_tsquery",
                    "sin ella plainto_tsquery une los términos con AND: una query descriptiva \
                     larga devuelve CERO resultados, en silencio. Aplicá la migración 0026.",
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "recall/or_tsquery",
            format!("no verificable: {e}"),
            "revisar permisos",
        )),
    }

    let model = onnx::current_model();
    if onnx::is_model_loaded() {
        checks.push(Check::ok("onnx_model", format!("cargado ({model})")));
    } else {
        let path = std::env::var("ONNX_MODEL_PATH").unwrap_or_else(|_| "<auto/cache>".into());
        checks.push(Check::fail(
            "onnx_model",
            format!("NO cargado — ONNX_MODEL_PATH={path}"),
            "la búsqueda vectorial degrada a hash. Corré `memory-industry models embed` y \
             `memory-industry models runtime` (cache ~/.cache/memory-industry o cuba-memorys), \
             o seteá ONNX_MODEL_PATH y ORT_DYLIB_PATH.",
        ));
    }

    let gpu = crate::gpu::status();
    checks.push(match gpu.hint {
        Some(hint) if gpu.degraded => Check::warn("gpu", gpu.detail, hint),
        _ => Check::ok("gpu", gpu.detail),
    });

    let rerank_configured = crate::search::rerank::is_configured();
    let rerank_resolved = crate::search::rerank::status_resolved();
    let rerank_enabled = rerank_resolved && crate::search::rerank::enabled();
    let rerank_disabled_by_resources = std::env::var("CUBA_RERANKER_PATH").ok().as_deref()
        == Some(crate::resources::disabled_model_path().as_str());
    checks.push(reranker_check(
        rerank_configured,
        rerank_resolved,
        rerank_enabled,
        rerank_disabled_by_resources,
        machine.ram_available_mb,
        resource_plan.tier.as_str(),
        mode.rerank_default(),
    ));

    if crate::cognitive::nli::available() {
        if crate::cognitive::nli::deferred_by_resource_plan() {
            checks.push(Check::ok(
                "nli_entailment",
                "modelo en disco; resource plan difiere la carga hasta que haya RAM",
            ));
        } else if deep && crate::cognitive::nli::enabled() {
            checks.push(Check::ok(
                "nli_entailment",
                "cargado (mDeBERTa-v3-xnli) — verify decide en ~50 ms, sin LLM",
            ));
        } else {
            checks.push(if deep {
                Check::fail(
                    "nli_entailment",
                    "hay un modelo NLI en disco pero NO carga",
                    "verify se cae al juez LLM (~20 s por afirmación) o al heurístico, que \
                     no decide nada. Suele ser libonnxruntime.so: comprobá ORT_DYLIB_PATH.",
                )
            } else {
                Check::ok(
                    "nli_entailment",
                    "presente en disco — carga a su primer veredicto (no lo cargo aquí: \
                     son ~1 GB; `doctor --deep` lo comprueba de verdad)",
                )
            });
        }
    } else {
        checks.push(Check::fail(
            "nli_entailment",
            "sin modelo NLI local",
            "memory-industry models nli",
        ));
    }

    {
        let (ok, detail, hint) = crate::llm_cli::doctor_line().await;
        if ok {
            checks.push(Check::ok("generative_llm", detail));
        } else {
            checks.push(Check::fail(
                "generative_llm",
                detail,
                if hint.is_empty() {
                    "memory-industry llm list && memory-industry llm set <provider> --key …".into()
                } else {
                    hint
                },
            ));
        }
    }

    {
        let g = crate::graph_db::status_summary();
        let backend = g.get("backend").and_then(|v| v.as_str()).unwrap_or("off");
        match backend {
            "off" => checks.push(Check::ok(
                "graph_db",
                "projection disabled (Postgres remains SoT) — set falkor when you want a graph engine",
            )),
            other if g.get("url_configured").and_then(|v| v.as_bool()) != Some(true) => {
                checks.push(Check::fail(
                    "graph_db",
                    format!("{other} selected but MEMORY_INDUSTRY_GRAPH_URL unset"),
                    "export MEMORY_INDUSTRY_GRAPH_URL=redis://127.0.0.1:6389",
                ));
            }
            other if g.get("reachable").and_then(|v| v.as_bool()) == Some(true) => {
                checks.push(Check::ok(
                    "graph_db",
                    format!(
                        "{other} reachable · projected_ops={}",
                        g.get("projected_ops").and_then(|v| v.as_u64()).unwrap_or(0)
                    ),
                ));
            }
            other => checks.push(Check::fail(
                "graph_db",
                format!(
                    "{other} configured but unreachable: {}",
                    g.get("last_error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("no detail")
                ),
                "docker compose --profile graph up -d",
            )),
        }
    }

    match sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_name = 'brain_observations' AND column_name = 'crdt_counter'",
    )
    .fetch_one(pool)
    .await
    {
        Ok(n) if n > 0 => {
            let covered: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM brain_observations WHERE crdt_actor IS NOT NULL",
            )
            .fetch_one(pool)
            .await
            .unwrap_or(0);
            checks.push(Check::ok(
                "crdt_clocks",
                format!("columns present · observations with actor={covered}"),
            ));
        }
        Ok(_) => checks.push(Check::fail(
            "crdt_clocks",
            "migration 0062 not applied",
            "memory-industry serve once to run migrations",
        )),
        Err(e) => checks.push(Check::warn(
            "crdt_clocks",
            format!("could not probe: {e}"),
            "check DATABASE_URL",
        )),
    }

    match sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = 'brain_artifacts'",
    )
    .fetch_one(pool)
    .await
    {
        Ok(n) if n > 0 => checks.push(Check::ok("brain_artifacts", "table present")),
        Ok(_) => checks.push(Check::fail(
            "brain_artifacts",
            "migration 0061 not applied",
            "memory-industry migrate / serve once against this database",
        )),
        Err(e) => checks.push(Check::warn(
            "brain_artifacts",
            format!("could not probe: {e}"),
            "check DATABASE_URL",
        )),
    }

    let runtime_dim = onnx::embedding_dim() as i64;
    match sqlx::query(
        "SELECT format_type(atttypid, atttypmod)::text FROM pg_attribute
         WHERE attrelid='brain_observations'::regclass AND attname='embedding'",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let col_type: String = row.try_get(0).unwrap_or_default();
            match parse_vector_dim(&col_type) {
                Some(col_dim) if col_dim == runtime_dim => checks.push(Check::ok(
                    "embedding_dim",
                    format!("runtime {runtime_dim}-d == columna {col_type}"),
                )),
                Some(col_dim) => checks.push(Check::fail(
                    "embedding_dim",
                    format!("runtime {runtime_dim}-d ≠ columna vector({col_dim})"),
                    "el modelo y la columna no coinciden: todo insert vectorial falla o se corrompe. \
                     Alineá CUBA_EMBEDDING_DIM con la columna, o migrá con scripts/migrate-embedding-dim.sh",
                )),
                None => checks.push(Check::warn(
                    "embedding_dim",
                    format!("tipo de columna inesperado: {col_type}"),
                    "se esperaba vector(N)",
                )),
            }
        }
        Err(e) => checks.push(Check::warn(
            "embedding_dim",
            format!("no verificable: {e}"),
            "revisar esquema",
        )),
    }

    match sqlx::query(
        "SELECT count(*)::bigint AS total,
                count(*) FILTER (WHERE embedding IS NULL)::bigint AS missing,
                count(*) FILTER (WHERE embedding IS NOT NULL AND embedding_model IS DISTINCT FROM $1)::bigint AS stale
         FROM brain_observations",
    )
    .bind(&model)
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let total: i64 = row.try_get("total").unwrap_or(0);
            let missing: i64 = row.try_get("missing").unwrap_or(0);
            let stale: i64 = row.try_get("stale").unwrap_or(0);
            if missing == 0 && stale == 0 {
                checks.push(Check::ok(
                    "embedding_coverage",
                    format!("{total} observaciones, todas con vector del modelo actual"),
                ));
            } else {
                checks.push(Check::warn(
                    "embedding_coverage",
                    format!("{total} observaciones: {missing} sin vector, {stale} de otro modelo"),
                    "cuba_zafra action=reembed",
                ));
            }
        }
        Err(e) => checks.push(Check::warn("embedding_coverage", format!("no verificable: {e}"), "revisar esquema")),
    }

    match sqlx::query(
        "SELECT count(*)::bigint FROM brain_facts
         WHERE valid_to IS NOT NULL AND valid_to <= valid_from",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let bad: i64 = row.try_get(0).unwrap_or(0);
            if bad == 0 {
                checks.push(Check::ok("bitemporal", "sin intervalos invertidos"));
            } else {
                checks.push(Check::fail(
                    "bitemporal",
                    format!("{bad} hechos con valid_to <= valid_from"),
                    "el histórico temporal miente: las consultas 'as of' devolverán resultados imposibles",
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "bitemporal",
            format!("no verificable: {e}"),
            "revisar esquema",
        )),
    }

    match sqlx::query(
        "SELECT count(*)::bigint FROM information_schema.columns
         WHERE table_name='brain_observations' AND column_name='last_decayed_at'",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let n: i64 = row.try_get(0).unwrap_or(0);
            if n > 0 {
                checks.push(Check::ok(
                    "decay_anchor",
                    "last_decayed_at presente (migración 0028)",
                ));
            } else {
                checks.push(Check::fail(
                    "decay_anchor",
                    "falta la columna last_decayed_at",
                    "sin ancla, el ciclo REM re-aplica el decaimiento cada 4 h y la importancia \
                     se hunde 10-80×. Aplicá la migración 0028.",
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "decay_anchor",
            format!("no verificable: {e}"),
            "revisar esquema",
        )),
    }

    match sqlx::query(
        "SELECT current_user::text AS usr,
                COALESCE((SELECT rolsuper FROM pg_roles WHERE rolname = current_user), false) AS super",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let usr: String = row.try_get("usr").unwrap_or_default();
            let is_super: bool = row.try_get("super").unwrap_or(false);
            let app_role_ready = if is_super {
                sqlx::query_scalar(
                    "SELECT NOT rolsuper AND NOT rolbypassrls AND rolcanlogin
                     FROM pg_roles WHERE rolname = $1",
                )
                .bind(crate::db::APP_ROLE)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
                .unwrap_or(false)
            } else {
                false
            };
            checks.push(runtime_role_check(&usr, is_super, app_role_ready));

            let audit_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_audit_log")
                .fetch_one(pool)
                .await
                .unwrap_or(0);
            checks.push(audit_chain_check(
                crate::handlers::archivo::audit_key().is_some(),
                audit_rows,
            ));

            let principals: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_principals")
                .fetch_one(pool)
                .await
                .unwrap_or(0);
            checks.push(rbac_check(principals));
        }
        Err(e) => checks.push(Check::warn("runtime_role", format!("no verificable: {e}"), "revisar permisos")),
    }

    checks.push(peer_token_check(
        std::env::var("CUBA_HTTP_TOKEN").ok().as_deref(),
        std::env::var("CUBA_PEER_TOKEN").ok().as_deref(),
    ));

    match sqlx::query(
        "SELECT (SELECT count(*) FROM brain_observations)::bigint AS obs,
                (SELECT count(*) FROM brain_entities)::bigint AS ent,
                (SELECT count(*) FROM brain_facts)::bigint AS facts,
                pg_size_pretty(pg_database_size(current_database()))::text AS size",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let obs: i64 = row.try_get("obs").unwrap_or(0);
            let ent: i64 = row.try_get("ent").unwrap_or(0);
            let facts: i64 = row.try_get("facts").unwrap_or(0);
            let size: String = row.try_get("size").unwrap_or_default();
            checks.push(Check::ok(
                "corpus",
                format!("{obs} observaciones · {ent} entidades · {facts} hechos · {size}"),
            ));
        }
        Err(e) => checks.push(Check::warn(
            "corpus",
            format!("no verificable: {e}"),
            "revisar esquema",
        )),
    }

    match sqlx::query(
        "SELECT count(*)::bigint AS isolated, (SELECT count(*) FROM brain_entities)::bigint AS total
         FROM brain_entities e
         WHERE NOT EXISTS (SELECT 1 FROM brain_relations r
                           WHERE r.from_entity = e.id OR r.to_entity = e.id)",
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => {
            let isolated: i64 = row.try_get("isolated").unwrap_or(0);
            let total: i64 = row.try_get("total").unwrap_or(0);
            let pct = if total > 0 { isolated * 100 / total } else { 0 };
            if pct >= 50 {
                checks.push(Check::warn(
                    "graph_connectivity",
                    format!("{isolated} de {total} entidades ({pct}%) no tienen ninguna relación"),
                    "el retrieval asociativo multi-hop y PageRank no las alcanzan: para el grafo \
                     no existen. cuba_reflexion las lista; cuba_puente las conecta.",
                ));
            } else {
                checks.push(Check::ok(
                    "graph_connectivity",
                    format!("{isolated} de {total} entidades aisladas ({pct}%)"),
                ));
            }
        }
        Err(e) => checks.push(Check::warn(
            "graph_connectivity",
            format!("no verificable: {e}"),
            "revisar esquema",
        )),
    }

    let stale = stale_processes();
    if stale.is_empty() {
        checks.push(Check::ok(
            "binary_freshness",
            "ningún proceso MCP corre un binario obsoleto",
        ));
    } else {
        let pids: Vec<String> = stale.iter().map(u32::to_string).collect();
        checks.push(Check::warn(
            "binary_freshness",
            format!(
                "{} proceso(s) MCP corren un binario más viejo que el de disco (pid {})",
                stale.len(),
                pids.join(", ")
            ),
            "recompilaste pero el cliente MCP sigue sirviendo la imagen vieja en memoria: \
             reiniciá el cliente para que tome el binario nuevo",
        ));
    }

    checks
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let check_updates = args.iter().any(|a| a == "--check-updates");
    let deep = args.iter().any(|a| a == "--deep");
    if args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!(
            "usage: cuba-memorys doctor [--json] [--check-updates] [--deep]\n\n\
             Read-only health check: asserts the invariants whose violation is silent\n\
             (zero recall, dead vector branch, dimension drift, inert RLS, decay anchor,\n\
             stale binary in a live MCP process). Exits 1 if any check fails.\n\n\
             --check-updates  compara con la última versión publicada en npm. Nunca actualiza\n\
                              solo, y no corre salvo que lo pidas: es la única red que toca.\n\
             --deep           carga de verdad el reranker y el NLI para comprobar que abren.\n\
                              Son ~2 GB y minutos: sin esto sólo se informa su presencia."
        );
        return Ok(());
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for doctor")?;

    let mut checks = run_checks_with(&pool, &url, deep).await;

    if check_updates {
        let current = env!("CARGO_PKG_VERSION");
        match latest_published_version() {
            Some(latest) if latest != current => checks.push(Check::warn(
                "version",
                format!("corriendo v{current}; la última publicada es v{latest}"),
                "actualizá cuando quieras — doctor nunca lo hace por vos",
            )),
            Some(latest) => checks.push(Check::ok("version", format!("v{latest}, al día"))),
            None => checks.push(Check::warn(
                "version",
                "no se pudo consultar el registro de npm".to_string(),
                "sin red, o curl no está disponible — no afecta a nada más",
            )),
        }
    }
    let checks = checks;
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "healthy": failed == 0,
                "failed": failed,
                "warnings": warned,
                "checks": checks,
            })
        );
    } else {
        println!("cuba-memorys doctor — v{}\n", env!("CARGO_PKG_VERSION"));
        for c in &checks {
            println!("[{}] {:<20} {}", c.status.glyph(), c.name, c.detail);
            if let Some(h) = &c.hint {
                println!("{:<8} {:<20} → {}", "", "", h);
            }
        }
        println!();
        if failed == 0 && warned == 0 {
            println!("Todo sano: {} chequeos, 0 fallos.", checks.len());
        } else {
            println!(
                "{} chequeos · {} fallo(s) · {} aviso(s).",
                checks.len(),
                failed,
                warned
            );
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_the_password() {
        assert_eq!(
            redact_url("postgresql://cuba:memorys2026@127.0.0.1:5491/brain"),
            "postgresql://cuba:***@127.0.0.1:5491/brain"
        );
    }

    #[test]
    fn redacts_url_without_credentials() {
        assert_eq!(
            redact_url("postgresql://localhost:5432/brain"),
            "postgresql://localhost:5432/brain"
        );
    }

    #[test]
    fn a_replaced_binary_is_recognized_despite_the_deleted_suffix() {
        let raw = "/home/x/rust/target/release/cuba-memorys (deleted)";
        let (path, deleted) = match raw.strip_suffix(" (deleted)") {
            Some(p) => (std::path::PathBuf::from(p), true),
            None => (std::path::PathBuf::from(raw), false),
        };
        assert!(deleted);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("cuba-memorys")
        );

        let live = "/home/x/rust/target/release/cuba-memorys";
        assert!(live.strip_suffix(" (deleted)").is_none());
    }

    #[test]
    fn parses_the_declared_vector_dimension() {
        assert_eq!(parse_vector_dim("vector(384)"), Some(384));
        assert_eq!(parse_vector_dim("vector(1024)"), Some(1024));
        assert_eq!(parse_vector_dim("text"), None);
    }
}

#[cfg(test)]
mod runtime_role_tests {
    use super::*;

    #[test]
    fn a_non_superuser_runtime_is_the_healthy_case() {
        let check = runtime_role_check("cuba_app", false, false);
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn a_superuser_runtime_with_the_app_role_ready_is_a_failure_not_a_warning() {
        let check = runtime_role_check("cuba", true, true);

        assert_eq!(
            check.status,
            Status::Fail,
            "cuba_app exists, cannot log in as superuser and cannot bypass RLS, and the app \
             still connects as an admin: the mitigation is built and unplugged. A warning has \
             been sitting there unread while the append-only audit trigger and the tenant \
             policy did nothing"
        );
        assert!(
            check.detail.contains("cuba_app"),
            "the message has to name the role that is ready: {}",
            check.detail
        );
    }

    #[test]
    fn a_superuser_runtime_without_the_app_role_only_warns() {
        let check = runtime_role_check("cuba", true, false);

        assert_eq!(
            check.status,
            Status::Warn,
            "a fresh install has no cuba_app yet, and its Docker container hands out a \
             superuser by design. Failing there would paint every new user red for a state \
             they were shipped in, and a doctor that is always red is a doctor nobody reads"
        );
        assert!(check.hint.is_some_and(|h| h.contains("secure")));
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;

    #[test]
    fn an_unkeyed_audit_chain_is_reported_as_forgeable() {
        let check = audit_chain_check(false, 20);

        assert_eq!(check.status, Status::Warn);
        let hint = check
            .hint
            .expect("a warning without a consequence is decoration");
        assert!(
            hint.contains("INSERT"),
            "the hint has to name the capability that breaks it: anyone who can INSERT can \
             forge a row whose hash recomputes, and verify() calls it ok. Measured on this \
             machine: 3 of 3 rows reproduce with plain SHA-256"
        );
    }

    #[test]
    fn a_keyed_chain_is_the_healthy_case() {
        assert_eq!(audit_chain_check(true, 20).status, Status::Ok);
    }

    #[test]
    fn an_empty_principal_table_is_reported_as_single_user_ok() {
        let check = rbac_check(0);

        assert_eq!(
            check.status,
            Status::Ok,
            "empty brain_principals is the designed single-user fail-open mode"
        );
        assert!(
            check.detail.contains("single-user") || check.detail.contains("vacío"),
            "detail should name the mode: {}",
            check.detail
        );
    }

    #[test]
    fn a_populated_principal_table_is_the_healthy_case() {
        assert_eq!(rbac_check(3).status, Status::Ok);
    }
}

#[cfg(test)]
mod reranker_tests {
    use super::*;

    #[test]
    fn resources_disabling_it_for_ram_is_distinguished_from_no_model_at_all() {
        let starved = reranker_check(false, false, false, true, 373, "minimal", false);
        assert_eq!(
            starved.status,
            Status::Warn,
            "a machine the resource plan starved of RAM is a warning, not a silent 'no model'"
        );
        assert!(
            starved.detail.contains("373"),
            "the RAM figure measured at startup has to be in the message, or the operator \
             cannot tell this apart from never having installed a model: {}",
            starved.detail
        );
        assert!(
            starved
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("reiniciá")),
            "resources.rs decides this once at startup — the hint has to say a restart is \
             required, or raising the memory limit later looks like it should have worked"
        );

        let never_installed = reranker_check(false, false, false, false, 8000, "full", false);
        assert_ne!(
            starved.detail, never_installed.detail,
            "two different root causes collapsing into the same sentence is the exact bug \
             this check exists to fix — an operator who raises the RAM limit and restarts \
             would see 'no configurado' again and have no idea it ever meant something else"
        );
    }

    #[test]
    fn a_loaded_model_says_whether_it_is_reachable_by_default() {
        let with_gpu = reranker_check(true, true, true, false, 8000, "full", true);
        assert_eq!(with_gpu.status, Status::Ok);
        assert!(
            with_gpu.detail.contains("activo por defecto: true"),
            "{}",
            with_gpu.detail
        );

        let without_gpu = reranker_check(true, true, true, false, 8000, "full", false);
        assert_eq!(without_gpu.status, Status::Ok);
        assert!(
            without_gpu.detail.contains("activo por defecto: false"),
            "the model can load fine on CPU and still be unreachable by default — the two \
             facts are independent and this message is the only place that says so: {}",
            without_gpu.detail
        );
        assert!(
            without_gpu.detail.contains("rerank:true"),
            "an operator who wants it anyway needs the exact escape hatch spelled out: {}",
            without_gpu.detail
        );
    }

    #[test]
    fn a_model_on_disk_that_fails_to_load_is_still_a_failure() {
        assert_eq!(
            reranker_check(true, true, false, false, 8000, "full", true).status,
            Status::Fail
        );
    }

    #[test]
    fn genuinely_no_model_on_disk_is_only_a_warning() {
        let check = reranker_check(false, false, false, false, 8000, "full", false);
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains("no hay modelo en disco"));
    }
}
