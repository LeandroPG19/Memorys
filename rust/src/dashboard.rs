use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::doctor::{self, Status};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut out: Option<String> = None;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry dashboard [salida.html]\n\n\
                     Genera un HTML autocontenido (sin servidor, sin red) con la salud del\n\
                     sistema y la forma del corpus. Por defecto: cuba-dashboard.html"
                );
                return Ok(());
            }
            other => out = Some(other.to_string()),
        }
    }
    let path = out.unwrap_or_else(|| "cuba-dashboard.html".to_string());

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for dashboard")?;

    let html = render(&pool, &url).await?;
    std::fs::write(&path, html).with_context(|| format!("no se pudo escribir {path}"))?;

    println!("Dashboard escrito en {path}");
    println!("Abrilo con un doble clic — no necesita servidor ni red.");
    Ok(())
}

async fn render(pool: &PgPool, url: &str) -> Result<String> {
    let checks = doctor::run_checks(pool, url).await;
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();

    let verdict = if failed > 0 {
        ("degradado", "fail")
    } else if warned > 0 {
        ("operativo con avisos", "warn")
    } else {
        ("sano", "ok")
    };

    let mut health = String::new();
    for c in &checks {
        let cls = match c.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        };
        let hint = c
            .hint
            .as_ref()
            .map(|h| format!("<div class=hint>{}</div>", esc(h)))
            .unwrap_or_default();
        health.push_str(&format!(
            "<tr class={cls}><td class=dot></td><td class=name>{}</td><td>{}{hint}</td></tr>",
            esc(&c.name),
            esc(&c.detail)
        ));
    }

    let row = sqlx::query(
        "SELECT (SELECT count(*) FROM brain_observations)::bigint AS obs,
                (SELECT count(*) FROM brain_entities)::bigint AS ent,
                (SELECT count(*) FROM brain_facts)::bigint AS facts,
                (SELECT count(*) FROM brain_relations)::bigint AS rels,
                (SELECT count(*) FROM brain_entities e
                 WHERE NOT EXISTS (SELECT 1 FROM brain_relations r
                                   WHERE r.from_entity = e.id OR r.to_entity = e.id))::bigint AS isolated",
    )
    .fetch_one(pool)
    .await?;
    let obs: i64 = row.try_get("obs").unwrap_or(0);
    let ent: i64 = row.try_get("ent").unwrap_or(0);
    let facts: i64 = row.try_get("facts").unwrap_or(0);
    let rels: i64 = row.try_get("rels").unwrap_or(0);
    let isolated: i64 = row.try_get("isolated").unwrap_or(0);
    let connected = ent - isolated;
    let pct_conn = if ent > 0 { connected * 100 / ent } else { 0 };

    let top = sqlx::query(
        "SELECT e.name, e.entity_type, e.importance,
                (SELECT count(*) FROM brain_observations o WHERE o.entity_id = e.id)::bigint AS n,
                (SELECT count(*) FROM brain_relations r
                 WHERE r.from_entity = e.id OR r.to_entity = e.id)::bigint AS deg
         FROM brain_entities e ORDER BY e.importance DESC LIMIT 25",
    )
    .fetch_all(pool)
    .await?;
    let mut top_rows = String::new();
    for r in &top {
        let name: String = r.try_get("name").unwrap_or_default();
        let kind: String = r.try_get("entity_type").unwrap_or_default();
        let imp: f64 = r.try_get("importance").unwrap_or(0.0);
        let n: i64 = r.try_get("n").unwrap_or(0);
        let deg: i64 = r.try_get("deg").unwrap_or(0);
        let orphan = if deg == 0 {
            " <span class=tag-orphan>aislada</span>"
        } else {
            ""
        };
        top_rows.push_str(&format!(
            "<tr><td>{}{orphan}</td><td class=muted>{}</td><td class=num>{imp:.3}</td>\
             <td class=num>{n}</td><td class=num>{deg}</td></tr>",
            esc(&name),
            esc(&kind)
        ));
    }

    let recent = sqlx::query(
        "SELECT e.name AS entity, o.content, o.observation_type, o.created_at::date::text AS d
         FROM brain_observations o JOIN brain_entities e ON e.id = o.entity_id
         ORDER BY o.created_at DESC LIMIT 400",
    )
    .fetch_all(pool)
    .await?;
    let items: Vec<serde_json::Value> = recent
        .iter()
        .map(|r| {
            serde_json::json!({
                "e": r.try_get::<String, _>("entity").unwrap_or_default(),
                "c": r.try_get::<String, _>("content").unwrap_or_default(),
                "t": r.try_get::<String, _>("observation_type").unwrap_or_default(),
                "d": r.try_get::<String, _>("d").unwrap_or_default(),
            })
        })
        .collect();
    let payload = serde_json::to_string(&items)?.replace("</", "<\\/");

    Ok(format!(
        r#"<!doctype html>
<meta charset=utf-8>
<title>MemoryIndustry — estado del cerebro</title>
<style>
  :root {{ --bg:#0f1115; --panel:#171a21; --line:#252a34; --fg:#e6e9ef; --muted:#8b94a7;
           --ok:#3fb950; --warn:#d29922; --fail:#f85149; --accent:#58a6ff; }}
  @media (prefers-color-scheme: light) {{
    :root {{ --bg:#f6f7f9; --panel:#fff; --line:#e3e6ea; --fg:#1c2128; --muted:#6b7280; }}
  }}
  * {{ box-sizing: border-box; }}
  body {{ margin:0; padding:2rem 1.25rem 4rem; background:var(--bg); color:var(--fg);
          font:15px/1.55 ui-sans-serif,system-ui,-apple-system,Segoe UI,Roboto,sans-serif; }}
  .wrap {{ max-width: 980px; margin: 0 auto; }}
  h1 {{ font-size:1.5rem; margin:0 0 .25rem; }}
  .sub {{ color:var(--muted); margin-bottom:2rem; }}
  .verdict {{ display:inline-block; padding:.15rem .6rem; border-radius:999px; font-size:.8rem;
              font-weight:600; margin-left:.5rem; }}
  .verdict.ok {{ background:rgba(63,185,80,.15); color:var(--ok); }}
  .verdict.warn {{ background:rgba(210,153,34,.15); color:var(--warn); }}
  .verdict.fail {{ background:rgba(248,81,73,.15); color:var(--fail); }}
  .cards {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(140px,1fr)); gap:.75rem; margin-bottom:2rem; }}
  .card {{ background:var(--panel); border:1px solid var(--line); border-radius:10px; padding:.9rem 1rem; }}
  .card .n {{ font-size:1.6rem; font-weight:650; }}
  .card .l {{ color:var(--muted); font-size:.8rem; }}
  section {{ background:var(--panel); border:1px solid var(--line); border-radius:12px;
             padding:1.1rem 1.25rem; margin-bottom:1.5rem; }}
  h2 {{ font-size:1rem; margin:0 0 .9rem; }}
  table {{ width:100%; border-collapse:collapse; }}
  td, th {{ padding:.45rem .5rem; border-bottom:1px solid var(--line); text-align:left; vertical-align:top; }}
  tr:last-child td {{ border-bottom:0; }}
  th {{ color:var(--muted); font-weight:500; font-size:.8rem; }}
  .num {{ text-align:right; font-variant-numeric:tabular-nums; }}
  .muted {{ color:var(--muted); }}
  .name {{ font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:.85rem; white-space:nowrap; }}
  td.dot {{ width:14px; }}
  td.dot::before {{ content:''; display:block; width:8px; height:8px; border-radius:50%; margin-top:.45rem; }}
  tr.ok td.dot::before {{ background:var(--ok); }}
  tr.warn td.dot::before {{ background:var(--warn); }}
  tr.fail td.dot::before {{ background:var(--fail); }}
  .hint {{ color:var(--muted); font-size:.85rem; margin-top:.2rem; }}
  .tag-orphan {{ background:rgba(210,153,34,.15); color:var(--warn); font-size:.7rem;
                 padding:.05rem .35rem; border-radius:4px; }}
  .bar {{ height:8px; border-radius:999px; background:var(--line); overflow:hidden; margin:.6rem 0 .3rem; }}
  .bar > i {{ display:block; height:100%; background:var(--accent); }}
  input[type=search] {{ width:100%; padding:.6rem .75rem; border-radius:8px; border:1px solid var(--line);
                        background:var(--bg); color:var(--fg); font-size:.95rem; }}
  #hits li {{ list-style:none; padding:.6rem 0; border-bottom:1px solid var(--line); }}
  #hits {{ padding:0; margin:.75rem 0 0; }}
  #hits .e {{ font-weight:600; }}
  #hits .m {{ color:var(--muted); font-size:.8rem; }}
  footer {{ color:var(--muted); font-size:.8rem; text-align:center; margin-top:2rem; }}
</style>
<div class=wrap>
  <h1>MemoryIndustry <span class="verdict {vcls}">{vtext}</span></h1>
  <div class=sub>Generado desde la base, solo lectura. {failed} fallo(s), {warned} aviso(s).</div>

  <div class=cards>
    <div class=card><div class=n>{obs}</div><div class=l>observaciones</div></div>
    <div class=card><div class=n>{ent}</div><div class=l>entidades</div></div>
    <div class=card><div class=n>{facts}</div><div class=l>hechos</div></div>
    <div class=card><div class=n>{rels}</div><div class=l>relaciones</div></div>
  </div>

  <section>
    <h2>Salud</h2>
    <table>{health}</table>
  </section>

  <section>
    <h2>Conectividad del grafo</h2>
    <div class=bar><i style="width:{pct_conn}%"></i></div>
    <div class=muted>{connected} de {ent} entidades ({pct_conn}%) tienen al menos una relación.
      Las {isolated} restantes son invisibles para el retrieval asociativo multi-hop y para PageRank:
      para el grafo, no existen.</div>
  </section>

  <section>
    <h2>Entidades más importantes</h2>
    <table>
      <tr><th>entidad</th><th>tipo</th><th class=num>importancia</th><th class=num>obs</th><th class=num>grado</th></tr>
      {top_rows}
    </table>
  </section>

  <section>
    <h2>Buscar (últimas 400 observaciones)</h2>
    <input type=search id=q placeholder="filtrar por texto o entidad…" autocomplete=off>
    <ul id=hits></ul>
  </section>

  <footer>MemoryIndustry v{version} · sin servidor, sin red, sin dependencias</footer>
</div>
<script type="application/json" id=data>{payload}</script>
<script>
  const DATA = JSON.parse(document.getElementById('data').textContent.replace(/<\\\
  const q = document.getElementById('q'), hits = document.getElementById('hits');
  function draw(list) {{
    hits.replaceChildren(...list.slice(0, 60).map(o => {{
      const li = document.createElement('li');
      const e = document.createElement('div'); e.className = 'e'; e.textContent = o.e;
      const c = document.createElement('div'); c.textContent = o.c;
      const m = document.createElement('div'); m.className = 'm'; m.textContent = o.t + ' · ' + o.d;
      li.append(e, c, m); return li;
    }}));
  }}
  q.addEventListener('input', () => {{
    const t = q.value.toLowerCase().trim();
    draw(!t ? DATA : DATA.filter(o =>
      o.c.toLowerCase().includes(t) || o.e.toLowerCase().includes(t)));
  }});
  draw(DATA);
</script>
"#,
        vcls = verdict.1,
        vtext = verdict.0,
        version = env!("CARGO_PKG_VERSION"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_cannot_rewrite_the_page() {
        let hostile = r#"<script>alert('xss')</script>"#;
        let safe = esc(hostile);
        assert!(!safe.contains("<script>"));
        assert!(safe.contains("&lt;script&gt;"));
        assert_eq!(
            esc(r#"a & b "c" 'd'"#),
            "a &amp; b &quot;c&quot; &#39;d&#39;"
        );
    }
}
