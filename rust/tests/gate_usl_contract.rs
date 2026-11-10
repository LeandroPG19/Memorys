//! Contract for `scripts/gate_usl.py`, the USL calibration tool of the local judge (CI-F5a), and for
//! `usl_point` in `scripts/gate_history.py`. Spec: `.cursor/handoffs/ci-rapido-F5.spec.md`, F5a (U1-U17).
//!
//! The tool never runs the judge: `barrido` compiles under a fake `cargo` put first in the `PATH`,
//! `ajustar` and `recomendar` read JSONL fixtures. Every scenario builds its world in a temporary
//! directory: a git repo with `rust/src/lib.rs` and copies of the scripts the tool imports, a
//! `CI_TIEMPOS_DIR`, a `CARGO_TARGET_DIR` and a pressure file (`GATE_USL_PSI`). Nothing touches the
//! real history, the real target or HOME. Every process the test starts has a deadline.
//!
//! The interface these tests drive (standard library only, `python3 -I scripts/gate_usl.py <order>`):
//! - `barrido --perfil {debug,bin,release} [--j 2,4,..] [--reps 2]`: one JSON line per point on stdout
//!   and in `usl.jsonl` (fields: repo, barrido, perfil, fila, J, rep, orden, seg, cpu_s, rss_pico, rc,
//!   psi_ini, psi_fin, load, cargado, sha, huella, arbol_sucio, C, fecha; `orden` is the 0-based
//!   position inside the replica), then `barrido <id>: <n> puntos, <k> cargados, <m> rojos`. A warm-up
//!   of the whole profile at the highest J comes first and writes nothing. Exit 2: unknown profile,
//!   no `CARGO_TARGET_DIR`, machine busy (`some avg10` of `GATE_USL_PSI` above 15 %), a red or timed
//!   out warm-up (`GATE_USL_PLAZO_S`, 1800 s per cargo order by default) or a `huella` that moved.
//! - `ajustar --perfil P [--puntos JSONL] [--json]`: exit 0 reliable, 3 not reliable (the reason in
//!   `motivo`), 2 unreadable input. `C` is the smallest `C` of the points.
//! - `recomendar [--tabla T] [--json]`: exit 0 when it recommended (even "se conserva"), 3 when an
//!   adjustment or a cold run is missing (`faltan`), 2 when the table or the history cannot be read
//!   or the history has no `R`. JSON: L, filas{id: d, L_j, h, h_L, estado}, juez_cuda, faltan,
//!   calibradas{fila: perfil, J_actual, J_propuesto, propone, ganancia_s, J_hist, runs, d_hist,
//!   d_por_J}, T_sim_actual, T_sim_propuesto, cpu_cap{C, N_estrella, propuesto}, diff, se_conserva, notas.
//! - Library: `rodilla(lam, sigma, kappa, J_min, J_max, C)`; `gate_history.usl_point(registro)`.
//!
//! Sabotage -> assertion (each one is a change in production code that turns the named test red):
//! - lambda fixed instead of profiled -> U1 (`sigma`, `rmse_rel`); no kappa term -> U1 (`N_estrella`);
//!   `J_top` without `min(C, ..)` -> U3; `N* = sqrt(sigma/kappa)` -> U1 (`N_estrella`).
//! - no spread criterion -> U4 "spread"; no stability criterion -> U4 "unstable"; no `cargado` filter -> U5.
//! - replica 1 not reversed -> U7 "replica 1 order"; `CARGO_BUILD_JOBS` set once -> U7 (first measured
//!   call); no `CARGO_INCREMENTAL=0` -> U7; no `os.utime` -> U8 "mtime"; the file rewritten -> U8 "huella".
//! - the pressure read after the warm-up -> U9 (a cargo call); a loaded point not marked -> U10.
//! - `usl_point` that raises -> U11; the write error of the history not caught -> U11.
//! - B3 without the cold filter -> U12; the slack from `L_in` alone -> U12; juez-cuda by run count -> U13.
//! - the knee chosen without simulating -> U14; a simulator that ignores `c` -> U14; a gain threshold of 0 -> U14.
//! - `J_hist` read from today's table -> U16; a command of a profile that drifted from `run-all-tests.sh` -> U17.
//! - a wait without a deadline -> U18.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A fixture process that is still running after this long is killed and the test fails.
const PLAZO: Duration = Duration::from_secs(60);

/// R of the 19th run: 32 GiB less the reserve (spec 0.1).
const R_BYTES: u64 = 34_319_294_464;
const C_NUCLEOS: u32 = 24;
const PSI_QUIETA: &str = "some avg10=1.00 avg60=0.00 avg300=0.00 total=0\n";
const PSI_OCUPADA: &str = "some avg10=40.00 avg60=0.00 avg300=0.00 total=0\n";

/// The judge's table with the figures that matter here (DBG at `{DBG}` jobs, B3 at 12 cores).
const TABLA: &str = "\
10 P - - normal 2 2 20 bash scripts/merge-gate.sh --step P
20 DBG P target:T_dbg,cluster:C_dbg,models normal 8 8 421 CARGO_BUILD_JOBS={DBG} CUBA_EMBED_INTRA_THREADS=4 CUBA_NLI_INTRA_THREADS=4 CUBA_RERANK_INTRA_THREADS=8 bash scripts/merge-gate.sh --step DBG
25 NLI DBG target:T_dbg,cluster:C_nli,models,reader exclusive 8 8 60 CARGO_BUILD_JOBS=8 CUBA_EMBED_INTRA_THREADS=4 CUBA_NLI_INTRA_THREADS=4 CUBA_RERANK_INTRA_THREADS=8 bash scripts/merge-gate.sh --step NLI
30 B3 P target:T_rel normal 12 16 150 CARGO_BUILD_JOBS=12 bash scripts/merge-gate.sh --step B3
40 REL B3 target:T_rel,cluster:C_relh,models,reader normal 8 12 221 CARGO_BUILD_JOBS=2 CUBA_EMBED_INTRA_THREADS=4 CUBA_NLI_INTRA_THREADS=4 CUBA_RERANK_INTRA_THREADS=8 bash scripts/merge-gate.sh --step REL
50 E2E B3,DBG target:T_rel,cluster:C_e2e,models,reader normal 8 12 113 CARGO_BUILD_JOBS=2 bash scripts/merge-gate.sh --step E2E
60 DC P target:T_docs,cluster:C_docs normal 8 8 325 CARGO_BUILD_JOBS=8 bash scripts/merge-gate.sh --step DC
70 CV P target:T_cov,cluster:C_cov untimed 4 4 45 CARGO_BUILD_JOBS=4 bash scripts/merge-gate.sh --step CV
80 M P target:T_mut,cluster:C_mut untimed 8 8 136 CARGO_BUILD_JOBS=8 MUTANTS_JOBS=6 bash scripts/merge-gate.sh --step M
";

/// A `cargo` that notes `jobs incremental pwd target mtime-of-lib.rs args` (tab separated), sleeps 0.05 s
/// (`USL_FAKE_SLEEP` changes it) and exits `USL_FAKE_RC`. With `USL_FAKE_PSI_ON=n` the n-th call writes
/// a loaded pressure into `GATE_USL_PSI` and every other call writes a quiet one.
const CARGO_FALSO: &str = r#"#!/bin/sh
d="$USL_FAKE_DIR"
n=$(cat "$d/n" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$d/n"
if [ -n "$USL_FAKE_PSI_ON" ]; then
  if [ "$n" = "$USL_FAKE_PSI_ON" ]; then
    echo "some avg10=30.00 avg60=0.00 avg300=0.00 total=0" > "$GATE_USL_PSI"
  else
    echo "some avg10=1.00 avg60=0.00 avg300=0.00 total=0" > "$GATE_USL_PSI"
  fi
fi
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$CARGO_BUILD_JOBS" "$CARGO_INCREMENTAL" "$PWD" "$CARGO_TARGET_DIR" "$(stat -c %y src/lib.rs 2>/dev/null)" "$*" >> "$USL_FAKE_LOG"
sleep "${USL_FAKE_SLEEP:-0.05}"
exit "${USL_FAKE_RC:-0}"
"#;

const CAMPOS_PUNTO: [&str; 20] = [
    "repo",
    "barrido",
    "perfil",
    "fila",
    "J",
    "rep",
    "orden",
    "seg",
    "cpu_s",
    "rss_pico",
    "rc",
    "psi_ini",
    "psi_fin",
    "load",
    "cargado",
    "sha",
    "huella",
    "arbol_sucio",
    "C",
    "fecha",
];

// ---------------------------------------------------------------------------------------------
// the world of a scenario

struct Salida {
    code: Option<i32>,
    out: String,
    err: String,
}

impl Salida {
    fn text(&self) -> String {
        format!(
            "exit {:?}\nstdout:\n{}stderr:\n{}",
            self.code, self.out, self.err
        )
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.out)
            .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e})\n{}", self.text()))
    }
}

struct Llamada {
    jobs: String,
    incr: String,
    pwd: String,
    target: String,
    mtime: String,
    args: String,
}

struct Fx {
    base: PathBuf,
    tree: PathBuf,
    home: PathBuf,
    hist: PathBuf,
    bin: PathBuf,
    fake: PathBuf,
    psi: PathBuf,
    destinos: PathBuf,
    sin_cgroup: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        assert!(
            scripts().join("gate_usl.py").is_file(),
            "scripts/gate_usl.py does not exist: F5a pass 2 writes it, and every scenario of gate_usl_contract needs it"
        );
        let base = scratch(name);
        let tree = base.join("arbol");
        let fx = Fx {
            home: base.join("home"),
            hist: base.join("historial"),
            bin: base.join("bin"),
            fake: base.join("fake"),
            psi: base.join("psi-cpu"),
            destinos: base.join("destinos"),
            sin_cgroup: base.join("cgroup-propio"),
            tree,
            base,
        };
        for dir in [
            fx.tree.join("scripts"),
            fx.tree.join("rust/src"),
            fx.home.clone(),
            fx.bin.clone(),
            fx.fake.clone(),
            fx.destinos.clone(),
        ] {
            fs::create_dir_all(&dir).expect("fixture dir");
        }
        for fichero in [
            "gate_usl.py",
            "gate_history.py",
            "gate-dag.py",
            "gate-lock.sh",
        ] {
            fs::copy(
                scripts().join(fichero),
                fx.tree.join("scripts").join(fichero),
            )
            .unwrap_or_else(|e| panic!("scripts/{fichero}: {e}"));
        }
        write(&fx.tree.join("rust/src/lib.rs"), "pub fn f() {}\n");
        write(&fx.psi, PSI_QUIETA);
        write(&fx.sin_cgroup, "");
        write(&fx.bin.join("cargo"), CARGO_FALSO);
        let permisos = fs::Permissions::from_mode(0o755);
        fs::set_permissions(fx.bin.join("cargo"), permisos).expect("cargo falso ejecutable");
        git(&fx.tree, &["init", "-q"]);
        fx.commit("fixture");
        fx
    }

    fn commit(&self, mensaje: &str) {
        git(&self.tree, &["add", "-A"]);
        git(&self.tree, &["commit", "-q", "-m", mensaje]);
    }

    fn comando(&self, args: &[&str], extra: &[(&str, Option<&str>)]) -> Command {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("python3");
        cmd.arg("-I")
            .arg("-B")
            .arg(self.tree.join("scripts/gate_usl.py"))
            .args(args)
            .current_dir(&self.base)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("HOME", &self.home)
            .env("TMPDIR", &self.base)
            .env("PATH", path)
            .env("CI_TIEMPOS_DIR", &self.hist)
            .env("CARGO_TARGET_DIR", &self.destinos)
            .env("GATE_USL_PSI", &self.psi)
            .env("GATE_PROC_SELF_CGROUP", &self.sin_cgroup)
            .env("USL_FAKE_DIR", &self.fake)
            .env("USL_FAKE_LOG", self.fake.join("llamadas.log"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for (nombre, valor) in extra {
            match valor {
                Some(v) => cmd.env(nombre, v),
                None => cmd.env_remove(nombre),
            };
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Salida {
        ejecutar(self.comando(args, &[]))
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, Option<&str>)]) -> Salida {
        ejecutar(self.comando(args, extra))
    }

    fn llamadas(&self) -> Vec<Llamada> {
        let cuerpo = fs::read_to_string(self.fake.join("llamadas.log")).unwrap_or_default();
        cuerpo
            .lines()
            .map(|linea| {
                let c: Vec<&str> = linea.split('\t').collect();
                assert_eq!(c.len(), 6, "a line of the fake cargo log: {linea:?}");
                Llamada {
                    jobs: c[0].to_string(),
                    incr: c[1].to_string(),
                    pwd: c[2].to_string(),
                    target: c[3].to_string(),
                    mtime: c[4].to_string(),
                    args: c[5].to_string(),
                }
            })
            .collect()
    }

    /// Every line of every `usl.jsonl` of the history, days in order.
    fn usl(&self) -> Vec<Value> {
        let raiz = self.hist.join("memory-industry");
        let mut dias: Vec<PathBuf> = fs::read_dir(&raiz)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        dias.sort();
        let mut lineas = Vec::new();
        for dia in dias {
            let cuerpo = fs::read_to_string(dia.join("usl.jsonl")).unwrap_or_default();
            lineas.extend(cuerpo.lines().map(|l| {
                serde_json::from_str::<Value>(l)
                    .unwrap_or_else(|e| panic!("usl.jsonl is not JSON lines ({e}): {l}"))
            }));
        }
        lineas
    }

    /// The tree's own key under `CARGO_TARGET_DIR` (`gate-dag.py: target_dir`).
    fn clave_del_arbol(&self) -> String {
        let real = fs::canonicalize(&self.tree).expect("canonical tree");
        let nombre = real
            .file_name()
            .and_then(|n| n.to_str())
            .expect("tree name");
        let hash = format!(
            "{:x}",
            Sha256::digest(real.to_str().expect("utf-8").as_bytes())
        );
        format!("{nombre}-{}", &hash[..12])
    }
}

fn ejecutar(mut cmd: Command) -> Salida {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut hijo = cmd.spawn().expect("python3 -I gate_usl.py");
    let mut so = hijo.stdout.take().expect("stdout");
    let mut se = hijo.stderr.take().expect("stderr");
    let (tx_o, rx_o) = mpsc::channel::<String>();
    let (tx_e, rx_e) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        let _ = tx_o.send(s);
    });
    thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        let _ = tx_e.send(s);
    });
    let limite = Instant::now() + PLAZO;
    let estado = loop {
        if let Some(estado) = hijo.try_wait().expect("try_wait") {
            break estado;
        }
        assert!(
            Instant::now() < limite,
            "the process was still running after {PLAZO:?}; killed: {cmd:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    // A grandchild that kept the pipes open must not hang the test either.
    let out = rx_o
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_default();
    let err = rx_e
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_default();
    Salida {
        code: estado.code(),
        out,
        err,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let res = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        res.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&res.stderr)
    );
}

fn scripts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("scripts")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-usl-{name}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

fn cerca(real: f64, esperado: f64, tolerancia: f64, que: &str) {
    assert!(
        (real - esperado).abs() <= tolerancia,
        "{que}: {real} is not within {tolerancia} of {esperado}"
    );
}

/// The `huella` of the fixture tree, asked to `gate_history` itself.
fn huella(fx: &Fx) -> String {
    let res = Command::new("python3")
        .arg("-I")
        .arg("-B")
        .arg("-c")
        .arg("import sys\nsys.path.insert(0, sys.argv[1])\nimport gate_history\nprint(gate_history.huella(sys.argv[2]))")
        .arg(fx.tree.join("scripts"))
        .arg(&fx.tree)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("python3 -I -c huella");
    assert!(
        res.status.success(),
        "huella: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    String::from_utf8_lossy(&res.stdout).trim().to_string()
}

/// `python3 -I -c` over the library `gate_usl` of the fixture tree.
fn biblioteca(fx: &Fx, cuerpo: &str) -> Salida {
    let codigo = format!(
        "import sys, json\nsys.path.insert(0, sys.argv[1])\nimport gate_usl as g\n{cuerpo}"
    );
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg("-B")
        .arg("-c")
        .arg(codigo)
        .arg(fx.tree.join("scripts"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HOME", &fx.home)
        .env("CI_TIEMPOS_DIR", &fx.hist);
    ejecutar(cmd)
}

// ---------------------------------------------------------------------------------------------
// curves and points

#[derive(Clone, Copy)]
struct Curva {
    lambda: f64,
    sigma: f64,
    kappa: f64,
}

impl Curva {
    fn x(&self, j: u32) -> f64 {
        let j = f64::from(j);
        self.lambda * j / (1.0 + self.sigma * (j - 1.0) + self.kappa * j * (j - 1.0))
    }

    fn t(&self, j: u32) -> f64 {
        1.0 / self.x(j)
    }
}

/// The curve of the spec's example (U1).
const CURVA_A: Curva = Curva {
    lambda: 1.0 / 240.0,
    sigma: 0.06,
    kappa: 0.0008,
};
const NIVELES_A: [u32; 7] = [2, 4, 6, 8, 12, 16, 24];

fn punto(perfil: &str, fila: &str, j: u32, rep: u32, seg: f64) -> Value {
    json!({
        "repo": "memory-industry", "barrido": "fixture", "perfil": perfil, "fila": fila,
        "J": j, "rep": rep, "orden": 0, "seg": seg, "cpu_s": null, "rss_pico": null,
        "rc": 0, "psi_ini": 1.0, "psi_fin": 1.0, "load": 0.5, "cargado": false,
        "sha": "fixture", "huella": "fixture", "arbol_sucio": false, "C": C_NUCLEOS,
        "fecha": "2026-11-09T12:00:00+00:00"
    })
}

/// The points of a curve at some levels. `modifica(level index, rep, J, T)` returns the T written.
fn puntos_de<F>(
    perfil: &str,
    fila: &str,
    curva: Curva,
    niveles: &[u32],
    reps: u32,
    modifica: F,
) -> Vec<Value>
where
    F: Fn(usize, u32, u32, f64) -> f64,
{
    let mut puntos = Vec::new();
    for (i, &j) in niveles.iter().enumerate() {
        for rep in 0..reps {
            puntos.push(punto(perfil, fila, j, rep, modifica(i, rep, j, curva.t(j))));
        }
    }
    puntos
}

fn exacta(perfil: &str, fila: &str, curva: Curva, niveles: &[u32]) -> Vec<Value> {
    puntos_de(perfil, fila, curva, niveles, 2, |_, _, _, t| t)
}

fn con_rss(puntos: &mut [Value], base: u64, por_job: u64) {
    for p in puntos.iter_mut() {
        let j = p["J"].as_u64().expect("J");
        p["rss_pico"] = json!(base + por_job * j);
    }
}

/// `ajustar --json` over a points file.
fn ajustar(fx: &Fx, perfil: &str, puntos: &[Value]) -> (Salida, Value) {
    let fichero = fx.base.join(format!(
        "puntos-{}.jsonl",
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let cuerpo: String = puntos.iter().map(|p| format!("{p}\n")).collect();
    write(&fichero, &cuerpo);
    let ruta = fichero.to_str().expect("utf-8 path");
    let salida = fx.run(&["ajustar", "--perfil", perfil, "--puntos", ruta, "--json"]);
    let v = salida.json();
    (salida, v)
}

// ---------------------------------------------------------------------------------------------
// U1-U6: the fit

#[test]
fn u1_recupera_los_parametros_de_una_curva_conocida() {
    let fx = Fx::new("u1");
    let (salida, v) = ajustar(&fx, "debug", &exacta("debug", "DBG", CURVA_A, &NIVELES_A));
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    assert_eq!(v["fiable"], json!(true), "{}", salida.text());
    assert_eq!(v["puntos"], json!(14));
    assert_eq!(v["niveles"].as_array().map(Vec::len), Some(7));
    cerca(num(&v["sigma"]), 0.06, 0.005, "sigma");
    let kappa = num(&v["kappa"]) / 0.0008;
    assert!((0.9..=1.1).contains(&kappa), "kappa / 0.0008 = {kappa}");
    let lambda = num(&v["lambda"]) * 240.0;
    assert!((0.98..=1.02).contains(&lambda), "lambda * 240 = {lambda}");
    let n_estrella = num(&v["N_estrella"]);
    assert!(
        (32.0..=37.0).contains(&n_estrella),
        "N_estrella = {n_estrella}"
    );
    assert_eq!(v["J_top"], json!(24));
    assert_eq!(v["J_rodilla"], json!(20), "{}", salida.text());
    assert!(num(&v["rmse_rel"]) <= 0.001, "rmse_rel = {}", v["rmse_rel"]);
    assert!(
        v["motivo"].is_null(),
        "a reliable fit has no reason: {}",
        v["motivo"]
    );
    cerca(num(&v["T_hat"]["8"]), CURVA_A.t(8), 0.1, "T_hat(8)");
}

#[test]
fn u2_con_ruido_de_medio_por_ciento_sigue_fiable_y_la_rodilla_apenas_se_mueve() {
    let fx = Fx::new("u2");
    let ruidosa = puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |i, rep, _, t| {
        let signo = if i.is_multiple_of(2) { 1.0 } else { -1.0 };
        let por_replica = if rep == 0 { 1.0 } else { -1.0 };
        t * (1.0 + 0.005 * signo * por_replica)
    });
    let (salida, v) = ajustar(&fx, "debug", &ruidosa);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    assert_eq!(v["fiable"], json!(true), "{}", salida.text());
    let rodilla = v["J_rodilla"].as_u64().expect("J_rodilla");
    assert!((18..=22).contains(&rodilla), "J_rodilla = {rodilla}");
    cerca(num(&v["sigma"]), 0.06, 0.03, "sigma");

    // One point of J_min 4 % off must not drag the fit: lambda is profiled, not read off a single point.
    let atipica = puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |i, rep, _, t| {
        if i == 0 && rep == 0 { t * 1.04 } else { t }
    });
    let (salida, v) = ajustar(&fx, "debug", &atipica);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    assert!(
        num(&v["rmse_rel"]) <= 0.012,
        "rmse_rel with an outlier = {}",
        v["rmse_rel"]
    );
    cerca(num(&v["sigma"]), 0.06, 0.01, "sigma with an outlier");
}

#[test]
fn u3_la_rodilla_y_n_estrella_salen_de_sus_formulas() {
    let fx = Fx::new("u3");
    let salida = biblioteca(
        &fx,
        "print(json.dumps([g.rodilla(1/240, 0.06, 0.0008, 2, 24, 24), g.rodilla(1/240, 0.06, 0.0008, 2, 24, 16)]))",
    );
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let rodillas: Vec<u64> = serde_json::from_str(&salida.out).expect("two knees");
    assert_eq!(rodillas[0], 20, "C = 24");
    // With C = 16 the knee is the smallest J whose X reaches 95 % of X(16), by the formula.
    let esperada = (2..=16)
        .find(|&j| CURVA_A.x(j) >= 0.95 * CURVA_A.x(16))
        .expect("a knee below 16");
    assert_eq!(rodillas[1], u64::from(esperada), "C = 16");

    let sin_coherencia = Curva {
        lambda: 1.0 / 240.0,
        sigma: 0.06,
        kappa: 0.0,
    };
    let niveles = [2, 4, 6, 8, 12, 16];
    let (_, v) = ajustar(
        &fx,
        "debug",
        &exacta("debug", "DBG", sin_coherencia, &niveles),
    );
    assert!(
        v["N_estrella"].is_null(),
        "kappa = 0 has no N*: {}",
        v["N_estrella"]
    );
    assert_eq!(v["J_top"], json!(16), "J_top = min(C, J_max measured) = 16");
}

#[test]
fn u4_no_fiable_por_cada_criterio_con_su_motivo_y_exit_3() {
    let fx = Fx::new("u4");
    let casos: Vec<(&str, Vec<Value>, &str)> = vec![
        (
            "levels",
            exacta("debug", "DBG", CURVA_A, &[2, 4, 8, 16]),
            "4 J levels, need 5",
        ),
        (
            "spread",
            // 22.2 % slower makes the spread (max - min) / median of the two replicas exactly 20 %.
            puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |_, rep, j, t| {
                if rep == 1 && j == 8 {
                    t * 11.0 / 9.0
                } else {
                    t
                }
            }),
            "spread 20",
        ),
        (
            "rmse",
            puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |i, _, _, t| {
                t * (1.0 + 0.15 * if i.is_multiple_of(2) { 1.0 } else { -1.0 })
            }),
            "rmse_rel",
        ),
        (
            "domain",
            puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |_, _, j, _| {
                100.0 * f64::from(j)
            }),
            "domain",
        ),
        (
            "unstable",
            // J = 24 slower by 30 %: the fit with it and the fit without it put the knee far apart.
            puntos_de("debug", "DBG", CURVA_A, &NIVELES_A, 2, |_, _, j, t| {
                if j == 24 { t * 1.3 } else { t }
            }),
            "unstable",
        ),
    ];
    for (nombre, puntos, motivo) in casos {
        let (salida, v) = ajustar(&fx, "debug", &puntos);
        assert_eq!(salida.code, Some(3), "{nombre}: {}", salida.text());
        assert_eq!(v["fiable"], json!(false), "{nombre}: {}", salida.text());
        let razon = v["motivo"].as_str().unwrap_or("");
        assert!(
            razon.contains(motivo),
            "{nombre}: the reason {razon:?} has to contain {motivo:?}"
        );
    }
}

#[test]
fn u5_los_puntos_rojos_o_cargados_no_entran() {
    let fx = Fx::new("u5");
    let mut puntos = exacta("debug", "DBG", CURVA_A, &NIVELES_A);
    let mut rojo = punto("debug", "DBG", 4, 2, 999.0);
    rojo["rc"] = json!(101);
    let mut cargado = punto("debug", "DBG", 12, 2, 1.0);
    cargado["cargado"] = json!(true);
    puntos.push(rojo);
    puntos.push(cargado);
    let (salida, v) = ajustar(&fx, "debug", &puntos);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    assert_eq!(v["excluidos"], json!({"rc": 1, "cargado": 1}));
    assert_eq!(v["puntos"], json!(14));
    cerca(num(&v["sigma"]), 0.06, 0.005, "sigma");
    assert!(num(&v["rmse_rel"]) <= 0.001, "rmse_rel = {}", v["rmse_rel"]);
    assert_eq!(v["J_rodilla"], json!(20));
}

#[test]
fn u6_la_recta_de_ram() {
    let fx = Fx::new("u6");
    let gib: u64 = 1 << 30;
    let mut puntos = exacta("debug", "DBG", CURVA_A, &NIVELES_A);
    con_rss(&mut puntos, 2 * gib, gib / 2);
    let (salida, v) = ajustar(&fx, "debug", &puntos);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    cerca(num(&v["ram"]["r0"]), 2.0 * gib as f64, 1.0, "ram.r0");
    cerca(num(&v["ram"]["r1"]), 0.5 * gib as f64, 1.0, "ram.r1");
}

#[test]
fn ajustar_con_una_entrada_ilegible_sale_2() {
    let fx = Fx::new("ilegible");
    let falta = fx.base.join("no-existe.jsonl");
    let salida = fx.run(&[
        "ajustar",
        "--perfil",
        "debug",
        "--puntos",
        falta.to_str().expect("utf-8 path"),
        "--json",
    ]);
    assert_eq!(salida.code, Some(2), "{}", salida.text());
    let rota = fx.base.join("rota.jsonl");
    write(&rota, "esto no es JSON\n");
    let salida = fx.run(&[
        "ajustar",
        "--perfil",
        "debug",
        "--puntos",
        rota.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(salida.code, Some(2), "{}", salida.text());
}

// ---------------------------------------------------------------------------------------------
// U7-U11: the sweep

#[test]
fn u7_un_punto_por_j_y_replica_en_el_orden_fijado_con_su_j_y_sin_incremental() {
    let fx = Fx::new("u7");
    let salida = fx.run(&[
        "barrido",
        "--perfil",
        "bin",
        "--j",
        "1,2,4,6,8",
        "--reps",
        "2",
    ]);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let llamadas = fx.llamadas();
    assert_eq!(llamadas.len(), 11, "1 warm-up + 10 points");
    assert!(
        llamadas
            .iter()
            .all(|l| l.args == "build --bin memory-industry"),
        "the E2E profile is one build of the bin"
    );
    assert_eq!(llamadas[0].jobs, "8", "the warm-up runs at the highest J");
    let lineas = fx.usl();
    assert_eq!(lineas.len(), 10, "the warm-up writes nothing");
    for linea in &lineas {
        assert_eq!(linea["repo"], json!("memory-industry"));
        assert_eq!(linea["perfil"], json!("bin"));
        assert_eq!(linea["fila"], json!("E2E"));
        for campo in CAMPOS_PUNTO {
            assert!(
                linea.get(campo).is_some(),
                "the point has no {campo}: {linea}"
            );
        }
    }
    let orden_de = |rep: u64| -> Vec<u64> {
        lineas
            .iter()
            .filter(|l| l["rep"].as_u64() == Some(rep))
            .map(|l| l["J"].as_u64().expect("J"))
            .collect()
    };
    assert_eq!(orden_de(0), vec![1, 8, 2, 6, 4], "replica 0 order");
    assert_eq!(orden_de(1), vec![4, 6, 2, 8, 1], "replica 1 order");
    for (posicion, linea) in lineas.iter().take(5).enumerate() {
        assert_eq!(
            linea["orden"],
            json!(posicion),
            "orden is the position inside the replica"
        );
    }
    // the stdout lines are the same points, and the last line sums them up
    let json_en_stdout = salida.out.lines().filter(|l| l.starts_with('{')).count();
    assert_eq!(json_en_stdout, 10, "{}", salida.text());
    let resumen = salida.out.lines().last().unwrap_or("");
    assert!(
        resumen.starts_with("barrido ") && resumen.ends_with(": 10 puntos, 0 cargados, 0 rojos"),
        "the last line of stdout: {resumen:?}"
    );
    let tree_rust = fs::canonicalize(fx.tree.join("rust")).expect("rust dir");
    let destino = fx.destinos.join(fx.clave_del_arbol()).join("T_rel");
    for (llamada, linea) in llamadas[1..].iter().zip(&lineas) {
        assert_eq!(
            llamada.jobs,
            linea["J"].as_u64().expect("J").to_string(),
            "CARGO_BUILD_JOBS of each measured call is the J of its line"
        );
        assert_eq!(llamada.incr, "0", "CARGO_INCREMENTAL=0");
        assert_eq!(Path::new(&llamada.pwd), tree_rust, "cwd is <root>/rust");
        assert_eq!(
            Path::new(&llamada.target),
            destino,
            "the target of the E2E row"
        );
    }
    let marca = fs::read_to_string(destino.join(".gate-tree")).expect(".gate-tree");
    let real = fs::canonicalize(&fx.tree).expect("canonical tree");
    assert_eq!(marca.trim(), real.to_str().expect("utf-8 path"));
}

#[test]
fn u8_el_crate_se_enfria_por_mtime_y_la_huella_no_cambia() {
    let fx = Fx::new("u8");
    let lib = fx.tree.join("rust/src/lib.rs");
    let vieja = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(&lib)
        .and_then(|f| f.set_modified(vieja))
        .expect("an old mtime for lib.rs");
    let mtime_inicial = String::from_utf8_lossy(
        &Command::new("stat")
            .args(["-c", "%y"])
            .arg(&lib)
            .output()
            .expect("stat")
            .stdout,
    )
    .trim()
    .to_string();
    let contenido = fs::read(&lib).expect("lib.rs");
    let huella_antes = huella(&fx);

    let salida = fx.run(&["barrido", "--perfil", "debug", "--j", "2,4", "--reps", "1"]);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let llamadas = fx.llamadas();
    assert_eq!(llamadas.len(), 9, "3 orders x (1 warm-up + 2 points)");
    let ordenes = [
        "clippy --all-targets -- -D warnings",
        "build --bin memory-industry",
        "test --no-run",
    ];
    for (i, llamada) in llamadas.iter().enumerate() {
        assert_eq!(
            llamada.args,
            ordenes[i % 3],
            "call {i}: the orders of a debug point, in order"
        );
    }
    let (punto_1, punto_2) = (&llamadas[3..6], &llamadas[6..9]);
    assert!(
        punto_1[0].mtime > mtime_inicial,
        "mtime of lib.rs did not advance before point 1: {} vs {mtime_inicial}",
        punto_1[0].mtime
    );
    assert!(
        punto_1.iter().all(|l| l.mtime == punto_1[0].mtime),
        "mtime moves once per point, not once per order"
    );
    assert!(
        punto_2[0].mtime > punto_1[0].mtime,
        "mtime of lib.rs did not advance before point 2: {} vs {}",
        punto_2[0].mtime,
        punto_1[0].mtime
    );
    assert_eq!(fs::read(&lib).expect("lib.rs"), contenido, "lib.rs content");
    assert_eq!(huella(&fx), huella_antes, "huella of the tree");
}

#[test]
fn u9_maquina_ocupada_o_entrada_invalida_sale_2_sin_llamar_a_cargo() {
    let fx = Fx::new("u9");
    write(&fx.psi, PSI_OCUPADA);
    let salida = fx.run(&["barrido", "--perfil", "debug"]);
    assert_eq!(salida.code, Some(2), "{}", salida.text());
    assert!(salida.text().contains("busy"), "{}", salida.text());
    assert!(
        fx.llamadas().is_empty(),
        "a busy machine must not reach cargo"
    );

    write(&fx.psi, PSI_QUIETA);
    let salida = fx.run(&["barrido", "--perfil", "mutantes"]);
    assert_eq!(salida.code, Some(2), "{}", salida.text());
    assert!(
        salida.text().contains("unknown profile"),
        "{}",
        salida.text()
    );

    let salida = fx.run_env(
        &["barrido", "--perfil", "bin"],
        &[("CARGO_TARGET_DIR", None)],
    );
    assert_eq!(salida.code, Some(2), "{}", salida.text());
    assert!(
        salida.text().contains("CARGO_TARGET_DIR"),
        "{}",
        salida.text()
    );
    assert!(fx.llamadas().is_empty(), "no invalid call reaches cargo");
}

#[test]
fn u10_un_punto_que_termina_con_presion_alta_queda_cargado() {
    let fx = Fx::new("u10");
    let salida = fx.run_env(
        &[
            "barrido",
            "--perfil",
            "bin",
            "--j",
            "1,2,4,6,8",
            "--reps",
            "1",
        ],
        &[("USL_FAKE_PSI_ON", Some("3"))],
    );
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let lineas = fx.usl();
    assert_eq!(lineas.len(), 5);
    // call 1 is the warm-up, so call 3 belongs to the second point
    for (i, linea) in lineas.iter().enumerate() {
        assert_eq!(linea["cargado"], json!(i == 1), "point {i}: {linea}");
    }
    cerca(
        num(&lineas[1]["psi_fin"]),
        30.0,
        1e-9,
        "psi_fin of the loaded point",
    );
    cerca(
        num(&lineas[0]["psi_fin"]),
        1.0,
        1e-9,
        "psi_fin of a quiet point",
    );
    assert!(
        salida
            .out
            .lines()
            .last()
            .unwrap_or("")
            .ends_with(": 5 puntos, 1 cargados, 0 rojos"),
        "{}",
        salida.text()
    );
}

#[test]
fn u11_el_historial_nunca_cambia_la_salida() {
    let fx = Fx::new("u11");
    let ilegible = fx.base.join("no-es-un-directorio");
    write(
        &ilegible,
        "un fichero normal: no se puede escribir debajo\n",
    );
    let salida = fx.run_env(
        &[
            "barrido",
            "--perfil",
            "bin",
            "--j",
            "1,2,4,6,8",
            "--reps",
            "1",
        ],
        &[("CI_TIEMPOS_DIR", ilegible.to_str())],
    );
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let puntos = salida.out.lines().filter(|l| l.starts_with('{')).count();
    assert_eq!(
        puntos,
        5,
        "the five points are in stdout\n{}",
        salida.text()
    );
    let avisos: Vec<&str> = salida.err.lines().collect();
    assert_eq!(avisos.len(), 1, "stderr has one line: {avisos:?}");
    assert!(avisos[0].starts_with("history:"), "{avisos:?}");

    // `usl_point` writes under the day folder with `repo`, and a record the history cannot
    // serialise is one warning too, never an exception.
    let fx = Fx::new("u11-directo");
    let mut escribe = Command::new("python3");
    escribe
        .arg("-I")
        .arg("-B")
        .arg("-c")
        .arg(
            "import sys\nsys.path.insert(0, sys.argv[1])\nimport gate_history as gh\n\
             gh.usl_point({'perfil': 'bin', 'J': 4})\n\
             gh.usl_point({'perfil': 'bin', 'J': {4, 8}})\n",
        )
        .arg(fx.tree.join("scripts"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HOME", &fx.home)
        .env("CI_TIEMPOS_DIR", &fx.hist);
    let salida = ejecutar(escribe);
    assert_eq!(
        salida.code,
        Some(0),
        "usl_point must not raise\n{}",
        salida.text()
    );
    assert_eq!(
        salida
            .err
            .lines()
            .filter(|l| l.starts_with("history:"))
            .count(),
        1,
        "{}",
        salida.text()
    );
    let lineas = fx.usl();
    assert_eq!(
        lineas.len(),
        1,
        "only the serialisable record is written: {lineas:?}"
    );
    assert_eq!(lineas[0]["repo"], json!("memory-industry"));
    assert_eq!(lineas[0]["perfil"], json!("bin"));
}

#[test]
fn u18_un_cargo_colgado_se_corta_en_su_plazo() {
    let fx = Fx::new("u18");
    let t0 = Instant::now();
    let salida = fx.run_env(
        &["barrido", "--perfil", "bin", "--j", "1", "--reps", "1"],
        &[
            ("USL_FAKE_SLEEP", Some("120")),
            ("GATE_USL_PLAZO_S", Some("1")),
        ],
    );
    assert_eq!(salida.code, Some(2), "{}", salida.text());
    assert!(salida.text().contains("timed out"), "{}", salida.text());
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "the sweep waited {:?} for a cargo that hung",
        t0.elapsed()
    );
}

// ---------------------------------------------------------------------------------------------
// U12-U17: the recommendation

/// The steps of one run as (id, idx, seconds).
type PasosDeCorrida = Vec<(&'static str, u32, f64)>;

/// (id, idx, seconds) of the steps of the 19th run (spec 0.1).
fn pasos_19(b3: f64, dbg: f64) -> PasosDeCorrida {
    vec![
        ("P", 10, 4.201),
        ("DBG", 20, dbg),
        ("NLI", 25, 37.508),
        ("B3", 30, b3),
        ("REL", 40, 102.530),
        ("E2E", 50, 180.680),
        ("DC", 60, 224.391),
        ("CV", 70, 68.583),
        ("M", 80, 146.363),
    ]
}

/// The 19th run with the rows that do not matter to U14-U16 short (REL 30 s, DC 20 s, CV 10 s, M 20 s). With
/// the 19th's own DC, M and REL (8 cores each) the 24 cores are the bottleneck of the whole table and no J of DBG
/// moves the simulated clock: the scenario is about DBG against B3, so the rest of the table has to fit around them.
fn pasos_cortos(b3: f64, dbg: f64) -> PasosDeCorrida {
    vec![
        ("P", 10, 4.201),
        ("DBG", 20, dbg),
        ("NLI", 25, 37.508),
        ("B3", 30, b3),
        ("REL", 40, 30.0),
        ("E2E", 50, 180.680),
        ("DC", 60, 20.0),
        ("CV", 70, 10.0),
        ("M", 80, 20.0),
    ]
}

fn sembrar_corrida(fx: &Fx, dia: &str, run: &str, tabla: &str, pasos: &[(&str, u32, f64)]) {
    let carpeta = fx.hist.join("memory-industry").join(dia);
    fs::create_dir_all(&carpeta).expect("day folder");
    let total: f64 = pasos.iter().map(|p| p.2).sum();
    let corrida = json!({
        "repo": "memory-industry", "modo": "serie", "tabla": tabla, "run": run, "rc": 0,
        "T": total, "seg": total, "C": C_NUCLEOS, "R": R_BYTES, "fecha": "2026-11-08T12:00:00+00:00"
    });
    anadir(&carpeta.join("corridas.jsonl"), &[corrida]);
    let lineas: Vec<Value> = pasos
        .iter()
        .map(|(id, idx, seg)| {
            json!({
                "repo": "memory-industry", "modo": "serie", "tabla": tabla, "run": run,
                "id": id, "idx": idx, "seg": seg, "rc": 0, "cpu_s": null, "anon_max": null
            })
        })
        .collect();
    anadir(&carpeta.join("pasos.jsonl"), &lineas);
}

fn sembrar_barrido(fx: &Fx, dia: &str, barrido: &str, puntos: &[Value]) {
    let carpeta = fx.hist.join("memory-industry").join(dia);
    fs::create_dir_all(&carpeta).expect("day folder");
    let lineas: Vec<Value> = puntos
        .iter()
        .map(|p| {
            let mut p = p.clone();
            p["barrido"] = json!(barrido);
            p
        })
        .collect();
    anadir(&carpeta.join("usl.jsonl"), &lineas);
}

fn anadir(fichero: &Path, lineas: &[Value]) {
    let mut cuerpo = fs::read_to_string(fichero).unwrap_or_default();
    for l in lineas {
        cuerpo.push_str(&format!("{l}\n"));
    }
    write(fichero, &cuerpo);
}

/// The table of the judge with DBG at `dbg_jobs` jobs, committed in the fixture repo; returns the key of the history
/// that the planner writes for it in `tabla` (K9c: `clave_de_tabla`, not the sha256 of the bytes of the file).
fn tabla_en_arbol(fx: &Fx, dbg_jobs: u32) -> String {
    let texto = TABLA.replace("{DBG}", &dbg_jobs.to_string());
    write(&fx.tree.join("scripts/gate-dag.txt"), &texto);
    fx.commit(&format!("table with DBG at {dbg_jobs} jobs"));
    clave_de_tabla(&texto)
}

/// Three green serial runs: the first one cold (B3 206.174 s), the other two warm (B3 1.693 s).
fn historial_de(fx: &Fx, tabla: &str, dbg: f64, pasos: fn(f64, f64) -> PasosDeCorrida) {
    sembrar_corrida(fx, "20261101", "run-1", tabla, &pasos(206.174, dbg));
    sembrar_corrida(fx, "20261102", "run-2", tabla, &pasos(1.693, dbg));
    sembrar_corrida(fx, "20261103", "run-3", tabla, &pasos(1.693, dbg));
}

const CURVA_DEBUG: Curva = Curva {
    lambda: 1.0 / 600.0,
    sigma: 0.06,
    kappa: 0.0008,
};
const NIVELES_DEBUG: [u32; 6] = [2, 4, 8, 12, 16, 24];
/// A shallow curve: T(2) - T(J) is 6.2 s at J = 4 and under 10 s at every J, so no J buys the E2E row the 11.5 s
/// threshold, but a gain above 0 is there for a threshold of 0 to propose (spec U14, sabotage "threshold 0").
const CURVA_BIN: Curva = Curva {
    lambda: 1.0 / 50.0,
    sigma: 0.5,
    kappa: 0.001,
};
const NIVELES_BIN: [u32; 5] = [1, 2, 4, 6, 8];

/// Both profiles measured, plus an older and unreliable debug sweep that the last one must hide.
fn barridos_buenos(fx: &Fx, con_bin: bool) {
    let viejo = puntos_de(
        "debug",
        "DBG",
        CURVA_DEBUG,
        &NIVELES_DEBUG,
        2,
        |_, _, j, _| 100.0 * f64::from(j),
    );
    sembrar_barrido(fx, "20261103", "debug-viejo", &viejo);
    let mut debug = exacta("debug", "DBG", CURVA_DEBUG, &NIVELES_DEBUG);
    con_rss(&mut debug, 2 << 30, (1 << 30) / 10);
    sembrar_barrido(fx, "20261104", "debug-bueno", &debug);
    if con_bin {
        let bin = exacta("bin", "E2E", CURVA_BIN, &NIVELES_BIN);
        sembrar_barrido(fx, "20261104", "bin-bueno", &bin);
    }
}

fn recomendar(fx: &Fx) -> (Salida, Value) {
    let salida = fx.run(&["recomendar", "--json"]);
    let v = salida.json();
    (salida, v)
}

#[test]
fn u12_holguras_y_veredicto_de_juez_cuda_con_el_historial_de_la_19() {
    let fx = Fx::new("u12");
    let tabla = tabla_en_arbol(&fx, 8);
    historial_de(&fx, &tabla, 391.036, pasos_19);
    let (salida, v) = recomendar(&fx);
    cerca(num(&v["L"]), 575.917, 0.001, "L");
    for id in ["DBG", "E2E", "P"] {
        assert_eq!(
            v["filas"][id]["estado"],
            json!("critica"),
            "{id}: {}",
            salida.text()
        );
    }
    let b3 = &v["filas"]["B3"];
    cerca(
        num(&b3["d"]),
        206.174,
        0.001,
        "B3 d (only the runs with B3 >= 60 s)",
    );
    cerca(num(&b3["h"]), 184.862, 0.001, "B3 h");
    cerca(num(&b3["h_L"]), 0.321, 0.001, "B3 h/L");
    let texto = fx.run(&["recomendar"]);
    assert!(
        texto
            .out
            .contains("juez-cuda: no (B3 slack 184.9 s, 32.1 %)"),
        "{}",
        texto.text()
    );
    assert_eq!(v["juez_cuda"], json!("no (B3 slack 184.9 s, 32.1 %)"));
}

#[test]
fn u13_release_critica_dice_juez_cuda_si_y_con_una_corrida_no_es_evaluable() {
    let fx = Fx::new("u13");
    let tabla = tabla_en_arbol(&fx, 8);
    historial_de(&fx, &tabla, 150.0, pasos_19);
    let (salida, v) = recomendar(&fx);
    assert_eq!(
        v["filas"]["B3"]["estado"],
        json!("critica"),
        "{}",
        salida.text()
    );
    cerca(num(&v["filas"]["B3"]["h"]), 0.0, 1e-6, "B3 h");
    let texto = fx.run(&["recomendar"]);
    assert!(texto.out.contains("juez-cuda: yes"), "{}", texto.text());

    let solo = Fx::new("u13-una");
    let tabla = tabla_en_arbol(&solo, 8);
    sembrar_corrida(
        &solo,
        "20261101",
        "run-1",
        &tabla,
        &pasos_19(206.174, 150.0),
    );
    let texto = solo.run(&["recomendar"]);
    assert!(
        texto
            .out
            .contains("juez-cuda: not evaluable (1 run, need 3)"),
        "{}",
        texto.text()
    );
}

#[test]
fn u14_la_simulacion_elige_el_j_que_cabe_no_la_rodilla_y_solo_propone_si_gana_lo_bastante() {
    let fx = Fx::new("u14");
    let tabla = tabla_en_arbol(&fx, 8);
    historial_de(&fx, &tabla, 391.036, pasos_cortos);
    barridos_buenos(&fx, true);
    let antes = fs::read(fx.tree.join("scripts/gate-dag.txt")).expect("table");
    let (salida, v) = recomendar(&fx);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let dbg = &v["calibradas"]["DBG"];
    assert_eq!(dbg["J_actual"], json!(8), "{}", salida.text());
    assert_eq!(
        dbg["J_propuesto"],
        json!(12),
        "not the knee (20) nor 16 or 24: DBG and B3 would stop fitting together\n{}",
        salida.text()
    );
    assert_eq!(dbg["propone"], json!(true));
    let diff = v["diff"].as_str().expect("a diff when a row changes");
    let quitan: Vec<&str> = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .collect();
    let ponen: Vec<&str> = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .collect();
    assert_eq!(
        (quitan.len(), ponen.len()),
        (1, 1),
        "the diff changes one line:\n{diff}"
    );
    let antes_campos: Vec<&str> = quitan[0][1..].split_whitespace().collect();
    let despues_campos: Vec<&str> = ponen[0][1..].split_whitespace().collect();
    assert_eq!(antes_campos[1], "DBG", "{diff}");
    assert_eq!(
        (antes_campos[5], despues_campos[5]),
        ("8", "12"),
        "the cpu column:\n{diff}"
    );
    assert!(despues_campos.contains(&"CARGO_BUILD_JOBS=12"), "{diff}");
    for (i, (a, b)) in antes_campos.iter().zip(&despues_campos).enumerate() {
        if i != 5 && !a.starts_with("CARGO_BUILD_JOBS=") {
            assert_eq!(a, b, "only the cpu and the jobs of DBG change:\n{diff}");
        }
    }
    let e2e = &v["calibradas"]["E2E"];
    assert_eq!(e2e["propone"], json!(false), "{}", salida.text());
    assert_eq!(e2e["J_propuesto"], e2e["J_actual"]);
    let umbral = (0.02 * num(&v["L"])).max(10.0);
    assert!(
        num(&e2e["ganancia_s"]) > 0.0,
        "the E2E row has a gain, under the threshold: {}",
        e2e["ganancia_s"]
    );
    assert!(
        num(&e2e["ganancia_s"]) < umbral,
        "E2E gain {} has to be under the threshold {umbral}",
        e2e["ganancia_s"]
    );
    assert!(
        num(&v["T_sim_propuesto"]) < num(&v["T_sim_actual"]),
        "{}",
        salida.text()
    );
    let texto = fx.run(&["recomendar"]);
    assert!(
        texto
            .out
            .lines()
            .any(|l| l.contains("E2E") && l.contains("se conserva")),
        "{}",
        texto.text()
    );
    assert_eq!(
        fs::read(fx.tree.join("scripts/gate-dag.txt")).expect("table"),
        antes,
        "the tool writes nothing"
    );
}

#[test]
fn u15_sin_ajuste_fiable_para_una_fila_critica_sale_3_con_la_orden_que_falta() {
    let fx = Fx::new("u15");
    let tabla = tabla_en_arbol(&fx, 8);
    historial_de(&fx, &tabla, 391.036, pasos_cortos);
    barridos_buenos(&fx, false);
    let (salida, v) = recomendar(&fx);
    assert_eq!(salida.code, Some(3), "{}", salida.text());
    let orden_bin = "python3 -I scripts/gate_usl.py barrido --perfil bin";
    assert!(salida.out.contains(orden_bin), "{}", salida.text());
    assert!(
        !salida.text().contains("--perfil debug"),
        "the debug sweep exists: {}",
        salida.text()
    );
    assert!(
        v["diff"].is_null(),
        "no diff without every adjustment: {}",
        v["diff"]
    );
    let texto = fx.run(&["recomendar"]);
    assert_eq!(texto.code, Some(3), "{}", texto.text());
    assert!(
        !texto.out.contains("@@") && !texto.out.contains("+++"),
        "no diff in the text either: {}",
        texto.text()
    );

    // A knee beyond the machine (N* = 99, J measured up to 48, C = 24): nothing above C is proposed.
    let ancha = Curva {
        lambda: 1.0 / 600.0,
        sigma: 0.01,
        kappa: 0.0001,
    };
    sembrar_barrido(
        &fx,
        "20261105",
        "debug-ancho",
        &exacta("debug", "DBG", ancha, &[4, 8, 16, 24, 32, 48]),
    );
    sembrar_barrido(
        &fx,
        "20261105",
        "bin-plano",
        &exacta("bin", "E2E", CURVA_BIN, &NIVELES_BIN),
    );
    let (salida, v) = recomendar(&fx);
    assert_eq!(salida.code, Some(0), "{}", salida.text());
    let tope = u64::from(C_NUCLEOS);
    for fila in ["DBG", "E2E"] {
        let j = v["calibradas"][fila]["J_propuesto"]
            .as_u64()
            .expect("J_propuesto");
        assert!(j <= tope, "{fila} proposed J = {j}, above C = {tope}");
    }
    assert!(
        v["cpu_cap"]["propuesto"].as_u64().expect("cpu_cap") <= tope,
        "cpu_cap above C: {}",
        v["cpu_cap"]
    );
    for linea in v["diff"]
        .as_str()
        .unwrap_or("")
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
    {
        let campos: Vec<&str> = linea[1..].split_whitespace().collect();
        let cpu: f64 = campos.get(5).and_then(|c| c.parse().ok()).unwrap_or(0.0);
        assert!(cpu <= f64::from(C_NUCLEOS), "cpu above C in {linea}");
        let jobs = campos
            .iter()
            .find_map(|c| c.strip_prefix("CARGO_BUILD_JOBS="))
            .and_then(|j| j.parse::<u32>().ok())
            .unwrap_or(0);
        assert!(jobs <= C_NUCLEOS, "jobs above C in {linea}");
    }
}

#[test]
fn u16_el_j_de_las_corridas_sale_de_git_no_de_la_tabla_de_hoy() {
    let fx = Fx::new("u16");
    let primera = tabla_en_arbol(&fx, 4);
    let _actual = tabla_en_arbol(&fx, 8);
    let desconocida = "ab".repeat(32);
    sembrar_corrida(
        &fx,
        "20261101",
        "run-1",
        &primera,
        &pasos_cortos(206.174, 391.036),
    );
    sembrar_corrida(
        &fx,
        "20261102",
        "run-2",
        &desconocida,
        &pasos_cortos(206.174, 900.0),
    );
    barridos_buenos(&fx, true);
    let (salida, v) = recomendar(&fx);
    let dbg = &v["calibradas"]["DBG"];
    assert_eq!(dbg["J_hist"], json!([4]), "{}", salida.text());
    assert_eq!(
        dbg["runs"],
        json!(["run-1"]),
        "the run of a table git does not know is not used\n{}",
        salida.text()
    );
    cerca(num(&dbg["d_hist"]), 391.036, 0.001, "d_hist");
    // d(J) = d_hist - T(J_hist) + T(J), with the curve of the sweep
    let esperada = 391.036 - CURVA_DEBUG.t(4) + CURVA_DEBUG.t(8);
    cerca(num(&dbg["d_por_J"]["8"]), esperada, 0.5, "d(8)");
    assert!(
        salida.text().contains("table abababababab not in git"),
        "{}",
        salida.text()
    );
}

// ---------------------------------------------------------------------------------------------
// U17: the profiles against the judge

/// Is `orden` in the judge's script? A line of the script has it literally, or, for the compile-only
/// form `... --no-run` of a test run, the script runs the same order without the flag.
fn aparece(script: &str, orden: &str) -> bool {
    if script.contains(orden) {
        return true;
    }
    let Some(sin_flag) = orden.strip_suffix(" --no-run") else {
        return false;
    };
    script.lines().any(|l| {
        let l = l.trim_end();
        l == sin_flag || l.ends_with(&format!(" {sin_flag}"))
    })
}

#[test]
fn u17_las_ordenes_de_cada_perfil_existen_tal_cual_en_run_all_tests() {
    let script = fs::read_to_string(scripts().join("run-all-tests.sh")).expect("run-all-tests.sh");
    for perfil in ["debug", "bin", "release"] {
        let fx = Fx::new(&format!("u17-{perfil}"));
        let salida = fx.run(&["barrido", "--perfil", perfil, "--j", "2", "--reps", "1"]);
        assert_eq!(salida.code, Some(0), "{perfil}: {}", salida.text());
        let llamadas = fx.llamadas();
        assert!(
            !llamadas.is_empty() && llamadas.len().is_multiple_of(2),
            "{perfil}: a warm-up and one point run the same orders: {} calls",
            llamadas.len()
        );
        for llamada in &llamadas[..llamadas.len() / 2] {
            let orden = format!("cargo {}", llamada.args);
            assert!(
                aparece(&script, &orden),
                "profile {perfil}: the order {orden:?} is not in scripts/run-all-tests.sh"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// K9c: the key of the history of a table is the one the planner writes

/// The key of the history (CI-J14k, gate-dag.py): the sha256 of the rows of the table, each with its spaces collapsed,
/// without comments and blank lines, joined by a newline. Written here from that sentence, not read from the planner.
fn clave_de_tabla(texto: &str) -> String {
    let filas: Vec<String> = texto
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    format!("{:x}", Sha256::digest(filas.join("\n").as_bytes()))
}

#[test]
fn u18_la_clave_de_la_tabla_es_la_que_escribe_el_planificador() {
    // A known answer, worked by hand: the one row, as `sha256sum` sees it with no newline at the end.
    assert_eq!(
        clave_de_tabla("# a comment\n\n10   p  -  -  normal 1 1 1 sh -c true\n"),
        "4548020ba9c0a9bb9784738d8e34ca22d7ddce74c8ae586f306f1f0549bd02b6",
        "the helper that spells the key must agree with sha256 of '10 p - - normal 1 1 1 sh -c true'"
    );
    let fx = Fx::new("u18");
    let tabla = fx.base.join("tabla-del-planificador.txt");
    write(
        &tabla,
        "# a comment\n\n10   p  -  -  normal 1 1 1 sh -c true\n",
    );
    let historial = fx.base.join("historial-del-planificador");
    let status = Command::new("python3")
        .arg("-I")
        .arg(fx.tree.join("scripts/gate-dag.py"))
        .args(["run", "--table"])
        .arg(&tabla)
        .current_dir(&fx.tree)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GATE_DAG", "0")
        .env("CI_TIEMPOS_DIR", &historial)
        .env("HOME", &fx.home)
        .env("TMPDIR", &fx.base)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the planner");
    assert!(
        status.success(),
        "the planner did not run the table: {status}"
    );
    let dia = fs::read_dir(historial.join("memory-industry"))
        .expect("the planner wrote no history")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.join("pasos.jsonl").is_file())
        .expect("the planner wrote no pasos.jsonl");
    let escrito = fs::read_to_string(dia.join("pasos.jsonl")).expect("pasos.jsonl");
    let paso: Value = serde_json::from_str(escrito.lines().next().expect("a step")).expect("json");
    assert_eq!(
        paso["tabla"],
        json!(clave_de_tabla(&fs::read_to_string(&tabla).expect("table"))),
        "the planner writes another key than the normalised one: {paso}"
    );
}

#[test]
fn u19_recomendar_encuentra_las_corridas_aunque_la_tabla_cambie_de_comentarios_y_espacios() {
    let fx = Fx::new("u19");
    // The table as it was committed first: a header comment and doubled spaces. Today's has neither. The history
    // is keyed by the rows, so both are the same table, and the bytes of the two files are not.
    let antes = format!(
        "# the table of 9-nov\n\n{}",
        TABLA.replace("{DBG}", "8").replace(' ', "  ")
    );
    write(&fx.tree.join("scripts/gate-dag.txt"), &antes);
    fx.commit("table with comments");
    let clave = tabla_en_arbol(&fx, 8);
    assert_eq!(
        clave,
        clave_de_tabla(&antes),
        "the fixture must hold two spellings of one table"
    );
    historial_de(&fx, &clave, 391.036, pasos_cortos);
    barridos_buenos(&fx, true);
    let (salida, v) = recomendar(&fx);
    let dbg = &v["calibradas"]["DBG"];
    assert_eq!(dbg["J_hist"], json!([8]), "{}", salida.text());
    assert!(
        dbg["runs"]
            .as_array()
            .is_some_and(|runs| runs.contains(&json!("run-1"))),
        "the runs keyed as the planner keys its table were not found\n{}",
        salida.text()
    );
    assert!(
        !salida.text().contains("not in git"),
        "a table git knows was reported as unknown\n{}",
        salida.text()
    );
}
