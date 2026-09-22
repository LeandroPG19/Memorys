use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sqlx::{PgPool, Row};

use crate::graph::community;

fn safe_note_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "sin-nombre".to_string()
    } else {
        trimmed.to_string()
    }
}

fn yaml_escape(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

fn neutralize_wikilinks(s: &str) -> String {
    s.replace("[[", "\\[\\[").replace("]]", "\\]\\]")
}

struct Entity {
    id: uuid::Uuid,
    name: String,
    entity_type: String,
    importance: f64,
    access_count: i32,
}

struct Relation {
    from: uuid::Uuid,
    to: uuid::Uuid,
    relation_type: String,
    strength: f64,
}

struct Observation {
    content: String,
    observation_type: String,
    created_at: String,
    importance: f64,
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut dir: Option<String> = None;
    let mut format = "obsidian".to_string();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--obsidian" => format = "obsidian".into(),
            "--format" => format = it.next().cloned().context("--format needs a value")?,
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry export <dir> [--obsidian]\n\n\
                     Escribe el grafo como un vault de Obsidian: una nota por entidad, con\n\
                     wikilinks que reflejan las relaciones reales, y comunidad + centralidad\n\
                     en el frontmatter. Solo lectura sobre la base."
                );
                return Ok(());
            }
            other => dir = Some(other.to_string()),
        }
    }

    if format != "obsidian" {
        bail!("formato no soportado: {format} (solo 'obsidian' por ahora)");
    }
    let Some(dir) = dir else {
        bail!("falta el directorio — uso: memory-industry export <dir> [--obsidian]");
    };

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for export")?;

    let n = export_obsidian(&pool, Path::new(&dir)).await?;
    println!("Exportadas {n} entidades a {dir}");
    println!("Abrilo en Obsidian: «Open folder as vault» → {dir}");
    Ok(())
}

pub fn refuse_unless_safe_to_write(dir: &Path, apply: bool) -> Result<()> {
    if apply || !dir.exists() {
        return Ok(());
    }
    let mut existing: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("no se pudo leer {}", dir.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    if existing.is_empty() {
        return Ok(());
    }
    existing.sort();
    let shown: Vec<&str> = existing.iter().take(5).map(String::as_str).collect();
    let more = existing.len().saturating_sub(shown.len());
    bail!(
        "{} ya contiene {} entrada(s) ({}{}). El export escribe README.md como índice y una \
         nota por entidad, así que sobrescribiría lo que coincida en nombre y eso no se \
         recupera. Volvé a lanzarlo con --apply si es lo que querés.",
        dir.display(),
        existing.len(),
        shown.join(", "),
        if more > 0 {
            format!(", y {more} más")
        } else {
            String::new()
        }
    )
}

pub async fn export_obsidian(pool: &PgPool, dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(dir).with_context(|| format!("no se pudo crear {}", dir.display()))?;

    let entities: Vec<Entity> = sqlx::query(
        "SELECT id, name, entity_type, importance, access_count
         FROM brain_entities ORDER BY importance DESC",
    )
    .fetch_all(pool)
    .await
    .context("reading entities")?
    .iter()
    .map(|r| Entity {
        id: r.try_get("id").unwrap_or_default(),
        name: r.try_get("name").unwrap_or_default(),
        entity_type: r.try_get("entity_type").unwrap_or_default(),
        importance: r.try_get("importance").unwrap_or(0.0),
        access_count: r.try_get("access_count").unwrap_or(0),
    })
    .collect();

    if entities.is_empty() {
        bail!("la base no tiene entidades — nada que exportar");
    }

    let by_id: HashMap<uuid::Uuid, &Entity> = entities.iter().map(|e| (e.id, e)).collect();

    let relations: Vec<Relation> =
        sqlx::query("SELECT from_entity, to_entity, relation_type, strength FROM brain_relations")
            .fetch_all(pool)
            .await
            .context("reading relations")?
            .iter()
            .map(|r| Relation {
                from: r.try_get("from_entity").unwrap_or_default(),
                to: r.try_get("to_entity").unwrap_or_default(),
                relation_type: r.try_get("relation_type").unwrap_or_default(),
                strength: r.try_get("strength").unwrap_or(0.0),
            })
            .collect();

    let mut obs_by_entity: HashMap<uuid::Uuid, Vec<Observation>> = HashMap::new();
    let obs_rows = sqlx::query(
        "SELECT entity_id, content, observation_type, created_at::date::text AS created_at, importance
         FROM brain_observations ORDER BY importance DESC, created_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("reading observations")?;
    for r in &obs_rows {
        let eid: uuid::Uuid = r.try_get("entity_id").unwrap_or_default();
        obs_by_entity.entry(eid).or_default().push(Observation {
            content: r.try_get("content").unwrap_or_default(),
            observation_type: r.try_get("observation_type").unwrap_or_default(),
            created_at: r.try_get("created_at").unwrap_or_default(),
            importance: r.try_get("importance").unwrap_or(0.0),
        });
    }

    let mut community_of: HashMap<String, usize> = HashMap::new();
    match community::detect(pool).await {
        Ok(communities) => {
            for (cid, members) in &communities {
                for name in members {
                    community_of.insert(name.clone(), *cid);
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "community detection failed — exporting without communities");
        }
    }

    for e in &entities {
        let note = safe_note_name(&e.name);
        let mut md = String::new();

        md.push_str("---\n");
        md.push_str(&format!("tipo: {}\n", yaml_escape(&e.entity_type)));
        md.push_str(&format!("importancia: {:.4}\n", e.importance));
        md.push_str(&format!("accesos: {}\n", e.access_count));
        if let Some(c) = community_of.get(&e.name) {
            md.push_str(&format!("comunidad: {c}\n"));
        }
        let obs = obs_by_entity.get(&e.id);
        md.push_str(&format!("observaciones: {}\n", obs.map_or(0, Vec::len)));
        md.push_str("---\n\n");

        md.push_str(&format!("# {}\n\n", e.name));

        let out: Vec<&Relation> = relations.iter().filter(|r| r.from == e.id).collect();
        let inc: Vec<&Relation> = relations.iter().filter(|r| r.to == e.id).collect();
        if !out.is_empty() || !inc.is_empty() {
            md.push_str("## Relaciones\n\n");
            for r in out {
                if let Some(target) = by_id.get(&r.to) {
                    md.push_str(&format!(
                        "- **{}** → [[{}]] _(fuerza {:.2})_\n",
                        r.relation_type,
                        safe_note_name(&target.name),
                        r.strength
                    ));
                }
            }
            for r in inc {
                if let Some(source) = by_id.get(&r.from) {
                    md.push_str(&format!(
                        "- [[{}]] — **{}** → _(entrante, fuerza {:.2})_\n",
                        safe_note_name(&source.name),
                        r.relation_type,
                        r.strength
                    ));
                }
            }
            md.push('\n');
        }

        if let Some(obs) = obs {
            md.push_str("## Observaciones\n\n");
            let mut grouped: HashMap<&str, Vec<&Observation>> = HashMap::new();
            for o in obs {
                grouped
                    .entry(o.observation_type.as_str())
                    .or_default()
                    .push(o);
            }
            let mut kinds: Vec<&&str> = grouped.keys().collect();
            kinds.sort_unstable();
            for kind in kinds {
                md.push_str(&format!("### {kind}\n\n"));
                for o in &grouped[*kind] {
                    let body = neutralize_wikilinks(&o.content).replace('\n', "\n  ");
                    md.push_str(&format!(
                        "- {body}\n  _({}, importancia {:.2})_\n",
                        o.created_at, o.importance
                    ));
                }
                md.push('\n');
            }
        }

        std::fs::write(dir.join(format!("{note}.md")), md)
            .with_context(|| format!("no se pudo escribir la nota de «{}»", e.name))?;
    }

    let mut idx = String::new();
    idx.push_str("# Cerebro (MemoryIndustry)\n\n");
    idx.push_str(&format!(
        "{} entidades · {} relaciones · {} observaciones\n\n",
        entities.len(),
        relations.len(),
        obs_rows.len()
    ));
    idx.push_str("Exportado read-only desde la base. Abrí la vista de grafo de Obsidian: las\n");
    idx.push_str(
        "aristas son las relaciones reales del knowledge graph, no wikilinks inventados.\n\n",
    );

    idx.push_str("## Más centrales (por importancia)\n\n");
    for e in entities.iter().take(20) {
        idx.push_str(&format!(
            "- [[{}]] — {} _(imp {:.3})_\n",
            safe_note_name(&e.name),
            e.entity_type,
            e.importance
        ));
    }

    if !community_of.is_empty() {
        let mut by_community: HashMap<usize, Vec<&str>> = HashMap::new();
        for e in &entities {
            if let Some(c) = community_of.get(&e.name) {
                by_community.entry(*c).or_default().push(&e.name);
            }
        }

        let mut groups: Vec<(usize, Vec<&str>)> = by_community.into_iter().collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        let (real, isolated): (Vec<_>, Vec<_>) = groups.into_iter().partition(|(_, m)| m.len() > 1);

        idx.push_str(&format!(
            "\n## Comunidades ({} con más de una entidad)\n\n",
            real.len()
        ));
        for (c, members) in &real {
            idx.push_str(&format!(
                "### Comunidad {c} ({} entidades)\n\n",
                members.len()
            ));
            for m in members.iter().take(12) {
                idx.push_str(&format!("- [[{}]]\n", safe_note_name(m)));
            }
            if members.len() > 12 {
                idx.push_str(&format!("- _…y {} más_\n", members.len() - 12));
            }
            idx.push('\n');
        }

        if !isolated.is_empty() {
            idx.push_str(&format!(
                "\n## Entidades aisladas ({} de {})\n\n\
                 Sin una sola relación en el grafo: invisibles para el retrieval asociativo\n\
                 multi-hop y para PageRank. No es un fallo del export — es el estado real del\n\
                 grafo. `cuba_reflexion` las lista; `cuba_puente` las conecta.\n\n",
                isolated.len(),
                entities.len()
            ));
            for (_, members) in isolated.iter().take(30) {
                for m in members {
                    idx.push_str(&format!("- [[{}]]\n", safe_note_name(m)));
                }
            }
            if isolated.len() > 30 {
                idx.push_str(&format!("- _…y {} más_\n", isolated.len() - 30));
            }
        }
    }

    std::fs::write(dir.join("README.md"), idx).context("no se pudo escribir el índice")?;

    Ok(entities.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_survive_filesystem_hostile_entities() {
        assert_eq!(safe_note_name("cuba-memorys"), "cuba-memorys");
        assert_eq!(safe_note_name("proyectos/MCP"), "proyectos-MCP");
        assert_eq!(safe_note_name("a:b*c?d"), "a-b-c-d");
        assert_eq!(safe_note_name("[[raro]]"), "--raro--");
        assert_eq!(safe_note_name("   "), "sin-nombre");
    }

    #[test]
    fn frontmatter_stays_valid_yaml() {
        assert_eq!(yaml_escape("simple"), "\"simple\"");
        assert_eq!(yaml_escape("con \"comillas\""), "\"con \\\"comillas\\\"\"");
        assert_eq!(yaml_escape("dos\nlíneas"), "\"dos líneas\"");
    }

    #[test]
    fn observation_text_cannot_forge_graph_edges() {
        assert_eq!(
            neutralize_wikilinks("ver [[otra-memoria]] para el detalle"),
            "ver \\[\\[otra-memoria\\]\\] para el detalle"
        );
        assert_eq!(neutralize_wikilinks("texto sin links"), "texto sin links");
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cuba-export-guard-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_is_safe() {
        let dir = scratch("missing");
        assert!(refuse_unless_safe_to_write(&dir, false).is_ok());
    }

    #[test]
    fn an_empty_directory_is_safe() {
        let dir = scratch("empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(refuse_unless_safe_to_write(&dir, false).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_repository_is_refused_because_the_index_is_called_readme() {
        let dir = scratch("repo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "someone else's readme").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();

        let refusal = refuse_unless_safe_to_write(&dir, false)
            .expect_err(
                "export writes README.md as its index and one note per entity. Run inside a                  project and it eats that project's README, which is the only irrecoverable                  loss the CLI can cause",
            )
            .to_string();

        assert!(refusal.contains("README.md"), "{refusal}");
        assert!(
            refusal.contains("--apply"),
            "a refusal has to say how to proceed on purpose: {refusal}"
        );

        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert_eq!(
            readme, "someone else's readme",
            "the guard must refuse BEFORE writing anything"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_is_the_way_to_write_into_a_directory_that_already_has_content() {
        let dir = scratch("apply");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();

        assert!(
            refuse_unless_safe_to_write(&dir, true).is_ok(),
            "re-exporting into your own vault is the normal case and must stay one flag away"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_holding_only_hidden_entries_still_counts_as_empty() {
        let dir = scratch("hidden");
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        assert!(
            refuse_unless_safe_to_write(&dir, false).is_ok(),
            "a freshly `git init`ed directory has a .git and nothing else; refusing there              would block the first export of the very workflow the README recommends"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
