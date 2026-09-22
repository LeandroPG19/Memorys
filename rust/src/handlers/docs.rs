use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

use crate::net::fetch;
use crate::search::cache::TtlLruCache;

pub fn enabled() -> bool {
    crate::mode::env_toggle("CUBA_DOCS").unwrap_or_else(|| crate::mode::active().docs_default())
}

static DOC_CACHE: OnceLock<Mutex<TtlLruCache<String>>> = OnceLock::new();

fn cache() -> &'static Mutex<TtlLruCache<String>> {
    DOC_CACHE.get_or_init(|| Mutex::new(TtlLruCache::new()))
}

fn resolve(library: &str) -> Result<String> {
    let lib = library.trim().to_lowercase();
    if lib.is_empty() {
        anyhow::bail!("¿documentación de qué? `library` está vacío");
    }
    if !lib
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        anyhow::bail!(
            "`{library}` no parece un nombre de paquete. Pasá sólo el nombre \
             (p.ej. `tokio`, `sqlx`, `fastapi`), no una URL ni una frase."
        );
    }

    Ok(match lib.as_str() {
        "fastapi" => "https://fastapi.tiangolo.com/".into(),
        "pydantic" => "https://docs.pydantic.dev/latest/".into(),
        "sqlalchemy" => "https://docs.sqlalchemy.org/en/20/".into(),
        "httpx" => "https://www.python-httpx.org/".into(),
        "pytest" => "https://docs.pytest.org/en/stable/".into(),
        "structlog" => "https://www.structlog.org/en/stable/".into(),
        "granian" => "https://github.com/emmett-framework/granian".into(),
        "react" => "https://react.dev/reference/react".into(),
        "next" | "nextjs" | "next.js" => "https://nextjs.org/docs".into(),
        "zod" => "https://zod.dev/".into(),
        "zustand" => "https://zustand.docs.pmnd.rs/".into(),
        "tailwind" | "tailwindcss" => "https://tailwindcss.com/docs".into(),
        "vitest" => "https://vitest.dev/guide/".into(),
        "postgresql" | "postgres" => "https://www.postgresql.org/docs/current/".into(),
        "pgvector" => "https://github.com/pgvector/pgvector".into(),
        "docker" => "https://docs.docker.com/".into(),
        other => format!(
            "https://docs.rs/{other}/latest/{}/",
            other.replace('-', "_")
        ),
    })
}

pub async fn handle(args: &Value) -> Result<Value> {
    if !enabled() {
        anyhow::bail!(
            "cuba_docs está apagado. Es la única parte de MemoryIndustry que sale a \
             internet, así que no se enciende sola: exportá CUBA_DOCS=1 en el entorno \
             del servidor MCP."
        );
    }
    let library = args
        .get("library")
        .and_then(Value::as_str)
        .context("falta `library`")?;
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");

    let url = resolve(library)?;

    let cached = cache()
        .lock()
        .map_err(|e| anyhow::anyhow!("cache envenenada: {e}"))?
        .get(&url);

    let text = match cached {
        Some(t) => t,
        None => {
            let html = fetch::get(&url)
                .await
                .with_context(|| format!("trayendo la documentación de `{library}` desde {url}"))?;
            let t = fetch::html_to_text(&html, 100);
            cache()
                .lock()
                .map_err(|e| anyhow::anyhow!("cache envenenada: {e}"))?
                .put(url.clone(), t.clone());
            t
        }
    };

    let (body, filtered) = match query.trim() {
        "" => (truncate(&text, 8_000), false),
        q => {
            let hits = grep(&text, q);
            if hits.is_empty() {
                return Ok(json!({
                    "library": library,
                    "url": url,
                    "query": q,
                    "found": false,
                    "documentation": null,
                    "note": format!(
                        "La documentación de `{library}` se descargó bien, pero no menciona «{q}». \
                         No se devuelve el resto de la página: no responde a lo que preguntaste."
                    ),
                }));
            }
            (truncate(&hits, 8_000), true)
        }
    };

    Ok(json!({
        "library": library,
        "url": url,
        "query": if query.is_empty() { Value::Null } else { json!(query) },
        "found": true,
        "filtered": filtered,
        "documentation": body,
    }))
}

fn grep(text: &str, query: &str) -> String {
    let needle = query.to_lowercase();
    let paras: Vec<&str> = text.split("\n\n").collect();
    let mut keep: Vec<usize> = Vec::new();

    for (i, p) in paras.iter().enumerate() {
        if p.to_lowercase().contains(&needle) {
            keep.extend([i.saturating_sub(1), i, (i + 1).min(paras.len() - 1)]);
        }
    }
    keep.sort_unstable();
    keep.dedup();
    keep.iter()
        .map(|&i| paras[i])
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n\n[…truncado en {max} caracteres]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_crates_resolve_to_docs_rs() {
        assert_eq!(
            resolve("tokio").unwrap(),
            "https://docs.rs/tokio/latest/tokio/"
        );
        assert_eq!(
            resolve("tiktoken-rs").unwrap(),
            "https://docs.rs/tiktoken-rs/latest/tiktoken_rs/"
        );
    }

    #[test]
    fn known_ecosystems_have_explicit_urls() {
        assert!(resolve("fastapi").unwrap().contains("tiangolo"));
        assert!(resolve("React").unwrap().contains("react.dev"));
    }

    #[test]
    fn the_network_is_off_unless_asked_for() {
        assert!(
            !enabled(),
            "CUBA_DOCS no está puesto: cuba_docs DEBE estar apagado"
        );
        assert!(
            !crate::handlers::is_known_tool("cuba_docs"),
            "apagado, el dispatcher no puede conocerla"
        );
        assert!(
            !crate::constants::tool_definitions()
                .iter()
                .any(|t| t.get("name").and_then(Value::as_str) == Some("cuba_docs")),
            "apagado, no puede aparecer en el catálogo: una herramienta que el agente \
             no ve es una que no puede llamar, y ESA es la garantía"
        );
    }

    #[test]
    fn a_url_is_not_a_package_name() {
        for bad in [
            "http://169.254.169.254/",
            "../../etc/passwd",
            "tokio; rm -rf /",
            "",
        ] {
            assert!(resolve(bad).is_err(), "`{bad}` no es un nombre de paquete");
        }
    }
}
