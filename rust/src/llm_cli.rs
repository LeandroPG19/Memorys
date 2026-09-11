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

/// One-line summary for `doctor`.
pub async fn doctor_line() -> (bool, String, String) {
    load_saved_config_into_env();
    match resolve_offline_llm() {
        Some(j) => {
            if j.backend_name() == "openai_compat" {
                let probe = OpenAiCompatJudge::from_env();
                match probe.probe_health().await {
                    Ok(()) => (
                        true,
                        format!(
                            "OK — {} ({})",
                            j.backend_name(),
                            j.model_name().unwrap_or_default()
                        ),
                        String::new(),
                    ),
                    Err(e) => (
                        false,
                        format!("configured but unreachable: {e:#}"),
                        "memory-industry llm status".into(),
                    ),
                }
            } else {
                (
                    true,
                    format!(
                        "OK — {} ({})",
                        j.backend_name(),
                        j.model_name().unwrap_or_default()
                    ),
                    String::new(),
                )
            }
        }
        None => (
            false,
            "no chat model configured".into(),
            "memory-industry llm set ollama   # or: llm set deepseek --key …".into(),
        ),
    }
}
