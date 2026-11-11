use anyhow::{Context, Result, bail};
use std::time::Duration;

use super::guard;

const TIMEOUT: Duration = Duration::from_secs(10);

const MAX_BYTES: usize = 2 * 1024 * 1024;

const MAX_HOPS: usize = 5;

pub async fn get(raw: &str) -> Result<String> {
    let mut target = raw.to_string();

    for hop in 0..=MAX_HOPS {
        let safe = guard::check(&target)?;
        let host = safe
            .url
            .host_str()
            .context("URL sin host tras la validación")?
            .to_string();

        let client = crate::http_client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&host, safe.addr)
            .user_agent(concat!("cuba-memorys/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("construyendo el cliente HTTP")?;

        let resp = client
            .get(safe.url.clone())
            .send()
            .await
            .with_context(|| format!("pidiendo {}", safe.url))?;

        let status = resp.status();

        if status.is_redirection() {
            if hop == MAX_HOPS {
                bail!("demasiadas redirecciones ({MAX_HOPS}) desde {raw}");
            }
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("redirección sin cabecera Location")?;

            target = safe
                .url
                .join(location)
                .with_context(|| format!("Location inválida: {location}"))?
                .to_string();
            continue;
        }

        if !status.is_success() {
            bail!("{} respondió {}", safe.url, status);
        }

        return read_capped(resp).await;
    }

    bail!("demasiadas redirecciones ({MAX_HOPS}) desde {raw}")
}

async fn read_capped(mut resp: reqwest::Response) -> Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("leyendo el cuerpo")? {
        if buf.len() + chunk.len() > MAX_BYTES {
            let room = MAX_BYTES - buf.len();
            buf.extend_from_slice(&chunk[..room]);
            tracing::warn!(
                limit = MAX_BYTES,
                "respuesta truncada: el servidor mandó más de lo permitido"
            );
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `from_read` `expect`s its own conversion, so a page nested deeper than `width` (each `<blockquote>` takes
/// columns) took the handler down with a panic. A page that does not fit is text wider than `width`, and
/// what still cannot be converted is an error of the tool.
pub fn html_to_text(html: &str, width: usize) -> Result<String> {
    Ok(html2text::config::plain()
        .allow_width_overflow()
        .string_from_read(html.as_bytes(), width)?)
}
