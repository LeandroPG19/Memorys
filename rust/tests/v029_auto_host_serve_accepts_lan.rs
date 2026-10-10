//! `memory-industry serve` takes the automatic LAN mode, `lan:<port>`, as a listen address,
//! from the argument and from `CUBA_HTTP_ADDR`. The binary used to check the address as
//! `ip:port` before handing it to the daemon, so the mode `setup service` writes could not
//! start: the end-to-end run with real processes exited 2 with «is not a listen address».
//!
//! No daemon is left listening: without `CUBA_HTTP_TOKEN` the daemon refuses `lan:` at its
//! own token check, after it has tried `DEAD_DB`, and exits 1. That refusal is the proof the
//! address got past the binary's check.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEAD_DB: &str = "postgresql://nobody@127.0.0.1:1/nothing";

/// The same ceiling `cli_contract` gives a serve that ends by itself against `DEAD_DB`.
const BUDGET: Duration = Duration::from_secs(60);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs `serve [arg]` with `CUBA_HTTP_ADDR` set to `env_addr` or removed, and kills it if it
/// is still alive after `BUDGET`: a serve that listens forever must fail this test, not hang it.
fn run_serve(arg: Option<&str>, env_addr: Option<&str>) -> (String, Option<i32>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memory-industry"));
    command.arg("serve");
    if let Some(arg) = arg {
        command.arg(arg);
    }
    match env_addr {
        Some(addr) => command.env("CUBA_HTTP_ADDR", addr),
        None => command.env_remove("CUBA_HTTP_ADDR"),
    };
    let mut child = command
        .env("DATABASE_URL", DEAD_DB)
        .env_remove("CUBA_HTTP_TOKEN")
        .env_remove("CUBA_PEER_TOKEN")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    let mut pipe = child.stderr.take().expect("stderr is piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        pipe.read_to_string(&mut s).ok();
        s
    });

    let deadline = Instant::now() + BUDGET;
    let code = loop {
        if let Some(status) = child.try_wait().expect("child can be polled") {
            break status.code();
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    (reader.join().expect("stderr reader"), code)
}

fn assert_lan_got_through(how: &str, lan: &str, stderr: &str, code: Option<i32>) {
    assert!(
        !stderr.contains("is not a listen address"),
        "{how}: `serve` refused {lan} as an address, so the automatic mode `setup service` \
         writes can never start (exit {code:?}).\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("connect to PostgreSQL") || stderr.contains("connected to PostgreSQL"),
        "{how}: with {lan} accepted, `serve` goes on to PostgreSQL before anything else \
         (exit {code:?}).\nstderr: {stderr}"
    );
    assert_eq!(
        code,
        Some(1),
        "{how}: with no CUBA_HTTP_TOKEN the daemon refuses {lan} at its token check and exits \
         1 (None = still running after {BUDGET:?}).\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(lan) && stderr.contains("CUBA_HTTP_TOKEN"),
        "{how}: the refusal is the daemon's token rule for {lan}, naming the token it needs.\n\
         stderr: {stderr}"
    );
}

#[test]
fn serve_takes_lan_auto_from_the_argument() {
    let lan = format!("lan:{}", free_port());

    let (stderr, code) = run_serve(Some(&lan), None);

    assert_lan_got_through("serve lan:<port>", &lan, &stderr, code);
}

#[test]
fn serve_takes_lan_auto_from_cuba_http_addr() {
    let lan = format!("lan:{}", free_port());

    let (stderr, code) = run_serve(None, Some(&lan));

    assert_lan_got_through("CUBA_HTTP_ADDR=lan:<port>", &lan, &stderr, code);
}
