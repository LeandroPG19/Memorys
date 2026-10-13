// Shared by the test files that declare `mod doctor_world;`. A `doctor` run from a test cannot
// read the HOME, the LOCALAPPDATA, the `llm.env`, the daemon or the `schtasks` of whoever runs the
// suite: each of those decides what doctor says, and none of them is the thing under test.
//
// It lives in a directory for the same reason as `common/`: cargo does not build it as a test
// target and `for file in tests/*.rs` never hands it to `--test`. Everything here is used by every
// file that includes it, or each copy trips dead_code under clippy `-D warnings`.
//
// Take the file's `ENV_GUARD` before `DoctorWorld::new` and let go of it after the world drops:
// locals drop in reverse order, so declare the guard first.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use memory_industry::doctor::Check;
use memory_industry::service_apply::{CommandOutcome, CommandRunner};
use sqlx::PgPool;

/// Variables that decide whether doctor reaches out of the process: the LLM provider and the graph
/// backend each trigger a probe, and the reranker path decides the `reranker` check.
const CLEARED: [&str; 7] = [
    "MEMORY_INDUSTRY_LLM_BASE_URL",
    "CUBA_LLM_BASE_URL",
    "MEMORY_INDUSTRY_LLM_PROVIDER",
    "CUBA_LLM_PROVIDER",
    "MEMORY_INDUSTRY_GRAPH_DB",
    "CUBA_GRAPH_DB",
    "CUBA_RERANKER_PATH",
];

struct Asked(Vec<Vec<String>>);

impl CommandRunner for Asked {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<CommandOutcome> {
        let mut argv = vec![program.to_string()];
        argv.extend_from_slice(args);
        self.0.push(argv);
        Ok(CommandOutcome {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

pub struct DoctorWorld {
    root: PathBuf,
    before: Vec<(&'static str, Option<OsString>)>,
    // Held open so the port doctor probes is the test's own: the bind it tries collides with this
    // socket and never with the 8787 or 8788 of the machine.
    _listener: std::net::TcpListener,
    runner: Asked,
}

impl DoctorWorld {
    pub fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("doctor-world-{tag}-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let local = root.join("local");
        std::fs::create_dir_all(&home).expect("the world's home is creatable");
        std::fs::create_dir_all(&local).expect("the world's local app data is creatable");
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a loopback port for the world's daemon address");
        let addr = listener.local_addr().expect("the listener's own address");

        let mut world = Self {
            root,
            before: Vec::new(),
            _listener: listener,
            runner: Asked(Vec::new()),
        };
        world.put("HOME", Some(home.clone().into()));
        world.put("USERPROFILE", Some(home.into()));
        world.put("LOCALAPPDATA", Some(local.into()));
        world.put("CUBA_HTTP_ADDR", Some(addr.to_string().into()));
        for name in CLEARED {
            world.put(name, None);
        }
        world
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `doctor` with the world's runner, then what the runner was asked: `schtasks` once on Windows
    /// and nothing elsewhere. The list is spelled out here and not read from
    /// `service::TASK_NAME`, so a sabotage of the constant does not move the reader and this
    /// fixture together.
    pub async fn run_checks(&mut self, pool: &PgPool, url: &str) -> Vec<Check> {
        let checks =
            memory_industry::doctor::run_checks_with(pool, url, false, &mut self.runner).await;
        let asked: Vec<Vec<&str>> = self
            .runner
            .0
            .iter()
            .map(|argv| argv.iter().map(String::as_str).collect())
            .collect();
        let expected: Vec<Vec<&str>> = if cfg!(windows) {
            vec![vec!["schtasks", "/Query", "/TN", "memory-industry"]]
        } else {
            Vec::new()
        };
        assert_eq!(
            asked, expected,
            "doctor asked the operating system something other than the one question it owes \
             the service check: a call missing means it ran the real `schtasks` instead of the \
             runner it was given, an extra one means it asks twice"
        );
        self.runner.0.clear();
        checks
    }

    fn put(&mut self, name: &'static str, value: Option<OsString>) {
        self.before.push((name, std::env::var_os(name)));
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

impl Drop for DoctorWorld {
    fn drop(&mut self) {
        for (name, previous) in self.before.drain(..).rev() {
            unsafe {
                match previous {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
