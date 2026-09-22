use anyhow::{Context, Result};

use crate::handlers::receta;

const TRUSTED: f64 = 0.5;

const NO_HISTORY: &str = "> **Sin historial.** Este procedimiento nunca se ejecutó, así que no hay evidencia \
     de que funcione. Verificá cada paso y reportá el resultado con \
     `cuba_receta action=outcome`.\n\n";

const UNRELIABLE: &str = "> Falla más veces de las que funciona. Tratalo como una hipótesis, \
     no como una receta: probablemente esté desactualizado.\n\n";

const THIN_EVIDENCE: &str = "> Ha funcionado más de lo que ha fallado, pero se ejecutó demasiado \
     pocas veces como para prometer nada. Verificá los pasos y reportá el \
     resultado — con unas pocas ejecuciones más, esto se vuelve fiable.\n\n";

fn verdict(successes: i64, failures: i64) -> (&'static str, Option<&'static str>) {
    let total = successes + failures;
    if total == 0 {
        return ("Sin historial", Some(NO_HISTORY));
    }
    if receta::wilson_lower_bound(successes, failures) >= TRUSTED {
        ("Probado", None)
    } else if (successes as f64) / (total as f64) < 0.5 {
        ("POCO FIABLE", Some(UNRELIABLE))
    } else {
        ("Sin evidencia suficiente", Some(THIN_EVIDENCE))
    }
}

fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "procedimiento".to_string()
    } else {
        trimmed
    }
}

fn yaml_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut dir: Option<String> = None;
    let mut min_reliability = 0.0_f64;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--min-reliability" => {
                min_reliability = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--min-reliability needs a float in [0,1]")?
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry skills <dir> [--min-reliability 0.0]\n\n\
                     Exports procedural memory as Claude Code Skills — one folder with a\n\
                     SKILL.md each. The agent loads them lazily: it sees the name and the\n\
                     description, and reads the body only when the task matches.\n\n\
                     Each Skill carries its own track record, so the agent knows what it is\n\
                     trusting. Typical target: ~/.claude/skills"
                );
                return Ok(());
            }
            other => dir = Some(other.to_string()),
        }
    }
    let Some(dir) = dir else {
        anyhow::bail!("falta el directorio — uso: memory-industry skills ~/.claude/skills");
    };

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for skills export")?;

    let procedures = receta::all_for_export(&pool).await?;
    if procedures.is_empty() {
        println!("No hay procedimientos todavía.");
        println!("Guardá el primero con la tool cuba_receta (action=add), o dejá que el agente");
        println!("lo haga la próxima vez que descubra cómo se levanta algo.");
        return Ok(());
    }

    let root = std::path::Path::new(&dir);
    let mut written = 0usize;
    let mut skipped = 0usize;

    for p in &procedures {
        let reliability = p["reliability"].as_f64().unwrap_or(0.0);
        if reliability < min_reliability {
            skipped += 1;
            continue;
        }
        let name = p["name"].as_str().unwrap_or("procedimiento");
        let trigger = p["trigger"].as_str().unwrap_or("");
        let markdown = p["markdown"].as_str().unwrap_or("");
        let preconditions = p["preconditions"].as_str().unwrap_or("");
        let verification = p["verification"].as_str().unwrap_or("");
        let successes = p["successes"].as_i64().unwrap_or(0);
        let failures = p["failures"].as_i64().unwrap_or(0);

        let folder = root.join(slug(name));
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("no se pudo crear {}", folder.display()))?;

        let description = if trigger.is_empty() {
            format!("Cómo: {name}.")
        } else {
            format!("Usar {trigger}. Procedimiento verificado: {name}.")
        };

        let mut body = String::new();
        body.push_str("---\n");
        body.push_str(&format!("name: {}\n", yaml_quote(name)));
        body.push_str(&format!("description: {}\n", yaml_quote(&description)));
        body.push_str("---\n\n");
        body.push_str(&format!("# {name}\n\n"));

        let total = successes + failures;
        let (mark, caveat) = verdict(successes, failures);
        if total > 0 {
            let pct = successes as f64 / total as f64 * 100.0;
            body.push_str(&format!(
                "> **{mark}: {successes} de {total} veces** ({pct:.0}%, fiabilidad {reliability:.2}).\n\n"
            ));
        }
        if let Some(c) = caveat {
            body.push_str(c);
        }

        if !preconditions.is_empty() {
            body.push_str(&format!("## Antes de empezar\n\n{preconditions}\n\n"));
        }
        body.push_str(&format!("## Pasos\n\n{markdown}\n"));
        if !verification.is_empty() {
            body.push_str(&format!("## Cómo saber que funcionó\n\n{verification}\n\n"));
        }
        body.push_str(
            "---\n_Generado desde la memoria procedimental de MemoryIndustry. \
             Después de ejecutarlo, reportá el resultado con `cuba_receta action=outcome` — \
             si no, la memoria no aprende nada._\n",
        );

        std::fs::write(folder.join("SKILL.md"), body)
            .with_context(|| format!("no se pudo escribir la Skill de «{name}»"))?;
        written += 1;
    }

    println!("{written} Skill(s) escritas en {dir}");
    if skipped > 0 {
        println!("{skipped} omitida(s) por fiabilidad < {min_reliability:.2}");
    }
    println!("\nEl agente las carga solas: ve el nombre y la descripción, y lee el cuerpo");
    println!("solo cuando la tarea coincide. No gastan contexto hasta que hacen falta.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_survive_real_names() {
        assert_eq!(
            slug("Levantar el entorno de desarrollo"),
            "levantar-el-entorno-de-desarrollo"
        );
        assert_eq!(slug("migrar a bge-m3 (1024-d)"), "migrar-a-bge-m3-1024-d");
        assert_eq!(slug("   "), "procedimiento");
        assert_eq!(slug("¿cómo?"), "c-mo");
    }

    #[test]
    fn low_confidence_is_not_the_same_as_a_bad_procedure() {
        assert_eq!(
            verdict(47, 3),
            ("Probado", None),
            "47 de 50 clears the Wilson floor: the only case the Skill may advertise \
             without a caveat, because the agent will follow it unquestioned"
        );

        assert_eq!(
            verdict(3, 1).0,
            "Sin evidencia suficiente",
            "3 de 4 works more often than not, so calling it unreliable would bury a \
             procedure that is probably fine — it just has not run enough times"
        );

        assert_eq!(
            verdict(1, 9).0,
            "POCO FIABLE",
            "1 de 10 fails far more than it works: the agent must be told to treat it \
             as a hypothesis instead of running it"
        );

        assert_eq!(
            verdict(4, 6).0,
            "POCO FIABLE",
            "the cut is rate < 0.5 and 4 de 10 falls on the bad side of it; moving that \
             threshold to 0.4 would silently promote this procedure to «sin evidencia \
             suficiente» and hide that it fails more than it works"
        );
        assert_eq!(
            verdict(5, 5).0,
            "Sin evidencia suficiente",
            "5 de 10 is the other side of the same cut: a coin flip is not «poco fiable», \
             it is unproven"
        );

        assert_eq!(
            verdict(0, 0).0,
            "Sin historial",
            "never executed is not the same as failing: wilson_lower_bound returns 0.0 \
             for both, so the branch order is what keeps them apart"
        );

        for (s, f) in [(3, 1), (1, 9), (4, 6), (5, 5), (0, 0)] {
            assert!(
                verdict(s, f).1.is_some(),
                "{s}/{f} is not trustworthy, so its Skill must carry the caveat that says \
                 so — without it the agent reads the procedure as verified"
            );
        }
    }

    #[test]
    fn frontmatter_cannot_be_broken_by_a_procedure_name() {
        assert_eq!(yaml_quote("simple"), "\"simple\"");
        assert_eq!(yaml_quote("con \"comillas\""), "\"con \\\"comillas\\\"\"");
        assert_eq!(yaml_quote("dos\nlíneas"), "\"dos líneas\"");
    }
}
