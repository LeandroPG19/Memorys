//! `memory-industry bridge doctor`: what the bridge tried to find the host, one line per
//! candidate, and the verdict. Nothing it prints can log anyone in.

use std::collections::HashSet;

use super::discovery::{Attempt, Discovery, Failure, Outcome, Verdict};
use super::state::BridgeState;
use crate::accounts;

/// A sweep tries a whole /24; past this many candidate lines the answer is buried.
const MAX_ATTEMPT_LINES: usize = 30;

const RE_PAIR: &str = "ejecuta `memory-industry bridge pair <código>` con un código nuevo que te dé \
     el operador de la madre";

/// A candidate that answered with something worth reading. The ones that did not answer, or
/// were never asked, are the bulk of a sweep and the first to be left out.
fn is_finding(attempt: &Attempt) -> bool {
    matches!(
        &attempt.outcome,
        Outcome::Checked(verdict) if !matches!(verdict, Verdict::NoAnswer(_))
    )
}

/// The indexes of the attempts that get a line: every finding first, then silent ones while
/// the budget lasts.
fn shown(attempts: &[Attempt]) -> HashSet<usize> {
    let (loud, quiet): (Vec<usize>, Vec<usize>) =
        (0..attempts.len()).partition(|i| is_finding(&attempts[*i]));
    loud.into_iter()
        .chain(quiet)
        .take(MAX_ATTEMPT_LINES)
        .collect()
}

fn attempt_line(attempt: &Attempt) -> String {
    format!(
        "{:<12} {}  {}",
        attempt.candidate.source.to_string(),
        attempt.candidate.addr,
        attempt.outcome
    )
}

fn attempt_lines(attempts: &[Attempt]) -> Vec<String> {
    let keep = shown(attempts);
    let mut lines: Vec<String> = attempts
        .iter()
        .enumerate()
        .filter(|(i, _)| keep.contains(i))
        .map(|(_, attempt)| attempt_line(attempt))
        .collect();
    let left_out = attempts.len() - keep.len();
    if left_out > 0 {
        lines.push(format!(
            "(+{left_out} candidatos más sin respuesta o sin probar, no listados)"
        ));
    }
    lines
}

fn advice(failure: &Failure) -> String {
    match failure {
        Failure::Revoked => format!("la madre revocó esta credencial: {RE_PAIR}"),
        Failure::Unknown => format!("la madre no conoce esta credencial: {RE_PAIR}"),
        Failure::WrongHost { .. } => format!(
            "contestó otra instalación de MemoryIndustry y no se le envió nada. Si la madre \
             cambió de verdad, {RE_PAIR}; si no, otro equipo tiene esa dirección"
        ),
        Failure::Impostor => format!(
            "algo contestó pero no pudo probar que es la madre y no se le envió ninguna \
             credencial. Si la madre es otra instalación, {RE_PAIR}"
        ),
        Failure::NotFound => "no se encontró la madre: comprueba que la PC madre está encendida \
             y en la misma red; si cambió de dirección, `memory-industry bridge pair <código> \
             --host <nombre-o-IP>` la indica a mano"
            .to_string(),
    }
}

/// The last line of a good report: the host that proved itself and the fingerprint the
/// operator compares with the one the host shows.
fn host_line(found: &Discovery) -> Option<String> {
    let addr = found.verified?;
    let fingerprint = found
        .attempts
        .iter()
        .find_map(|attempt| match &attempt.outcome {
            Outcome::Checked(Verdict::Verified { fingerprint })
                if attempt.candidate.addr == addr =>
            {
                Some(fingerprint.as_str())
            }
            _ => None,
        });
    Some(match fingerprint {
        Some(fingerprint) => format!("madre: {addr} (huella {fingerprint})"),
        None => format!("madre: {addr}"),
    })
}

pub fn render_report(found: &Discovery, state: Option<&BridgeState>) -> String {
    let mut lines = Vec::new();
    if let Some(pinned) = state {
        lines.push(format!(
            "fijada: {} (huella {}), cuenta {}/{}",
            pinned.host_name,
            accounts::fingerprint(&pinned.host_id),
            pinned.account,
            pinned.device_label
        ));
    }
    lines.push(format!("candidatos probados: {}", found.attempts.len()));
    lines.extend(attempt_lines(&found.attempts));
    match (host_line(found), found.failure()) {
        (Some(host), _) => lines.push(host),
        (None, Some(failure)) => lines.push(advice(&failure)),
        (None, None) => {}
    }
    lines.join("\n")
}

/// 0 when a host proved itself, 1 when none did.
pub fn exit_code(found: &Discovery) -> i32 {
    i32::from(found.verified.is_none())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use uuid::Uuid;

    use super::super::discovery::{Attempt, Candidate, Outcome, Source, Verdict};
    use super::super::fake_host::{Identity, hex_runs_of_64};
    use super::*;
    use crate::accounts;

    const FINGERPRINT: &str = "5F3A-91C2-77D0-AB14";

    fn addr(text: &str) -> SocketAddr {
        text.parse().expect("a socket address")
    }

    fn tried(source: Source, at: &str, verdict: Verdict) -> Attempt {
        Attempt {
            candidate: Candidate {
                addr: addr(at),
                source,
            },
            outcome: Outcome::Checked(verdict),
        }
    }

    fn no_answer(source: Source, at: &str) -> Attempt {
        tried(
            source,
            at,
            Verdict::NoAnswer("conexión rechazada o sin ruta".to_string()),
        )
    }

    fn verified(at: &str) -> Attempt {
        tried(
            Source::Sweep,
            at,
            Verdict::Verified {
                fingerprint: FINGERPRINT.to_string(),
            },
        )
    }

    /// The five attempts of the scenario, host found by the last one.
    fn five(seen: Uuid) -> Discovery {
        Discovery {
            verified: Some(addr("192.168.0.10:8787")),
            attempts: vec![
                no_answer(Source::LastGood, "192.168.0.20:8787"),
                tried(
                    Source::Stored,
                    "192.168.0.30:8787",
                    Verdict::WrongHost { seen },
                ),
                tried(Source::Name, "192.168.0.40:8787", Verdict::Impostor),
                tried(
                    Source::Sweep,
                    "192.168.0.50:8787",
                    Verdict::NotMemoryIndustry,
                ),
                verified("192.168.0.10:8787"),
            ],
        }
    }

    fn lines_with<'a>(report: &'a str, needle: &str) -> Vec<&'a str> {
        report
            .lines()
            .filter(|line| !line.starts_with("madre:") && line.contains(needle))
            .collect()
    }

    /// I3
    #[test]
    fn each_candidate_gets_one_line_with_source_address_verdict_and_fingerprint() {
        let seen = Uuid::new_v4();
        let report = render_report(&five(seen), None);

        let expected = [
            ("192.168.0.20:8787", "última buena", "sin respuesta"),
            ("192.168.0.30:8787", "guardada", "otra instalación"),
            (
                "192.168.0.40:8787",
                "por nombre",
                "no pudo probar que es la madre",
            ),
            ("192.168.0.50:8787", "barrido", "no es MemoryIndustry"),
            ("192.168.0.10:8787", "barrido", "verificada"),
        ];
        for (at, source, verdict) in expected {
            let found = lines_with(&report, at);
            assert_eq!(
                found.len(),
                1,
                "{at} must appear on exactly one line (one line per candidate). Report:\n{report}"
            );
            assert!(
                found[0].contains(source) && found[0].contains(verdict),
                "the line for {at} must say where it came from («{source}») and what it \
                 answered («{verdict}»). Got: {}",
                found[0]
            );
        }
        let wrong_host = lines_with(&report, "192.168.0.30:8787")[0];
        assert!(
            wrong_host.contains(&accounts::fingerprint(&seen)),
            "a host that answered as another installation is told apart by the fingerprint it \
             showed. Got: {wrong_host}"
        );
        assert!(
            lines_with(&report, "192.168.0.10:8787")[0].contains(FINGERPRINT),
            "the verified candidate shows the fingerprint the operator compares with the host"
        );
    }

    /// I3
    #[test]
    fn the_report_ends_with_the_host_it_found_and_exits_zero() {
        let found = five(Uuid::new_v4());

        let report = render_report(&found, None);

        assert_eq!(
            report.trim_end().lines().last(),
            Some("madre: 192.168.0.10:8787 (huella 5F3A-91C2-77D0-AB14)"),
            "the last line is the answer an operator reads first: which host and its fingerprint"
        );
        assert_eq!(exit_code(&found), 0, "a verified host is a healthy bridge");
    }

    fn none_verified() -> Discovery {
        Discovery {
            verified: None,
            attempts: vec![
                no_answer(Source::LastGood, "192.168.0.20:8787"),
                no_answer(Source::Name, "192.168.0.21:8787"),
                no_answer(Source::Sweep, "192.168.0.22:8787"),
            ],
        }
    }

    /// I3
    #[test]
    fn without_a_verified_host_the_report_says_what_was_tried_and_exits_one() {
        let found = none_verified();

        let report = render_report(&found, None);

        for at in [
            "192.168.0.20:8787",
            "192.168.0.21:8787",
            "192.168.0.22:8787",
        ] {
            assert!(
                report.contains(at),
                "the operator needs to see {at} was tried. Report:\n{report}"
            );
        }
        assert!(
            !report
                .lines()
                .any(|l| l.starts_with("madre:") && l.contains("huella")),
            "no host was verified, so no line may claim one. Report:\n{report}"
        );
        assert_eq!(
            exit_code(&found),
            1,
            "a bridge that cannot find its host must fail `bridge doctor` for scripts to notice"
        );
    }

    /// I3
    #[test]
    fn a_wrong_host_an_impostor_or_a_revoked_credential_say_what_to_do() {
        for (verdict, name) in [
            (
                Verdict::WrongHost {
                    seen: Uuid::new_v4(),
                },
                "WrongHost",
            ),
            (Verdict::Impostor, "Impostor"),
            (Verdict::Revoked, "Revoked"),
            (Verdict::Unknown, "Unknown"),
        ] {
            let found = Discovery {
                verified: None,
                attempts: vec![tried(Source::Stored, "192.168.0.30:8787", verdict)],
            };

            let report = render_report(&found, None);

            assert!(
                report.contains("bridge pair <código>"),
                "{name}: the only way out is a new pairing, and the report must name the \
                 command. Report:\n{report}"
            );
            assert_eq!(exit_code(&found), 1, "{name} is not a working bridge");
        }
    }

    /// I3
    #[test]
    fn the_report_never_carries_a_secret_a_bearer_or_a_credential_id() {
        let paired = Identity::new();
        let state = paired.state(
            8787,
            vec![addr("192.168.0.10:8787")],
            Some(addr("192.168.0.10:8787")),
        );
        let found = Discovery {
            verified: None,
            attempts: vec![
                no_answer(Source::LastGood, "192.168.0.10:8787"),
                tried(Source::Stored, "192.168.0.30:8787", Verdict::Revoked),
                tried(Source::Name, "192.168.0.40:8787", Verdict::Impostor),
            ],
        };

        let report = render_report(&found, Some(&state));

        assert!(
            report.contains("192.168.0.30:8787"),
            "the anchor: the report is not empty, so the absences below mean something. \
             Report:\n{report}"
        );
        let credential = paired.credential_id;
        for (needle, what) in [
            (paired.secret_hex(), "the secret"),
            (accounts::bearer(credential, &paired.secret), "the bearer"),
            (credential.to_string(), "the credential id"),
            (
                credential.simple().to_string(),
                "the credential id (simple)",
            ),
            ("mi1.".to_string(), "the credential prefix"),
        ] {
            assert!(
                !report.contains(&needle),
                "the report prints {what}: a pasted `bridge doctor` would hand a login to \
                 whoever reads the ticket. Report:\n{report}"
            );
        }
        assert!(
            hex_runs_of_64(report.as_bytes()).is_empty(),
            "a 64-digit hex run is the shape of a secret or a MAC, and has no place here"
        );
    }

    /// I3 (bounded)
    #[test]
    fn a_whole_subnet_sweep_stays_short_and_keeps_the_findings() {
        let mut attempts: Vec<Attempt> = (1..=253u32)
            .map(|n| no_answer(Source::Sweep, &format!("10.0.0.{n}:8787")))
            .collect();
        attempts.insert(
            200,
            tried(
                Source::Sweep,
                "10.0.0.254:8787",
                Verdict::WrongHost {
                    seen: Uuid::new_v4(),
                },
            ),
        );
        attempts.push(verified("10.0.1.7:8787"));
        let found = Discovery {
            verified: Some(addr("10.0.1.7:8787")),
            attempts,
        };

        let report = render_report(&found, None);

        assert!(
            report.lines().count() <= 40,
            "a sweep tries 253 addresses; one line each buries the answer. {} lines:\n{report}",
            report.lines().count()
        );
        assert!(
            !lines_with(&report, "10.0.0.254:8787").is_empty(),
            "bounded does not mean hiding the interesting ones: the host that answered as \
             another installation must still be listed. Report:\n{report}"
        );
        assert!(
            report.trim_end().ends_with("(huella 5F3A-91C2-77D0-AB14)"),
            "and the verdict line stays last. Report:\n{report}"
        );
    }

    /// The window holds thirty candidates and says nothing about leaving any out. Past it,
    /// the count is exactly the candidates that did not fit: forty minus thirty is ten.
    #[test]
    fn the_window_lists_thirty_candidates_and_counts_the_rest_exactly() {
        let sweep = |n: u32| -> Vec<Attempt> {
            (1..=n)
                .map(|i| no_answer(Source::Sweep, &format!("10.0.0.{i}:8787")))
                .collect()
        };

        let thirty = attempt_lines(&sweep(30));
        assert_eq!(
            thirty.len(),
            30,
            "a window that holds every candidate lists them and no count. Got: {thirty:?}"
        );
        assert!(
            thirty.iter().all(|line| !line.starts_with("(+")),
            "nothing was left out, so no line may count what was left out. Got: {thirty:?}"
        );

        let forty = attempt_lines(&sweep(40));
        assert_eq!(
            forty.len(),
            31,
            "thirty candidates and one line that counts the other ten. Got: {forty:?}"
        );
        assert_eq!(
            forty.last().map(String::as_str),
            Some("(+10 candidatos más sin respuesta o sin probar, no listados)"),
            "the count is what did not fit: 40 - 30"
        );
    }

    /// The fingerprint on the last line is the one of the candidate whose address was
    /// verified. Another candidate's fingerprint, even one that verified first, is not the
    /// one the operator compares with the host's own.
    #[test]
    fn the_last_line_carries_the_fingerprint_of_the_verified_address_only() {
        let verified_at = |at: &str, fingerprint: &str| {
            tried(
                Source::Sweep,
                at,
                Verdict::Verified {
                    fingerprint: fingerprint.to_string(),
                },
            )
        };
        let found = Discovery {
            verified: Some(addr("192.168.0.20:8787")),
            attempts: vec![
                verified_at("192.168.0.10:8787", "1111-2222-3333-4444"),
                verified_at("192.168.0.20:8787", FINGERPRINT),
            ],
        };
        assert_eq!(
            render_report(&found, None).lines().last(),
            Some("madre: 192.168.0.20:8787 (huella 5F3A-91C2-77D0-AB14)"),
            "the host line names the fingerprint of the address that verified"
        );

        let elsewhere = Discovery {
            verified: Some(addr("192.168.0.20:8787")),
            attempts: vec![verified_at("192.168.0.10:8787", FINGERPRINT)],
        };
        assert_eq!(
            render_report(&elsewhere, None).lines().last(),
            Some("madre: 192.168.0.20:8787"),
            "no candidate at the verified address carries a fingerprint, so none is claimed"
        );
    }
}
