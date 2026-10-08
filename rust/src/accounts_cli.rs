//! `memory-industry account …`: the host operator's side of accounts and pairing codes.
//!
//! PASS-1 STUB: `parse`, `render_list` and `render_pair` exist so the inline tests below
//! compile; they fail on purpose until pass 2 writes them. `run_cli` is still the slice-P
//! skeleton (`--help` works, everything else is "not implemented yet").

use anyhow::{Result, bail};

use crate::accounts_db::{AccountSummary, IssuedCode};

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

// PASS-1 STUB
pub fn parse(_args: &[String]) -> Result<Command> {
    bail!("PASS-1 STUB: account argument parsing is not implemented yet (slice F, pass 2)")
}

// PASS-1 STUB
pub fn render_list(_accounts: &[AccountSummary], _json: bool) -> String {
    String::new()
}

// PASS-1 STUB
pub fn render_pair(_issued: &IssuedCode) -> String {
    String::new()
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return Ok(());
    }
    bail!("account: not implemented yet")
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
}
