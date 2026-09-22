use anyhow::Result;
use serde_json::Value;

const SECRET_FIELD_NAMES: [(&str, &str); 7] = [
    ("password", "password field"),
    ("passwd", "password field"),
    ("pwd", "password field"),
    ("token", "token field"),
    ("secret", "secret field"),
    ("api_key", "api key field"),
    ("apikey", "api key field"),
];

const PROVIDER_PREFIXES: [(&str, &str); 7] = [
    ("sk-", "provider api key"),
    ("ghp_", "github token"),
    ("gho_", "github token"),
    ("github_pat_", "github token"),
    ("xoxb-", "slack token"),
    ("xoxp-", "slack token"),
    ("AKIA", "aws access key id"),
];

const MIN_OPAQUE_VALUE_CHARS: usize = 6;
const MIN_ALL_LETTER_VALUE_CHARS: usize = 16;

struct Hit {
    pattern: &'static str,
    char_offset: usize,
}

struct Scan {
    redacted: String,
    hit: Option<Hit>,
}

fn secret_field_pattern(key: &str) -> Option<&'static str> {
    // `'\u{5f}'` is `'_'`: lizard's Rust reader reads a plain `'_'` as the
    // lifetime `'_` and measures nothing to the next apostrophe. That is where
    // the CC 23 in lizard-baseline.txt came from: the lost region, not this
    // body. Long version in service.rs::keys_offered.
    let key = key.trim_matches(|c: char| !c.is_alphanumeric() && c != '\u{5f}');
    let lower = key.to_lowercase();
    SECRET_FIELD_NAMES
        .iter()
        .find(|(name, _)| lower.ends_with(name))
        .map(|(_, pattern)| *pattern)
}

fn value_is_opaque(value: &str) -> bool {
    let value = value.trim_matches(|c: char| !c.is_alphanumeric());
    let chars = value.chars().count();
    chars >= MIN_OPAQUE_VALUE_CHARS
        && (value.chars().any(|c| c.is_ascii_digit()) || chars >= MIN_ALL_LETTER_VALUE_CHARS)
}

fn scan(s: &str) -> Scan {
    let mut out = String::with_capacity(s.len());
    let mut hit: Option<Hit> = None;
    let mut expecting_value: Option<&'static str> = None;
    let mut char_offset = 0usize;

    for token in s.split_inclusive(char::is_whitespace) {
        let at = char_offset;
        char_offset += token.chars().count();

        let trimmed = token.trim_end();
        let trailing = &token[trimmed.len()..];

        if trimmed.is_empty() {
            out.push_str(token);
            continue;
        }

        if let Some(pattern) = expecting_value.take() {
            out.push_str("***");
            out.push_str(trailing);
            if hit.is_none() && value_is_opaque(trimmed) {
                hit = Some(Hit {
                    pattern,
                    char_offset: at,
                });
            }
            continue;
        }

        if let Some(at_sign) = trimmed.find('@')
            && let Some(scheme_end) = trimmed.find("://")
            && at_sign > scheme_end
        {
            let creds = &trimmed[scheme_end + 3..at_sign];
            if let Some(colon) = creds.find(':') {
                out.push_str(&trimmed[..scheme_end + 3 + colon + 1]);
                out.push_str("***");
                out.push_str(&trimmed[at_sign..]);
                out.push_str(trailing);
                if hit.is_none() {
                    hit = Some(Hit {
                        pattern: "credentials in a url",
                        char_offset: at,
                    });
                }
                continue;
            }
        }

        if let Some(sep) = trimmed.find(['=', ':'])
            && sep > 0
            && let Some(pattern) = secret_field_pattern(&trimmed[..sep])
        {
            out.push_str(&trimmed[..=sep]);
            if sep + 1 < trimmed.len() {
                out.push_str("***");
                if hit.is_none() && value_is_opaque(&trimmed[sep + 1..]) {
                    hit = Some(Hit {
                        pattern,
                        char_offset: at,
                    });
                }
            } else {
                expecting_value = Some(pattern);
            }
            out.push_str(trailing);
            continue;
        }

        let bare = trimmed.trim_start_matches(|c: char| !c.is_alphanumeric());
        let embedded = PROVIDER_PREFIXES
            .iter()
            .filter_map(|(prefix, pattern)| bare.find(prefix).map(|at| (at, *prefix, *pattern)))
            .filter(|(at, prefix, _)| bare.len() - at > prefix.len() + 8)
            .min_by_key(|(at, _, _)| *at);
        let provider = embedded.map(|(_, _, pattern)| pattern);
        let jwt = (bare.starts_with("eyJ") && bare.matches('.').count() == 2)
            .then_some("jwt bearer token");

        if let Some(pattern) = provider.or(jwt) {
            let keep = match embedded {
                Some((at, _, _)) if at > 0 => &bare[..at],
                _ => "",
            };
            let prefix_len = trimmed.len() - bare.len();
            out.push_str(&trimmed[..prefix_len]);
            out.push_str(keep);
            out.push_str("***");
            out.push_str(trailing);
            if hit.is_none() {
                hit = Some(Hit {
                    pattern,
                    char_offset: at,
                });
            }
            continue;
        }

        out.push_str(token);
    }

    Scan { redacted: out, hit }
}

pub fn redact_secrets(s: &str) -> String {
    scan(s).redacted
}

pub fn looks_like_secret(s: &str) -> Option<&'static str> {
    scan(s).hit.map(|hit| hit.pattern)
}

pub fn refuse_secrets(args: &Value, field: &str, text: &str) -> Result<()> {
    if args.get("allow_secret").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }

    match scan(text).hit {
        None => Ok(()),
        Some(hit) => anyhow::bail!(
            "refusing to write {field}: what looks like a {} starts near character {} of it. \
             Stored memory comes back in every search, is exported to JSON files that live \
             inside a git repository, and is served to any client that reaches this graph — a \
             credential written here does not stay here. Remove it and store a pointer to where \
             the credential lives instead, or pass allow_secret=true if it is not a live one.",
            hit.pattern,
            hit.char_offset
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_reach_the_llm() {
        let dirty = "la app conecta a postgresql://cuba:hunter2-fake@127.0.0.1:5488/brain";
        let clean = redact_secrets(dirty);
        assert!(
            !clean.contains("hunter2-fake"),
            "la contraseña salió al prompt: {clean}"
        );
        assert!(clean.contains("postgresql://cuba:***@127.0.0.1:5488/brain"));
    }

    #[test]
    fn provider_tokens_and_jwts_are_stripped() {
        assert_eq!(
            redact_secrets("token ghp_abcdefghijklmnop fin"),
            "token *** fin"
        );
        for glued in [
            "token=ghp_abcdefghijklmnop",
            "Authorization:ghp_abcdefghijklmnop",
            "GITHUB_TOKEN=ghp_abcdefghijklmnop",
            "usa-ghp_abcdefghijklmnop-aqui",
        ] {
            assert!(
                !redact_secrets(glued).contains("ghp_abcdefghijklmnop"),
                "a token glued to a word survived redaction: {glued:?}. The scan trimmed only \
                 leading NON-alphanumeric characters, so a quote or a bracket in front was \
                 stripped and the token found — but `token=`, `GITHUB_TOKEN=` and \
                 `Authorization:` start with letters, and those are the shapes a credential \
                 actually has in an env file, a header or an error message. The same scan backs \
                 refuse_secrets, so this was also the write gate letting one through"
            );
            assert!(
                looks_like_secret(glued).is_some(),
                "and the gate that refuses writes has to see it too: {glued:?}"
            );
        }
        assert_eq!(redact_secrets("bearer eyJhbG.eyJzdWI.SflKxw"), "bearer ***");
        assert!(!redact_secrets("key sk-ant-api03-XXXXXXXXXXXX").contains("sk-ant"));
        assert_eq!(redact_secrets("sk-1"), "sk-1");
    }

    #[test]
    fn key_value_secrets_are_stripped_but_the_key_stays() {
        assert_eq!(
            redact_secrets("DISCORD_TOKEN=abc123xyz"),
            "DISCORD_TOKEN=***"
        );
        assert_eq!(redact_secrets("password: hunter2"), "password: ***");
        assert_eq!(
            redact_secrets("api_key: sk-live-1234 fin"),
            "api_key: *** fin"
        );
        assert_eq!(redact_secrets("x=1 nota: todo bien"), "x=1 nota: todo bien");
        assert_eq!(redact_secrets("ratio 3:1 y listo"), "ratio 3:1 y listo");
    }

    #[test]
    fn the_detector_says_which_pattern_it_matched() {
        assert_eq!(
            looks_like_secret("el deploy usa ghp_abcdefghijklmnop"),
            Some("github token")
        );
        assert_eq!(
            looks_like_secret("Authorization: Bearer eyJhbG.eyJzdWI.SflKxw"),
            Some("jwt bearer token")
        );
        assert_eq!(
            looks_like_secret("DISCORD_TOKEN=abc123xyz"),
            Some("token field")
        );
        assert_eq!(
            looks_like_secret("password: hunter2"),
            Some("password field")
        );
        assert_eq!(
            looks_like_secret("postgresql://cuba:hunter2-fake@127.0.0.1:5488/brain"),
            Some("credentials in a url")
        );
        assert_eq!(
            looks_like_secret("AKIAIOSFODNN7EXAMPLE está en el ejemplo de AWS"),
            Some("aws access key id"),
            "an AWS key id is refused even when the surrounding prose calls it an example: the \
             detector cannot tell a live key from a retired one, and the caller has allow_secret \
             to say so"
        );
    }

    #[test]
    fn prose_that_merely_talks_about_credentials_is_not_a_credential() {
        for legitimate in [
            "el bug era que la password no se validaba antes de guardarla",
            "revisamos el token de sesión y el secret del webhook: ninguno rotaba",
            "password: sin definir todavía",
            "el token: temporal, caduca en 15 minutos",
            "la doc dice que api_key es obligatorio",
            "ratio 3:1 y listo",
            "x=1 nota: todo bien",
            "secret_scanning: enabled en el repo",
            "arreglado en la línea 42: faltaba el pwd del contenedor",
        ] {
            assert_eq!(
                looks_like_secret(legitimate),
                None,
                "una observación legítima fue rechazada, y rechazarla es perder el dato que el \
                 usuario creía haber guardado: {legitimate:?}"
            );
        }
    }

    #[test]
    fn a_token_inside_compact_json_is_seen_without_pretty_printing_it_first() {
        let context = serde_json::json!({"file": "deploy.rs", "header": "ghp_abcdefghijklmnop"});

        let pretty = serde_json::to_string_pretty(&context).expect("a Value always serialises");
        assert_eq!(
            looks_like_secret(&pretty),
            Some("github token"),
            "the scanner splits on whitespace, so a quoted value is only a token of its own once \
             the JSON has line breaks in it"
        );

        assert_eq!(
            looks_like_secret(&context.to_string()),
            Some("github token"),
            "compact JSON used to hide a token: the scan split on whitespace and a compact \
             object is one unbroken run, so this asserted None and the alarma handler \
             pretty-printed `context` specifically to work around it. Looking for the prefix \
             INSIDE each run — the fix for `Authorization:ghp_...`, which starts with letters \
             and so was never trimmed down to the token — closes this one for free. The \
             pretty-printing stays because it is also what makes the offset in the refusal \
             message point somewhere a person can find"
        );
    }

    #[test]
    fn the_two_views_of_the_detector_cannot_drift_apart() {
        for text in [
            "el deploy usa ghp_abcdefghijklmnop",
            "DISCORD_TOKEN=abc123xyz",
            "password: hunter2",
            "postgresql://cuba:hunter2-fake@127.0.0.1:5488/brain",
            "Authorization: Bearer eyJhbG.eyJzdWI.SflKxw",
        ] {
            assert!(looks_like_secret(text).is_some());
            assert_ne!(
                redact_secrets(text),
                text,
                "anything the write gate refuses must also be redacted on the way to the judge: \
                 if these two ever disagree, one of them stopped being the same detector — the \
                 exact failure this module exists to prevent. Text: {text:?}"
            );
        }
    }

    #[test]
    fn the_refusal_names_the_pattern_and_never_repeats_the_secret() {
        let args = serde_json::json!({});
        let err = refuse_secrets(&args, "content", "el deploy usa ghp_abcdefghijklmnop")
            .expect_err("a github token in free text must not be storable");
        let message = format!("{err:#}");

        assert!(
            !message.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals are logged: the gate would become the \
             leak it exists to stop. Message: {message}"
        );
        assert!(
            message.contains("github token") && message.contains("content"),
            "a refusal that does not say which pattern matched and in which field leaves the \
             caller guessing which part of a 10.000 character text to change. Message: {message}"
        );
        assert!(
            message.contains("14"),
            "the refusal must point at roughly where the match starts (character 14 here) so a \
             long observation can be fixed without rereading it whole. Message: {message}"
        );
    }

    #[test]
    fn allow_secret_is_the_only_way_past_the_gate() {
        let secret = "el deploy usa ghp_abcdefghijklmnop";
        assert!(
            refuse_secrets(
                &serde_json::json!({"allow_secret": true}),
                "content",
                secret
            )
            .is_ok(),
            "without an escape hatch the gate would make a legitimate write impossible, and the \
             user would work around it by storing the secret somewhere with no gate at all"
        );
        assert!(
            refuse_secrets(
                &serde_json::json!({"allow_secret": false}),
                "content",
                secret
            )
            .is_err()
        );
        assert!(
            refuse_secrets(
                &serde_json::json!({"allow_secret": "true"}),
                "content",
                secret
            )
            .is_err(),
            "a string is not the boolean the schema declares: accepting it would let a typo \
             disable the gate silently"
        );
    }
}
