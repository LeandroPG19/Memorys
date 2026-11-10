// needs-a-server: seeds the eval smoke fixture through the real binary over MCP stdio, into a scratch database it creates and drops
//
//! The eval smoke fixture, `rust/eval-datasets/smoke-corpus.jsonl`, goes into a database the way
//! `scripts/eval_seed.py` puts it there: the real binary, one stdio MCP session, `cuba_jornada`
//! start in the `eval-smoke` project, then one `cuba_cronica add` per line. Every line has to come
//! back as a row of its own. An answer with `deduplicated` or `reinforced_id` means the line was
//! folded into another row (same entity, and a near-enough sentence), so the eval would read fewer
//! observations than the fixture has.
//!
//! `DATABASE_URL` names the server. The database is one this file creates and drops
//! (`common::in_a_scratch_database`). The embedder is the model `ONNX_MODEL_PATH` names, which the
//! binary inherits: without it the semantic dedup does not run, so this test says what is missing
//! and stops before it creates anything.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::common::in_a_scratch_database;
use serde_json::{Value, json};
use sqlx::Connection;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

/// The project the eval reads from, and the one the seeding session starts in, as in `eval_seed.py`.
const EVAL_PROJECT: &str = "eval-smoke";

/// The ceiling `eval_seed.py` gives one tool call. The first embedding loads the model.
const CALL_BUDGET: Duration = Duration::from_secs(60);

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("eval-datasets/smoke-corpus.jsonl")
}

/// Names what is missing from the environment, before any database is created.
fn refuse_if_something_is_missing() {
    let url = std::env::var("DATABASE_URL").unwrap_or_default();
    assert!(
        !url.trim().is_empty(),
        "falta DATABASE_URL: este test crea y tira su base en el servidor que nombra, y sin él no corre"
    );
    let model = std::env::var("ONNX_MODEL_PATH").unwrap_or_default();
    assert!(
        !model.trim().is_empty() && Path::new(&model).exists(),
        "falta el modelo: ONNX_MODEL_PATH ({model:?}) está vacío o no existe. Sin el modelo no corre el dedup semántico y este test no puede comprobar lo que dice"
    );
    assert!(
        fixture().is_file(),
        "falta la fixture {}",
        fixture().display()
    );
}

/// The non-blank lines of the fixture, with their 1-based line number in the file.
fn fixture_lines() -> Vec<(usize, Value)> {
    let body = std::fs::read_to_string(fixture()).expect("leyendo smoke-corpus.jsonl");
    body.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let row = serde_json::from_str::<Value>(line)
                .unwrap_or_else(|why| panic!("fixture line {}: no es JSON: {why}", index + 1));
            (index + 1, row)
        })
        .collect()
}

/// One `memory-industry` process speaking MCP over stdio. Dropping it kills the process, so a
/// failed assertion leaves nothing running behind.
struct McpStdio {
    child: Child,
    stdin: ChildStdin,
    answers: Receiver<Value>,
    next_id: u64,
}

impl McpStdio {
    fn start(database_url: &str) -> Self {
        let mut child = Command::new(BIN)
            .env("DATABASE_URL", database_url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("arrancando el binario memory-industry");
        let stdin = child.stdin.take().expect("stdin del binario");
        let stdout = child.stdout.take().expect("stdout del binario");
        let (sender, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(&line)
                    && sender.send(message).is_err()
                {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            answers,
            next_id: 0,
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|why| format!("escribiendo al binario: {why}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let message = self.answers.recv_timeout(CALL_BUDGET).map_err(|_| {
                format!(
                    "sin respuesta a {method} en {} s (el binario no contestó o se cerró)",
                    CALL_BUDGET.as_secs()
                )
            })?;
            if message.get("id") == Some(&json!(id)) {
                return Ok(message);
            }
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    /// The tool's own answer as JSON. A JSON-RPC error, an `isError`, a body that is not JSON or a
    /// body that carries an `error` are all failed calls.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        if let Some(error) = response.get("error") {
            return Err(format!("{name}: error JSON-RPC {error}"));
        }
        let result = &response["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        if result["isError"].as_bool() == Some(true) {
            return Err(format!("{name}: isError {text}"));
        }
        let body: Value =
            serde_json::from_str(text).map_err(|_| format!("{name}: la respuesta no es JSON"))?;
        if let Some(error) = body.get("error") {
            return Err(format!("{name}: {error}"));
        }
        Ok(body)
    }
}

impl Drop for McpStdio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A line counts only when the answer carries the id of a new row and neither sign of folding.
fn check_inserted(number: usize, body: &Value) -> Result<(), String> {
    let shown: String = body.to_string().chars().take(300).collect();
    if body.get("deduplicated").is_some() {
        return Err(format!(
            "fixture line {number}: respondió deduplicated, la línea se fundió con otra fila: {shown}"
        ));
    }
    if body.get("reinforced_id").is_some() {
        return Err(format!(
            "fixture line {number}: respondió reinforced_id, la línea refuerza otra fila: {shown}"
        ));
    }
    let has_id = body
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    if !has_id {
        return Err(format!(
            "fixture line {number}: la respuesta no trae id, así que no se insertó: {shown}"
        ));
    }
    Ok(())
}

/// What `eval_seed.py` does, in the same order: start the session in the eval project, then add
/// each line through `cuba_cronica`.
fn seed(session: &mut McpStdio, rows: &[(usize, Value)]) -> Result<(), String> {
    session.request(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "eval-seed-test", "version": "1.0"},
        }),
    )?;
    session.notify("notifications/initialized")?;
    session.call_tool(
        "cuba_jornada",
        json!({"action": "start", "name": "eval-smoke-seed", "project": EVAL_PROJECT}),
    )?;
    for (number, row) in rows {
        let body = session
            .call_tool(
                "cuba_cronica",
                json!({
                    "action": "add",
                    "entity_name": row["entity_name"],
                    "content": row["content"],
                    "observation_type": row["observation_type"],
                    "source": row.get("source").cloned().unwrap_or_else(|| json!("agent")),
                }),
            )
            .map_err(|why| format!("fixture line {number}: {why}"))?;
        check_inserted(*number, &body)?;
    }
    Ok(())
}

#[tokio::test]
async fn the_eval_fixture_goes_in_line_by_line_through_the_real_binary() {
    refuse_if_something_is_missing();
    let rows = fixture_lines();
    let expected = rows.len() as i64;
    in_a_scratch_database("brain_eval_seed", move |url| async move {
        let mut session = McpStdio::start(&url);
        seed(&mut session, &rows).unwrap_or_else(|why| panic!("{why}"));
        drop(session);

        let mut db = sqlx::PgConnection::connect(&url)
            .await
            .expect("conectando a la base desechable");
        let stored = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM brain_observations")
            .fetch_one(&mut db)
            .await
            .expect("contando brain_observations");
        assert_eq!(
            stored, expected,
            "la base tiene {stored} observaciones y la fixture tiene {expected} líneas"
        );
    })
    .await;
}
