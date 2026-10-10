//! `memory-industry account …`: the host operator's side of accounts and pairing codes.
//!
//! It talks to the database directly, like `sync` and `project`, and never needs the daemon or
//! its token. Nothing it prints can log anyone in: a device is named by the first 8 hex of its
//! credential id, and the one secret that exists, the pairing code, is printed once, by `pair`.

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;

use crate::accounts_db::{self, AccountSummary, IssuedCode};

const USAGE: &str = "usage: memory-industry account <add|list|revoke|revoke-device|pair> …\n\n\
Run on the host. Accounts authenticate and attribute; they do not partition the graph.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Add {
        name: String,
    },
    List {
        json: bool,
    },
    Revoke {
        name: String,
    },
    RevokeDevice {
        name: String,
        label: String,
    },
    Pair {
        name: String,
        device: Option<String>,
    },
    Help,
}

/// What `scan` found: the plain arguments in order, and the two options a command may take.
#[derive(Default)]
struct Scanned {
    positional: Vec<String>,
    json: bool,
    device: Option<String>,
}

/// An option that carries something: `--json` is a switch, `--device` takes the next argument.
fn take_option<'a>(
    out: &mut Scanned,
    option: &str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Result<()> {
    if option == "--json" {
        out.json = true;
        return Ok(());
    }
    let value = rest
        .next()
        .ok_or_else(|| anyhow!("{option} needs a value"))?;
    out.device = Some(value.clone());
    Ok(())
}

/// Splits `rest` into positional arguments and the `allowed` options. An option nobody asked
/// for is refused by name: a typo in `--device` must not mint a code for the wrong device.
fn scan(rest: &[String], allowed: &[&str]) -> Result<Scanned> {
    let mut out = Scanned::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if !arg.starts_with("--") {
            out.positional.push(arg.clone());
        } else if !allowed.contains(&arg.as_str()) {
            bail!("unknown option {arg} (see `memory-industry account --help`)");
        } else {
            take_option(&mut out, arg, &mut it)?;
        }
    }
    Ok(out)
}

/// `scan`, then exactly one positional argument for each of `names`.
fn scan_exact(rest: &[String], allowed: &[&str], names: &[&str]) -> Result<Scanned> {
    let scanned = scan(rest, allowed)?;
    if let Some(missing) = names.get(scanned.positional.len()) {
        bail!("missing {missing} (see `memory-industry account --help`)");
    }
    if let Some(extra) = scanned.positional.get(names.len()) {
        bail!("unexpected argument {extra:?} (see `memory-industry account --help`)");
    }
    Ok(scanned)
}

fn one_name(rest: &[String]) -> Result<String> {
    Ok(scan_exact(rest, &[], &["account name"])?
        .positional
        .remove(0))
}

fn parse_revoke_device(rest: &[String]) -> Result<Command> {
    let mut s = scan_exact(rest, &[], &["account name", "device label"])?;
    let label = s.positional.remove(1);
    Ok(Command::RevokeDevice {
        name: s.positional.remove(0),
        label,
    })
}

fn parse_pair(rest: &[String]) -> Result<Command> {
    let mut s = scan_exact(rest, &["--device"], &["account name"])?;
    Ok(Command::Pair {
        name: s.positional.remove(0),
        device: s.device,
    })
}

pub fn parse(args: &[String]) -> Result<Command> {
    let Some((sub, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    match sub.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "add" => Ok(Command::Add {
            name: one_name(rest)?,
        }),
        "revoke" => Ok(Command::Revoke {
            name: one_name(rest)?,
        }),
        "list" => Ok(Command::List {
            json: scan_exact(rest, &["--json"], &[])?.json,
        }),
        "revoke-device" => parse_revoke_device(rest),
        "pair" => parse_pair(rest),
        other => bail!("unknown account command {other:?} (see `memory-industry account --help`)"),
    }
}

fn when(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// The first 8 hex of a credential id: enough to tell devices apart, half of nothing a bearer needs.
fn short_id(id: &uuid::Uuid) -> String {
    id.simple().to_string()[..8].to_string()
}

fn state_of(revoked_at: Option<DateTime<Utc>>) -> &'static str {
    if revoked_at.is_some() {
        "revoked"
    } else {
        "active"
    }
}

fn list_json(accounts: &[AccountSummary]) -> serde_json::Value {
    accounts
        .iter()
        .map(|a| {
            let devices: Vec<_> = a
                .credentials
                .iter()
                .map(|c| {
                    json!({
                        "id": short_id(&c.id),
                        "label": c.device_label,
                        "state": state_of(c.revoked_at),
                        "created_at": c.created_at,
                        "last_seen_at": c.last_seen_at,
                    })
                })
                .collect();
            json!({
                "name": a.name,
                "state": state_of(a.revoked_at),
                "created_at": a.created_at,
                "credentials": devices,
            })
        })
        .collect()
}

fn list_text(accounts: &[AccountSummary]) -> String {
    if accounts.is_empty() {
        return "no accounts yet: create one with `memory-industry account add <name>`".into();
    }
    let mut out = String::new();
    for a in accounts {
        let state = state_of(a.revoked_at);
        out.push_str(&format!(
            "{}  {state}  created {}\n",
            a.name,
            when(a.created_at)
        ));
        for c in &a.credentials {
            let state = state_of(c.revoked_at);
            let seen = c.last_seen_at.map_or("never".to_string(), when);
            out.push_str(&format!(
                "  {}  {}  {state}  last seen {seen}\n",
                short_id(&c.id),
                c.device_label
            ));
        }
    }
    out
}

pub fn render_list(accounts: &[AccountSummary], json: bool) -> String {
    if json {
        return serde_json::to_string_pretty(&list_json(accounts)).unwrap_or_default();
    }
    list_text(accounts)
}

/// The code appears once, inside the command the client types, so the operator can read it out
/// whole; the fingerprint lets the client check by eye that it reached the right host. The
/// expiry is cut to the minute, which can only make it earlier than the real one.
pub fn render_pair(issued: &IssuedCode) -> String {
    format!(
        "Pairing code issued. It is shown only now and is stored only as a key.\n\n\
         On the client, run:\n\n  memory-industry bridge pair {}\n\n\
         Host fingerprint: {} (the client prints the same one; compare them)\n\
         valid until {} (24 hours, single use)\n",
        issued.code,
        issued.fingerprint,
        when(issued.expires_at)
    )
}

async fn pool() -> Result<PgPool> {
    let url = crate::setup::resolve_database_url().await;
    crate::db::create_pool(&url)
        .await
        .map_err(|e| e.context("connecting to database"))
}

async fn pair(pool: &PgPool, name: &str, device: Option<&str>, now: DateTime<Utc>) -> Result<()> {
    let issued = accounts_db::issue_pairing_code(pool, name, device, now).await?;
    print!("{}", render_pair(&issued));
    Ok(())
}

async fn revoke(pool: &PgPool, name: &str, now: DateTime<Utc>) -> Result<()> {
    let report = accounts_db::revoke_account(pool, name, now).await?;
    println!(
        "revoked account {name:?}: {} device(s) revoked, {} unused pairing code(s) deleted",
        report.credentials_revoked, report.codes_deleted
    );
    Ok(())
}

async fn revoke_device(pool: &PgPool, name: &str, label: &str, now: DateTime<Utc>) -> Result<()> {
    let revoked = accounts_db::revoke_device(pool, name, label, now).await?;
    println!("revoked {revoked} device(s) labelled {label:?} on account {name:?}");
    Ok(())
}

async fn execute(pool: &PgPool, command: Command, now: DateTime<Utc>) -> Result<()> {
    match command {
        Command::Add { name } => {
            let created = accounts_db::add_account(pool, &name, now).await?;
            println!("created account {:?}", created.name);
            Ok(())
        }
        Command::List { json } => {
            println!(
                "{}",
                render_list(&accounts_db::list_accounts(pool).await?, json)
            );
            Ok(())
        }
        Command::Revoke { name } => revoke(pool, &name, now).await,
        Command::RevokeDevice { name, label } => revoke_device(pool, &name, &label, now).await,
        Command::Pair { name, device } => pair(pool, &name, device.as_deref(), now).await,
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return Ok(());
    }
    let command = parse(args)?;
    if command == Command::Help {
        println!("{USAGE}");
        return Ok(());
    }
    execute(&pool().await?, command, Utc::now()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts_db::CredentialSummary;
    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 9, hour, minute, second)
            .single()
            .expect("a valid instant")
    }

    #[test]
    fn every_subcommand_parses_into_its_own_command() {
        let cases = [
            (argv(&["add", "ana"]), Command::Add { name: "ana".into() }),
            (argv(&["list"]), Command::List { json: false }),
            (argv(&["list", "--json"]), Command::List { json: true }),
            (
                argv(&["revoke", "ana"]),
                Command::Revoke { name: "ana".into() },
            ),
            (
                argv(&["revoke-device", "ana", "pc 1"]),
                Command::RevokeDevice {
                    name: "ana".into(),
                    label: "pc 1".into(),
                },
            ),
            (
                argv(&["pair", "ana"]),
                Command::Pair {
                    name: "ana".into(),
                    device: None,
                },
            ),
            (
                argv(&["pair", "ana", "--device", "pc-1"]),
                Command::Pair {
                    name: "ana".into(),
                    device: Some("pc-1".into()),
                },
            ),
            (argv(&["--help"]), Command::Help),
            (argv(&["help"]), Command::Help),
        ];
        for (args, expected) in cases {
            let parsed = parse(&args).unwrap_or_else(|e| {
                panic!(
                    "`account {}` must parse, and failed with: {e:#}",
                    args.join(" ")
                )
            });
            assert_eq!(
                parsed,
                expected,
                "`account {}` parsed into the wrong command: the operator would run something \
                 other than what they typed",
                args.join(" ")
            );
        }
    }

    #[test]
    fn a_missing_name_is_asked_for_by_name() {
        for args in [argv(&["pair"]), argv(&["add"])] {
            let err = parse(&args)
                .expect_err("a command that needs an account name cannot parse without one");
            assert!(
                format!("{err:#}").contains("name"),
                "`account {}` without a name must say that a name is missing, got: {err:#}",
                args.join(" ")
            );
        }
    }

    #[test]
    fn an_unknown_option_is_refused_by_name() {
        for args in [
            argv(&["pair", "ana", "--force"]),
            argv(&["list", "--force"]),
            argv(&["add", "ana", "--force"]),
        ] {
            let err = parse(&args).expect_err(
                "an option the command does not know must not be silently ignored: a typo in \
                 --device would mint a code for the wrong device",
            );
            assert!(
                format!("{err:#}").contains("--force"),
                "the refusal of `account {}` must name the option it refused, got: {err:#}",
                args.join(" ")
            );
        }
    }

    #[test]
    fn a_device_option_without_its_value_is_refused() {
        let err = parse(&argv(&["pair", "ana", "--device"]))
            .expect_err("--device with nothing after it has no label to fix");
        assert!(
            format!("{err:#}").contains("--device"),
            "the refusal must name --device, got: {err:#}"
        );
    }

    fn two_accounts() -> Vec<AccountSummary> {
        let active = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let revoked = Uuid::from_u128(0xfedc_ba98_7654_3210_fedc_ba98_7654_3210);
        vec![
            AccountSummary {
                id: Uuid::from_u128(1),
                name: "ana".into(),
                created_at: at(8, 0, 0),
                revoked_at: None,
                credentials: vec![
                    CredentialSummary {
                        id: active,
                        device_label: "pc lab 1".into(),
                        created_at: at(8, 5, 0),
                        last_seen_at: None,
                        revoked_at: None,
                    },
                    CredentialSummary {
                        id: revoked,
                        device_label: "pc-02".into(),
                        created_at: at(8, 6, 0),
                        last_seen_at: Some(at(9, 0, 0)),
                        revoked_at: Some(at(9, 30, 0)),
                    },
                ],
            },
            AccountSummary {
                id: Uuid::from_u128(2),
                name: "zed".into(),
                created_at: at(8, 1, 0),
                revoked_at: Some(at(9, 40, 0)),
                credentials: vec![],
            },
        ]
    }

    #[test]
    fn the_list_names_each_device_by_a_short_id_and_its_state() {
        let text = render_list(&two_accounts(), false);
        for needle in [
            "ana", "zed", "01234567", "fedcba98", "pc lab 1", "pc-02", "active", "revoked", "never",
        ] {
            assert!(
                text.contains(needle),
                "`account list` must show {needle:?}: the operator tells devices apart by the \
                 8-hex id, the label and the state, and sees `never` for a device that has not \
                 called yet. Output was:\n{text}"
            );
        }
        assert!(
            text.find("ana") < text.find("zed"),
            "the list keeps the order it was given, which is by name:\n{text}"
        );
    }

    #[test]
    fn the_list_never_prints_a_whole_credential_id_or_a_bearer() {
        let text = render_list(&two_accounts(), false);
        assert!(
            text.contains("01234567"),
            "control: the list shows the short id, so the absences below are absences and not an \
             empty output:\n{text}"
        );
        assert!(
            !text.contains("0123456789abcdef0123456789abcdef"),
            "the whole credential id is half of a bearer; the list shows the first 8 hex \
             only:\n{text}"
        );
        assert!(
            !text.contains("mi1."),
            "no bearer may ever appear in `account list`:\n{text}"
        );
    }

    #[test]
    fn the_json_list_is_an_array_of_accounts_in_order() {
        let text = render_list(&two_accounts(), true);
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`account list --json` must be JSON ({e}); got:\n{text}"));
        let accounts = parsed
            .as_array()
            .unwrap_or_else(|| panic!("the JSON list is an array of accounts; got:\n{text}"));
        assert_eq!(accounts.len(), 2, "one element per account:\n{text}");
        assert_eq!(accounts[0]["name"], "ana", "first by name:\n{text}");
        assert_eq!(accounts[1]["name"], "zed", "then by name:\n{text}");
        assert!(
            !text.contains("secret") && !text.contains("mi1."),
            "no secret material in the JSON list either:\n{text}"
        );
    }

    fn issued() -> IssuedCode {
        IssuedCode {
            code: "K7QM-3WXP-9HNR".into(),
            expires_at: Utc
                .with_ymd_and_hms(2026, 10, 9, 12, 0, 30)
                .single()
                .expect("a valid instant"),
            fingerprint: "5F3A-91C2-77D0-AB14".into(),
        }
    }

    #[test]
    fn the_pairing_output_shows_the_code_once_inside_the_client_command() {
        let text = render_pair(&issued());
        assert_eq!(
            text.matches("K7QM-3WXP-9HNR").count(),
            1,
            "the code is shown exactly once; a second copy is one more place to leak it \
             from:\n{text}"
        );
        assert!(
            !text.contains("K7QM3WXP9HNR"),
            "no unhyphenated copy of the code either:\n{text}"
        );
        let command_line = text
            .lines()
            .find(|l| l.contains("K7QM-3WXP-9HNR"))
            .unwrap_or_else(|| panic!("the output must hold the code; it was:\n{text}"));
        assert!(
            command_line
                .trim_start()
                .starts_with("memory-industry bridge pair"),
            "the one copy of the code sits in the command the client types, so the operator \
             can read it out whole; the line was {command_line:?}"
        );
        assert!(
            text.contains("5F3A-91C2-77D0-AB14"),
            "the host fingerprint is printed so the client can compare it by eye:\n{text}"
        );
        assert!(
            text.contains("valid until 2026-10-09 12:00 UTC"),
            "the expiry is shown in UTC to the minute, rounded down so it is never later than \
             the real one:\n{text}"
        );
    }

    /// `connect_lazy` only parses the URL and never dials it. Each test that gets this pool must
    /// be refused before its first query, or it would wait out the timeout and fail there.
    fn pool_that_is_never_dialled() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://accounts-cli-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn the_operator_commands_refuse_a_bad_name_before_the_database() {
        let pool = pool_that_is_never_dialled();
        let now = at(12, 0, 0);

        let err = pair(&pool, "a b", None, now)
            .await
            .expect_err("a name with a space must not get a pairing code");
        assert!(
            format!("{err:#}").contains("account name"),
            "`account pair` must say which part was refused: {err:#}"
        );

        let err = revoke_device(&pool, "a b", "pc-1", now)
            .await
            .expect_err("a name with a space must not revoke a device");
        assert!(
            format!("{err:#}").contains("account name"),
            "`account revoke-device` must say which part was refused: {err:#}"
        );

        let err = execute(&pool, Command::Add { name: "a b".into() }, now)
            .await
            .expect_err("a name with a space must not create an account");
        assert!(
            format!("{err:#}").contains("account name"),
            "`account add` must say which part was refused: {err:#}"
        );
    }

    #[tokio::test]
    async fn run_cli_refuses_an_unknown_command_before_the_database() {
        let err = run_cli(&argv(&["bogus"]))
            .await
            .expect_err("an unknown command must exit non-zero, not succeed silently");
        assert!(
            format!("{err:#}").contains("unknown account command"),
            "the refusal must name the problem: {err:#}"
        );
    }

    /// `account revoke` is the command that takes an account out of service, so the database has
    /// to be the one that does it: `revoke -> Ok(())` would return success and touch nothing. A
    /// name with a space is refused by the name rule before the first query, so the pool is never
    /// dialled.
    #[tokio::test]
    async fn the_revoke_command_refuses_a_bad_name_before_the_database() {
        let pool = pool_that_is_never_dialled();
        let err = revoke(&pool, "a b", at(12, 0, 0))
            .await
            .expect_err("a name with a space must not revoke an account");
        assert!(
            format!("{err:#}").contains("account name"),
            "`account revoke` must say which part was refused: {err:#}"
        );
    }
}
