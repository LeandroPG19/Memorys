//! `memory-industry setup client`: the block a client's MCP config gets so it talks to the
//! local bridge. The decisions live here (setup_agent.rs only wires): the block has no
//! credential and no host address, the merge never touches another server, and a second run
//! writes nothing.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::service::Target;

/// The clients `setup client` knows, in the order `all` writes them.
pub const CLIENTS: [&str; 4] = ["claude", "mcp", "cursor", "warp"];

#[derive(Debug, PartialEq, Eq)]
pub struct ClientArgs {
    pub clients: Vec<&'static str>,
    pub port: Option<u16>,
    pub client_id: Option<String>,
    pub apply: bool,
}

#[derive(Debug)]
pub struct MergeOutcome {
    pub updated: Value,
    pub changed: bool,
    pub replaced_stdio: bool,
}

/// What `apply_client_config` did, and the lines to tell the operator.
#[derive(Debug)]
pub struct Applied {
    pub changed: bool,
    pub replaced_stdio: bool,
    pub backup: Option<PathBuf>,
    pub lines: Vec<String>,
}

// PASS-1 STUB: every function answers "nothing" until pass 2 implements it.
pub fn bridge_block(_port: u16, _client_id: Option<&str>) -> Value {
    Value::Null
}

// PASS-1 STUB
pub fn merge_bridge_entry(cfg: &Value, _block: &Value) -> Result<MergeOutcome> {
    Ok(MergeOutcome {
        updated: cfg.clone(),
        changed: false,
        replaced_stdio: false,
    })
}

// PASS-1 STUB
pub fn apply_client_config(
    _path: &Path,
    _block: &Value,
    _now: DateTime<Utc>,
    _apply: bool,
) -> Result<Applied> {
    Ok(Applied {
        changed: false,
        replaced_stdio: false,
        backup: None,
        lines: Vec::new(),
    })
}

// PASS-1 STUB
pub fn client_config_path(_client: &str, _home: &str, _target: Target) -> Result<PathBuf> {
    Ok(PathBuf::new())
}

// PASS-1 STUB
pub fn resolve_port(_state_path: &Path, _flag: Option<u16>) -> Result<(u16, Option<String>)> {
    Ok((0, None))
}

// PASS-1 STUB
pub fn parse_client_args(_args: &[String]) -> Result<ClientArgs> {
    Ok(ClientArgs {
        clients: Vec::new(),
        port: None,
        client_id: None,
        apply: false,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use serde_json::json;

    use super::super::fake_host::{
        Identity, TempDir, dir_entries, hex_runs_of_64, write_state_json,
    };
    use super::*;
    use crate::bridge::STATE_BASENAME;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a fixed instant")
            .with_timezone(&Utc)
    }

    fn noon() -> DateTime<Utc> {
        at("2026-10-08T12:00:00Z")
    }

    fn plain_block() -> Value {
        json!({"type": "http", "url": "http://127.0.0.1:8797/mcp"})
    }

    /// A client config as a real one looks: other servers (one stdio, one http), the legacy
    /// key, and unrelated top-level settings the client keeps there.
    fn crowded() -> Value {
        json!({
            "numStartups": 7,
            "mcpServers": {
                "stdio-one": {"command": "npx", "args": ["-y", "thing"], "env": {"K": "v"}},
                "http-one": {
                    "type": "http",
                    "url": "http://tools.example/mcp",
                    "headers": {"X-Key": "abc"}
                },
                "cuba-memorys": {"command": "/old/cuba-memorys", "args": []}
            }
        })
    }

    fn merged(cfg: &Value, block: &Value) -> MergeOutcome {
        match merge_bridge_entry(cfg, block) {
            Ok(outcome) => outcome,
            Err(err) => panic!("merging into {cfg} must work. Got: {err:#}"),
        }
    }

    fn write_json(path: &Path, value: &Value) -> String {
        let text = serde_json::to_string_pretty(value).expect("a JSON document");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("a directory");
        }
        std::fs::write(path, &text).expect("the seed file");
        text
    }

    fn read_json(path: &Path) -> Value {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("{} must exist: {err}", path.display()));
        serde_json::from_str(&text).expect("the file holds JSON")
    }

    /// A file that was written long ago, so that a rewrite shows in its mtime.
    fn age(path: &Path) -> std::time::SystemTime {
        let old = UNIX_EPOCH + Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("the file opens")
            .set_modified(old)
            .expect("the mtime is set");
        old
    }

    fn has_dotted_quad(text: &str) -> bool {
        let words = text.split(|c: char| !(c.is_ascii_digit() || c == '.'));
        words.into_iter().any(|word| {
            let parts: Vec<&str> = word.split('.').collect();
            parts.len() == 4
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
    }

    // ── I4: the block ────────────────────────────────────────────────────────────────

    /// I4
    #[test]
    fn the_block_points_at_the_local_bridge() {
        assert_eq!(
            bridge_block(8797, None),
            plain_block(),
            "a client talks to http://127.0.0.1:<port>/mcp and nothing else: the bridge holds \
             the credential and finds the host"
        );
        assert_eq!(
            bridge_block(9100, None),
            json!({"type": "http", "url": "http://127.0.0.1:9100/mcp"}),
            "the port in the URL is the one the bridge listens on"
        );
    }

    /// I4
    #[test]
    fn a_client_id_adds_exactly_one_header() {
        assert_eq!(
            bridge_block(8797, Some("planta")),
            json!({
                "type": "http",
                "url": "http://127.0.0.1:8797/mcp",
                "headers": {"Mcp-Client-Id": "planta"}
            }),
            "`Mcp-Client-Id` keeps two windows of the same client apart; the block carries \
             nothing else"
        );
    }

    /// I4
    #[test]
    fn the_block_holds_no_credential_and_no_address_but_loopback() {
        for block in [bridge_block(8797, None), bridge_block(8797, Some("planta"))] {
            let text = serde_json::to_string(&block).expect("serialises");
            assert!(
                text.contains("127.0.0.1:8797"),
                "the anchor: the block is the real one, so the absences below mean something. \
                 Got: {text}"
            );
            for forbidden in ["authorization", "bearer", "mi1.", "token", "secret"] {
                assert!(
                    !text.to_ascii_lowercase().contains(forbidden),
                    "the block carries «{forbidden}»: a credential in a client config is what \
                     the bridge exists to avoid. Got: {text}"
                );
            }
            assert!(
                !has_dotted_quad(&text.replace("127.0.0.1", "loopback")),
                "the block names an address other than loopback: the host's IP must never be in \
                 a client config. Got: {text}"
            );
        }
    }

    // ── I5: the merge ────────────────────────────────────────────────────────────────

    /// I5
    #[test]
    fn the_merge_sets_our_entry_keeps_every_other_server_and_drops_the_legacy_key() {
        let before = crowded();

        let outcome = merged(&before, &plain_block());

        assert!(outcome.changed, "an entry that was missing is a change");
        assert_eq!(
            outcome.updated["mcpServers"]["memory-industry"],
            plain_block(),
            "the entry is exactly the bridge block"
        );
        for other in ["stdio-one", "http-one"] {
            assert_eq!(
                outcome.updated["mcpServers"][other], before["mcpServers"][other],
                "{other} belongs to somebody else and must come out semantically equal"
            );
        }
        assert!(
            outcome.updated["mcpServers"].get("cuba-memorys").is_none(),
            "the legacy key is retired: leaving it starts two memory servers"
        );
        assert_eq!(
            outcome.updated["numStartups"], before["numStartups"],
            "settings outside mcpServers are the client's own"
        );
    }

    /// I5
    #[test]
    fn merging_the_same_block_again_changes_nothing() {
        let once = merged(&crowded(), &plain_block());
        assert_eq!(
            once.updated["mcpServers"]["memory-industry"],
            plain_block(),
            "the anchor: the first merge did the work, so «nothing changes» below is a finding \
             and not an empty merge"
        );

        let twice = merged(&once.updated, &plain_block());

        assert!(
            !twice.changed,
            "the second run finds the file already right: changed must be false"
        );
        assert_eq!(twice.updated, once.updated, "and the content is the same");
    }

    /// I4/I5
    #[test]
    fn an_old_lan_entry_with_a_token_is_replaced_whole_not_merged() {
        let before = json!({"mcpServers": {"memory-industry": {
            "type": "http",
            "url": "http://192.168.0.10:8787/mcp",
            "headers": {"Authorization": "Bearer abc123", "Mcp-Client-Id": "ws"}
        }}});

        let outcome = merged(&before, &plain_block());

        assert!(
            outcome.changed,
            "the LAN entry differs from the bridge block"
        );
        assert_eq!(
            outcome.updated["mcpServers"]["memory-industry"],
            plain_block(),
            "a deep merge would keep the old Authorization header: the entry is replaced whole"
        );
        let text = outcome.updated.to_string();
        assert!(
            !text.contains("Bearer") && !text.contains("192.168.0.10"),
            "after `setup client` neither the token nor the host IP is left. Got: {text}"
        );
        assert!(
            !outcome.replaced_stdio,
            "an http entry becoming another http entry is not a move from local to bridge"
        );
    }

    /// I5
    #[test]
    fn a_stdio_entry_is_reported_as_replaced() {
        let stdio = json!({"mcpServers": {"memory-industry": {"command": "/bin/mi", "args": []}}});

        let outcome = merged(&stdio, &plain_block());

        assert!(
            outcome.replaced_stdio,
            "this client ran the server locally and now goes through the bridge: the operator \
             must be told"
        );
        let absent = merged(&json!({}), &plain_block());
        assert!(
            !absent.replaced_stdio,
            "a client that had no entry replaced nothing"
        );
    }

    /// I5
    #[test]
    fn a_config_that_is_not_an_object_is_refused_not_overwritten() {
        for (cfg, what) in [
            (json!([1, 2]), "a root array"),
            (json!({"mcpServers": ["x"]}), "an mcpServers array"),
        ] {
            let err = merge_bridge_entry(&cfg, &plain_block())
                .expect_err("overwriting a shape we do not understand would destroy the file");
            assert!(
                format!("{err:#}").contains("mcpServers") || what == "a root array",
                "{what}: the error must name the key it could not use. Got: {err:#}"
            );
        }
    }

    // ── I5: the file ─────────────────────────────────────────────────────────────────

    /// I5
    #[test]
    fn the_first_apply_backs_up_and_writes_and_the_second_writes_nothing() {
        let dir = TempDir::new("client-idem");
        let path = dir.path().join(".claude.json");
        let original = write_json(&path, &crowded());

        let first = apply_client_config(&path, &plain_block(), noon(), true)
            .expect("the first apply works");

        assert!(first.changed, "the file needed the bridge block");
        assert_eq!(
            dir_entries(dir.path()),
            vec![
                ".claude.json".to_string(),
                ".claude.json.bak-20261008T120000Z".to_string()
            ],
            "exactly one backup, stamped with the time, next to the file"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".claude.json.bak-20261008T120000Z"))
                .expect("the backup reads"),
            original,
            "the backup is the file as it was, so the change can be undone by copying it back"
        );
        let after = read_json(&path);
        assert_eq!(after["mcpServers"]["memory-industry"], plain_block());
        assert_eq!(
            after["mcpServers"]["stdio-one"],
            crowded()["mcpServers"]["stdio-one"]
        );
        assert_eq!(
            after["mcpServers"]["http-one"],
            crowded()["mcpServers"]["http-one"]
        );
        assert!(after["mcpServers"].get("cuba-memorys").is_none());

        let old = age(&path);
        let bytes = std::fs::read(&path).expect("reads");
        let second = apply_client_config(&path, &plain_block(), at("2026-10-08T13:00:00Z"), true)
            .expect("the second apply works");

        assert!(!second.changed, "the second run finds nothing to do");
        assert!(second.backup.is_none(), "and takes no backup");
        assert_eq!(
            std::fs::read(&path).expect("reads"),
            bytes,
            "the bytes are the same"
        );
        assert_eq!(
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime"),
            old,
            "the file was not even rewritten: some clients watch the file and reload on any \
             write"
        );
        assert_eq!(
            dir_entries(dir.path()).len(),
            2,
            "no second backup either: reruns must not pile copies up"
        );
    }

    /// I5
    #[test]
    fn replacing_a_stdio_entry_warns_that_the_client_goes_from_local_to_the_bridge() {
        let dir = TempDir::new("client-stdio");
        let path = dir.path().join("mcp.json");
        write_json(
            &path,
            &json!({"mcpServers": {"memory-industry": {"command": "/bin/mi", "args": []}}}),
        );

        let applied =
            apply_client_config(&path, &plain_block(), noon(), true).expect("the apply works");

        assert!(applied.replaced_stdio, "the old entry was a local process");
        let said = applied.lines.join("\n").to_lowercase();
        assert!(
            said.contains("local") && said.contains("puente"),
            "the output must say that this client now goes from local to the bridge. \
             Got: {said}"
        );
    }

    /// I5
    #[test]
    fn without_apply_nothing_on_disk_changes() {
        let dir = TempDir::new("client-plan");
        let path = dir.path().join(".claude.json");
        let original = write_json(&path, &crowded());
        let missing = dir.path().join("sub").join("mcp.json");

        let plan = apply_client_config(&path, &plain_block(), noon(), false).expect("a plan works");
        let plan_missing = apply_client_config(&missing, &plain_block(), noon(), false)
            .expect("a plan for a file that does not exist works");

        assert!(plan.changed, "the plan says the file WOULD change");
        assert!(
            plan_missing.changed,
            "and that a missing file WOULD be created"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads"),
            original,
            "a plan leaves the config byte for byte"
        );
        assert_eq!(
            dir_entries(dir.path()),
            vec![".claude.json".to_string()],
            "no backup, no new directory, no new file: a plan is read-only"
        );
    }

    /// I5
    #[test]
    fn a_missing_config_is_created_with_its_directories_and_needs_no_backup() {
        let dir = TempDir::new("client-new");
        let path = dir.path().join(".cursor").join("mcp.json");

        let applied = apply_client_config(&path, &plain_block(), noon(), true)
            .expect("a new config is created");

        assert!(applied.changed);
        assert!(applied.backup.is_none(), "there was nothing to back up");
        assert_eq!(
            read_json(&path),
            json!({"mcpServers": {"memory-industry": plain_block()}}),
            "a new file holds exactly the block"
        );
        assert_eq!(dir_entries(&dir.path().join(".cursor")), vec!["mcp.json"]);
    }

    /// I5
    #[test]
    fn a_config_that_is_not_json_is_left_alone() {
        let dir = TempDir::new("client-broken");
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "{ \"mcpServers\": ").expect("seed");

        let err = apply_client_config(&path, &plain_block(), noon(), true)
            .expect_err("a file we cannot read is not ours to replace");

        assert!(
            format!("{err:#}").contains(".claude.json"),
            "the error must name the file the operator has to look at. Got: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads"),
            "{ \"mcpServers\": ",
            "the file is untouched: treating it as empty would erase the user's other servers"
        );
        assert_eq!(
            dir_entries(dir.path()),
            vec![".claude.json".to_string()],
            "and no backup of a file we did not touch"
        );
    }

    /// I4
    #[test]
    fn a_paired_bridge_next_to_the_config_leaks_nothing_into_it() {
        let dir = TempDir::new("client-leak");
        let paired = Identity::new();
        let mut state = paired.state(
            8787,
            vec!["192.168.0.10:8787".parse().expect("addr")],
            Some("192.168.0.10:8787".parse().expect("addr")),
        );
        state.port = 9100;
        write_state_json(&dir.path().join(STATE_BASENAME), &state);
        let (port, _) = resolve_port(&dir.path().join(STATE_BASENAME), None)
            .expect("the state file is readable");
        let path = dir.path().join(".claude.json");

        apply_client_config(&path, &bridge_block(port, Some("planta")), noon(), true)
            .expect("the apply works");

        let text = std::fs::read_to_string(&path).expect("reads");
        assert!(
            text.contains("127.0.0.1:9100"),
            "the anchor: the block was written with the port from bridge.json. Got: {text}"
        );
        for (needle, what) in [
            (paired.secret_hex(), "the secret"),
            ("192.168.0.10".to_string(), "the host address"),
            ("Authorization".to_string(), "an Authorization header"),
            ("Bearer".to_string(), "a bearer"),
            ("mi1.".to_string(), "the credential prefix"),
            (
                paired.credential_id.simple().to_string(),
                "the credential id",
            ),
        ] {
            assert!(
                !text.contains(&needle),
                "the client config holds {what}: it is read by every program of that user. \
                 Got: {text}"
            );
        }
        assert!(hex_runs_of_64(text.as_bytes()).is_empty());
    }

    // ── I6: paths and port ───────────────────────────────────────────────────────────

    /// I6
    #[test]
    fn each_client_has_its_own_file_under_the_windows_home() {
        for (client, expected) in [
            ("claude", r"C:\Users\ana\.claude.json"),
            ("mcp", r"C:\Users\ana\.mcp.json"),
            ("cursor", r"C:\Users\ana\.cursor\mcp.json"),
            ("warp", r"C:\Users\ana\.warp\.mcp.json"),
        ] {
            let path = client_config_path(client, r"C:\Users\ana", Target::Windows)
                .unwrap_or_else(|err| panic!("{client} is a known client. Got: {err:#}"));
            assert_eq!(
                path.to_string_lossy(),
                expected,
                "{client}'s config is read from this file; any other name is a file nobody reads"
            );
        }
    }

    /// I6
    #[test]
    fn on_linux_the_same_clients_live_under_the_home_directory() {
        let path = client_config_path("cursor", "/home/ana", Target::Linux)
            .expect("cursor is a known client");
        assert_eq!(path, Path::new("/home/ana/.cursor/mcp.json"));
    }

    /// I6
    #[test]
    fn an_unknown_client_is_refused_by_name() {
        let err = client_config_path("vim", r"C:\Users\ana", Target::Windows)
            .expect_err("nobody knows where vim keeps its MCP config");
        assert!(
            format!("{err:#}").contains("vim"),
            "the error must name what it did not understand. Got: {err:#}"
        );
    }

    /// I6
    #[test]
    fn the_port_comes_from_bridge_json_and_otherwise_from_the_default_with_a_warning() {
        let dir = TempDir::new("client-port");
        let state_path = dir.path().join(STATE_BASENAME);

        let (absent_port, warning) = resolve_port(&state_path, None).expect("a missing file is ok");
        assert_eq!(
            absent_port, 8797,
            "unpaired: the default port of the bridge"
        );
        let warning = warning.expect("an unpaired device is told so");
        assert!(
            warning.contains("bridge pair"),
            "the warning says what to run next. Got: {warning}"
        );

        let mut state = Identity::new().state(8787, Vec::new(), None);
        state.port = 9100;
        write_state_json(&state_path, &state);
        assert_eq!(
            resolve_port(&state_path, None).expect("readable"),
            (9100, None),
            "a paired device uses the port its bridge listens on, silently"
        );
        assert_eq!(
            resolve_port(&state_path, Some(9200)).expect("readable"),
            (9200, None),
            "--port wins over the file"
        );
    }

    /// I6
    #[test]
    fn a_state_file_from_another_version_is_an_error_naming_it() {
        let dir = TempDir::new("client-port-bad");
        let state_path = dir.path().join(STATE_BASENAME);
        std::fs::write(&state_path, r#"{"version": 99}"#).expect("seed");

        let err = resolve_port(&state_path, None)
            .expect_err("guessing a port from a file we cannot read points the client elsewhere");

        assert!(
            format!("{err:#}").contains(STATE_BASENAME),
            "the error must name the file. Got: {err:#}"
        );
    }

    // ── the command line ─────────────────────────────────────────────────────────────

    /// I5/I6
    #[test]
    fn the_arguments_name_one_client_or_all_and_carry_their_options() {
        let parsed = parse_client_args(&strings(&[
            "claude",
            "--port",
            "9100",
            "--client-id",
            "planta",
            "--apply",
        ]))
        .expect("a complete command parses");
        assert_eq!(
            parsed,
            ClientArgs {
                clients: vec!["claude"],
                port: Some(9100),
                client_id: Some("planta".to_string()),
                apply: true,
            }
        );

        let all = parse_client_args(&strings(&["all"])).expect("all parses");
        assert_eq!(
            all,
            ClientArgs {
                clients: vec!["claude", "mcp", "cursor", "warp"],
                port: None,
                client_id: None,
                apply: false,
            },
            "`all` is the four known clients, and without --apply it is a plan"
        );
        assert_eq!(
            parse_client_args(&strings(&["--apply", "cursor"]))
                .expect("options may come first")
                .clients,
            vec!["cursor"]
        );
    }

    /// I5/I6
    #[test]
    fn a_bad_command_line_is_refused_with_what_to_fix() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            (vec![], "all"),
            (vec!["nope"], "nope"),
            (vec!["claude", "--port", "0"], "port"),
            (vec!["claude", "--port", "70000"], "port"),
            (vec!["claude", "--port", "abc"], "port"),
            (vec!["claude", "--bogus"], "--bogus"),
            (vec!["claude", "--client-id", ""], "client-id"),
        ];
        for (args, mentions) in cases {
            let err = parse_client_args(&strings(&args))
                .expect_err(&format!("`setup client {args:?}` must be refused"));
            assert!(
                format!("{err:#}").contains(mentions),
                "`setup client {args:?}` is refused, but the reason must mention «{mentions}». \
                 Got: {err:#}"
            );
        }
        assert!(
            parse_client_args(&strings(&["claude", "mcp"])).is_err(),
            "two clients at once is `all`, or two commands: guessing which was meant writes the \
             wrong file"
        );
        assert!(
            parse_client_args(&strings(&["claude", "--port"])).is_err(),
            "`--port` with no value must not swallow the next flag or default silently"
        );
    }
}
