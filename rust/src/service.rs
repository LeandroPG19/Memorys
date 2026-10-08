//! `memory-industry setup service` — the deployment artifacts, rendered by the
//! same binary that reads them back.
//!
//! The defect this module closes: `packaging/cuba-memorys.service:67` pinned
//! `CUBA_GPU_MEM_LIMIT_MB` to a hand-written number, and
//! `resources::set_if_absent` lets an already-set variable win, so that number
//! beat the planner's measurement on every machine with a card. A boot artifact
//! written by hand is a second source of truth for the defaults. Everything
//! under `packaging/` is now rendered from here, and the gate compares the tree
//! against this code.
//!
//! Nothing here opens a database or reads a variable that belongs to the
//! deployment surface: `cli_contract.rs` drives `--help` for every command
//! against a dead `DATABASE_URL`, and `doc_contract.rs` derives that surface
//! from `http.rs`, `gpu.rs`, `mode.rs` and `resources.rs` alone.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Where the units point the reader. `packaging/cuba-memorys.service:3` pointed
/// at `github.com/LeandroPG19/cuba-memorys`, which does not exist;
/// `the_units_point_at_the_repository_that_exists` ties this to `package.json`.
pub const REPO_URL: &str = "https://github.com/LeandroPG19/Memorys";

/// The daemon's own default, so the installer cannot drift from the address
/// `serve` binds when nobody says otherwise.
pub const DEFAULT_LOOPBACK_ADDR: &str = crate::http::DEFAULT_ADDR;

/// The canonical install, per target.
///
/// `%h` is a systemd specifier the unit expands; `%LOCALAPPDATA%` is expanded
/// by cmd.exe and by Task Scheduler. Both stay literal in the rendered file,
/// which is the only reason a byte-for-byte golden is possible at all.
const LINUX_EXE: &str = "%h/.local/bin/memory-industry";
const LINUX_ENV: &str = "%h/.config/memory-industry/memory-industry.env";
const WINDOWS_EXE: &str = "%LOCALAPPDATA%\\MemoryIndustry\\memory-industry.exe";
const WINDOWS_ENV: &str = "%LOCALAPPDATA%\\MemoryIndustry\\memory-industry.env";

/// What `documented` says instead of a measurement. `gpu::status()` answers for
/// the machine it runs on, and a versioned file cannot carry that answer.
const DOCUMENTED_WHY: &str =
    "Placement is decided by the resource planner at startup; doctor --deep prints what it chose.";

/// The basename every render writes the operator's variables to. The example
/// beside it ends in `.env.example`; this one ends in `.env` and `.gitignore`
/// covers it, because it carries the bearer token for the whole graph.
///
/// Public because `--uninstall` names this file in the line that tells the
/// operator it was left behind. A second literal there could name a file that
/// is not the one kept.
pub const ENV_BASENAME: &str = "memory-industry.env";

/// The launcher basename, used by `write_all` and by `--out` alike. One
/// expression, never two: if the XML's `<Command>` and the file `--apply` puts
/// on disk could disagree, the installer itself could ship a task pointing at a
/// `.cmd` that is not there — and on Windows nothing reports that.
pub const LAUNCHER_BASENAME: &str = "memory-industry.cmd";

fn joined(lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Loopback,
    Lan,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "loopback" => Ok(Profile::Loopback),
            "lan" => Ok(Profile::Lan),
            other => bail!(
                "perfil desconocido «{other}». Los que existen son «loopback» (127.0.0.1, solo \
                 esta máquina) y «lan» (una dirección que otras máquinas alcanzan, con token \
                 obligatorio)"
            ),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Profile::Loopback => "loopback",
            Profile::Lan => "lan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Linux,
    Windows,
}

impl Target {
    pub fn host() -> Self {
        if cfg!(windows) {
            Target::Windows
        } else {
            Target::Linux
        }
    }

    pub fn is_windows(self) -> bool {
        matches!(self, Target::Windows)
    }
}

/// Which of the three things `setup service` was asked to do.
///
/// A type and not the flag's own text: with a `&str` the dispatch in
/// `setup_agent.rs` needed a fourth arm for a mode nothing could produce, and a
/// branch no input reaches is a branch no test can cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Print,
    Apply,
    Uninstall,
}

impl Mode {
    /// The flag that asks for it, without the dashes. The exclusivity refusal
    /// prints it, so the message names the flags the operator actually typed.
    fn label(self) -> &'static str {
        match self {
            Mode::Print => "print",
            Mode::Apply => "apply",
            Mode::Uninstall => "uninstall",
        }
    }

    fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--print" => Some(Mode::Print),
            "--apply" => Some(Mode::Apply),
            "--uninstall" => Some(Mode::Uninstall),
            _ => None,
        }
    }
}

/// What the flags add up to, with nothing left to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub mode: Mode,
    pub target: Target,
    pub profile: Profile,
    pub addr: Option<String>,
    pub token: Option<String>,
    pub out: Option<PathBuf>,
}

/// `--help` answers before anything else is decided, so it is not a fourth
/// mode: somebody who writes `--print --help` is asking what the flags are, and
/// refusing that pair as «two modes at once» answers a question they did not
/// ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    Help,
    Run(Request),
}

/// The flags as they arrive. Every field is optional because the defaults are
/// `into_request`'s job: whether `--windows` was typed is not the same question
/// as which target this render is for.
#[derive(Debug, Default)]
struct Flags {
    mode: Option<Mode>,
    target: Option<Target>,
    profile: Option<Profile>,
    addr: Option<String>,
    token: Option<String>,
    out: Option<PathBuf>,
}

impl Flags {
    /// One argument, plus the iterator the flags that take a word read it from.
    fn take<'a>(&mut self, flag: &str, rest: &mut impl Iterator<Item = &'a String>) -> Result<()> {
        if let Some(asked) = Mode::from_flag(flag) {
            return self.set_mode(asked);
        }
        match flag {
            "--linux" => self.target = Some(Target::Linux),
            "--windows" => self.target = Some(Target::Windows),
            "--profile" | "--addr" | "--token" | "--out" => self.set_value(flag, rest.next())?,
            other => bail!("opción desconocida para `setup service`: {other}"),
        }
        Ok(())
    }

    /// The three modes are exclusive, and the refusal names both of them: an
    /// operator who wrote `--print --apply` meant one, and cannot tell from an
    /// install which one won. The same flag twice is not a conflict.
    fn set_mode(&mut self, asked: Mode) -> Result<()> {
        if let Some(already) = self.mode
            && already != asked
        {
            bail!(
                "--{} y --{} son excluyentes: elegí uno",
                already.label(),
                asked.label()
            );
        }
        self.mode = Some(asked);
        Ok(())
    }

    /// The word after a flag that takes one.
    ///
    /// One table, so each flag's refusal says what it needed. `--addr necesita
    /// un valor` sends the operator to `--help` for the one thing the message
    /// could have told them. `--profile` is parsed here rather than at the end
    /// on purpose: an unknown profile is refused where it was typed, and not
    /// behind whatever the next flag turns out to be wrong about.
    fn set_value(&mut self, flag: &str, raw: Option<&String>) -> Result<()> {
        match flag {
            "--profile" => {
                let raw = raw.context("--profile necesita loopback o lan")?;
                self.profile = Some(Profile::parse(raw)?);
            }
            "--addr" => {
                self.addr = Some(
                    raw.context("--addr necesita una dirección ip:puerto")?
                        .clone(),
                );
            }
            "--token" => self.token = Some(raw.context("--token necesita un valor")?.clone()),
            _ => self.out = Some(PathBuf::from(raw.context("--out necesita un directorio")?)),
        }
        Ok(())
    }

    /// The defaults, and the one combination that is refused.
    fn into_request(self) -> Result<Request> {
        let mode = self.mode.unwrap_or(Mode::Print);
        if self.out.is_some() && mode != Mode::Print {
            bail!(
                "--out solo va con --print: --apply y --uninstall trabajan en la raíz de \
                 instalación, no en un directorio que elijas"
            );
        }
        Ok(Request {
            mode,
            target: self.target.unwrap_or_else(Target::host),
            profile: self.profile.unwrap_or(Profile::Loopback),
            addr: self.addr,
            token: self.token,
            out: self.out,
        })
    }
}

/// `setup service`'s flags, turned into the one thing it was asked for.
///
/// Here and not beside the `println!`s in `setup_agent.rs`, because
/// `quality-gate.sh:164` keeps that file out of `cargo mutants`: a decision
/// that lives there has no judge. What stays there is the printing.
pub fn parse_request(args: &[String]) -> Result<Asked> {
    let mut flags = Flags::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Asked::Help);
        }
        flags.take(arg, &mut rest)?;
    }
    Ok(Asked::Run(flags.into_request()?))
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub profile: Profile,
    pub exe: PathBuf,
    pub addr: SocketAddr,
    pub token: Option<String>,
    pub env_file: PathBuf,
    /// What `resources::plan_env` computes. Rendered COMMENTED and without a
    /// value; the `Option` is this machine's measurement, as an annotation.
    ///
    /// The measurement is shown and never set. A number here, even behind a
    /// `#`, is what somebody uncomments at two in the morning — which is the
    /// hand-pinned ceiling defect with a new file name.
    pub computed: Vec<(String, Option<String>)>,
    /// What only the operator knows. Rendered uncommented, or the file starts
    /// nothing and the operator writes their own by hand again.
    pub chosen: Vec<(String, String)>,
    /// Why each model landed where it did, from `gpu::status()`. A comment, not
    /// a knob: it is what the operator went to `rust/src` to find out.
    pub why: String,
    /// PASS-1 STUB (0.29.0 slice A, S-4): the home of the user who installs, so
    /// the launcher can hand it to a daemon that SYSTEM starts. Pass 2 fills it
    /// in `documented` and `from_env` and renders it in the launcher.
    pub home_dir: PathBuf,
}

impl Unit {
    /// The canonical install, with no measurement and no token. This is what
    /// the goldens under `packaging/` are rendered from: `current_exe()`, the
    /// measured `Plan` and `placement_summary()` all differ per machine, so a
    /// byte-for-byte golden cannot come from them.
    pub fn documented(profile: Profile, target: Target) -> Unit {
        let (exe, env_file) = if target.is_windows() {
            (WINDOWS_EXE, WINDOWS_ENV)
        } else {
            (LINUX_EXE, LINUX_ENV)
        };
        let addr: SocketAddr = DEFAULT_LOOPBACK_ADDR
            .parse()
            .expect("DEFAULT_LOOPBACK_ADDR is a literal this crate owns");

        Unit {
            profile,
            exe: PathBuf::from(exe),
            addr,
            token: None,
            env_file: PathBuf::from(env_file),
            computed: planner_keys().into_iter().map(|key| (key, None)).collect(),
            chosen: vec![
                ("CUBA_HTTP_ADDR".to_string(), addr.to_string()),
                ("CUBA_MODE".to_string(), "completo".to_string()),
                ("DATABASE_URL".to_string(), String::new()),
                ("ONNX_MODEL_PATH".to_string(), String::new()),
                ("ORT_DYLIB_PATH".to_string(), String::new()),
            ],
            why: DOCUMENTED_WHY.to_string(),
            home_dir: PathBuf::new(),
        }
    }

    /// What `--print` and `--apply` render on a real machine.
    pub fn from_env(
        profile: Profile,
        target: Target,
        addr_flag: Option<&str>,
        token_flag: Option<&str>,
    ) -> Result<Unit> {
        let addr = addr_for(profile, addr_flag)?;
        let token = match token_flag {
            Some(given) => Some(given.to_string()),
            None if addr.ip().is_loopback() => None,
            // Not a refusal: the operator asked for a routable daemon, and the
            // installer can produce a token the daemon accepts more reliably
            // than a person inventing one at the prompt.
            None => Some(new_token()),
        };
        if let Some(why) = refuse_unsafe(profile, &addr, token.as_deref()) {
            bail!(why);
        }

        let exe = std::env::current_exe().context("no se pudo resolver la ruta de este binario")?;
        let home = crate::envs::home()
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let local_app_data = std::env::var("LOCALAPPDATA").ok();
        let root = install_root(target, home.as_deref(), local_app_data.as_deref())?;

        let measured =
            crate::resources::plan_env(&crate::resources::plan(&crate::resources::probe()));
        let computed = planner_keys()
            .into_iter()
            .map(|key| {
                let value = measured
                    .iter()
                    .find(|(k, _)| *k == key.as_str())
                    .map(|(_, v)| v.clone());
                (key, value)
            })
            .collect();

        Ok(Unit {
            profile,
            exe,
            addr,
            token,
            env_file: root.join(ENV_BASENAME),
            computed,
            chosen: vec![
                ("CUBA_HTTP_ADDR".to_string(), addr.to_string()),
                ("CUBA_MODE".to_string(), read_or_empty("CUBA_MODE")),
                // `std::env::var`, never `setup::resolve_database_url()`: that
                // one is async and resolves against the database, and every
                // command's `--help` runs against a dead URL in cli_contract.
                ("DATABASE_URL".to_string(), read_or_empty("DATABASE_URL")),
                (
                    "ONNX_MODEL_PATH".to_string(),
                    read_or_empty("ONNX_MODEL_PATH"),
                ),
                (
                    "ORT_DYLIB_PATH".to_string(),
                    read_or_empty("ORT_DYLIB_PATH"),
                ),
            ],
            why: crate::gpu::status().detail,
            home_dir: PathBuf::new(),
        })
    }
}

fn read_or_empty(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// The keys `resources::plan_env` can emit: the union over a plan with
/// everything on and one with everything off.
///
/// Derived, never listed. A hand-kept list is the second source of truth this
/// whole module exists to remove, and it would go stale the first time the
/// planner learned a new knob.
pub fn planner_keys() -> Vec<String> {
    use crate::resources::{Plan, Tier};

    // The numbers are placeholders, and so is `reranker_on_gpu`: `plan_env`
    // pushes CUBA_RERANK_DEVICE either way and that flag only picks its value.
    // Which keys come out depends on `reranker`, `nli` and on whether
    // `gpu_mem_limit_mb` is present, so those are the three the off plan flips.
    let everything_on = Plan {
        tier: Tier::Full,
        embedder: true,
        reranker: true,
        reranker_on_gpu: true,
        nli: true,
        embed_intra_threads: 1,
        rerank_intra_threads: 1,
        nli_intra_threads: 1,
        rerank_chunk: 1,
        gpu_mem_limit_mb: Some(1),
        gpu_mem_floor_mb: Some(1),
        worker_threads: 1,
        max_blocking_threads: 1,
        db_max_connections: 1,
        ood_fit_limit: 1,
        budget_mb: 1,
        committed_mb: 1,
    };
    // Spelled out field by field instead of `..everything_on.clone()`: a
    // functional update makes cargo-mutants emit "delete field X from struct
    // Plan expression" mutants, and `--exclude-re` does not match those
    // (27.1.0), so a survivor there cannot be filtered out of the gate. Only
    // `reranker`, `nli` and `gpu_mem_limit_mb` differ from the plan above —
    // the three the off plan flips — and repeating the rest is what that costs.
    let everything_off = Plan {
        tier: Tier::Full,
        embedder: true,
        reranker: false,
        reranker_on_gpu: true,
        nli: false,
        embed_intra_threads: 1,
        rerank_intra_threads: 1,
        nli_intra_threads: 1,
        rerank_chunk: 1,
        gpu_mem_limit_mb: None,
        gpu_mem_floor_mb: Some(1),
        worker_threads: 1,
        max_blocking_threads: 1,
        db_max_connections: 1,
        ood_fit_limit: 1,
        budget_mb: 1,
        committed_mb: 1,
    };

    let mut keys: Vec<String> = Vec::new();
    for plan in [&everything_on, &everything_off] {
        for (key, _) in crate::resources::plan_env(plan) {
            if !keys.iter().any(|k| k.as_str() == key) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

pub fn addr_for(profile: Profile, explicit: Option<&str>) -> Result<SocketAddr> {
    let Some(raw) = explicit else {
        return match profile {
            Profile::Loopback => Ok(DEFAULT_LOOPBACK_ADDR
                .parse()
                .expect("DEFAULT_LOOPBACK_ADDR is a literal this crate owns")),
            Profile::Lan => bail!(
                "el perfil «lan» necesita --addr con la dirección concreta de la interfaz que \
                 usan tus clientes, por ejemplo --addr 192.168.0.10:8787. No voy a poner \
                 0.0.0.0 por vos: eso publica también la VPN de la empresa y el hotspot del \
                 teléfono"
            ),
        };
    };

    let addr: SocketAddr = raw
        .parse()
        .with_context(|| format!("«{raw}» no es una dirección ip:puerto"))?;

    if addr.ip().is_unspecified() {
        bail!(
            "{addr} no significa «la LAN»: significa todas las interfaces de esta máquina, y \
             eso incluye la VPN de la empresa y el hotspot del teléfono. Dame la dirección \
             concreta de la interfaz que usan tus clientes"
        );
    }
    match profile {
        Profile::Loopback if !addr.ip().is_loopback() => bail!(
            "{addr} no es una dirección loopback, así que el perfil «loopback» no es el que \
             querés. El perfil para publicar en la LAN es «lan», y pide token"
        ),
        Profile::Lan if addr.ip().is_loopback() => bail!(
            "{addr} es loopback: a ese daemon no lo alcanza ninguna otra máquina, así que un \
             perfil «lan» ahí funciona en la máquina que lo instaló y falla para todas las \
             demás. O usás --profile loopback, o dame la dirección de la interfaz real"
        ),
        _ => Ok(addr),
    }
}

/// Why this profile, address and token should not be installed, if they should
/// not.
///
/// The verdict is the ADDRESS's, as in `http::ensure_loopback`, never the
/// profile label's: if the label decided, `--profile loopback --addr <lan ip>`
/// would install an open daemon with the installer's blessing.
///
/// The length rule is written out here rather than delegated to
/// `http::token_too_weak`, on purpose. Delegating would make
/// `the_installer_and_the_daemon_agree_token_by_token` compare a function with
/// itself, and the `chars().count()` → `len()` mutant — the one that accepts 32
/// multibyte characters the daemon rejects — would survive it. The constant is
/// shared, because a second 32 is a second thing to keep in step.
pub fn refuse_unsafe(profile: Profile, addr: &SocketAddr, token: Option<&str>) -> Option<String> {
    if addr.ip().is_loopback() {
        return None;
    }
    let floor = crate::http::MIN_ROUTABLE_TOKEN_CHARS;
    let Some(token) = token else {
        return Some(format!(
            "--profile {} publica en {addr}, que otras máquinas alcanzan, y no hay token. El \
             token es lo único entre el grafo entero y cualquiera que sepa rutear un paquete \
             hasta ese puerto. Pasá --token con al menos {floor} caracteres, o dejá que el \
             instalador genere uno",
            profile.label()
        ));
    };
    let found = token.chars().count();
    if found >= floor {
        return None;
    }
    Some(format!(
        "el token tiene {found} caracteres y en {addr}, que otras máquinas alcanzan, es lo \
         único entre el grafo entero y cualquiera que sepa rutear un paquete, así que necesita \
         al menos {floor}. El daemon lo va a rechazar igual al arrancar"
    ))
}

/// 64 hexadecimal characters, from two v4 UUIDs. `uuid` is already a dependency
/// and draws from a CSPRNG, so no new crate enters for this.
///
/// 64 and not 32 because 32 is exactly the floor: a token sitting on the edge
/// falls off it the first time the floor moves.
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Where the artifacts go, or an error naming the variable that is missing.
///
/// Pure over its arguments so the missing-variable branch is testable without
/// touching the process environment. The real resolution stays in
/// `envs::home()`; this never reads a variable itself.
pub fn install_root(
    target: Target,
    home: Option<&str>,
    local_app_data: Option<&str>,
) -> Result<PathBuf> {
    if target.is_windows() {
        // Deliberately no fallback to the home directory. Guessing succeeds and
        // leaves the file that holds the token wherever the operator happened
        // to be standing, which is how `setup claude --apply` used to write
        // `.claude.json` somewhere no MCP client ever looks.
        let base = local_app_data.context(
            "no sé dónde instalar: LOCALAPPDATA no está definida. Definila y repetí — adivinar \
             un directorio dejaría el fichero con el token donde nadie lo busca",
        )?;
        return Ok(PathBuf::from(base).join("MemoryIndustry"));
    }
    let base = home.context(
        "no sé dónde instalar: ni HOME ni USERPROFILE están definidas. Definí una de las dos y \
         repetí — adivinar un directorio dejaría el fichero con el token donde nadie lo busca",
    )?;
    Ok(PathBuf::from(base).join(".config").join("memory-industry"))
}

pub fn render_systemd(u: &Unit) -> String {
    joined(vec![
        "[Unit]".to_string(),
        "Description=MemoryIndustry — shared MCP knowledge-graph daemon".to_string(),
        format!("Documentation={REPO_URL}"),
        "After=network-online.target".to_string(),
        "Wants=network-online.target".to_string(),
        String::new(),
        "[Service]".to_string(),
        "Type=exec".to_string(),
        format!("ExecStart={} serve {}", u.exe.display(), u.addr),
        "# Every knob lives in the env file, never here. A value pinned in a unit is a".to_string(),
        "# second source of truth for the defaults, and that is how a hand-written GPU".to_string(),
        "# ceiling beat the planner's measurement on every card this daemon ran on.".to_string(),
        format!("EnvironmentFile={}", u.env_file.display()),
        "# on-failure, not always: a clean idle shutdown exits 0, and Restart=always".to_string(),
        "# would bounce the daemon straight back up and defeat it entirely.".to_string(),
        "Restart=on-failure".to_string(),
        "RestartSec=3".to_string(),
        String::new(),
        "# One process holds every ONNX model, so this is where the memory shows up.".to_string(),
        "# MemoryHigh throttles and reclaims; MemoryMax OOM-kills, so Max is the last".to_string(),
        "# line of defence only.".to_string(),
        "MemoryHigh=4500M".to_string(),
        "MemoryMax=6G".to_string(),
        "OOMScoreAdjust=200".to_string(),
        "Nice=5".to_string(),
        String::new(),
        "# What is here is what this daemon survives; what is missing is missing on".to_string(),
        "# purpose. PrivateDevices would take away /dev/nvidia*, ProtectHome would take"
            .to_string(),
        "# away the model cache, ProtectSystem=strict would make the sync directory".to_string(),
        "# read-only, and MemoryDenyWriteExecute breaks ONNX Runtime, which JITs its".to_string(),
        "# kernels. ProtectControlGroups and ProtectKernelTunables stay read-only rather"
            .to_string(),
        "# than hidden: the resource planner reads /sys/fs/cgroup and /proc at startup."
            .to_string(),
        "NoNewPrivileges=yes".to_string(),
        "PrivateTmp=yes".to_string(),
        "ProtectSystem=full".to_string(),
        "ProtectKernelTunables=yes".to_string(),
        "ProtectKernelModules=yes".to_string(),
        "ProtectControlGroups=yes".to_string(),
        "ProtectClock=yes".to_string(),
        "RestrictSUIDSGID=yes".to_string(),
        "RestrictRealtime=yes".to_string(),
        "LockPersonality=yes".to_string(),
        "LimitNOFILE=4096".to_string(),
        String::new(),
        "[Install]".to_string(),
        "WantedBy=default.target".to_string(),
    ])
}

pub fn render_socket(u: &Unit) -> String {
    joined(vec![
        "# Socket activation: this unit owns the port, so the daemon does not have to be"
            .to_string(),
        "# running to keep it. The first connection starts memory-industry.service.".to_string(),
        "#".to_string(),
        "#   systemctl --user daemon-reload".to_string(),
        "#   systemctl --user disable --now memory-industry.service".to_string(),
        "#   systemctl --user enable  --now memory-industry.socket".to_string(),
        "#".to_string(),
        "# `serve` adopts the socket systemd passes as fd 3 (LISTEN_FDS), so no client".to_string(),
        "# sees a refused connection while the daemon is down. Unix only — on Windows".to_string(),
        "# there is no systemd and `serve` binds the address itself.".to_string(),
        "[Unit]".to_string(),
        "Description=MemoryIndustry daemon socket — brings the daemon up on the first MCP call"
            .to_string(),
        format!("Documentation={REPO_URL}"),
        String::new(),
        "[Socket]".to_string(),
        format!("ListenStream={}", u.addr),
        "Accept=no".to_string(),
        String::new(),
        "[Install]".to_string(),
        "WantedBy=default.target".to_string(),
    ])
}

pub fn render_windows_launcher(u: &Unit) -> String {
    joined(vec![
        "@echo off".to_string(),
        "rem Generated by `memory-industry setup service`. Not edited by hand.".to_string(),
        "rem".to_string(),
        "rem Task Scheduler cannot set environment variables, and this binary reads no".to_string(),
        "rem .env file of its own. Without this launcher the task starts a daemon with".to_string(),
        "rem no DATABASE_URL and no token, and on Windows nothing reports that: the task"
            .to_string(),
        "rem 'ran', and the only symptom is a daemon that never appears.".to_string(),
        "setlocal".to_string(),
        // The same failure, one step earlier: `for /f` over a file that is not
        // there runs its body zero times and falls through, so a missing env
        // file would start exactly the empty daemon the lines above describe.
        "rem Same failure, one step earlier: `for /f` over a file that is not there runs"
            .to_string(),
        "rem its body zero times and falls through, so a missing env file starts exactly"
            .to_string(),
        "rem the empty daemon those lines describe. The exit code below is what reaches"
            .to_string(),
        "rem Task Scheduler's Last Run Result; falling through is what reports success."
            .to_string(),
        format!("if not exist \"{}\" (", u.env_file.display()),
        format!(
            "  echo memory-industry: {} is not there.",
            u.env_file.display()
        ),
        "  echo Nothing would set DATABASE_URL or the bearer token, so the daemon would start"
            .to_string(),
        "  echo deaf and the task would report success. Run: memory-industry setup service --apply"
            .to_string(),
        "  exit /b 1".to_string(),
        ")".to_string(),
        // eol=# skips the commented knobs; blank lines `for /f` skips already.
        format!(
            "for /f \"usebackq eol=# tokens=1,* delims==\" %%a in (\"{}\") do set \"%%a=%%b\"",
            u.env_file.display()
        ),
        format!("\"{}\" serve {}", u.exe.display(), u.addr),
    ])
}

/// Takes no `Unit`: the task is the same for every profile. The address lives
/// in the `.cmd`, and the task's whole job is to run that file. A parameter
/// this function never reads would claim there is a task per profile.
pub fn render_windows_task(launcher_path: &Path) -> String {
    joined(vec![
        // UTF-16, and the file really has to BE UTF-16LE with a BOM.
        // `schtasks /Create /XML` reads the bytes, not this declaration, and
        // refuses a UTF-8 file before it ever parses it:
        //
        //     (1,40)::ERROR: no se pudo cambiar la codificación
        //
        // Column 40 of line 1 is exactly where `encoding=` sits. Measured
        // against the real scheduler, because no contract that reads this file
        // as text could have found it — which is what QA-4bis is for.
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>".to_string(),
        "<!-- Generated by `memory-industry setup service`. Not edited by hand. -->".to_string(),
        "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">"
            .to_string(),
        "  <RegistrationInfo>".to_string(),
        "    <Description>MemoryIndustry — shared MCP knowledge-graph daemon</Description>"
            .to_string(),
        "    <URI>\\memory-industry</URI>".to_string(),
        "  </RegistrationInfo>".to_string(),
        "  <Triggers>".to_string(),
        "    <LogonTrigger>".to_string(),
        "      <Enabled>true</Enabled>".to_string(),
        "    </LogonTrigger>".to_string(),
        "  </Triggers>".to_string(),
        "  <Principals>".to_string(),
        "    <Principal id=\"Author\">".to_string(),
        "      <LogonType>InteractiveToken</LogonType>".to_string(),
        "      <!-- A per-user task needs no administrator, and asking for one turns a".to_string(),
        "           silent install into a UAC prompt somebody has to be there to answer. -->"
            .to_string(),
        "      <RunLevel>LeastPrivilege</RunLevel>".to_string(),
        "    </Principal>".to_string(),
        "  </Principals>".to_string(),
        "  <Settings>".to_string(),
        "    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>".to_string(),
        "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>".to_string(),
        "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>".to_string(),
        "    <StartWhenAvailable>true</StartWhenAvailable>".to_string(),
        "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>".to_string(),
        "    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>".to_string(),
        "    <Enabled>true</Enabled>".to_string(),
        "    <Hidden>false</Hidden>".to_string(),
        "    <RestartOnFailure>".to_string(),
        "      <Interval>PT1M</Interval>".to_string(),
        "      <Count>3</Count>".to_string(),
        "    </RestartOnFailure>".to_string(),
        "  </Settings>".to_string(),
        "  <Actions Context=\"Author\">".to_string(),
        "    <Exec>".to_string(),
        // The launcher, never the .exe. Task Scheduler sets no environment, so
        // a task pointed straight at the binary starts a daemon with no
        // DATABASE_URL and no token — and reports that it "ran".
        //
        // Unquoted, including when the path has spaces. `<Command>` is typed
        // `pathType` in the Task Scheduler schema — "a string that defines a
        // file path", not a command line — and it maps to `IExecAction::Path`,
        // so the element already delimits the value and quotes would become
        // part of the path. Microsoft's own example is `<Command>notepad.exe`.
        // The quoting advice that circulates is for `schtasks /TR` and for the
        // "Program/script" box, which really are command lines. `pathType` has
        // no pattern restriction, so quotes ARE schema-valid and
        // `schtasks /Create /XML` takes them: this one only breaks at run time,
        // which is why it is written down here instead of being obvious.
        format!("      <Command>{}</Command>", launcher_path.display()),
        "    </Exec>".to_string(),
        "  </Actions>".to_string(),
        "</Task>".to_string(),
    ])
}

pub fn render_env_file(u: &Unit) -> String {
    let mut lines = vec![
        format!("# MemoryIndustry — {} profile", u.profile.label()),
        "#".to_string(),
        "# Generated by `memory-industry setup service`. The generator is the same".to_string(),
        "# binary that reads these back, so the copy under packaging/ is not edited by".to_string(),
        "# hand: rust/tests/packaging_contract.rs compares it against the renderer.".to_string(),
        "#".to_string(),
        format!("# {}", u.why),
        String::new(),
        "# --- Only you know these -----------------------------------------------------"
            .to_string(),
        format!("CUBA_HTTP_TOKEN={}", u.token.as_deref().unwrap_or("")),
    ];
    for (key, value) in &u.chosen {
        lines.push(format!("{key}={value}"));
    }

    lines.push(String::new());
    lines.push(
        "# --- The resource planner measures these -------------------------------------"
            .to_string(),
    );
    lines.push(
        "# Offered, never set. A value written here beats the measurement for good,".to_string(),
    );
    lines.push(
        "# because resources::set_if_absent leaves an already-set variable alone — that"
            .to_string(),
    );
    lines.push(
        "# is how a hand-pinned GPU ceiling beat every card this daemon ever ran on.".to_string(),
    );
    lines.push("# To use less VRAM, say so where it is true: CUBA_RERANK_DEVICE=cpu.".to_string());
    for (key, measured) in &u.computed {
        lines.push(format!("# {key}="));
        if let Some(value) = measured {
            lines.push(format!("#   esta máquina midió: {value}"));
        }
    }
    joined(lines)
}

pub fn render_plan(u: &Unit, target: Target, launcher_path: &Path) -> String {
    let root = launcher_path.parent().unwrap_or(Path::new("."));
    let mut lines = vec![format!(
        "Perfil «{}» para {}, desde {}",
        u.profile.label(),
        if target.is_windows() {
            "windows"
        } else {
            "linux"
        },
        u.exe.display()
    )];
    lines.push(String::new());
    lines.push("Escribiría:".to_string());
    for (relative, _) in rendered_files(u, target.is_windows(), launcher_path) {
        let name = basename(&relative);
        let destination = if is_env_example(name) {
            u.env_file.clone()
        } else {
            root.join(name)
        };
        lines.push(format!("  {}", destination.display()));
    }
    lines.push(String::new());
    lines.push("Y lo habilitarías con:".to_string());
    lines.push(format!("  {}", enable_command(target, root)));
    lines.push(String::new());
    lines.push("Esto fue un plan — no se tocó ningún fichero. Con --apply se instala.".to_string());
    joined(lines)
}

/// The command that turns the written files into a running daemon. Printed by
/// the plan and after `--apply`, because the artifacts on disk do nothing until
/// somebody runs it.
pub fn enable_command(target: Target, root: &Path) -> String {
    if target.is_windows() {
        format!(
            "schtasks /Create /XML \"{}\" /TN memory-industry",
            root.join("memory-industry-task.xml").display()
        )
    } else {
        "systemctl --user daemon-reload && systemctl --user enable --now memory-industry.service"
            .to_string()
    }
}

/// PASS-1 STUB (0.29.0 slice A): the argv `--register` will run. Pass 2 returns
/// `["schtasks","/Create","/XML",<xml>,"/TN","memory-industry","/F"]`.
pub fn schtasks_create_argv(_xml: &Path) -> Vec<String> {
    Vec::new()
}

/// PASS-1 STUB (0.29.0 slice A): `netsh advfirewall firewall delete rule
/// name=memory-industry`.
pub fn firewall_delete_argv() -> Vec<String> {
    Vec::new()
}

/// PASS-1 STUB (0.29.0 slice A): `netsh advfirewall firewall add rule
/// name=memory-industry dir=in action=allow protocol=TCP localport=<port>
/// profile=private,domain`.
pub fn firewall_add_argv(_port: u16) -> Vec<String> {
    Vec::new()
}

/// PASS-1 STUB (0.29.0 slice A): `[]` for loopback, `[delete, add]` otherwise.
pub fn firewall_commands(_addr: &SocketAddr) -> Vec<String> {
    Vec::new()
}

fn basename(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap_or(relative)
}

/// Whether this rendered file is the operator's variables.
///
/// ONE expression for a rule three places read: the plan says where it would
/// land, `write_all` gives it the name and the secrecy an install needs, and
/// `removable_files` keeps `--uninstall` away from it. Three copies of
/// `.env.example` is three chances for the installer to write the token to one
/// path and delete another.
fn is_env_example(name: &str) -> bool {
    name.ends_with(".env.example")
}

/// ONE answer to "what does this profile produce". `--print`, `--out`,
/// `--apply` and the goldens all come from here, or they diverge — and the
/// divergence is exactly what put a hand-written unit in `packaging/`.
pub fn rendered_files(u: &Unit, windows: bool, launcher_path: &Path) -> Vec<(String, String)> {
    if windows {
        vec![
            (
                format!("windows/{LAUNCHER_BASENAME}"),
                render_windows_launcher(u),
            ),
            (
                "windows/memory-industry-task.xml".to_string(),
                render_windows_task(launcher_path),
            ),
            (
                "windows/memory-industry.env.example".to_string(),
                render_env_file(u),
            ),
        ]
    } else {
        vec![
            ("memory-industry.service".to_string(), render_systemd(u)),
            ("memory-industry.socket".to_string(), render_socket(u)),
            (
                "memory-industry.env.example".to_string(),
                render_env_file(u),
            ),
        ]
    }
}

/// `Secret` is ONE decision, not two: 0600 on unix AND never overwritten.
///
/// Splitting it would make "the file that carries the token" and "the file we
/// do not clobber" two lists that have to agree, and the day they disagree the
/// installer rotates a live token under every client already configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secrecy {
    Plain,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    Created { restricted: bool },
    Kept,
}

/// What `--apply` reports for one file it touched.
///
/// A line rather than a `println!`, and in this file rather than beside the
/// one: `conservado` is the only signal the operator gets that the token their
/// clients are already configured with survived a second `--apply`, and
/// `setup_agent.rs` is outside the mutation judge (`quality-gate.sh:164`).
pub fn wrote_line(path: &Path, wrote: Wrote) -> String {
    match wrote {
        Wrote::Created { restricted: true } => {
            format!("escrito     {} (0600: solo tu usuario)", path.display())
        }
        Wrote::Created { restricted: false } => format!("escrito     {}", path.display()),
        Wrote::Kept => format!(
            "conservado  {} — lleva el token y la DATABASE_URL que tus clientes ya usan; \
             reescribirlo los rompe a todos de golpe",
            path.display()
        ),
    }
}

/// The bytes that go on disk, which are not always the UTF-8 of the render.
///
/// The Task Scheduler XML is the one artifact here that cannot ship as UTF-8:
/// `schtasks` refuses it at the encoding stage, before parsing. Keyed on the
/// extension rather than passed in, because this module writes exactly one
/// `.xml` and it is that file; a fourth argument on `write_one` would put the
/// decision in seven call sites instead of one.
fn bytes_for(path: &Path, contents: &str) -> Vec<u8> {
    if path.extension().is_some_and(|e| e == "xml") {
        // BOM first: it is what the scheduler looks at.
        let mut out = vec![0xFF, 0xFE];
        for unit in contents.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        return out;
    }
    contents.as_bytes().to_vec()
}

pub fn write_one(path: &Path, contents: &str, secrecy: Secrecy) -> Result<Wrote> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("no se pudo crear {}", parent.display()))?;
    }
    if secrecy == Secrecy::Secret && path.exists() {
        return Ok(Wrote::Kept);
    }
    std::fs::write(path, bytes_for(path, contents))
        .with_context(|| format!("no se pudo escribir {}", path.display()))?;

    let restricted = match secrecy {
        Secrecy::Secret => restrict(path)?,
        Secrecy::Plain => false,
    };
    Ok(Wrote::Created { restricted })
}

/// Whether the installer actually took the file away from the other accounts on
/// the box.
///
/// On unix that is `chmod 0600`. Elsewhere it is nothing: AppData is per user
/// and there is no portable chmod. What we do NOT do comes back as a value,
/// because a silent omission cannot be told apart from an oversight.
///
/// One function with the `cfg` inside, never two `cfg`-split ones: a body the
/// gate's platform never compiles is a mutant the mutation run still reports
/// and no test on that platform can kill.
fn restrict(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("no se pudo restringir {}", path.display()))?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

pub fn write_all(u: &Unit, target: Target, root: &Path) -> Result<Vec<(PathBuf, Wrote)>> {
    let launcher = root.join(LAUNCHER_BASENAME);
    let mut written = Vec::new();

    for (relative, body) in rendered_files(u, target.is_windows(), &launcher) {
        let name = basename(&relative);
        // The example is what the repository publishes; what an install gets is
        // the real env file, and it is the one that must survive a second
        // --apply.
        let (path, secrecy) = if is_env_example(name) {
            (root.join(ENV_BASENAME), Secrecy::Secret)
        } else {
            (root.join(name), Secrecy::Plain)
        };
        let wrote = write_one(&path, &body, secrecy)?;
        written.push((path, wrote));
    }
    Ok(written)
}

/// The files `--uninstall` may take away, under the install root.
///
/// The env file is not among them, and the filter is what keeps it that way: it
/// is the one artifact here that cannot be regenerated, because it carries the
/// token and the DATABASE_URL every client is already configured with.
///
/// Derived from `rendered_files`, never listed: a hand-kept list is how a
/// fourth artifact gets written by `--apply` and left behind by `--uninstall`,
/// with nothing saying so.
pub fn removable_files(u: &Unit, target: Target, root: &Path) -> Vec<PathBuf> {
    let launcher = root.join(LAUNCHER_BASENAME);
    rendered_files(u, target.is_windows(), &launcher)
        .into_iter()
        .map(|(relative, _)| basename(&relative).to_string())
        .filter(|name| !is_env_example(name))
        .map(|name| root.join(name))
        .collect()
}

/// The mutation surface of `setup service`.
///
/// These live in the library, not in `rust/tests/`: the gate runs
/// `cargo mutants -- --lib`, and an integration test kills none of its mutants.
/// Everything that needs the repository tree is in
/// `rust/tests/packaging_contract.rs` instead.
#[cfg(test)]
mod tests {
    use super::*;

    const ROUTABLE: &str = "192.168.0.10:8787";

    fn addr(s: &str) -> SocketAddr {
        s.parse()
            .unwrap_or_else(|e| panic!("{s} is an address: {e}"))
    }

    /// A scratch directory of this test's own, removed on the way out.
    ///
    /// `verificacion.md` → Inmutable: the test builds its world and throws it away.
    /// A shared fixture directory would make two of these race under `cargo test`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("mi-service-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("scratch directory is creatable");
            Self(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A unit on a routable address, built field by field rather than through
    /// `documented`, so what a test asserts about rendering cannot be an accident
    /// of another function it did not mean to exercise.
    fn lan_unit(target: Target, token: &str) -> Unit {
        let windows = matches!(target, Target::Windows);
        Unit {
            profile: Profile::Lan,
            exe: PathBuf::from(if windows {
                "C:\\Users\\operador\\AppData\\Local\\MemoryIndustry\\memory-industry.exe"
            } else {
                "/home/operador/.local/bin/memory-industry"
            }),
            addr: addr(ROUTABLE),
            token: Some(token.to_string()),
            env_file: PathBuf::from(if windows {
                "C:\\Users\\operador\\AppData\\Local\\MemoryIndustry\\memory-industry.env"
            } else {
                "/home/operador/.config/memory-industry/memory-industry.env"
            }),
            computed: vec![
                (
                    "CUBA_GPU_MEM_LIMIT_MB".to_string(),
                    Some("5388".to_string()),
                ),
                ("CUBA_RERANK_CHUNK".to_string(), Some("16".to_string())),
                ("CUBA_RERANK_DEVICE".to_string(), Some("gpu".to_string())),
                (
                    "CUBA_RERANK_INTRA_THREADS".to_string(),
                    Some("2".to_string()),
                ),
                ("CUBA_DB_MAX_CONNECTIONS".to_string(), None),
            ],
            chosen: vec![
                ("CUBA_HTTP_ADDR".to_string(), ROUTABLE.to_string()),
                (
                    "DATABASE_URL".to_string(),
                    "postgresql://localhost:5488/brain".to_string(),
                ),
                ("CUBA_MODE".to_string(), "completo".to_string()),
            ],
            why: "cuda — runtime GPU y GPU detectados · colocación: reranker=gpu".to_string(),
            home_dir: PathBuf::from(if windows {
                "C:\\Users\\operador"
            } else {
                "/home/operador"
            }),
        }
    }

    /// A directory the host's own `Path` can take apart. `lan_unit` spells its
    /// Windows paths with backslashes, which a unix `Path` sees as ONE component:
    /// `parent()` of them is empty, so a log "beside the env file" cannot be
    /// derived from them on the machine that runs this gate.
    fn native_root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:\\opt\\mi"
        } else {
            "/opt/mi"
        })
    }

    /// A Windows unit whose exe and env file live under `native_root()`, so the
    /// log the launcher writes has a directory the host can name.
    fn windows_unit_at_native_root() -> Unit {
        let root = native_root();
        let mut unit = lan_unit(Target::Windows, &"a".repeat(64));
        unit.exe = root.join("memory-industry.exe");
        unit.env_file = root.join(ENV_BASENAME);
        unit
    }

    /// The same, on 127.0.0.1: what a loopback install resolves to.
    fn loopback_unit(target: Target) -> Unit {
        let mut unit = lan_unit(target, &"a".repeat(64));
        unit.profile = Profile::Loopback;
        unit.token = None;
        unit.addr = addr(DEFAULT_LOOPBACK_ADDR);
        unit
    }

    /// What sits between `<tag>` and `</tag>`. A tag the render lost is a panic
    /// that quotes the XML, not an empty string that satisfies an absence check.
    fn xml_block<'a>(xml: &'a str, tag: &str) -> &'a str {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml
            .find(&open)
            .unwrap_or_else(|| panic!("the task XML has no {open}:\n{xml}"))
            + open.len();
        let end = xml[start..]
            .find(&close)
            .unwrap_or_else(|| panic!("the task XML opens {open} and never closes it:\n{xml}"))
            + start;
        &xml[start..end]
    }

    /// The names of the elements directly under a block, comments skipped.
    fn direct_children(block: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut depth = 0usize;
        let mut rest = block;
        while let Some(open) = rest.find('<') {
            rest = &rest[open..];
            if let Some(comment) = rest.strip_prefix("<!--") {
                rest = comment.split_once("-->").map_or("", |(_, after)| after);
                continue;
            }
            let end = rest.find('>').expect("every tag in the render is closed");
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            if tag.starts_with('/') {
                depth = depth.saturating_sub(1);
                continue;
            }
            if depth == 0 {
                names.push(tag.split([' ', '/']).next().unwrap_or(tag).to_string());
            }
            if !tag.ends_with('/') {
                depth += 1;
            }
        }
        names
    }

    /// The lines of one `[Section]` of a unit file, header excluded.
    fn section<'a>(unit_file: &'a str, header: &str) -> Vec<&'a str> {
        unit_file
            .lines()
            .skip_while(|l| *l != header)
            .skip(1)
            .take_while(|l| !l.starts_with('['))
            .collect()
    }

    /// The index of the launcher line that starts the daemon: the quoted exe,
    /// then `serve`.
    fn launch_line(cmd: &str, unit: &Unit) -> usize {
        let start = format!("\"{}\" serve", unit.exe.display());
        cmd.lines()
            .position(|l| l.trim_start().starts_with(&start))
            .unwrap_or_else(|| panic!("no line of the launcher starts {start}:\n{cmd}"))
    }

    fn launcher_for(target: Target) -> PathBuf {
        PathBuf::from(if matches!(target, Target::Windows) {
            "C:\\Users\\operador\\AppData\\Local\\MemoryIndustry\\memory-industry.cmd"
        } else {
            "/home/operador/.local/bin/memory-industry"
        })
    }

    /// Pins the two variables `Unit::from_env` resolves the install root from.
    ///
    /// Without them a `--windows` row fails for want of LOCALAPPDATA on a unix
    /// host and a `--linux` row for want of HOME under a Windows service
    /// account, and a row that ends in an error has stopped testing whatever it
    /// was written for.
    ///
    /// Take `session::GLOBAL_STATE_GUARD` before calling: `set_var` is unsound
    /// while another thread may be reading the environment.
    fn pinned_install_root(scratch: &Scratch) -> [crate::envs::ScopedEnv; 2] {
        let root = scratch.0.display().to_string();
        [
            crate::envs::ScopedEnv::set("HOME", &root),
            crate::envs::ScopedEnv::set("LOCALAPPDATA", &root),
        ]
    }

    /// Reads a file this module wrote, whichever encoding it went out in.
    ///
    /// The task XML ships as UTF-16LE with a BOM because `schtasks` refuses
    /// anything else, so `read_to_string` fails on it. Decoding here keeps the
    /// comparison below about content, which is what it was ever about.
    fn read_text(path: &Path) -> String {
        let bytes =
            std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        if bytes.starts_with(&[0xFF, 0xFE]) {
            // The leftover byte is dropped here and checked in the contract's
            // reader instead. This one reads a file the same process wrote a
            // moment ago through `fs::write`, which errors on a short write, so
            // an odd length is not a state that can be reached — and a guard
            // for it would be the speculative handling the rules rule out.
            let units: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            return String::from_utf16(&units)
                .unwrap_or_else(|e| panic!("{} is not valid UTF-16: {e}", path.display()));
        }
        String::from_utf8(bytes)
            .unwrap_or_else(|e| panic!("{} is not valid UTF-8: {e}", path.display()))
    }

    /// The keys an env file offers, commented or not — `# CUBA_X=` and `CUBA_X=`
    /// both count as offering `CUBA_X`.
    fn keys_offered(body: &str) -> Vec<String> {
        body.lines()
            .map(|l| l.trim_start().trim_start_matches('#').trim())
            .filter_map(|l| l.split('=').next())
            .filter(|w| {
                // `'\u{5f}'` is the underscore, spelled as the escape on purpose.
                // lizard's Rust reader adds `'\w+\b` for lifetimes, so it takes a
                // plain `'_'` for the lifetime `'_` followed by a lone quote that
                // opens a literal nothing closes, and everything from there to the
                // next apostrophe in the file stops existing for the CRAP half of
                // scripts/quality-gate.sh. Measured 2026-09-21: this function was
                // reported at 225 NLOC and 1370 length, and the seven tests between
                // it and the `daemon's` in the comment below were not measured at
                // all. `b'_'` blinds it the same way; `'\u{5f}'` does not.
                !w.is_empty()
                    && w.starts_with(|c: char| c.is_ascii_uppercase())
                    && w.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '\u{5f}')
            })
            .map(str::to_string)
            .collect()
    }

    // --- Profile and Target ------------------------------------------------------

    #[test]
    fn every_profile_round_trips_through_its_label() {
        for profile in [Profile::Loopback, Profile::Lan] {
            assert_eq!(
                Profile::parse(profile.label()).expect("a label the code prints, parses"),
                profile,
                "`--profile {}` has to come back as the profile whose label that is. A label that \
                 names the other one sends `--profile loopback` down the LAN path, which is the \
                 difference between a daemon on 127.0.0.1 and one anybody on the subnet can reach",
                profile.label()
            );
        }

        assert_ne!(
            Profile::Loopback.label(),
            Profile::Lan.label(),
            "two profiles sharing one label makes the flag meaningless"
        );
    }

    #[test]
    fn an_unknown_profile_names_the_two_that_exist() {
        let said = Profile::parse("lanparty")
            .expect_err("`lanparty` is not a profile and must not be taken for one")
            .to_string();

        for expected in ["loopback", "lan"] {
            assert!(
                said.contains(expected),
                "the refusal must name the profiles that do exist, or the operator guesses a \
                 second time: {said}"
            );
        }
    }

    // --- refuse_unsafe: the installer says no at install time --------------------

    #[test]
    fn the_host_target_is_the_one_this_binary_was_built_for() {
        assert_eq!(
            Target::host().is_windows(),
            cfg!(windows),
            "`setup service` with no --linux/--windows renders for the machine it is standing \
             on. Getting this backwards writes a systemd unit on Windows, which nothing there \
             reads and nothing reports"
        );
        assert!(Target::Windows.is_windows());
        assert!(!Target::Linux.is_windows());
    }

    #[test]
    fn a_loopback_install_never_needs_a_token() {
        let loopback = addr(DEFAULT_LOOPBACK_ADDR);
        let long = "a".repeat(64);

        for token in [None, Some("short"), Some(long.as_str())] {
            assert_eq!(
                refuse_unsafe(Profile::Loopback, &loopback, token),
                None,
                "on {loopback} whoever is asking already has the machine, exactly as \
                 `http::ensure_loopback` decides it. Demanding a token here would make the \
                 installer stricter than the daemon and stop an install that works"
            );
        }
    }

    #[test]
    fn a_lan_install_without_a_token_is_refused() {
        let said = refuse_unsafe(Profile::Lan, &addr(ROUTABLE), None).expect(
            "a routable address with no token at all publishes the whole graph. `token?` \
             returning early here turns the missing token into «nothing to object»",
        );

        assert!(
            said.to_lowercase().contains("token"),
            "the refusal has to say what is missing: {said}"
        );
    }

    #[test]
    fn the_installer_refuses_at_the_exact_length_the_daemon_does() {
        let routable = addr(ROUTABLE);

        // 31 and 32 are the pair that kills `<` ↔ `<=`. 32 is accepted rather than
        // demanded-plus-one: an installer that asked for 33 would reject a token
        // the daemon takes, and the operator would be fighting two different rules.
        for (n, refused) in [
            (0usize, true),
            (1, true),
            (31, true),
            (32, false),
            (33, false),
        ] {
            let token = "a".repeat(n);
            assert_eq!(
                refuse_unsafe(Profile::Lan, &routable, Some(token.as_str())).is_some(),
                refused,
                "a {n}-character token on {routable} should {} be refused",
                if refused { "" } else { "not" }
            );
        }
    }

    #[test]
    fn the_installer_and_the_daemon_agree_token_by_token() {
        let routable = addr(ROUTABLE);

        // The 32 multibyte characters occupy 64 bytes: `chars().count()` and
        // `len()` give different answers and only one of them is the daemon's. An
        // installer that accepts what the daemon later rejects is worse than one
        // that checks nothing, because the failure moves to the first client call.
        let tokens = [
            String::new(),
            "a".repeat(31),
            "a".repeat(32),
            "ñ".repeat(32),
            "a".repeat(40),
        ];

        for token in tokens {
            let installer = refuse_unsafe(Profile::Lan, &routable, Some(token.as_str())).is_some();
            let daemon = crate::http::token_too_weak(false, Some(token.as_str())).is_some();
            assert_eq!(
                installer,
                daemon,
                "installer and daemon disagree on a {}-character ({} byte) token: installer \
                 refuses={installer}, daemon refuses={daemon}",
                token.chars().count(),
                token.len()
            );
        }
    }

    #[test]
    fn a_generated_token_clears_the_floor_and_is_never_the_same_twice() {
        let token = new_token();

        assert_eq!(
            token.chars().count(),
            64,
            "two v4 UUIDs in simple form are 64 hex characters: {token}"
        );
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "a token that travels in a systemd unit and a .cmd must not need quoting: {token}"
        );
        assert_ne!(
            token,
            new_token(),
            "a constant token is one every install in the field shares, which is the same as \
             having none"
        );
        assert_eq!(
            refuse_unsafe(Profile::Lan, &addr(ROUTABLE), Some(token.as_str())),
            None,
            "the installer must not generate a token its own check rejects"
        );
        assert_eq!(
            crate::http::token_too_weak(false, Some(token.as_str())),
            None,
            "nor one the daemon will refuse to start with"
        );
    }

    // --- addr_for: the address decides, and it is never guessed ------------------

    #[test]
    fn a_lan_profile_without_an_address_asks_for_the_interface() {
        let said = addr_for(Profile::Lan, None)
            .expect_err("`lan` with no --addr must ask, not pick one")
            .to_string();

        assert!(
            said.contains("0.0.0.0"),
            "the message has to name the wildcard as the thing it is NOT going to do, or the \
             operator's next move is to reach for it: {said}"
        );
        assert!(
            said.to_lowercase().contains("--addr"),
            "and it has to name the flag that answers it: {said}"
        );

        assert_eq!(
            addr_for(Profile::Loopback, None).expect("loopback has a default"),
            addr(DEFAULT_LOOPBACK_ADDR),
            "loopback is the one profile with a right answer nobody has to supply"
        );
    }

    #[test]
    fn a_wildcard_address_is_refused_by_the_interfaces_it_would_add() {
        // 0.0.0.0 does not mean "the LAN". It means every interface this machine
        // has, which on a laptop is also the VPN into the corporate network and the
        // phone's hotspot.
        for wildcard in ["0.0.0.0:8787", "[::]:8787"] {
            let said = addr_for(Profile::Lan, Some(wildcard))
                .expect_err("the wildcard publishes more than the operator is picturing")
                .to_string()
                .to_lowercase();

            for interface in ["vpn", "hotspot"] {
                assert!(
                    said.contains(interface),
                    "the refusal has to name the {interface} it would publish on — «it is not \
                     safe» is advice, and the operator overrides advice: {said}"
                );
            }
        }
    }

    #[test]
    fn a_loopback_profile_with_a_routable_address_is_a_contradiction() {
        let said = addr_for(Profile::Loopback, Some(ROUTABLE))
            .expect_err("`loopback` on a LAN address is a contradiction, not a default")
            .to_string()
            .to_lowercase();

        assert!(
            said.contains("lan"),
            "the message has to name the profile that publishes on the LAN, or the operator \
             reruns the same command: {said}"
        );

        // The verdict belongs to the address, as in `ensure_loopback`. If the label
        // decided it, `--profile loopback --addr <lan ip>` would install an open
        // daemon with the installer's blessing.
        assert!(
            addr_for(Profile::Lan, Some(ROUTABLE)).is_ok(),
            "the same address under `lan` is the supported install"
        );
    }

    #[test]
    fn a_lan_profile_pointing_at_loopback_reaches_nobody() {
        let said = addr_for(Profile::Lan, Some(DEFAULT_LOOPBACK_ADDR))
            .expect_err(
                "a `lan` daemon on 127.0.0.1 works for the machine that installed it and \
                         for nobody else — that belongs to the install minute, not to the first \
                         client call",
            )
            .to_string();

        assert!(
            said.contains(DEFAULT_LOOPBACK_ADDR),
            "the refusal has to quote the address it is refusing, or the operator cannot tell \
             which of the two flags they got wrong: {said}"
        );

        assert_eq!(
            addr_for(Profile::Loopback, Some(DEFAULT_LOOPBACK_ADDR)).expect("the supported pair"),
            addr(DEFAULT_LOOPBACK_ADDR),
            "the same address under `loopback` is exactly the default install"
        );
    }

    // --- from_env: this machine and these flags, not the goldens ----------------

    #[tokio::test]
    async fn a_token_is_generated_exactly_when_the_address_can_be_reached() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let scratch = Scratch::new("token-when-routable");
        let _pinned = pinned_install_root(&scratch);

        // The guard on that arm is the whole difference between an install
        // nobody else can reach and one anybody on the subnet can. Always
        // taken, a LAN install comes back with no token and the daemon
        // publishes the graph; never taken, a loopback install carries a bearer
        // token nobody asked for and every client has to be told about it.
        for (profile, given_addr, expects_token) in [
            (Profile::Loopback, None, false),
            (Profile::Loopback, Some(DEFAULT_LOOPBACK_ADDR), false),
            (Profile::Lan, Some(ROUTABLE), true),
        ] {
            for target in [Target::Linux, Target::Windows] {
                let unit = match Unit::from_env(profile, target, given_addr, None) {
                    Ok(unit) => unit,
                    Err(e) => panic!(
                        "--profile {} --addr {given_addr:?} is a supported install and it was \
                         refused: {e}",
                        profile.label()
                    ),
                };

                assert_eq!(
                    unit.token.is_some(),
                    expects_token,
                    "--profile {} on {} must {}come back with a token",
                    profile.label(),
                    unit.addr,
                    if expects_token { "" } else { "NOT " }
                );

                if let Some(token) = unit.token.as_deref() {
                    assert_eq!(
                        refuse_unsafe(profile, &unit.addr, Some(token)),
                        None,
                        "the installer generated a token its own check rejects, so `--apply` \
                         fails on a machine where nothing is wrong"
                    );
                }
            }
        }

        // The arm above the guard: a token the operator passed is the one
        // installed, or `--token` is a flag that quietly does nothing and the
        // clients they already configured stop being able to talk.
        let given = "d".repeat(64);
        let unit = Unit::from_env(
            Profile::Lan,
            Target::Linux,
            Some(ROUTABLE),
            Some(given.as_str()),
        )
        .expect("a routable address with a long token is the supported LAN install");
        assert_eq!(unit.token.as_deref(), Some(given.as_str()));
    }

    #[tokio::test]
    async fn what_the_operator_already_set_travels_into_the_env_file_verbatim() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let scratch = Scratch::new("chosen-from-env");
        let _pinned = pinned_install_root(&scratch);

        // A real mode rather than an invented word: `mode::active()` reads this
        // same variable, and leaving nonsense in it while the guard is held
        // would show every other reader in this binary a mode that is not one.
        let _mode = crate::envs::ScopedEnv::set("CUBA_MODE", "red");
        let _db = crate::envs::ScopedEnv::cleared("DATABASE_URL");

        let unit = Unit::from_env(Profile::Loopback, Target::Linux, None, None)
            .expect("a loopback install needs nothing but a home directory");

        assert_eq!(
            unit.chosen
                .iter()
                .find(|(key, _)| key.as_str() == "CUBA_MODE")
                .map(|(_, value)| value.as_str()),
            Some("red"),
            "the file has to carry the mode this machine is already running. A render that \
             ignores what is set hands the operator a file that moves them back to «local» the \
             next time the daemon starts, and nothing says so"
        );
        assert_eq!(
            unit.chosen
                .iter()
                .find(|(key, _)| key.as_str() == "DATABASE_URL")
                .map(|(_, value)| value.as_str()),
            Some(""),
            "a variable nobody set has to come out empty. Anything else is written into the \
             file as though the operator had chosen it, and `resources::set_if_absent` then \
             leaves that invention in charge for good"
        );

        let body = render_env_file(&unit);
        assert!(
            body.lines().any(|l| l == "CUBA_MODE=red"),
            "what from_env read has to reach the file uncommented, or it is not what the daemon \
             will start with:\n{body}"
        );
        assert!(
            body.lines().any(|l| l == "DATABASE_URL="),
            "the empty line is the prompt the operator fills in; an invented value is a \
             connection string that fails at the first query:\n{body}"
        );
    }

    #[tokio::test]
    async fn each_measurement_is_annotated_on_the_key_it_was_measured_for() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let scratch = Scratch::new("annotations");
        let _pinned = pinned_install_root(&scratch);

        let unit = Unit::from_env(Profile::Loopback, Target::Linux, None, None)
            .expect("a loopback install needs nothing but a home directory");

        // Anchor before absence: an empty `computed` would satisfy any search
        // below while proving nothing.
        assert!(
            unit.computed.len() >= 5,
            "from_env offered {} planner keys, so nothing below this can fail",
            unit.computed.len()
        );

        // Pairing a key with its own measurement is the whole job of that
        // lookup, and looking it up by «any key but this one» still finds
        // something — the first pair whose key is a different one. The operator
        // then reads a thread count where the placement of the reranker should
        // be. CUBA_RERANK_DEVICE is the one key whose values are words, so it
        // is the one that tells the two lookups apart on any machine.
        let device = unit
            .computed
            .iter()
            .find(|(key, _)| key.as_str() == "CUBA_RERANK_DEVICE")
            .map(|(_, measured)| measured.clone())
            .expect("CUBA_RERANK_DEVICE is a key `resources::plan_env` always emits");

        assert!(
            matches!(device.as_deref(), Some("gpu" | "cpu")),
            "this machine's CUBA_RERANK_DEVICE is annotated {device:?}, which is not a place a \
             model can run. That line is what the operator reads to find out whether the \
             reranker landed on the card, and anything else there is another key's measurement \
             wearing this key's name"
        );
    }

    // --- What the rendered files may and may not contain ------------------------

    #[test]
    fn a_rendered_token_appears_in_the_env_file_and_nowhere_else() {
        // A literal with no meaning anywhere else, so a hit is this token and not
        // an accident of the template.
        let token = "f00dcafe".repeat(8);

        for target in [Target::Linux, Target::Windows] {
            let unit = lan_unit(target, &token);
            let launcher = launcher_for(target);
            let files = rendered_files(&unit, matches!(target, Target::Windows), &launcher);

            assert!(
                files.len() >= 3,
                "every profile produces the unit, the socket or the task, and the env file"
            );

            let mut carriers = Vec::new();
            for (name, body) in &files {
                if body.contains(&token) {
                    carriers.push(name.clone());
                }
            }

            assert_eq!(
                carriers.len(),
                1,
                "the token must live in exactly one file and it must be the env file. The unit, \
                 the socket, the XML and the .cmd are versioned artifacts — a token in any of \
                 them is a token in the repository. Carried by: {carriers:?}"
            );
            assert!(
                carriers[0].contains("env"),
                "the one file carrying the token is not the env file but {}",
                carriers[0]
            );
        }
    }

    #[test]
    fn no_key_the_planner_computes_is_written_uncommented() {
        let unit = lan_unit(Target::Linux, &"a".repeat(64));
        let body = render_env_file(&unit);

        for (key, measured) in &unit.computed {
            assert!(
                body.lines().any(|l| l.trim() == format!("# {key}=")),
                "{key} has to be offered commented and WITHOUT a value. A number here, even \
                 behind a `#`, is the one somebody uncomments at two in the morning — and \
                 `set_if_absent` makes it beat the measurement for good. That is the 2048 \
                 defect with a new file name.\n{body}"
            );
            assert!(
                !body
                    .lines()
                    .any(|l| l.trim_start().starts_with(&format!("{key}="))),
                "{key} is written uncommented, so a hand-written number wins over what the \
                 planner measures on the machine it runs on.\n{body}"
            );

            if let Some(value) = measured {
                assert!(
                    body.contains(value.as_str()),
                    "this machine measured {key}={value} and the file never says so. Showing the \
                     measurement is what the operator went to rust/src to find out; the point is \
                     that it is shown and not set.\n{body}"
                );
            }
        }

        // Positive control: what only the operator knows IS written, or the file
        // starts nothing and they go back to writing their own by hand.
        for (key, _) in &unit.chosen {
            assert!(
                body.lines()
                    .any(|l| l.trim_start().starts_with(&format!("{key}="))),
                "{key} is the operator's to choose and must be uncommented.\n{body}"
            );
        }
    }

    #[test]
    fn every_key_the_planner_computes_is_at_least_offered() {
        let unit = lan_unit(Target::Linux, &"a".repeat(64));

        // Anchor before absence: a `computed` that arrived empty would satisfy any
        // loop below while proving nothing.
        assert!(
            unit.computed.len() >= 5,
            "the fixture offers fewer keys than the planner emits, so this test cannot fail"
        );

        let offered = keys_offered(&render_env_file(&unit));
        let missing: Vec<&str> = unit
            .computed
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| !offered.iter().any(|o| o.as_str() == *key))
            .collect();

        assert!(
            missing.is_empty(),
            "the env file never mentions {missing:?}. A knob the planner can set and the file \
             does not name is one the operator discovers by reading the source, which is where \
             this whole module started"
        );
    }

    #[test]
    fn the_planner_keys_are_the_union_of_what_it_can_emit() {
        let keys = planner_keys();

        // `CUBA_GPU_MEM_LIMIT_MB` is only emitted when the plan has a ceiling, and
        // `CUBA_RERANKER_PATH` only when the reranker is off. Demanding both is
        // what makes this a union rather than one plan's opinion — and the ceiling
        // is the very knob whose hand-written 2048 started all of this.
        for anchor in [
            "CUBA_RERANK_CHUNK",
            "CUBA_RERANK_DEVICE",
            "CUBA_GPU_MEM_LIMIT_MB",
            "CUBA_RERANKER_PATH",
            "CUBA_NLI_PATH",
        ] {
            assert!(
                keys.iter().any(|k| k.as_str() == anchor),
                "{anchor} is a key `resources::plan_env` can emit and the env file would never \
                 offer it. Found: {keys:?}"
            );
        }
    }

    #[test]
    fn the_installed_files_say_why_each_model_landed_where_it_did() {
        let mut on_the_card = lan_unit(Target::Linux, &"a".repeat(64));
        on_the_card.why =
            "cuda — runtime GPU y GPU detectados · colocación: reranker=gpu".to_string();

        let mut on_the_cpu = lan_unit(Target::Linux, &"a".repeat(64));
        on_the_cpu.why =
            "compilado con cuda, pero no detecté GPU NVIDIA → corriendo en CPU".to_string();

        let with_card = render_env_file(&on_the_card);
        let without = render_env_file(&on_the_cpu);

        for (body, unit) in [(&with_card, &on_the_card), (&without, &on_the_cpu)] {
            assert!(
                body.lines()
                    .any(|l| l.trim_start().starts_with('#') && l.contains(&unit.why)),
                "the placement has to travel as a COMMENT, not a knob: it is what the operator \
                 went looking for in rust/src.\n{body}"
            );
        }

        assert_ne!(
            with_card, without,
            "a machine with a card and one without must not render the same explanation, or the \
             explanation is a constant and says nothing about this machine"
        );
    }

    // --- Each render names THIS unit, not a template ----------------------------
    //
    // `rust/tests/packaging_contract.rs` pins the fixed text of these files
    // against the tree under `packaging/`, and it can only do that for the one
    // `Unit` the goldens were rendered from — `documented()`, with no
    // measurement and no token. What varies per install, which is the binary,
    // the address and the env file, has no golden and is asserted here.

    #[test]
    fn the_unit_starts_this_binary_on_this_address_and_takes_its_knobs_from_the_env_file() {
        let unit = lan_unit(Target::Linux, &"a".repeat(64));
        let systemd = render_systemd(&unit);

        let Some(exec) = systemd.lines().find(|l| l.starts_with("ExecStart=")) else {
            panic!("a unit with no ExecStart starts nothing:\n{systemd}");
        };
        assert!(
            exec.contains(&unit.exe.display().to_string()),
            "ExecStart has to name the binary this install resolved, not one a golden rendered \
             on somebody else's machine: {exec}"
        );
        assert_eq!(
            exec,
            format!("ExecStart={} serve", unit.exe.display()),
            "ExecStart carries no address (S-5): `serve` reads CUBA_HTTP_ADDR from the env file, \
             so the address lives in one place. An address here is the second source that \
             `--apply` rewrites in the env file and not in the unit, and the daemon then listens \
             where the operator did not ask"
        );
        assert!(
            !exec.contains(&unit.addr.to_string()),
            "the unit repeats {} and the env file says it too; they drift the first time one of \
             them is edited: {exec}",
            unit.addr
        );

        let Some(env_line) = systemd.lines().find(|l| l.starts_with("EnvironmentFile=")) else {
            panic!(
                "with no EnvironmentFile the daemon starts with no DATABASE_URL and no token, \
                 and systemd reports it as running:\n{systemd}"
            );
        };
        assert!(
            env_line.contains(&unit.env_file.display().to_string()),
            "the unit has to read the file --apply actually writes: {env_line}"
        );
        assert!(
            !systemd.lines().any(|l| l.starts_with("Environment=")),
            "a value pinned in the unit is a second source of truth for the defaults, and \
             `resources::set_if_absent` lets an already-set variable win — which is exactly how \
             a hand-written GPU ceiling beat the planner on every card this daemon ran \
             on:\n{systemd}"
        );
        assert!(
            systemd.contains(REPO_URL),
            "Documentation= pointed at a repository that does not exist for a whole release, \
             and the operator who follows it is the one already lost:\n{systemd}"
        );
    }

    #[test]
    fn the_socket_holds_the_port_the_service_is_told_to_serve() {
        let unit = lan_unit(Target::Linux, &"a".repeat(64));
        let socket = render_socket(&unit);

        let Some(listen) = socket.lines().find(|l| l.starts_with("ListenStream=")) else {
            panic!(
                "a .socket with no ListenStream holds no port, so nothing starts the daemon on \
                 the first MCP call and the client sees a refused connection:\n{socket}"
            );
        };
        assert_eq!(
            listen,
            format!("ListenStream={}", unit.addr),
            "the socket unit listens on the address this install chose, or it hands `serve` an \
             fd for a port nobody dials — and the daemon is then up and unreachable at once"
        );
        let service = render_systemd(&unit);
        assert!(
            !service.contains(&unit.addr.to_string()),
            "the service must NOT repeat the address the socket holds (S-5): a unit cannot read \
             the env file, so the .socket is the one place systemd keeps it, and `--apply` \
             rewrites that .socket. Two copies here is socket activation handing over one port \
             while `serve` is told another, and neither file says so:\n{service}"
        );
        assert!(
            service
                .lines()
                .any(|l| l.starts_with("ExecStart=") && l.ends_with(" serve")),
            "the service still starts `serve`, just without an address to disagree with the \
             socket:\n{service}"
        );
        assert!(
            socket.contains(REPO_URL),
            "the socket is the file the operator finds first when the port is taken, so it has \
             to point somewhere real:\n{socket}"
        );
    }

    #[test]
    fn the_plan_names_every_file_it_would_write_and_writes_none_of_them() {
        for target in [Target::Linux, Target::Windows] {
            let unit = lan_unit(target, &"a".repeat(64));
            let root = PathBuf::from("install-root");
            let plan = render_plan(&unit, target, &root.join(LAUNCHER_BASENAME));

            let Some(header) = plan.lines().next() else {
                panic!("an empty plan says nothing about what --apply is about to do");
            };
            assert!(
                header.contains(unit.profile.label()),
                "the plan has to say which profile it is about: `loopback` and `lan` differ by \
                 whether the daemon is reachable from other machines: {header}"
            );
            assert!(
                header.contains(&unit.exe.display().to_string()),
                "and which binary it would install, or a second copy on the same box is \
                 indistinguishable from the one the operator meant: {header}"
            );
            let rendered_for = if target.is_windows() {
                "windows"
            } else {
                "linux"
            };
            assert!(
                header.contains(rendered_for),
                "and which target was rendered. A plan that ignores --windows/--linux prints a \
                 systemd install on a machine that has no systemd: {header}"
            );
            assert!(
                plan.contains(&unit.env_file.display().to_string()),
                "the env file is the one destination that does NOT sit beside the others, and \
                 it is the one carrying the token. A plan that hides it is a plan the operator \
                 cannot check before running --apply:\n{plan}"
            );

            let beside_the_launcher = if target.is_windows() {
                [LAUNCHER_BASENAME, "memory-industry-task.xml"]
            } else {
                ["memory-industry.service", "memory-industry.socket"]
            };
            for name in beside_the_launcher {
                assert!(
                    plan.contains(&root.join(name).display().to_string()),
                    "--print is the only look at {name} the operator gets before --apply writes \
                     it:\n{plan}"
                );
            }

            assert!(
                plan.contains("--apply"),
                "a plan that does not name the flag that carries it out leaves the operator \
                 guessing, and the guess is usually to run the same command again:\n{plan}"
            );
        }
    }

    #[test]
    fn enabling_the_daemon_names_the_supervisor_each_target_actually_has() {
        let root = Path::new("install-root");
        let linux = enable_command(Target::Linux, root);
        let windows = enable_command(Target::Windows, root);

        assert!(
            linux.contains("systemctl --user enable --now memory-industry.service"),
            "the files on disk do nothing until this runs, so the command has to be the one \
             that starts THIS unit: {linux}"
        );
        assert!(
            linux.contains("daemon-reload"),
            "systemd does not see a unit file it has not reloaded, so enabling before the \
             reload fails on the very first install: {linux}"
        );
        assert!(
            !linux.contains("schtasks"),
            "there is no Task Scheduler on linux: {linux}"
        );

        assert!(
            windows.contains("schtasks /Create /XML"),
            "and there is no systemd on windows: {windows}"
        );
        assert!(
            windows.ends_with(" /F"),
            "the command is printed for an operator who runs it twice, and `schtasks /Create` \
             without /F stops to ask whether to overwrite the task — a prompt nobody is there \
             to answer when `--register` runs the same argv: {windows}"
        );
        assert!(
            windows.contains(&root.join("memory-industry-task.xml").display().to_string()),
            "the command has to point at the XML --apply just wrote. Anywhere else registers \
             nothing, `schtasks` says so once, and the daemon never comes up at logon: {windows}"
        );
        assert!(
            !windows.contains("systemctl"),
            "there is no systemd on windows: {windows}"
        );

        // One expression, printed: a plan that described the command in its own
        // words could drift from the command, and the operator would be running
        // the description.
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        assert!(
            render_plan(&unit, Target::Windows, &root.join(LAUNCHER_BASENAME)).contains(&windows),
            "the plan must print the command it is telling the operator to run"
        );
    }

    // --- Writing: the secret survives the second --apply ------------------------

    #[test]
    fn the_env_file_that_already_exists_is_kept_not_rewritten() {
        let scratch = Scratch::new("kept");
        let path = scratch.join("memory-industry.env");

        assert_eq!(
            write_one(&path, "CUBA_HTTP_TOKEN=first\n# mine\n", Secrecy::Secret)
                .expect("the first write creates the file"),
            Wrote::Created {
                restricted: cfg!(unix)
            },
            "the first --apply creates it"
        );

        assert_eq!(
            write_one(&path, "CUBA_HTTP_TOKEN=second\n", Secrecy::Secret)
                .expect("the second write is allowed to decide to keep"),
            Wrote::Kept,
            "rewriting rotates the token under every client already configured and takes the \
             DATABASE_URL the operator edited by hand with it"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("still readable"),
            "CUBA_HTTP_TOKEN=first\n# mine\n",
            "byte for byte what was there, comments the operator added included"
        );

        // The other half of the same decision: what is not secret IS refreshed, or
        // --apply stops being able to ship a fixed unit.
        let unit = scratch.join("memory-industry.service");
        write_one(&unit, "first", Secrecy::Plain).expect("created");
        assert_eq!(
            write_one(&unit, "second", Secrecy::Plain).expect("rewritten"),
            Wrote::Created { restricted: false },
            "the unit, the socket, the XML and the .cmd carry no secret and must be refreshed"
        );
        assert_eq!(std::fs::read_to_string(&unit).expect("readable"), "second");
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_file_is_unreadable_to_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("mode");
        let path = scratch.join("memory-industry.env");

        assert_eq!(
            write_one(&path, "CUBA_HTTP_TOKEN=abc\n", Secrecy::Secret).expect("created"),
            Wrote::Created { restricted: true },
            "on unix the restriction is something the installer did, so it is reported"
        );

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "this file holds the bearer token for the whole graph and the DATABASE_URL; 0o{mode:o} \
             lets every other account on the box read both"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_secret_file_on_windows_reports_that_it_did_not_restrict_anything() {
        let scratch = Scratch::new("nomode");
        let path = scratch.join("memory-industry.env");

        assert_eq!(
            write_one(&path, "CUBA_HTTP_TOKEN=abc\n", Secrecy::Secret).expect("created"),
            Wrote::Created { restricted: false },
            "AppData is per user and there is no portable chmod. What we do NOT do comes back as \
             a value: a silent omission cannot be told apart from an oversight"
        );
    }

    #[test]
    fn print_and_apply_render_the_same_bytes() {
        let scratch = Scratch::new("same-bytes");

        for target in [Target::Linux, Target::Windows] {
            let unit = lan_unit(target, &"a".repeat(64));
            let root = scratch.join(if matches!(target, Target::Windows) {
                "win"
            } else {
                "linux"
            });
            // The same formula `write_all` uses, and deliberately not a second
            // one: if the XML's <Command> and the file --apply writes could come
            // from two different expressions, the installer itself would be able
            // to ship a task pointing at a .cmd that is not where it says.
            let launcher = root.join("memory-industry.cmd");
            let printed = rendered_files(&unit, matches!(target, Target::Windows), &launcher);

            let written =
                write_all(&unit, target, &root).expect("write_all writes into a scratch root");

            assert_eq!(
                written.len(),
                printed.len(),
                "--print and --apply must produce the same set of files, or one of them is a \
                 second rendering path and the goldens only ever check the other"
            );

            let mut on_disk: Vec<String> =
                written.iter().map(|(path, _)| read_text(path)).collect();
            let mut as_printed: Vec<String> = printed.into_iter().map(|(_, body)| body).collect();
            on_disk.sort();
            as_printed.sort();

            assert_eq!(
                on_disk, as_printed,
                "what --apply put on disk is not byte-for-byte what --print showed, so the \
                 goldens under packaging/ verify a rendering nobody installs"
            );
        }
    }

    // --- Where it all lands -----------------------------------------------------

    #[test]
    fn an_install_root_without_its_variable_says_which_one_is_missing() {
        let said = install_root(Target::Windows, Some("C:\\Users\\operador"), None)
            .expect_err("windows without LOCALAPPDATA must not guess a directory")
            .to_string();
        assert!(
            said.contains("LOCALAPPDATA"),
            "the message has to name the variable to set: {said}"
        );

        let said = install_root(Target::Linux, None, None)
            .expect_err("no home means no install root")
            .to_string();
        for expected in ["HOME", "USERPROFILE"] {
            assert!(
                said.contains(expected),
                "falling back to `.` was worse than failing: it left the file holding the token \
                 in whatever directory the operator was standing in. The message must name \
                 {expected}: {said}"
            );
        }

        // Positive control: with the variables set it resolves, and the two targets
        // do not resolve to the same place.
        let linux = install_root(Target::Linux, Some("/home/operador"), None)
            .expect("a home is all linux needs");
        let windows = install_root(
            Target::Windows,
            Some("C:\\Users\\operador"),
            Some("C:\\Users\\operador\\AppData\\Local"),
        )
        .expect("LOCALAPPDATA is all windows needs");
        assert_ne!(linux, windows);
    }

    // --- Windows: the task calls a launcher, never the bare binary --------------

    #[test]
    fn the_windows_launcher_loads_the_env_file_before_it_runs_the_binary() {
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        let launcher_path = launcher_for(Target::Windows);
        let cmd = render_windows_launcher(&unit);

        let env_at = cmd
            .find(&unit.env_file.display().to_string())
            .expect("the launcher must name the same env file as the rest of the render");
        let exe_at = cmd
            .find(&unit.exe.display().to_string())
            .expect("the launcher must name the binary it starts");
        assert!(
            env_at < exe_at,
            "the line that reads the env file has to come before the one that executes the \
             binary, or the daemon starts with no DATABASE_URL and no token:\n{cmd}"
        );

        // S8: the XML is what makes the launcher run at all. Pointing <Command> at
        // the .exe leaves `the_windows_task_names_the_binary_that_exists` happy —
        // the .exe does exist — and the task then starts a daemon with none of
        // these variables set. On Windows nothing reports that: the task "ran", and
        // the only symptom is a daemon that is not there. This is the assertion
        // that turns that silence into a red test.
        let xml = render_windows_task(&launcher_path);
        let commanded = xml
            .split("<Command>")
            .nth(1)
            .and_then(|rest| rest.split("</Command>").next())
            .expect("the task declares a <Command>")
            .trim()
            .trim_matches('"')
            .to_string();

        assert_eq!(
            commanded,
            launcher_path.display().to_string(),
            "the scheduled task must run the launcher, not the binary. Task Scheduler cannot set \
             environment variables and this binary reads no .env file, so a task pointed straight \
             at the .exe starts a daemon with no DATABASE_URL and no token"
        );
        assert_ne!(
            commanded,
            unit.exe.display().to_string(),
            "pointing <Command> at the .exe is the 203/EXEC nobody reports on Windows"
        );
    }

    #[test]
    fn the_windows_launcher_refuses_to_start_a_daemon_with_no_env_file() {
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        let cmd = render_windows_launcher(&unit);
        let env_file = unit.env_file.display().to_string();
        let exe = unit.exe.display().to_string();

        let guard = cmd
            .lines()
            .position(|l| l.trim_start().starts_with("if not exist") && l.contains(&env_file))
            .expect(
                "`for /f` over a file that is not there runs its body zero times and falls \
                 through, so with no guard the launcher starts a daemon with no DATABASE_URL \
                 and no token — the exact failure the comment above it describes, and the one \
                 nothing on Windows reports",
            );
        let refuses = cmd.lines().position(|l| l.contains("exit /b 1")).expect(
            "the guard has to end the script non-zero: that code is what reaches Task \
                 Scheduler's Last Run Result, and falling through is what reports success",
        );
        let runs = cmd
            .lines()
            .position(|l| l.contains(&exe))
            .expect("the launcher names the binary it starts");

        assert!(
            guard < refuses && refuses < runs,
            "the check, the refusal and the binary have to come in that order, or the daemon \
             starts anyway and the task calls it a success:\n{cmd}"
        );
    }

    #[test]
    fn the_scheduled_task_is_written_in_the_only_encoding_schtasks_reads() {
        let scratch = Scratch::new("utf16");
        let root = scratch.join("win");
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        let written = write_all(&unit, Target::Windows, &root).expect("writes into a scratch root");

        let xml = written
            .iter()
            .map(|(path, _)| path)
            .find(|path| path.extension().is_some_and(|e| e == "xml"))
            .expect("the windows render writes a task XML");
        let bytes = std::fs::read(xml).expect("what --apply wrote is readable");

        // Both halves, because either one alone passes over a file Windows
        // rejects. `schtasks /Create /XML` looks at the bytes: a UTF-8 file that
        // declares UTF-16 dies at (1,40) — the column `encoding=` sits in —
        // before the parser ever runs, and a UTF-16 file that declares UTF-8
        // dies the same way.
        assert!(
            bytes.starts_with(&[0xFF, 0xFE]),
            "the task XML does not start with the UTF-16LE BOM, so schtasks refuses it at the \
             encoding stage and never reports anything about the task itself. First bytes: {:?}",
            &bytes[..bytes.len().min(4)]
        );
        assert!(
            read_text(xml).contains("encoding=\"UTF-16\""),
            "the bytes are UTF-16 and the declaration says otherwise, which schtasks rejects \
             just as loudly as the other way round"
        );
    }

    #[test]
    fn the_scheduled_task_runs_at_logon_and_comes_back_after_a_crash() {
        let xml = render_windows_task(&launcher_for(Target::Windows));

        assert!(
            xml.contains("<LogonTrigger>"),
            "a per-user task that does not start at logon has to be started by hand after every \
             reboot, which is the artisanal supervisor this replaces:\n{xml}"
        );
        // `<RestartOnFailure>` with `<Interval>` and `<Count>` children is the
        // Task Scheduler schema. There is no `<RestartInterval>` element, and a
        // contract that reads the XML cannot tell a valid document from an
        // invented one — only Windows can. QA-4 runs `schtasks /Create /XML`
        // against this file for exactly that reason.
        assert!(
            xml.contains("<RestartOnFailure>")
                && xml.contains("<Interval>")
                && xml.contains("<Count>"),
            "without a restart policy the daemon stays down after the first crash and the only \
             signal is that it is gone:\n{xml}"
        );
        assert!(
            xml.contains("LeastPrivilege"),
            "SYSTEM needs no elevation request, and asking for one turns a silent install into \
             a UAC prompt the operator has to be there to answer:\n{xml}"
        );
        assert!(
            !xml.contains("HighestAvailable"),
            "nothing here needs elevation:\n{xml}"
        );
    }

    // --- 0.29.0 · slice A: the task survives a reboot nobody logs in to ---------

    #[test]
    fn the_task_runs_as_system_with_no_stored_password() {
        let xml = render_windows_task(&launcher_for(Target::Windows));
        let principals = xml_block(&xml, "Principals");

        assert_eq!(
            principals.matches("<Principal ").count(),
            1,
            "exactly one principal runs this task; a second one is a second account whose \
             rights nobody reviewed:\n{principals}"
        );
        assert!(
            xml.contains("<UserId>S-1-5-18</UserId>"),
            "S-1 (S-1-5-18 is SYSTEM): a per-user principal only exists once somebody logs in, \
             and the point of 0.29.0 is a daemon that is back after a reboot nobody logs in \
             to:\n{xml}"
        );
        assert!(
            xml.contains("<LogonType>ServiceAccount</LogonType>"),
            "SYSTEM is a service account: any other LogonType makes schtasks ask for a \
             password this installer does not have:\n{xml}"
        );
        assert!(
            xml.contains("<RunLevel>LeastPrivilege</RunLevel>"),
            "SYSTEM never has to ask for elevation, and HighestAvailable would only turn the \
             install into a UAC prompt:\n{xml}"
        );
        for forbidden in ["InteractiveToken", "<Password>", "HighestAvailable"] {
            assert!(
                !xml.contains(forbidden),
                "the task XML mentions {forbidden}. InteractiveToken needs a session, a \
                 <Password> would be stored in the scheduler, and HighestAvailable is the UAC \
                 prompt:\n{xml}"
            );
        }
    }

    #[test]
    fn the_task_fires_at_boot_and_at_logon_and_keeps_asking_in_between() {
        let xml = render_windows_task(&launcher_for(Target::Windows));
        let triggers = xml_block(&xml, "Triggers");

        let mut children = direct_children(triggers);
        children.sort();
        assert_eq!(
            children,
            ["BootTrigger", "LogonTrigger"],
            "the task needs BOTH: BootTrigger brings the daemon up with nobody logged in, \
             LogonTrigger covers the session that starts after a task that was disabled or \
             stopped. Only {children:?} found:\n{triggers}"
        );

        for name in ["BootTrigger", "LogonTrigger"] {
            assert!(
                xml_block(triggers, name).contains("<Enabled>true</Enabled>"),
                "{name} exists and is not enabled, which is the same as not having it:\n{triggers}"
            );
        }

        let boot = xml_block(triggers, "BootTrigger");
        assert!(
            boot.contains("<Repetition>") && boot.contains("<Interval>PT5M</Interval>"),
            "S-2: RestartOnFailure is unverified against a .cmd that exits non-zero, so the \
             boot trigger re-fires every five minutes. With MultipleInstancesPolicy=IgnoreNew \
             a repeat while the daemon lives starts nothing; without it a crashed daemon that \
             the scheduler did not restart stays down:\n{boot}"
        );
        assert!(
            boot.contains("<StopAtDurationEnd>false</StopAtDurationEnd>"),
            "the repetition must not stop after a duration, or the supervisor retires itself \
             and the daemon is left down for good:\n{boot}"
        );
    }

    #[test]
    fn the_task_restarts_without_a_practical_limit_and_never_duplicates_the_daemon() {
        let xml = render_windows_task(&launcher_for(Target::Windows));
        let settings = xml_block(&xml, "Settings");
        let restart = xml_block(settings, "RestartOnFailure");

        assert!(
            restart.contains("<Interval>PT1M</Interval>"),
            "one minute between restarts: shorter hammers a daemon that fails for want of a \
             resource that is not up yet, longer leaves the clients without it:\n{restart}"
        );
        assert!(
            restart.contains("<Count>999</Count>"),
            "999 is the largest count the schema takes. The old 3 meant a daemon that died four \
             times in a bad minute stayed dead until somebody noticed:\n{restart}"
        );
        assert!(
            settings.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"),
            "the BootTrigger repetition and the LogonTrigger both fire while the daemon lives; \
             anything but IgnoreNew starts a second daemon on the same port:\n{settings}"
        );
        assert!(
            settings.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "the daemon is meant to run forever; the scheduler's default limit would kill it \
             after three days:\n{settings}"
        );
    }

    #[test]
    fn a_real_render_does_not_lean_on_variables_that_mean_another_thing_for_system() {
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        let launcher_path = launcher_for(Target::Windows);
        let cmd = render_windows_launcher(&unit);
        let xml = render_windows_task(&launcher_path);

        for (name, body) in [("launcher", &cmd), ("task XML", &xml)] {
            for variable in ["%LOCALAPPDATA%", "%APPDATA%", "%USERPROFILE%", "%HOME%"] {
                assert!(
                    !body.contains(variable),
                    "the {name} of a real install holds {variable}. For SYSTEM that expands to \
                     C:\\Windows\\System32\\config\\systemprofile, where nothing is installed \
                     and nothing says so (S-3):\n{body}"
                );
            }
        }

        let commanded = xml_block(&xml, "Command").trim();
        assert_eq!(
            commanded,
            launcher_path.display().to_string(),
            "the task runs the absolute path of the .cmd --apply wrote, and nothing it has to \
             expand"
        );
    }

    #[tokio::test]
    async fn from_env_bakes_absolute_paths_and_the_users_home_into_a_windows_unit() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let scratch = Scratch::new("absolute-paths");
        let _pinned = pinned_install_root(&scratch);

        let unit = Unit::from_env(Profile::Loopback, Target::Windows, None, None)
            .expect("a loopback windows install needs a home and LOCALAPPDATA");

        assert_eq!(
            unit.home_dir, scratch.0,
            "S-4: the unit carries the home of the user who INSTALLS. SYSTEM has none, so \
             the launcher has to hand it over or the models, the NLI cache and the GPU probe \
             look under the wrong directory and the daemon starts deaf"
        );

        let cmd = render_windows_launcher(&unit);
        let xml = render_windows_task(&unit.env_file.with_file_name(LAUNCHER_BASENAME));
        for (name, body) in [("launcher", &cmd), ("task XML", &xml)] {
            for variable in ["%LOCALAPPDATA%", "%APPDATA%", "%USERPROFILE%", "%HOME%"] {
                assert!(
                    !body.contains(variable),
                    "from_env put {variable} in the {name} instead of the path it resolved \
                     (S-3). For SYSTEM it expands to a directory with nothing in it:\n{body}"
                );
            }
        }
    }

    #[test]
    fn the_launcher_sets_the_users_home_before_it_reads_the_env_file() {
        let unit = lan_unit(Target::Windows, &"a".repeat(64));
        let cmd = render_windows_launcher(&unit);
        let lines: Vec<&str> = cmd.lines().collect();

        let set_home = lines
            .iter()
            .position(|l| l.trim() == "set \"USERPROFILE=C:\\Users\\operador\"")
            .unwrap_or_else(|| {
                panic!(
                    "the launcher never sets USERPROFILE to the installing user's home \
                     (S-4). Under SYSTEM the model cache, nli, rerank and gpu all resolve \
                     their paths from it:\n{cmd}"
                )
            });
        let setlocal = lines
            .iter()
            .position(|l| l.trim() == "setlocal")
            .expect("the launcher scopes its variables with setlocal");
        let reads_env = lines
            .iter()
            .position(|l| l.trim_start().starts_with("for /f"))
            .expect("the launcher loads the env file with for /f");
        let runs = launch_line(&cmd, &unit);

        assert!(
            setlocal < set_home,
            "set before setlocal leaks USERPROFILE into the parent shell of whoever runs the \
             .cmd by hand:\n{cmd}"
        );
        assert!(
            set_home < reads_env,
            "the home goes BEFORE `for /f`, so the env file can still override it (an operator \
             who needs another profile directory writes USERPROFILE there). After it, the \
             launcher silently wins over the file it is supposed to obey:\n{cmd}"
        );
        assert!(
            reads_env < runs,
            "and the env file is read before the daemon starts:\n{cmd}"
        );
    }

    #[test]
    fn the_address_lives_in_the_env_file_and_the_launcher_and_the_service_do_not_repeat_it() {
        let windows = lan_unit(Target::Windows, &"a".repeat(64));
        let cmd = render_windows_launcher(&windows);
        let launch = cmd
            .lines()
            .nth(launch_line(&cmd, &windows))
            .expect("launch_line indexes a line of this launcher");
        let after_serve = launch
            .trim_start()
            .strip_prefix(&format!("\"{}\" serve", windows.exe.display()))
            .expect("launch_line found a line that starts this way");

        assert!(
            after_serve.trim_start().starts_with(">>"),
            "D-c: the launcher runs `\"exe\" serve` and nothing else but the redirect to the \
             log. An address after `serve` beats CUBA_HTTP_ADDR from the env file, which is how \
             `--apply` used to put a LAN install back on 127.0.0.1: `{launch}`"
        );

        let linux = lan_unit(Target::Linux, &"a".repeat(64));
        let service = render_systemd(&linux);
        let exec = service
            .lines()
            .find(|l| l.starts_with("ExecStart="))
            .expect("a unit with no ExecStart starts nothing");
        assert_eq!(
            exec,
            format!("ExecStart={} serve", linux.exe.display()),
            "S-5: the same drift on Linux. The address is read from the env file"
        );

        for (name, body) in [("launcher", &cmd), ("service", &service)] {
            assert!(
                !body.contains("192.168.0.10"),
                "the {name} names the LAN address. It is rendered into the env file and, for \
                 systemd, into the .socket; any other copy is the one that goes stale:\n{body}"
            );
        }

        let env_file = render_env_file(&windows);
        assert!(
            env_file
                .lines()
                .any(|l| l == "CUBA_HTTP_ADDR=192.168.0.10:8787"),
            "the env file is the one place the address is written for `serve`:\n{env_file}"
        );
        assert!(
            render_socket(&linux)
                .lines()
                .any(|l| l == "ListenStream=192.168.0.10:8787"),
            "a unit cannot read the env file, so the .socket keeps the address it listens on"
        );
    }

    #[test]
    fn the_launcher_appends_the_daemons_output_to_a_log_beside_the_env_file() {
        let unit = windows_unit_at_native_root();
        let cmd = render_windows_launcher(&unit);
        let log = native_root().join("memory-industry.log");
        let lines: Vec<&str> = cmd.lines().collect();
        let runs = launch_line(&cmd, &unit);

        assert!(
            lines[runs].ends_with(&format!(">> \"{}\" 2>&1", log.display())),
            "S-6: with no redirect, whatever the daemon prints under SYSTEM goes nowhere, and a \
             daemon that dies at startup leaves no trace. The log sits in the env file's \
             directory, appended, with stderr in it: `{}`",
            lines[runs]
        );
        assert_eq!(
            lines.get(runs + 1).map(|l| l.trim()),
            Some("exit /b %errorlevel%"),
            "the launcher has to leave with the daemon's own code: that is what reaches Task \
             Scheduler's Last Run Result and decides whether RestartOnFailure fires. Falling \
             off the end of a .cmd reports the redirect's success instead:\n{cmd}"
        );
    }

    #[test]
    fn the_launcher_rotates_the_log_at_five_mebibytes_before_it_starts_the_daemon() {
        let unit = windows_unit_at_native_root();
        let cmd = render_windows_launcher(&unit);
        let log = native_root().join("memory-industry.log");
        let runs = launch_line(&cmd, &unit);

        let rotation = cmd.lines().take(runs).find(|l| {
            l.split(|c: char| !c.is_ascii_digit())
                .any(|number| number == "5242880")
        });
        let Some(rotation) = rotation else {
            panic!(
                "no line before the daemon starts compares the log with 5242880 bytes (5 MiB). \
                 Without it the log grows for as long as the machine lives:\n{cmd}"
            );
        };
        let lowered = rotation.to_lowercase();

        assert!(
            lowered.contains("exist"),
            "`%%~zf` of a log that does not exist yet is empty and the comparison is a syntax \
             error, so the first start after an install would fail on its own rotation: \
             `{rotation}`"
        );
        assert!(
            lowered.contains("move /y"),
            "rotating is moving, and /Y is what lets it replace the previous .1 instead of \
             stopping to ask a question nobody answers under SYSTEM: `{rotation}`"
        );
        assert!(
            rotation.contains(&format!("\"{}\"", log.display()))
                && rotation.contains(&format!("\"{}.1\"", log.display())),
            "the log moves to <log>.1, in the same directory: `{rotation}`"
        );
    }

    #[test]
    fn the_launchers_refusal_is_written_to_the_log_and_still_exits_non_zero() {
        let unit = windows_unit_at_native_root();
        let cmd = render_windows_launcher(&unit);
        let log = native_root().join("memory-industry.log");
        let lines: Vec<&str> = cmd.lines().collect();

        let guard = lines
            .iter()
            .position(|l| l.trim_start().starts_with("if not exist"))
            .expect("the launcher guards against a missing env file");
        let refuses = lines
            .iter()
            .position(|l| l.trim() == "exit /b 1")
            .expect("the guard ends the script non-zero");
        assert!(guard < refuses, "the refusal follows its guard:\n{cmd}");

        let echoes: Vec<&&str> = lines[guard..refuses]
            .iter()
            .filter(|l| l.trim_start().starts_with("echo"))
            .collect();
        assert!(
            !echoes.is_empty(),
            "the guard says why it refuses; with no message the task just fails:\n{cmd}"
        );
        for echo in echoes {
            assert!(
                echo.contains(&format!(">> \"{}\"", log.display())),
                "S-6: this refusal is the one message that explains an env file that is not \
                 there, and under SYSTEM a console nobody reads is where it would go. Each echo \
                 of the guard is appended to the log: `{echo}`"
            );
        }
        assert_eq!(
            lines.get(refuses + 1).map(|l| l.trim()),
            Some(")"),
            "and the block still ends in `exit /b 1`:\n{cmd}"
        );
    }

    #[test]
    fn the_socket_may_be_opened_before_its_address_exists_only_when_that_address_is_routable() {
        let lan = render_socket(&lan_unit(Target::Linux, &"a".repeat(64)));
        assert!(
            section(&lan, "[Socket]").contains(&"FreeBind=yes"),
            "S-5/A8: at boot the LAN interface may not have its address yet, and without \
             FreeBind systemd cannot open the socket on it, so the daemon is down until a \
             human restarts the socket:\n{lan}"
        );

        let loopback = render_socket(&loopback_unit(Target::Linux));
        assert!(
            section(&loopback, "[Socket]").contains(&"ListenStream=127.0.0.1:8787"),
            "anchor: this really is the loopback socket:\n{loopback}"
        );
        assert!(
            !loopback.contains("FreeBind"),
            "127.0.0.1 always exists. FreeBind there only hides a typo in the address that \
             would otherwise fail at once:\n{loopback}"
        );
    }

    #[test]
    fn what_is_printed_is_what_register_would_run() {
        let root = native_root();
        let xml = root.join("memory-industry-task.xml").display().to_string();

        assert_eq!(
            schtasks_create_argv(&root.join("memory-industry-task.xml")),
            [
                "schtasks",
                "/Create",
                "/XML",
                xml.as_str(),
                "/TN",
                "memory-industry",
                "/F"
            ],
            "the argv `--register` runs: /F so a second run replaces the task instead of \
             prompting, and the name the QA steps (`schtasks /Query /TN memory-industry`) look \
             for"
        );
        assert_eq!(
            enable_command(Target::Windows, &root),
            format!("schtasks /Create /XML \"{xml}\" /TN memory-industry /F"),
            "the command that is printed is the argv that is run, quoted for a console; two \
             spellings are how the printed one stops working while `--register` still does"
        );

        assert_eq!(
            firewall_delete_argv(),
            [
                "netsh",
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                "name=memory-industry"
            ],
            "S-8: the delete goes first, so a second run and a changed port leave ONE rule"
        );

        let add = firewall_add_argv(8787);
        assert_eq!(
            add.iter().take(5).map(String::as_str).collect::<Vec<_>>(),
            ["netsh", "advfirewall", "firewall", "add", "rule"],
            "the add is a netsh advfirewall rule: {add:?}"
        );
        let mut properties: Vec<&str> = add.iter().skip(5).map(String::as_str).collect();
        properties.sort_unstable();
        assert_eq!(
            properties,
            [
                "action=allow",
                "dir=in",
                "localport=8787",
                "name=memory-industry",
                "profile=private,domain",
                "protocol=TCP"
            ],
            "inbound TCP on the daemon's port, allowed on private and domain networks only: \
             `profile=any` would open it on a café's wifi. Found {add:?}"
        );
    }

    #[test]
    fn the_firewall_is_opened_only_for_an_address_somebody_else_can_reach() {
        assert!(
            firewall_commands(&addr(DEFAULT_LOOPBACK_ADDR)).is_empty(),
            "nothing reaches 127.0.0.1 from outside, so there is no door to open (S-8)"
        );

        let commands = firewall_commands(&addr(ROUTABLE));
        assert_eq!(
            commands,
            [
                firewall_delete_argv().join(" "),
                firewall_add_argv(8787).join(" ")
            ],
            "[delete, add], in that order and spelled as the argv that runs: add first and the \
             delete removes the rule that was just made"
        );
        let delete = commands.first().cloned().unwrap_or_default();
        assert!(
            delete.starts_with("netsh advfirewall firewall delete rule"),
            "anchor: the list holds the netsh commands and not empty strings: {commands:?}"
        );
    }

    #[test]
    fn the_plan_prints_the_firewall_commands_only_for_a_routable_windows_install() {
        let root = native_root();
        let launcher = root.join(LAUNCHER_BASENAME);

        let lan = render_plan(
            &lan_unit(Target::Windows, &"a".repeat(64)),
            Target::Windows,
            &launcher,
        );
        let commands = firewall_commands(&addr(ROUTABLE));
        assert_eq!(commands.len(), 2, "anchor: a LAN address has two commands");
        for command in &commands {
            assert!(
                lan.contains(command.as_str()),
                "the plan has to print `{command}`: the operator reads the plan to see what \
                 `--register` is going to do to the firewall:\n{lan}"
            );
        }

        let loopback = render_plan(&loopback_unit(Target::Windows), Target::Windows, &launcher);
        assert!(
            !loopback.contains("netsh"),
            "a loopback plan opens no port, so it must not talk about the firewall:\n{loopback}"
        );
    }

    #[tokio::test]
    async fn a_windows_install_without_a_home_refuses_naming_both_variables() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let scratch = Scratch::new("no-home");
        let _local_app_data =
            crate::envs::ScopedEnv::set("LOCALAPPDATA", &scratch.0.display().to_string());
        let _home = crate::envs::ScopedEnv::cleared("HOME");
        let _profile = crate::envs::ScopedEnv::cleared("USERPROFILE");

        let said = Unit::from_env(Profile::Loopback, Target::Windows, None, None)
            .expect_err(
                "S-4: with LOCALAPPDATA and no home the old code installed anyway, and the \
                 daemon SYSTEM starts cannot find a single model. It has to refuse",
            )
            .to_string();

        for variable in ["HOME", "USERPROFILE"] {
            assert!(
                said.contains(variable),
                "the refusal has to name {variable} or the operator defines the wrong one: \
                 {said}"
            );
        }
    }

    // --- The flags: what `setup service` was asked for --------------------------

    /// `parse_request` over a literal command line. The real caller hands it the
    /// slice `main` collected, so the owned `Vec` is not an accident of the test.
    fn parse(args: &[&str]) -> Result<Asked> {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_request(&owned)
    }

    fn request(args: &[&str]) -> Request {
        let line = args.join(" ");
        match parse(args)
            .unwrap_or_else(|e| panic!("`setup service {line}` is a supported command line: {e}"))
        {
            Asked::Run(request) => request,
            Asked::Help => panic!("`setup service {line}` is not a request for the usage"),
        }
    }

    fn refusal(args: &[&str]) -> String {
        let line = args.join(" ");
        match parse(args) {
            Err(e) => e.to_string(),
            Ok(taken) => {
                panic!("`setup service {line}` had to be refused, and came back as {taken:?}")
            }
        }
    }

    #[test]
    fn nothing_after_the_command_is_a_plan_for_this_machine_on_loopback() {
        assert_eq!(
            request(&[]),
            Request {
                mode: Mode::Print,
                target: Target::host(),
                profile: Profile::Loopback,
                addr: None,
                token: None,
                out: None,
            },
            "`setup service` with nothing after it is the plan. A default of --apply installs a \
             daemon for somebody who was asking what it would do, and a default of --uninstall \
             takes one away"
        );
    }

    #[test]
    fn every_mode_flag_asks_for_the_mode_it_names() {
        for (flag, expected) in [
            ("--print", Mode::Print),
            ("--apply", Mode::Apply),
            ("--uninstall", Mode::Uninstall),
        ] {
            assert_eq!(
                request(&[flag]).mode,
                expected,
                "`{flag}` came back as another mode. The three do opposite things to a machine \
                 that is already installed, and nothing between the flag and the filesystem \
                 would say so"
            );
        }
    }

    #[test]
    fn two_modes_at_once_are_refused_naming_both_and_the_same_one_twice_is_not() {
        let said = refusal(&["--print", "--apply"]);
        for expected in ["--print", "--apply"] {
            assert!(
                said.contains(expected),
                "the refusal has to name both flags: whoever typed them meant one of the two and \
                 cannot tell from the outcome which one won: {said}"
            );
        }
        assert!(
            refusal(&["--apply", "--uninstall"]).contains("excluyentes"),
            "install and remove is the pair that matters most, and it is the same rule"
        );

        assert_eq!(
            request(&["--apply", "--apply"]).mode,
            Mode::Apply,
            "the same flag twice is not a conflict: a wrapper that appends --apply to a line \
             that already had it asked for one thing, and refusing it stops an install where \
             nothing is wrong"
        );
    }

    #[test]
    fn help_answers_before_anything_else_is_decided() {
        for flag in ["-h", "--help"] {
            assert_eq!(
                parse(&[flag]).expect("asking for the usage is not an error"),
                Asked::Help,
                "`{flag}` has to come back as the usage and nothing else: as a mode it would go \
                 through the exclusivity check, and `--print --help` would be refused as two \
                 modes"
            );
        }

        assert_eq!(
            parse(&["-h", "--nonsense"]).expect("-h answers first"),
            Asked::Help,
            "somebody asking what the flags are has, by definition, not got them right yet: \
             refusing the rest of their line answers nothing"
        );
        assert!(
            refusal(&["--nonsense", "-h"]).contains("--nonsense"),
            "and what comes before it is still read, or a trailing -h makes every typo silent"
        );
    }

    #[test]
    fn a_flag_with_nothing_after_it_says_what_it_needed() {
        for (flag, expected) in [
            ("--profile", "loopback"),
            ("--addr", "ip:puerto"),
            ("--token", "valor"),
            ("--out", "directorio"),
        ] {
            let said = refusal(&[flag]);
            assert!(
                said.contains(flag) && said.contains(expected),
                "`{flag}` with nothing after it has to say what it needed. One message for all \
                 four sends the operator to --help for the only thing it could have told them: \
                 {said}"
            );
        }
    }

    #[test]
    fn the_word_after_each_flag_lands_in_the_field_the_render_reads() {
        let token = "b".repeat(64);
        assert_eq!(
            request(&[
                "--windows",
                "--profile",
                "lan",
                "--addr",
                ROUTABLE,
                "--token",
                token.as_str(),
                "--out",
                "packaging",
            ]),
            Request {
                mode: Mode::Print,
                target: Target::Windows,
                profile: Profile::Lan,
                addr: Some(ROUTABLE.to_string()),
                token: Some(token.clone()),
                out: Some(PathBuf::from("packaging")),
            },
            "a word that lands in the wrong field is an install nobody asked for: the address \
             taken as the token is a daemon behind 17 characters the daemon itself refuses, and \
             the token taken as the address fails naming the flag that was right"
        );

        assert_eq!(
            request(&["--linux"]).target,
            Target::Linux,
            "`--linux` on a Windows host is how packaging/ gets its systemd unit; taken as the \
             host it would render the Task Scheduler XML instead"
        );
    }

    #[test]
    fn an_unknown_flag_is_quoted_back_instead_of_ignored() {
        assert!(
            refusal(&["--profil", "lan"]).contains("--profil"),
            "a flag this command does not have has to be named. Ignored, `--profil lan` installs \
             the loopback default and reports success, and the operator finds out when the other \
             machines cannot reach it"
        );
    }

    #[test]
    fn a_profile_is_refused_where_it_was_typed() {
        let said = refusal(&["--profile", "lanparty", "--nonsense"]);
        assert!(
            said.contains("lanparty"),
            "the first thing wrong on the line is the one to answer. Parsing the profile only \
             after the whole line is read blames a flag that came later, and the operator goes \
             and fixes the wrong one: {said}"
        );
    }

    #[test]
    fn an_out_directory_only_goes_with_the_plan() {
        for args in [
            ["--apply", "--out", "packaging"],
            ["--uninstall", "--out", "packaging"],
            // Whichever order they were typed in: it is the pair that is
            // refused, not the sequence.
            ["--out", "packaging", "--apply"],
        ] {
            let said = refusal(&args);
            assert!(
                said.contains("--out") && said.contains("--print"),
                "--apply and --uninstall work in the install root. A --out they accepted and \
                 ignored leaves the operator reading a directory nothing was installed into: \
                 {said}"
            );
        }

        assert_eq!(
            request(&["--print", "--out", "packaging"]).out,
            Some(PathBuf::from("packaging")),
            "with --print it is the supported pair: it is how the files under packaging/ are \
             regenerated"
        );
    }

    // --- What --apply and --uninstall report and touch ---------------------------

    #[test]
    fn what_apply_reports_tells_a_kept_secret_from_a_written_file() {
        let path = PathBuf::from("/srv/mi/memory-industry.env");

        let restricted = wrote_line(&path, Wrote::Created { restricted: true });
        assert!(
            restricted.contains("escrito") && restricted.contains("0600"),
            "the installer took this file away from every other account on the box; not saying \
             so makes the operator go and check by hand: {restricted}"
        );

        let plain = wrote_line(&path, Wrote::Created { restricted: false });
        assert!(
            plain.contains("escrito") && !plain.contains("0600"),
            "and claiming 0600 where nothing was restricted is worse than saying nothing at all: \
             {plain}"
        );

        let kept = wrote_line(&path, Wrote::Kept);
        assert!(
            kept.contains("conservado") && kept.contains("token") && kept.contains("DATABASE_URL"),
            "«conservado» is the only signal that the token every client is already configured \
             with survived a second --apply. Reported as «escrito», the operator goes looking \
             for a rotation that never happened: {kept}"
        );

        for line in [restricted, plain, kept] {
            assert!(
                line.contains("memory-industry.env"),
                "every line has to name the file it is about, or a run over four files is four \
                 verdicts with nothing attached to them: {line}"
            );
        }
    }

    #[test]
    fn uninstall_takes_away_what_apply_wrote_except_the_file_that_carries_the_token() {
        let scratch = Scratch::new("removable");

        for target in [Target::Linux, Target::Windows] {
            let unit = lan_unit(target, &"c".repeat(64));
            let root = scratch.join(if matches!(target, Target::Windows) {
                "win"
            } else {
                "linux"
            });

            let written: Vec<PathBuf> = write_all(&unit, target, &root)
                .expect("write_all writes into a scratch root")
                .into_iter()
                .map(|(path, _)| path)
                .collect();
            let removable = removable_files(&unit, target, &root);

            assert!(
                !removable.is_empty(),
                "an --uninstall that removes nothing leaves the unit and the task pointing at a \
                 binary the operator was told had been taken away"
            );

            let env_file = root.join(ENV_BASENAME);
            let left: Vec<&PathBuf> = written
                .iter()
                .filter(|path| !removable.contains(path))
                .collect();
            assert_eq!(
                left,
                vec![&env_file],
                "--apply writes N files and --uninstall has to take away N-1. The one left is the \
                 env file, and it is the only artifact here that cannot be regenerated: it \
                 carries the token and the DATABASE_URL every client already uses"
            );

            for path in &removable {
                assert!(
                    written.contains(path),
                    "{} is removed by --uninstall and written by nothing, so what --uninstall \
                     deletes is a file some other install put there",
                    path.display()
                );
            }
        }
    }
}
