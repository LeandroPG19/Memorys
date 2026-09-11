use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

pub mod alarma;
pub mod alma;
pub mod archivo;
pub mod artefacto;
pub mod calibrar;
pub mod centinela;
pub mod contexto;
pub mod contradiccion;
pub mod cronica;
pub mod decreto;
#[cfg(feature = "docs")]
pub mod docs;
pub mod eco;
pub mod expediente;
pub mod faro;
pub mod forget;
pub mod hipotesis;
pub mod ingesta;
pub mod jornada;
pub mod juez;
pub mod pizarra;
pub mod pre_compact;
pub mod proyecto;
pub mod puente;
pub mod receta;
pub mod reflexion;
pub mod remedio;
pub mod sync;
pub mod tools;
pub mod vigia;
pub mod whoami;
pub mod zafra;

/// Map `memory_*` aliases (and a few synonyms) onto canonical `cuba_*` tool names.
pub fn canonicalize_tool_name(name: &str) -> &str {
    match name {
        "memory_whoami" | "cuba_whoami" => "cuba_whoami",
        "memory_artifact" | "memory_artefacto" | "cuba_artefacto" => "cuba_artefacto",
        "memory_context" | "memory_contexto" | "cuba_contexto" => "cuba_contexto",
        other if let Some(rest) = other.strip_prefix("memory_") => {
            // memory_faro -> cuba_faro etc. Resolved via a static table below.
            CANON_MEMORY
                .iter()
                .find(|(alias, _)| *alias == rest)
                .map(|(_, canon)| *canon)
                .unwrap_or(name)
        }
        other => other,
    }
}

const CANON_MEMORY: &[(&str, &str)] = &[
    ("alma", "cuba_alma"),
    ("cronica", "cuba_cronica"),
    ("faro", "cuba_faro"),
    ("receta", "cuba_receta"),
    ("tools", "cuba_tools"),
    ("call", "cuba_call"),
    ("forget", "cuba_forget"),
    ("hipotesis", "cuba_hipotesis"),
    ("puente", "cuba_puente"),
    ("reflexion", "cuba_reflexion"),
    ("eco", "cuba_eco"),
    ("alarma", "cuba_alarma"),
    ("remedio", "cuba_remedio"),
    ("expediente", "cuba_expediente"),
    ("jornada", "cuba_jornada"),
    ("decreto", "cuba_decreto"),
    ("vigia", "cuba_vigia"),
    ("zafra", "cuba_zafra"),
    ("centinela", "cuba_centinela"),
    ("contradiccion", "cuba_contradiccion"),
    ("calibrar", "cuba_calibrar"),
    ("ingesta", "cuba_ingesta"),
    ("proyecto", "cuba_proyecto"),
    ("pre_compact", "cuba_pre_compact"),
    ("sync", "cuba_sync"),
    ("juez", "cuba_juez"),
    ("pizarra", "cuba_pizarra"),
    ("archivo", "cuba_archivo"),
    ("docs", "cuba_docs"),
    ("whoami", "cuba_whoami"),
    ("artefacto", "cuba_artefacto"),
    ("artifact", "cuba_artefacto"),
    ("contexto", "cuba_contexto"),
    ("context", "cuba_contexto"),
];

#[tracing::instrument(skip(pool, args), fields(tool = %tool_name))]
pub async fn dispatch(pool: &PgPool, tool_name: &str, args: Value) -> Result<Value> {
    let tool_name = canonicalize_tool_name(tool_name);
    let start = std::time::Instant::now();

    if crate::session::current_scope() == crate::session::Scope::Peer {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");
        let permitted = crate::session::PEER_VERBS
            .iter()
            .any(|(tool, verb)| *tool == tool_name && *verb == action);
        if !permitted {
            let allowed: Vec<String> = crate::session::PEER_VERBS
                .iter()
                .map(|(tool, verb)| format!("{tool} action={verb}"))
                .collect();
            tracing::warn!(tool = %tool_name, action = %action, "peer token refused");
            anyhow::bail!(
                "a peer token may call {} and nothing else; {tool_name} action={action:?} is \
                 refused. This check lives in dispatch on purpose: cuba_call reaches every \
                 handler through the same function, so an allow-list enforced anywhere above it \
                 would let a peer ask for cuba_forget by name inside a permitted envelope.",
                allowed.join(", ")
            );
        }
    }

    let dispatch_result: Result<Value> = async {
        match tool_name {
            "cuba_alma" => alma::handle(pool, args).await,
            "cuba_cronica" => cronica::handle(pool, args).await,
            "cuba_faro" => faro::handle(pool, args).await,
            "cuba_receta" => receta::handle(pool, args).await,
            "cuba_tools" => tools::handle_tools(pool, args).await,
            "cuba_call" => tools::handle_call(pool, args).await,
            "cuba_forget" => forget::handle(pool, args).await,
            "cuba_hipotesis" => hipotesis::handle(pool, args).await,
            "cuba_puente" => puente::handle(pool, args).await,
            "cuba_reflexion" => reflexion::handle(pool, args).await,
            "cuba_eco" => eco::handle(pool, args).await,
            "cuba_alarma" => alarma::handle(pool, args).await,
            "cuba_remedio" => remedio::handle(pool, args).await,
            "cuba_expediente" => expediente::handle(pool, args).await,
            "cuba_jornada" => jornada::handle(pool, args).await,
            "cuba_decreto" => decreto::handle(pool, args).await,
            "cuba_vigia" => vigia::handle(pool, args).await,
            "cuba_zafra" => zafra::handle(pool, args).await,
            "cuba_centinela" => centinela::handle(pool, args).await,
            "cuba_contradiccion" => contradiccion::handle(pool, args).await,
            "cuba_calibrar" => calibrar::handle(pool, args).await,
            "cuba_ingesta" => ingesta::handle(pool, args).await,
            "cuba_proyecto" => proyecto::handle(pool, args).await,
            "cuba_pre_compact" => pre_compact::handle(pool, args).await,
            "cuba_sync" => sync::handle(pool, args).await,
            "cuba_juez" => juez::handle(pool, args).await,
            "cuba_pizarra" => pizarra::handle(pool, args).await,
            "cuba_archivo" => archivo::handle(pool, args).await,
            "cuba_whoami" => whoami::handle(pool, args).await,
            "cuba_artefacto" => artefacto::handle(pool, args).await,
            "cuba_contexto" => contexto::handle(pool, args).await,
            #[cfg(feature = "docs")]
            "cuba_docs" => docs::handle(&args).await,
            _ => {
                tracing::warn!(tool = %tool_name, "unknown tool");
                anyhow::bail!("Unknown tool: {tool_name}")
            }
        }
    }
    .await;

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let outcome = if dispatch_result.is_ok() {
        "ok"
    } else {
        "error"
    };
    crate::observability::record_handler(tool_name, outcome, elapsed.as_secs_f64());
    tracing::info!(tool = %tool_name, elapsed_ms = %elapsed_ms, outcome = %outcome, "handler completed");

    if let Err(why) = &dispatch_result {
        let redacted = crate::redact::redact_secrets(&format!("{why:#}"));
        let recorded = sqlx::query(
            "INSERT INTO brain_handler_failures (tool, client, error, elapsed_ms)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(tool_name)
        .bind(crate::session::current_client())
        .bind(&redacted)
        .bind(elapsed_ms.min(i32::MAX as u128) as i32)
        .execute(pool)
        .await;
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "could not record the handler failure");
        }
    }

    let result = dispatch_result?;

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&result)?
        }]
    }))
}

pub fn is_known_tool(name: &str) -> bool {
    let name = canonicalize_tool_name(name);
    #[cfg(feature = "docs")]
    if name == "cuba_docs" {
        return docs::enabled();
    }
    matches!(
        name,
        "cuba_alma"
            | "cuba_cronica"
            | "cuba_faro"
            | "cuba_forget"
            | "cuba_hipotesis"
            | "cuba_puente"
            | "cuba_reflexion"
            | "cuba_eco"
            | "cuba_alarma"
            | "cuba_remedio"
            | "cuba_expediente"
            | "cuba_jornada"
            | "cuba_decreto"
            | "cuba_vigia"
            | "cuba_zafra"
            | "cuba_contradiccion"
            | "cuba_centinela"
            | "cuba_calibrar"
            | "cuba_ingesta"
            | "cuba_proyecto"
            | "cuba_pre_compact"
            | "cuba_sync"
            | "cuba_archivo"
            | "cuba_pizarra"
            | "cuba_juez"
            | "cuba_tools"
            | "cuba_call"
            | "cuba_receta"
            | "cuba_whoami"
            | "cuba_artefacto"
            | "cuba_contexto"
    )
}

#[cfg(test)]
mod alias_tests {
    use super::canonicalize_tool_name;

    #[test]
    fn memory_aliases_resolve_to_cuba_tools() {
        assert_eq!(canonicalize_tool_name("memory_whoami"), "cuba_whoami");
        assert_eq!(canonicalize_tool_name("memory_artifact"), "cuba_artefacto");
        assert_eq!(canonicalize_tool_name("memory_context"), "cuba_contexto");
        assert_eq!(canonicalize_tool_name("memory_faro"), "cuba_faro");
        assert_eq!(canonicalize_tool_name("cuba_faro"), "cuba_faro");
    }
}
