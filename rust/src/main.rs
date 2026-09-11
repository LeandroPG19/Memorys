use std::time::Duration;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

async fn drain_then_report(result: anyhow::Result<()>, command: &str) {
    drain_background_tasks().await;
    if let Err(e) = result {
        tracing::error!(error = %format!("{e:#}"), command, "command failed");
        eprintln!("{command} error: {e:#}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "MemoryIndustry {version} — knowledge-graph memory server (MCP)

USAGE:
  memory-industry                  run the MCP server on stdio (how an MCP client launches it)
  memory-industry serve [addr]     run one shared daemon over HTTP for every client
  memory-industry <command> [args]

  (deprecated alias for this release: cuba-memorys — same binary)

ONE PROCESS FOR EVERY CLIENT:
  serve             stdio gives each client its own process, and each process its
                    own ~6 GB of ONNX models. `serve` loads them once and answers
                    every editor window over loopback HTTP (default 127.0.0.1:8787).
                    Point clients at http://127.0.0.1:8787/mcp with \"type\": \"http\",
                    and give each one an Mcp-Client-Id header so their sessions
                    stay separate. CUBA_HTTP_ADDR overrides the address;
                    CUBA_HTTP_TOKEN requires a bearer token.
  tunnel            publish that daemon through a Cloudflare quick tunnel and print
                    the client config, so Claude on the web can reach it. Requires
                    CUBA_HTTP_TOKEN: a tunnel puts the whole graph on the internet.

THE BRAIN, WITHOUT AN LLM IN BETWEEN:
  search <query>    hybrid search (use --format verbose for the score breakdown)
  save <content>    write an observation
  delete <id>       remove an observation
  export <dir>      write the graph as an Obsidian vault (refuses a non-empty dir
                    without --apply: the index is README.md)
  dashboard         what is in there, at a glance

OPERATIONS:
  doctor            health check: schema, embedding dim, config coherence, stale processes
  recall            the session-start context injection (wire it with `setup hook`)
  reembed           recompute every embedding — after changing model or dimension
  calibrate         recompute the abstention threshold from the live corpus
  link              auto-link entities by NPMI co-occurrence
  dedupe            find entities that are the same thing under different names
  skills <dir>      export procedures as Claude Code skills
  eval              retrieval benchmark (nDCG@10, MRR, recall) — read-only
  sync              git-friendly export/import of the graph (export|import|diff|status)
  hook install      wire git so sync export/import run on commit/checkout automatically
  codegraph build   parse source (tree-sitter, rust|python) into brain_entities/relations
  rem               run one consolidation cycle now (decay, autolink, backfill, PageRank)
  setup             wire this server into your MCP clients; `setup check` audits them
  models            download the embedding, NLI and reranker models and the ONNX runtime
  llm               pick a chat model the easy way (DeepSeek/Qwen/Ollama/…); saves config
  graph             optional FalkorDB/Neo4j projection status|reconcile (Postgres stays SoT)
  secure            create the non-superuser cuba_app role so RLS and the audit trigger bite

  -h, --help        this
  -V, --version     print the version and exit — touches no database

DATABASE_URL points at the brain. `doctor` will tell you if anything is off.
Docs: https://github.com/LeandroPG19/cuba-memorys",
        version = env!("CARGO_PKG_VERSION")
    );
}

async fn drain_background_tasks() {
    let lost = memory_industry::tasks::drain(DRAIN_TIMEOUT).await;
    if lost > 0 {
        tracing::error!(
            lost,
            "background tasks did not finish before shutdown — some embeddings \
             were not persisted; recover with cuba_zafra action=reembed"
        );
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "memory_industry=info".parse().unwrap()),
        )
        .init();

    let machine = memory_industry::resources::probe();
    let plan = memory_industry::resources::plan(&machine);
    tracing::info!(
        ram_total_mb = machine.ram_total_mb,
        ram_available_mb = machine.ram_available_mb,
        cgroup_limit_mb = machine.cgroup_limit_mb,
        swap_total_mb = machine.swap_total_mb,
        cores_logical = machine.cores_logical,
        cores_physical = machine.cores_physical,
        vram_free_mb = machine.vram_free_mb,
        plan = %plan.describe(),
        "resource plan"
    );
    memory_industry::resources::apply(&plan);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(plan.worker_threads)
        .max_blocking_threads(plan.max_blocking_threads)
        .enable_all()
        .build()
        .expect("building the tokio runtime");

    runtime.block_on(async_main());
}

async fn async_main() {
    // Saved by `memory-industry llm set` — easiest path for humans and agents.
    memory_industry::llm_cli::load_saved_config_into_env();
    memory_industry::graph_db::load_saved_config_into_env();

    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("eval") => {
            if let Err(e) = memory_industry::eval::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "eval failed");
                eprintln!("eval error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("skills") => {
            if let Err(e) = memory_industry::skills_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "skills failed");
                eprintln!("skills error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("reembed") => {
            let result = memory_industry::reembed_cli::run_cli(&argv[2..]).await;
            drain_then_report(result, "reembed").await;
            return;
        }
        Some("recall") => {
            if let Err(e) = memory_industry::recall_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "recall failed");
                eprintln!("recall error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("models") => {
            if let Err(e) = memory_industry::models_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "models failed");
                eprintln!("models: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("llm") => {
            if let Err(e) = memory_industry::llm_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "llm failed");
                eprintln!("llm: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("graph") => {
            if let Err(e) = memory_industry::graph_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "graph failed");
                eprintln!("graph: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("link") => {
            if let Err(e) = memory_industry::link_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "link failed");
                eprintln!("link error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("calibrate") => {
            if let Err(e) = memory_industry::calibrate_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "calibrate failed");
                eprintln!("calibrate error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("dedupe") => {
            if let Err(e) = memory_industry::dedupe_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "dedupe failed");
                eprintln!("dedupe error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("sync") => {
            if let Err(e) = memory_industry::sync_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "sync failed");
                eprintln!("sync error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("rem") => {
            let result = memory_industry::rem_cli::run_cli(&argv[2..]).await;
            drain_then_report(result, "rem").await;
            return;
        }
        Some("codegraph") => {
            if let Err(e) = memory_industry::codegraph_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "codegraph failed");
                eprintln!("codegraph error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("hook") => {
            if let Err(e) = memory_industry::hooks_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "hook failed");
                eprintln!("hook error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("secure") => {
            if let Err(e) = memory_industry::secure_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "secure failed");
                eprintln!("secure: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("doctor") => {
            if let Err(e) = memory_industry::doctor::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "doctor failed");
                eprintln!("doctor error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some(cmd @ ("search" | "save" | "delete" | "export" | "dashboard")) => {
            let rest = &argv[2..];
            let result = match cmd {
                "search" => memory_industry::cli::run_search(rest).await,
                "save" => memory_industry::cli::run_save(rest).await,
                "delete" => memory_industry::cli::run_delete(rest).await,
                "dashboard" => memory_industry::dashboard::run_cli(rest).await,
                _ => memory_industry::export::run_cli(rest).await,
            };
            drain_then_report(result, cmd).await;
            return;
        }
        Some("setup") => {
            if let Err(e) = memory_industry::setup_agent::run_cli(&argv[2..]) {
                tracing::error!(error = %format!("{e:#}"), "setup failed");
                eprintln!("setup: {e:#}");
                std::process::exit(1);
            }
            return;
        }

        Some("tunnel") => {
            if let Err(e) = memory_industry::tunnel_cli::run_cli(&argv[2..]).await {
                tracing::error!(error = %format!("{e:#}"), "tunnel failed");
                eprintln!("tunnel error: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("serve") => {
            let addr = argv
                .get(2)
                .cloned()
                .unwrap_or_else(memory_industry::http::bind_addr);
            let outcome = memory_industry::http::serve(&addr).await;
            drain_background_tasks().await;
            if let Err(e) = outcome {
                tracing::error!(error = %format!("{e:#}"), "daemon failed");
                eprintln!("serve: {e:#}");
                std::process::exit(1);
            }
            return;
        }

        Some("--version" | "-V" | "version") => {
            println!("memory-industry {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            return;
        }

        Some(unknown) => {
            eprintln!(
                "memory-industry: unknown command '{unknown}'\n\nknown commands: {}\n",
                memory_industry::cli::COMMANDS.join(", ")
            );
            print_help();
            std::process::exit(2);
        }

        None => {}
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "MemoryIndustry starting"
    );

    {
        let url = memory_industry::setup::resolve_database_url().await;
        match memory_industry::db::create_pool(&url).await {
            Ok(pool) => {
                if let Err(e) = memory_industry::db::assert_embedding_dim(&pool).await {
                    tracing::error!(error = %format!("{e:#}"), "arranque abortado");
                    eprintln!("\nMemoryIndustry NO puede arrancar:\n\n{e:#}\n");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "no se pudo verificar la dimensión del embedding al arrancar");
            }
        }
    }

    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("SIGINT received"),
            _ = terminate => tracing::info!("SIGTERM received"),
        }
    };

    tokio::select! {
        result = memory_industry::protocol::run_mcp() => {
            if let Err(e) = result {
                tracing::error!(error = %e, "MCP protocol error");
                drain_background_tasks().await;
                std::process::exit(1);
            }
        }
        _ = shutdown => {
            tracing::info!("shutting down gracefully");
        }
    }

    drain_background_tasks().await;
}
