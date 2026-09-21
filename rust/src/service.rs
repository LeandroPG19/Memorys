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
const ENV_BASENAME: &str = "memory-industry.env";

/// The launcher basename, used by `write_all` and by `--out` alike. One
/// expression, never two: if the XML's `<Command>` and the file `--apply` puts
/// on disk could disagree, the installer itself could ship a task pointing at a
/// `.cmd` that is not there — and on Windows nothing reports that.
const LAUNCHER_BASENAME: &str = "memory-industry.cmd";

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

    // The numbers are placeholders: only which keys come out depends on the
    // booleans and on `gpu_mem_limit_mb` being present.
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
    let everything_off = Plan {
        reranker: false,
        reranker_on_gpu: false,
        nli: false,
        gpu_mem_limit_mb: None,
        gpu_mem_floor_mb: None,
        ..everything_on.clone()
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
        let destination = if name.ends_with(".env.example") {
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

fn basename(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap_or(relative)
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

#[cfg(unix)]
fn restrict(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("no se pudo restringir {}", path.display()))?;
    Ok(true)
}

/// AppData is per user and there is no portable chmod. What we do NOT do comes
/// back as a value: a silent omission cannot be told apart from an oversight.
#[cfg(windows)]
fn restrict(_path: &Path) -> Result<bool> {
    Ok(false)
}

pub fn write_all(u: &Unit, target: Target, root: &Path) -> Result<Vec<(PathBuf, Wrote)>> {
    let launcher = root.join(LAUNCHER_BASENAME);
    let mut written = Vec::new();

    for (relative, body) in rendered_files(u, target.is_windows(), &launcher) {
        let name = basename(&relative);
        // The example is what the repository publishes; what an install gets is
        // the real env file, and it is the one that must survive a second
        // --apply.
        let (path, secrecy) = if name.ends_with(".env.example") {
            (root.join(ENV_BASENAME), Secrecy::Secret)
        } else {
            (root.join(name), Secrecy::Plain)
        };
        let wrote = write_one(&path, &body, secrecy)?;
        written.push((path, wrote));
    }
    Ok(written)
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
        }
    }

    fn launcher_for(target: Target) -> PathBuf {
        PathBuf::from(if matches!(target, Target::Windows) {
            "C:\\Users\\operador\\AppData\\Local\\MemoryIndustry\\memory-industry.cmd"
        } else {
            "/home/operador/.local/bin/memory-industry"
        })
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
                !w.is_empty()
                    && w.starts_with(|c: char| c.is_ascii_uppercase())
                    && w.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
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
            "a per-user task needs no administrator, and asking for one turns a silent install \
             into a UAC prompt the operator has to be there to answer:\n{xml}"
        );
        assert!(
            !xml.contains("HighestAvailable"),
            "nothing here needs elevation:\n{xml}"
        );
    }
}
