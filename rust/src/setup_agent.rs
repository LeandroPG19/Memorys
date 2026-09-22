use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::envs::home;
use crate::service::{self, Asked, Mode, Request, Secrecy, Unit};

const REQUIRED_ENV: [&str; 3] = ["DATABASE_URL", "ONNX_MODEL_PATH", "ORT_DYLIB_PATH"];

const MUST_AGREE: [&str; 2] = ["CUBA_EMBEDDING_DIM", "CUBA_EMBED_MODEL"];

fn known_targets() -> Result<Vec<(&'static str, PathBuf)>> {
    let home = home()?;
    Ok(vec![
        ("claude", home.join(".claude.json")),
        ("mcp", home.join(".mcp.json")),
        ("cursor", home.join(".cursor").join("mcp.json")),
        ("warp", home.join(".warp").join(".mcp.json")),
    ])
}

fn project_configs() -> Result<Vec<(String, PathBuf)>> {
    let home = home()?;
    let mut out = Vec::new();
    let roots = [home.join("proyectos"), home.join("projects")];

    for root in roots.iter().filter(|r| r.is_dir()) {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten().take(200) {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let mut candidates = vec![dir.join(".mcp.json")];
            if let Ok(subs) = std::fs::read_dir(&dir) {
                for sub in subs.flatten().take(100) {
                    if sub.path().is_dir() {
                        candidates.push(sub.path().join(".mcp.json"));
                    }
                }
            }
            for c in candidates {
                if c.is_file() && read_json(&c).as_ref().and_then(cuba_block).is_some() {
                    let label = c.parent().and_then(|p| p.file_name()).map_or_else(
                        || "proyecto".to_string(),
                        |n| n.to_string_lossy().to_string(),
                    );
                    out.push((label, c));
                }
            }
        }
    }
    Ok(out)
}

fn desired_config() -> Result<Value> {
    let exe = std::env::current_exe().context("no se pudo resolver la ruta del binario")?;

    let db = std::env::var("DATABASE_URL").unwrap_or_default();
    let onnx = match std::env::var("ONNX_MODEL_PATH") {
        Ok(v) => v,
        Err(_) => prefer_cache_subdir("models")?.display().to_string(),
    };
    let ort = match std::env::var("ORT_DYLIB_PATH") {
        Ok(v) => v,
        Err(_) => prefer_cache_subdir("onnxruntime")?
            .join("libonnxruntime.so")
            .display()
            .to_string(),
    };

    let mut env = json!({
        "DATABASE_URL": db,
        "ONNX_MODEL_PATH": onnx,
        "ORT_DYLIB_PATH": ort,
    });
    if let Some(id) = workspace_client_id() {
        env["MEMORY_INDUSTRY_CLIENT_ID"] = json!(id);
    }

    Ok(json!({
        "command": exe.display().to_string(),
        "args": [],
        "env": env
    }))
}

fn workspace_client_id() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    if !(cwd.join(".git").exists()
        || cwd.join("Cargo.toml").exists()
        || cwd.join(".mcp.json").exists())
    {
        return None;
    }
    let name = cwd.file_name()?.to_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

fn prefer_cache_subdir(subdir: &str) -> Result<PathBuf> {
    let home = home()?;
    let preferred = home.join(".cache/memory-industry").join(subdir);
    let legacy = home.join(".cache/cuba-memorys").join(subdir);
    Ok(if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    })
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

const MCP_SERVER_KEY: &str = "memory-industry";
const MCP_SERVER_KEY_LEGACY: &str = "cuba-memorys";

fn http_config_problem(block: &Value) -> Option<String> {
    let url = block.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return None;
    }
    let headers = block.get("headers").and_then(Value::as_object);
    let has_id = headers.is_some_and(|h| {
        h.get("Mcp-Client-Id")
            .or_else(|| h.get("mcp-client-id"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    });
    if !has_id {
        return Some(
            "HTTP sin header Mcp-Client-Id — todos los workspaces compartirían la misma jornada"
                .into(),
        );
    }
    None
}

fn cuba_block(cfg: &Value) -> Option<&Value> {
    let servers = cfg.get("mcpServers")?;
    servers
        .get(MCP_SERVER_KEY)
        .or_else(|| servers.get(MCP_SERVER_KEY_LEGACY))
}

fn run_check() -> Result<()> {
    println!("Auditoría de configuraciones de cliente MCP\n");

    let mut found = 0;
    let mut problems = 0;
    let mut seen: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();

    let targets: Vec<(String, PathBuf)> = known_targets()?
        .into_iter()
        .map(|(n, p)| (n.to_string(), p))
        .chain(project_configs()?)
        .collect();

    for (name, path) in targets {
        let Some(cfg) = read_json(&path) else {
            continue;
        };
        let Some(block) = cuba_block(&cfg) else {
            continue;
        };
        found += 1;

        println!("── {name}  ({})", path.display());
        let command = block.get("command").and_then(Value::as_str).unwrap_or("");
        println!("   command: {command}");
        if let Some(why) = http_config_problem(block) {
            println!("   PROBLEMA: {why}");
            problems += 1;
        }
        if !command.is_empty() && !Path::new(command).exists() {
            println!("   PROBLEMA: ese binario no existe");
            problems += 1;
        }

        let env = block.get("env").and_then(Value::as_object);
        for key in REQUIRED_ENV {
            match env.and_then(|e| e.get(key)).and_then(Value::as_str) {
                Some(v) if !v.is_empty() => {
                    seen.entry(key.to_string())
                        .or_default()
                        .insert(v.to_string());
                    if key != "DATABASE_URL" && !Path::new(v).exists() {
                        println!("   PROBLEMA: {key} apunta a una ruta inexistente ({v})");
                        problems += 1;
                    } else {
                        println!("   {key}: ok");
                    }
                }
                _ => {
                    println!("   PROBLEMA: falta {key}");
                    if key == "ONNX_MODEL_PATH" || key == "ORT_DYLIB_PATH" {
                        println!(
                            "     → sin esto la rama vectorial devuelve vacío EN SILENCIO: \
                             la búsqueda queda solo léxica"
                        );
                    }
                    problems += 1;
                }
            }
        }

        for key in MUST_AGREE {
            let value = env
                .and_then(|e| e.get(key))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map_or_else(
                    || match key {
                        "CUBA_EMBEDDING_DIM" => "384 (default)".to_string(),
                        _ => "multilingual-e5-small (default)".to_string(),
                    },
                    std::string::ToString::to_string,
                );
            println!("   {key}: {value}");
            seen.entry(key.to_string()).or_default().insert(value);
        }
        println!();
    }

    if found == 0 {
        println!(
            "No encontré ninguna config con un bloque «memory-industry» (ni el legado «cuba-memorys»)."
        );
        println!("Generá una con:  memory-industry setup print");
        return Ok(());
    }

    for (key, values) in &seen {
        if values.len() > 1 {
            println!("DIVERGENCIA en {key}: los clientes no coinciden");
            for v in values {
                println!("   - {v}");
            }
            if key == "CUBA_EMBEDDING_DIM" || key == "CUBA_EMBED_MODEL" {
                println!(
                    "   → GRAVE. Los clientes hablan con la MISMA base con modelos de dimensiones\n\
                     \x20    distintas. El que no coincida con la columna no puede comparar vectores:\n\
                     \x20    su búsqueda híbrida se vuelve LÉXICA sin avisar, y devuelve peores\n\
                     \x20    resultados con la misma confianza. Alineá todas las configs.\n"
                );
            } else {
                println!(
                    "   → los procesos MCP se comportan distinto según quién los lance. \
                     Esto es exactamente el bug que mató el recall vectorial.\n"
                );
            }
            problems += 1;
        }
    }

    if problems == 0 {
        println!("{found} config(s) revisada(s): todas completas y coherentes entre sí.");
    } else {
        println!("{found} config(s) revisada(s): {problems} problema(s).");
        println!("Arreglalo con:  memory-industry setup <cliente> --apply");
    }
    Ok(())
}

fn run_write(target: &str, apply: bool) -> Result<()> {
    let path = known_targets()?
        .into_iter()
        .find(|(n, _)| *n == target)
        .map(|(_, p)| p)
        .with_context(|| format!("cliente desconocido: {target} (claude | mcp | cursor)"))?;

    let desired = desired_config()?;

    if let Some(env) = desired.get("env").and_then(Value::as_object) {
        for key in ["ONNX_MODEL_PATH", "ORT_DYLIB_PATH"] {
            if let Some(v) = env.get(key).and_then(Value::as_str)
                && !Path::new(v).exists()
            {
                println!("AVISO: {key} apunta a {v}, que no existe todavía.");
                println!("       El servidor arrancará, pero sin búsqueda vectorial.\n");
            }
        }
    }

    println!("Se escribiría en {}:\n", path.display());
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({ "mcpServers": { MCP_SERVER_KEY: desired } }))?
    );
    println!();
    println!(
        "Si el cliente es HTTP (`serve`): url http://127.0.0.1:8787/mcp y un header \
         Mcp-Client-Id distinto por workspace."
    );
    println!();

    if !apply {
        println!("Esto fue un plan — no se tocó ningún archivo.");
        println!("Para aplicarlo:  memory-industry setup {target} --apply");
        return Ok(());
    }

    let mut cfg = read_json(&path).unwrap_or_else(|| json!({}));
    if !cfg.is_object() {
        bail!("{} no contiene un objeto JSON en la raíz", path.display());
    }

    if path.exists() {
        let backup = path.with_extension(format!(
            "json.bak-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::copy(&path, &backup)
            .with_context(|| format!("no se pudo respaldar {}", path.display()))?;
        println!("Backup: {}", backup.display());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    cfg.as_object_mut()
        .expect("checked above")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    cfg["mcpServers"][MCP_SERVER_KEY] = desired;
    if let Some(servers) = cfg["mcpServers"].as_object_mut() {
        servers.remove(MCP_SERVER_KEY_LEGACY);
    }

    std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)
        .with_context(|| format!("no se pudo escribir {}", path.display()))?;

    println!("Escrito en {}", path.display());
    println!("Reiniciá el cliente MCP para que tome el binario y el entorno nuevos.");
    Ok(())
}

pub fn run_cli(args: &[String]) -> Result<()> {
    // Before the loop below, which would take every one of `service`'s own
    // flags for a target. It stays a subcommand of `setup` rather than a new
    // entry in `cli::COMMANDS`, because the README states that count and
    // `cli_contract.rs` checks the two agree.
    if args.first().is_some_and(|a| a == "service") {
        return run_service(&args[1..]);
    }

    let mut target: Option<String> = None;
    let mut apply = false;

    for a in args {
        match a.as_str() {
            "--apply" => apply = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry setup <check | print | service | claude | mcp | cursor> [--apply]\n\n\
                     check   audita las configs existentes: variables faltantes, rutas muertas,\n\
                             y divergencias entre clientes (el bug que mató el recall vectorial).\n\
                     print   imprime el bloque correcto para pegarlo donde haga falta.\n\
                     hook    instala un SessionStart que inyecta la memoria automáticamente.\n\
                     service instala el daemon: unidad systemd o tarea de Windows, con el env\n\
                             file que lleva el token. `setup service --help` para sus flags.\n\
                     claude  ~/.claude.json     mcp  ~/.mcp.json     cursor  ~/.cursor/mcp.json\n\n\
                     Sin --apply, solo muestra el plan. Con --apply, respalda el archivo y mergea."
                );
                return Ok(());
            }
            other => target = Some(other.to_string()),
        }
    }

    match target.as_deref() {
        Some("hook") => run_hook(apply),
        Some("check") | None => run_check(),
        Some("print") => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({ "mcpServers": { MCP_SERVER_KEY: desired_config()? } })
                )?
            );
            Ok(())
        }
        Some(t) => run_write(t, apply),
    }
}

const SERVICE_HELP: &str = "\
usage: memory-industry setup service [--print | --apply | --uninstall]
                                     [--linux | --windows]
                                     [--profile loopback|lan]
                                     [--token T] [--addr A] [--out DIR]

Sin modo, --print: muestra el plan y no escribe nada.

--out DIR    escribe el render canónico en DIR en vez de a stdout. Es como se
             regeneran los ficheros de packaging/, y solo va con --print.
--apply      instala. NO pisa un env file que ya exista: lleva el token y la
             DATABASE_URL que tus clientes ya usan.
--uninstall  retira lo que generó y deja el env file donde está.
--profile lan exige --addr con la dirección concreta de la interfaz, y genera
             un token si no le pasás uno.";

/// `setup service`: the flags say what, and the four functions below do it.
///
/// The deciding half — which flags are exclusive, what each refusal says, what
/// gets written and what is kept — is in `service.rs`, because
/// `quality-gate.sh:164` keeps this file out of `cargo mutants` and a decision
/// here has no judge. What is left here is the printing and the filesystem.
fn run_service(args: &[String]) -> Result<()> {
    let request = match service::parse_request(args)? {
        Asked::Help => {
            eprintln!("{SERVICE_HELP}");
            return Ok(());
        }
        Asked::Run(request) => request,
    };

    // `--out` reaches here only with `--print`, because `parse_request` refuses
    // the other two pairs. There is no fifth arm for a mode nothing produces.
    match (request.mode, request.out.as_deref()) {
        (Mode::Print, Some(dir)) => write_canonical_render(&request, dir),
        (Mode::Print, None) => print_plan(&request),
        (Mode::Apply, _) => install(&request),
        (Mode::Uninstall, _) => uninstall(&request),
    }
}

/// This machine and these flags, which is what `--print` and `--apply` install.
fn unit_from(request: &Request) -> Result<Unit> {
    Unit::from_env(
        request.profile,
        request.target,
        request.addr.as_deref(),
        request.token.as_deref(),
    )
}

/// `--print --out DIR`: the canonical render, never this machine's. A
/// measurement or a `current_exe()` inside a versioned file is the very defect
/// this command exists to close.
fn write_canonical_render(request: &Request, dir: &Path) -> Result<()> {
    let unit = Unit::documented(request.profile, request.target);
    let launcher = unit.exe.with_extension("cmd");
    for (relative, body) in service::rendered_files(&unit, request.target.is_windows(), &launcher) {
        let path = dir.join(&relative);
        service::write_one(&path, &body, Secrecy::Plain)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn print_plan(request: &Request) -> Result<()> {
    let unit = unit_from(request)?;
    let root = install_root_of(&unit);
    let launcher = root.join(service::LAUNCHER_BASENAME);
    print!("{}", service::render_plan(&unit, request.target, &launcher));
    Ok(())
}

/// `--apply`. Named for what the help text calls it («--apply instala»), and
/// not `apply`: `run_write` and `run_hook` both take an `apply: bool`, and a
/// function with that name would be shadowed by the parameter inside them.
fn install(request: &Request) -> Result<()> {
    let unit = unit_from(request)?;
    let root = install_root_of(&unit);
    for (path, wrote) in service::write_all(&unit, request.target, &root)? {
        println!("{}", service::wrote_line(&path, wrote));
    }
    println!();
    println!(
        "Habilitalo con:\n  {}",
        service::enable_command(request.target, &root)
    );
    Ok(())
}

fn uninstall(request: &Request) -> Result<()> {
    let home = crate::envs::home()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let local_app_data = std::env::var("LOCALAPPDATA").ok();
    let root = service::install_root(request.target, home.as_deref(), local_app_data.as_deref())?;

    let unit = Unit::documented(request.profile, request.target);
    for path in service::removable_files(&unit, request.target, &root) {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("borrado    {}", path.display()),
            // A file that was never there is not a failure: `--uninstall` after
            // a plan is the operator checking, and an error would read as
            // though the install were damaged.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("no estaba  {}", path.display());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("no se pudo borrar {}", path.display()));
            }
        }
    }
    println!();
    println!(
        "{} sigue ahí: lleva el token y la DATABASE_URL. Borrarlo es un acto aparte.",
        root.join(service::ENV_BASENAME).display()
    );
    Ok(())
}

/// Where this unit was resolved to install. `from_env` already put the env file
/// under the install root, so asking it back beats resolving the root twice and
/// risking two answers.
fn install_root_of(unit: &Unit) -> PathBuf {
    unit.env_file
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

fn run_hook(apply: bool) -> Result<()> {
    let exe = std::env::current_exe().context("no se pudo resolver la ruta del binario")?;
    let path = home()?.join(".claude").join("settings.json");

    let command = format!("{} recall --quiet", exe.display());
    let hook = json!({
        "matcher": "*",
        "hooks": [{ "type": "command", "command": command }]
    });

    println!("Se añadiría a {} un hook SessionStart:\n", path.display());
    println!("{}\n", serde_json::to_string_pretty(&hook)?);
    println!("Inyecta la última sesión, los errores sin resolver y las decisiones ya tomadas");
    println!("— unos 300 tokens — antes de que el agente escriba nada.\n");

    if !apply {
        println!("Esto fue un plan — no se tocó ningún archivo.");
        println!("Para aplicarlo:  memory-industry setup hook --apply");
        return Ok(());
    }

    let mut cfg = read_json(&path).unwrap_or_else(|| json!({}));
    if !cfg.is_object() {
        bail!("{} no contiene un objeto JSON en la raíz", path.display());
    }

    if path.exists() {
        let backup = path.with_extension(format!(
            "json.bak-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::copy(&path, &backup)
            .with_context(|| format!("no se pudo respaldar {}", path.display()))?;
        println!("Backup: {}", backup.display());
    }

    let obj = cfg.as_object_mut().expect("checked above");
    obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = cfg["hooks"]
        .as_object_mut()
        .context("hooks no es un objeto")?;
    hooks.entry("SessionStart").or_insert_with(|| json!([]));
    let list = cfg["hooks"]["SessionStart"]
        .as_array_mut()
        .context("SessionStart no es una lista")?;

    let already = list.iter().any(|h| {
        h.get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner.iter().any(|i| {
                    i.get("command").and_then(Value::as_str).is_some_and(|c| {
                        c.contains("recall")
                            && (c.contains("memory-industry") || c.contains("cuba-memorys"))
                    })
                })
            })
    });
    if already {
        println!("El hook ya estaba instalado — no se duplicó.");
        return Ok(());
    }

    list.push(hook);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)
        .with_context(|| format!("no se pudo escribir {}", path.display()))?;

    println!("Instalado. La próxima sesión arrancará con la memoria ya cargada,");
    println!("obedezca el modelo su CLAUDE.md o no.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envs::ScopedEnv;

    #[tokio::test]
    async fn the_claude_config_goes_under_the_resolved_home_not_beside_the_operator() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        // A `C:\…` literal would make the `is_absolute` assertion below vacuous
        // on the Linux runner; `temp_dir` is absolute on both. Never created:
        // `known_targets` only joins.
        let root = std::env::temp_dir().join(format!("memory-industry-{}", std::process::id()));
        let _h = ScopedEnv::cleared("HOME");
        let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());

        let (_, claude) = known_targets()
            .expect("USERPROFILE answers")
            .into_iter()
            .find(|(n, _)| *n == "claude")
            .expect("claude is one of the known targets");

        assert_eq!(
            claude,
            root.join(".claude.json"),
            "this is the path `setup claude --apply` writes. Relative to the working \
             directory it is a file the client never reads"
        );
        assert!(
            claude.is_absolute(),
            "a relative target means the config lands wherever the operator was standing"
        );
    }

    #[tokio::test]
    async fn the_config_carries_the_vars_whose_absence_is_silent() {
        // Takes the guard because `desired_config` now resolves the home, and
        // the `envs` home tests clear HOME and USERPROFILE for the whole process.
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let cfg = desired_config().expect("current_exe resolves under test");
        let env = cfg
            .get("env")
            .and_then(Value::as_object)
            .expect("env block");
        for key in REQUIRED_ENV {
            assert!(env.contains_key(key), "falta {key} en el bloque generado");
        }
        let command = cfg.get("command").and_then(Value::as_str).unwrap_or("");
        assert!(
            Path::new(command).is_absolute(),
            "el command debe ser absoluto"
        );
    }

    #[test]
    fn an_absent_dimension_is_a_value_that_can_disagree() {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();

        seen.insert("1024".to_string());
        seen.insert("384 (default)".to_string());

        assert_eq!(
            seen.len(),
            2,
            "una config sin la var diverge de una que la fija"
        );
    }

    #[test]
    fn the_vars_that_must_agree_include_the_dimension() {
        assert!(MUST_AGREE.contains(&"CUBA_EMBEDDING_DIM"));
        assert!(MUST_AGREE.contains(&"CUBA_EMBED_MODEL"));
    }

    #[test]
    fn an_http_client_without_its_own_id_is_a_problem() {
        // The port assertion that used to live here said `:8788` is Cursor's
        // OAuth callback and MemoryIndustry belongs on 8787. That is one
        // workstation's Cursor install, and it is backwards on the deployment
        // this ships to, where the daemon runs on 8787. A port belongs to a
        // machine; `doctor` now reports whether the address is actually free.
        let no_id = json!({"url": "http://127.0.0.1:8787/mcp"});
        assert!(
            http_config_problem(&no_id)
                .expect("missing client id")
                .contains("Mcp-Client-Id")
        );
        let ok = json!({
            "url": "http://127.0.0.1:8787/mcp",
            "headers": {"Mcp-Client-Id": "Memorys"}
        });
        assert_eq!(http_config_problem(&ok), None);
        assert_eq!(http_config_problem(&json!({"command": "/bin/x"})), None);
    }

    #[test]
    fn finds_the_cuba_block_only_when_present() {
        let with = json!({"mcpServers": {"memory-industry": {"command": "/bin/x"}}});
        let legacy = json!({"mcpServers": {"cuba-memorys": {"command": "/bin/x"}}});
        let without = json!({"mcpServers": {"otro": {"command": "/bin/y"}}});
        assert!(cuba_block(&with).is_some());
        assert!(cuba_block(&legacy).is_some());
        assert!(cuba_block(&without).is_none());
        assert!(cuba_block(&json!({})).is_none());
    }
}
