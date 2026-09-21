//! Easy LLM setup for humans and agent installers.
//!
//!   memory-industry llm              # status + what to do next
//!   memory-industry llm list         # providers in plain language
//!   memory-industry llm set deepseek --key sk-...
//!   memory-industry llm set ollama   # local, no key
//!   memory-industry llm clear
//!
//! Saves ~/.config/memory-industry/llm.env and auto-loads it on every start.

use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::cognitive::judge::{
    OpenAiCompatJudge, generative_llm_setup_hint, llm_provider_ids, llm_provider_preset,
    resolve_offline_llm, which_in_path,
};

pub fn config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("no HOME/USERPROFILE")?;
    #[cfg(windows)]
    let base = PathBuf::from(&home)
        .join("AppData")
        .join("Roaming")
        .join("memory-industry");
    #[cfg(not(windows))]
    let base = PathBuf::from(&home).join(".config").join("memory-industry");
    Ok(base.join("llm.env"))
}

/// Load saved llm.env into the process environment (does not override vars already set).
pub fn load_saved_config_into_env() {
    let Ok(path) = config_path() else {
        return;
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"');
        if k.is_empty() {
            continue;
        }
        if std::env::var_os(k).is_none() {
            // SAFETY: single-threaded startup before tokio workers spawn MCP handlers.
            unsafe {
                std::env::set_var(k, v);
            }
        }
    }
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    match cmd {
        "status" | "" => {
            print_status().await;
            Ok(())
        }
        "list" | "providers" => {
            print_list();
            Ok(())
        }
        "set" => set_provider(&args[1..]).await,
        "clear" => clear_config(),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => bail!("unknown llm subcommand `{other}`.\n\n{}", help_text()),
    }
}

fn help_text() -> String {
    format!(
        "MemoryIndustry LLM — one command to pick any chat model (CN / US / local).\n\n\
         Easy path:\n\
           memory-industry llm list\n\
           memory-industry llm set deepseek --key YOUR_KEY\n\
           memory-industry llm set ollama\n\
           memory-industry llm status\n\n\
         That writes {} and is loaded automatically next time.\n\n\
         Or set MEMORY_INDUSTRY_LLM_BASE_URL to any OpenAI-compatible /v1 URL.\n\
         {}",
        config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "~/.config/memory-industry/llm.env".into()),
        generative_llm_setup_hint()
    )
}

fn print_help() {
    println!("{}", help_text());
}

fn print_list() {
    println!("Pick one provider (Chinese clouds, Western clouds, or local):\n");
    println!("  LOCAL (no API key)");
    println!("    ollama      http://127.0.0.1:11434/v1   — `ollama pull llama3.2`");
    println!("    lmstudio    http://127.0.0.1:1234/v1");
    println!("    vllm        http://127.0.0.1:8000/v1");
    println!();
    println!("  CHINA (OpenAI-compatible)");
    for id in [
        "deepseek",
        "qwen",
        "moonshot",
        "zhipu",
        "siliconflow",
        "yi",
        "doubao",
        "baichuan",
        "minimax",
    ] {
        if let Some(p) = llm_provider_preset(id) {
            println!("    {id:<12} default model: {}", p.default_model);
        }
    }
    println!();
    println!("  GLOBAL");
    for id in [
        "openai",
        "openrouter",
        "together",
        "groq",
        "mistral",
        "fireworks",
        "gemini_openai",
    ] {
        if let Some(p) = llm_provider_preset(id) {
            println!("    {id:<12} default model: {}", p.default_model);
        }
    }
    println!();
    println!("Then:  memory-industry llm set <name> --key <API_KEY>");
    println!("   or:  memory-industry llm set ollama");
    println!(
        "Custom URL: memory-industry llm set custom --url https://host/v1 --model NAME --key KEY"
    );
    println!();
    println!("Known ids: {}", llm_provider_ids().join(", "));
}

async fn print_status() {
    load_saved_config_into_env();
    let path = config_path().ok();
    println!("=== MemoryIndustry LLM status ===\n");
    if let Some(p) = &path {
        if p.exists() {
            println!(
                "config file: {} (loaded if env not already set)",
                p.display()
            );
        } else {
            println!(
                "config file: {} (not created yet — run `llm set`)",
                p.display()
            );
        }
    }
    let provider =
        std::env::var("MEMORY_INDUSTRY_LLM_PROVIDER").unwrap_or_else(|_| "(none)".into());
    let base = std::env::var("MEMORY_INDUSTRY_LLM_BASE_URL")
        .or_else(|_| std::env::var("CUBA_LLM_BASE_URL"))
        .unwrap_or_else(|_| "(none)".into());
    let model = std::env::var("MEMORY_INDUSTRY_LLM_MODEL")
        .or_else(|_| std::env::var("CUBA_JUEZ_MODEL"))
        .unwrap_or_else(|_| "(default)".into());
    println!("provider:    {provider}");
    println!("base_url:    {base}");
    println!("model:       {model}");
    println!(
        "api_key:     {}",
        if std::env::var("MEMORY_INDUSTRY_LLM_API_KEY").is_ok()
            || std::env::var("CUBA_LLM_API_KEY").is_ok()
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("DEEPSEEK_API_KEY").is_ok()
            || std::env::var("DASHSCOPE_API_KEY").is_ok()
        {
            "set"
        } else {
            "missing"
        }
    );
    println!(
        "claude CLI:  {}",
        if which_in_path("claude") {
            "on PATH"
        } else {
            "not found"
        }
    );
    println!(
        "gemini CLI:  {}",
        if which_in_path("gemini") {
            "on PATH"
        } else {
            "not found"
        }
    );

    match resolve_offline_llm() {
        Some(j) => {
            println!(
                "\nresolver:    OK — backend={} model={:?}",
                j.backend_name(),
                j.model_name()
            );
            if j.backend_name() == "openai_compat" {
                let probe = OpenAiCompatJudge::from_env();
                match probe.probe_health().await {
                    Ok(()) => println!("probe:       OK — endpoint answers"),
                    Err(e) => {
                        println!("probe:       FAIL — {e:#}");
                        println!("\nNext: check the API key / that the server is up.");
                    }
                }
            }
        }
        None => {
            println!("\nresolver:    NOT READY");
            println!("\nEasiest fix (pick one):");
            println!("  1) Local free:   memory-industry llm set ollama");
            println!("                   (install ollama.com then: ollama pull llama3.2)");
            println!("  2) DeepSeek:     memory-industry llm set deepseek --key sk-...");
            println!("  3) Qwen:         memory-industry llm set qwen --key sk-...");
            println!(
                "  4) Any URL:      memory-industry llm set custom --url https://…/v1 --model NAME --key KEY"
            );
            println!("  5) Claude CLI:   claude auth login");
            println!("\nThen: memory-industry llm status");
        }
    }
}

async fn set_provider(args: &[String]) -> Result<()> {
    let name = args.first().map(String::as_str).ok_or_else(|| {
        anyhow::anyhow!("usage: memory-industry llm set <provider> [--key K] [--model M] [--url U]")
    })?;

    let mut key: Option<String> = None;
    let mut model: Option<String> = None;
    let mut url: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--key" | "-k" => {
                key = args.get(i + 1).cloned();
                i += 2;
            }
            "--model" | "-m" => {
                model = args.get(i + 1).cloned();
                i += 2;
            }
            "--url" | "-u" => {
                url = args.get(i + 1).cloned();
                i += 2;
            }
            other if other.starts_with("--key=") => {
                key = Some(other.trim_start_matches("--key=").to_string());
                i += 1;
            }
            other => bail!("unknown flag `{other}`"),
        }
    }

    let preset = llm_provider_preset(name);
    if name != "custom" && preset.is_none() && url.is_none() {
        bail!(
            "unknown provider `{name}`. Run `memory-industry llm list`.\n\
             Or: memory-industry llm set custom --url https://host/v1 --model NAME --key KEY"
        );
    }

    let needs_key = !matches!(name, "ollama" | "lmstudio" | "vllm");
    if needs_key && key.as_ref().map(|k| k.is_empty()).unwrap_or(true) && url.is_none() {
        // allow if vendor env already present
        let has_env_key = preset.is_some_and(|p| {
            p.api_key_envs.iter().any(|e| std::env::var(e).is_ok())
                || std::env::var("MEMORY_INDUSTRY_LLM_API_KEY").is_ok()
        });
        if !has_env_key {
            println!(
                "Tip: pass --key YOUR_API_KEY (or export the vendor key).\n\
                 Continuing to save provider={name} without a key in the file…"
            );
        }
    }

    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }

    let mut out = String::new();
    out.push_str("# Generated by `memory-industry llm set` — edit or `llm clear`\n");
    if name != "custom" {
        out.push_str(&format!("MEMORY_INDUSTRY_LLM_PROVIDER={name}\n"));
    }
    if let Some(u) = &url {
        out.push_str(&format!(
            "MEMORY_INDUSTRY_LLM_BASE_URL={}\n",
            u.trim_end_matches('/')
        ));
    } else if let Some(p) = preset {
        out.push_str(&format!("MEMORY_INDUSTRY_LLM_BASE_URL={}\n", p.base_url));
    }
    let model = model
        .or_else(|| preset.map(|p| p.default_model.to_string()))
        .unwrap_or_else(|| "default".into());
    out.push_str(&format!("MEMORY_INDUSTRY_LLM_MODEL={model}\n"));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        out.push_str(&format!("MEMORY_INDUSTRY_LLM_API_KEY={k}\n"));
    }

    let mut f = fs::File::create(&path)?;
    f.write_all(out.as_bytes())?;
    println!("Saved {}", path.display());
    println!("Provider={name}  model={model}");
    println!();
    // Load into this process and probe.
    load_saved_config_into_env();
    // Force our new values for this process (file load skips existing env).
    unsafe {
        if name != "custom" {
            std::env::set_var("MEMORY_INDUSTRY_LLM_PROVIDER", name);
        }
        if let Some(u) = url {
            std::env::set_var("MEMORY_INDUSTRY_LLM_BASE_URL", u.trim_end_matches('/'));
        } else if let Some(p) = preset {
            std::env::set_var("MEMORY_INDUSTRY_LLM_BASE_URL", p.base_url);
        }
        std::env::set_var("MEMORY_INDUSTRY_LLM_MODEL", &model);
    }
    print_status().await;
    println!("\nDone. Agents and `merge-gate` will pick this up automatically.");
    Ok(())
}

fn clear_config() -> Result<()> {
    let path = config_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
        println!("Removed {}", path.display());
    } else {
        println!("Nothing to clear ({})", path.display());
    }
    Ok(())
}

/// What the generative model is configured as, with nothing asked of it.
///
/// `doctor_line()` cannot be reused for this: it calls `probe_health().await`
/// against the provider. `/health` is polled every few seconds by monitors
/// asking about *this daemon*, and one that waits on a stopped Ollama reports
/// the daemon as slow when the daemon is fine — the same class of lie this
/// release is removing from `doctor`, pointing the other way.
///
/// Nothing here can carry a secret: `backend` is a compile-time string,
/// `model` is the model name an operator typed, and `base_url` is redacted.
///
/// It also does not call `load_saved_config_into_env()`, and must not: that
/// function calls `set_var`, whose own safety note says it is sound only on
/// single-threaded startup before the workers exist. `main` runs it there, so
/// by the time anything asks this the saved config is already in the
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSummary {
    pub configured: bool,
    pub backend: Option<&'static str>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

pub fn configured_summary() -> LlmSummary {
    match resolve_offline_llm() {
        Some(judge) => LlmSummary {
            configured: true,
            backend: Some(judge.backend_name()),
            model: judge.model_name(),
            base_url: redacted_base_url(),
        },
        None => LlmSummary {
            configured: false,
            backend: None,
            model: None,
            base_url: None,
        },
    }
}

/// The provider URL with the credentials out — the ones `redact_url` knows
/// about and the ones it does not.
///
/// `redact_url` strips `user:pass@`, which is where a DATABASE_URL hides a
/// secret. A chat provider URL is typed by an operator and several vendors
/// document the API key as a query parameter, so everything after `?` goes
/// too. What is left — scheme, host, port, path — is the part that answers
/// "which Ollama is this talking to", which is the question that sent an
/// operator to read the source in the first place.
fn redacted_base_url() -> Option<String> {
    let raw = crate::envs::alias("MEMORY_INDUSTRY_LLM_BASE_URL", "CUBA_LLM_BASE_URL").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let without_query = raw.split_once('?').map_or(raw, |(head, _)| head);
    Some(crate::doctor::redact_url(without_query))
}

/// One-line summary for `doctor`.
/// What `doctor` should say about the generative LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmVerdict {
    /// A provider answered, or a host that can sample is attached.
    Ready,
    /// Configured to get its model through MCP sampling, inside a process that
    /// structurally cannot ask for it.
    SamplingUnreachable,
    /// Nothing configured at all.
    Missing,
}

/// Pure, because the interesting case is the one nobody can reproduce by hand.
///
/// Under the HTTP daemon `CLIENT_SUPPORTS_SAMPLING` is never set — protocol.rs
/// leaves it false on purpose — and `request_sampling_max` needs the outgoing
/// stdio channel that HTTP does not have. So a deployment that serves over
/// HTTP and sets the judge to mcp_sampling has no generative model at all, and
/// reporting one is a green over something that cannot work.
pub fn llm_verdict(offline_ready: bool, judge_is_sampling: bool, in_daemon: bool) -> LlmVerdict {
    if offline_ready {
        return LlmVerdict::Ready;
    }
    match (judge_is_sampling, in_daemon) {
        (true, false) => LlmVerdict::Ready,
        (true, true) => LlmVerdict::SamplingUnreachable,
        (false, _) => LlmVerdict::Missing,
    }
}

/// Whether the judge is set to take its model from the MCP host.
pub fn judge_is_sampling() -> bool {
    judge_is_sampling_from(
        std::env::var("MEMORY_INDUSTRY_JUDGE")
            .or_else(|_| std::env::var("CUBA_JUDGE"))
            .ok()
            .as_deref(),
    )
}

fn judge_is_sampling_from(mode: Option<&str>) -> bool {
    matches!(
        mode.unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "mcp_sampling" | "sampling"
    )
}

pub async fn doctor_line() -> (LlmVerdict, String, String) {
    load_saved_config_into_env();
    match resolve_offline_llm() {
        Some(j) => {
            if j.backend_name() == "openai_compat" {
                let probe = OpenAiCompatJudge::from_env();
                match probe.probe_health().await {
                    Ok(()) => (
                        LlmVerdict::Ready,
                        format!(
                            "OK — {} ({})",
                            j.backend_name(),
                            j.model_name().unwrap_or_default()
                        ),
                        String::new(),
                    ),
                    Err(e) => (
                        LlmVerdict::Missing,
                        format!("configured but unreachable: {e:#}"),
                        "memory-industry llm status".into(),
                    ),
                }
            } else {
                (
                    LlmVerdict::Ready,
                    format!(
                        "OK — {} ({})",
                        j.backend_name(),
                        j.model_name().unwrap_or_default()
                    ),
                    String::new(),
                )
            }
        }
        None => match llm_verdict(false, judge_is_sampling(), crate::session::daemon_mode()) {
            LlmVerdict::SamplingUnreachable => (
                LlmVerdict::SamplingUnreachable,
                "el juez está en mcp_sampling y este proceso es el daemon HTTP: el muestreo MCP necesita el canal saliente de stdio, que HTTP no tiene, así que no hay modelo generativo".into(),
                "configurá uno propio: memory-industry llm set ollama, o MEMORY_INDUSTRY_LLM_BASE_URL a cualquier /v1 compatible. Por stdio el host sí lo provee.".into(),
            ),
            verdict => (
                verdict,
                "no chat model configured".into(),
                "memory-industry llm set ollama   # or: llm set deepseek --key …".into(),
            ),
        },
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    #[test]
    fn sampling_is_not_a_generative_llm_under_the_http_daemon() {
        assert_eq!(
            llm_verdict(false, true, true),
            LlmVerdict::SamplingUnreachable,
            "a 2026-09 deployment served over HTTP, set the judge to mcp_sampling, and its technical report recorded doctor saying OK. Under the daemon the sampling flag is never set and there is no server-to-client channel, so that OK was over a model that could not be reached. This is the exact patch that must not be ported."
        );
    }

    #[test]
    fn the_other_three_answers_stay_what_they_were() {
        assert_eq!(
            llm_verdict(false, true, false),
            LlmVerdict::Ready,
            "over stdio the host really does provide the model, and refusing it there would send an operator to configure a provider they do not need"
        );
        assert_eq!(
            llm_verdict(true, false, true),
            LlmVerdict::Ready,
            "a configured provider works under the daemon like anywhere else"
        );
        assert_eq!(
            llm_verdict(true, true, true),
            LlmVerdict::Ready,
            "an unreachable sampling setting does not matter when a provider answers"
        );
        assert_eq!(llm_verdict(false, false, true), LlmVerdict::Missing);
        assert_eq!(llm_verdict(false, false, false), LlmVerdict::Missing);
    }

    #[test]
    fn only_the_sampling_judge_counts_as_sampling() {
        for yes in ["mcp_sampling", "sampling", " MCP_Sampling ", "SAMPLING"] {
            assert!(judge_is_sampling_from(Some(yes)), "{yes:?}");
        }
        for no in [
            "auto",
            "heuristic",
            "claude_cli",
            "nli",
            "",
            "mcp",
            "sampling_x",
        ] {
            assert!(
                !judge_is_sampling_from(Some(no)),
                "{no:?} is not the sampling judge, and reading it as one would make doctor warn about a deployment that is fine"
            );
        }
        assert!(
            !judge_is_sampling_from(None),
            "no judge configured is not the sampling judge either"
        );
    }
}
