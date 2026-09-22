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

/// What one detector decided about one token.
///
/// The detectors are pure: each reads a token and answers with this. Who owns
/// the output string, the character offsets and the first-hit rule is the
/// tokenizer, and only the tokenizer.
struct Redaction {
    /// What goes out in place of the token. The token's trailing whitespace is
    /// not in here: it belongs to the tokenizer, which puts it back.
    replacement: String,
    /// The pattern name when the write gate must refuse this token too; `None`
    /// when the redactor scrubs and the gate stays quiet. The two views are
    /// asymmetric on purpose, and the direction is pinned by
    /// `the_redactor_scrubs_more_than_the_write_gate_refuses`.
    pattern: Option<&'static str>,
    /// The value this token names but does not carry: it is the NEXT token.
    /// Only `secret_field` fills it and only `pending_value` reads it.
    announces: Option<&'static str>,
}

/// D1 — the value `secret_field` announced one token earlier.
///
/// This is the one detector that cannot decide from its own token: `password:`
/// ends a token and its value is the next one, so the pattern name has to cross
/// the gap. The tokenizer carries it (`announced` in `scan`) instead of a
/// detector reading ahead, which is what keeps the other four pure — and the
/// coupling is load-bearing: lose it and `password: hunter2` stops being
/// redacted while every other case still works.
///
/// The replacement is unconditional and the refusal is not. A separator
/// announces a value, so whatever word arrives is scrubbed on the way to the
/// LLM, while the gate refuses only a word opaque enough to be a credential.
fn pending_value(trimmed: &str, announced: &'static str) -> Redaction {
    Redaction {
        replacement: String::from("***"),
        pattern: value_is_opaque(trimmed).then_some(announced),
        announces: None,
    }
}

/// D2 — `scheme://user:password@host`.
///
/// The user survives and only what follows the colon goes: it is the one part
/// of the url that says which account. Userinfo with no colon carries no
/// password, so this answers `None` and the token falls through to D3 whole —
/// `user@host` is the shape a git remote has.
fn credentials_in_url(trimmed: &str) -> Option<Redaction> {
    let at_sign = trimmed.find('@')?;
    // The `://` has to come FIRST, or what follows is not userinfo and the
    // slice below would run backwards.
    let scheme_end = trimmed.find("://").filter(|end| at_sign > *end)?;
    let creds = &trimmed[scheme_end + 3..at_sign];
    let colon = creds.find(':')?;
    Some(Redaction {
        replacement: format!(
            "{}***{}",
            &trimmed[..scheme_end + 3 + colon + 1],
            &trimmed[at_sign..]
        ),
        pattern: Some("credentials in a url"),
        announces: None,
    })
}

/// D3 — `secret_field=value` or `secret_field: value`.
///
/// The key stays and the value goes, or the reader cannot tell what was
/// removed. When nothing follows the separator the value is in the next token,
/// and that is what `announces` is for.
fn secret_field(trimmed: &str) -> Option<Redaction> {
    let sep = trimmed.find(['=', ':']).filter(|sep| *sep > 0)?;
    let pattern = secret_field_pattern(&trimmed[..sep])?;
    let key = &trimmed[..=sep];
    if sep + 1 < trimmed.len() {
        Some(Redaction {
            replacement: format!("{key}***"),
            pattern: value_is_opaque(&trimmed[sep + 1..]).then_some(pattern),
            announces: None,
        })
    } else {
        Some(Redaction {
            replacement: String::from(key),
            pattern: None,
            announces: Some(pattern),
        })
    }
}

/// D4 — a provider prefix anywhere inside the run, and where it starts.
///
/// Looking INSIDE the run rather than only at its start is what sees
/// `Authorization:ghp_…` and a token inside compact JSON. The length guard is
/// what keeps `sk-1` from being a key. The earliest match wins, so the run is
/// cut at the first credential it carries.
fn provider_prefix(bare: &str) -> Option<(usize, &'static str)> {
    PROVIDER_PREFIXES
        .iter()
        .filter_map(|(prefix, pattern)| bare.find(prefix).map(|at| (at, *prefix, *pattern)))
        .filter(|(at, prefix, _)| bare.len() - at > prefix.len() + 8)
        .min_by_key(|(at, _, _)| *at)
        .map(|(at, _, pattern)| (at, pattern))
}

/// D5 — a JWS compact serialization: `eyJ…` and exactly two dots.
///
/// Exactly two: one dot is a truncated paste and three is not a JWS. Both are
/// base64ish words, and a gate that refuses every base64ish word is a gate the
/// user turns off.
fn jwt_bearer(bare: &str) -> Option<&'static str> {
    (bare.starts_with("eyJ") && bare.matches('.').count() == 2).then_some("jwt bearer token")
}

/// What D4 and D5 share: both cut the run at the credential and keep what came
/// before it. A provider prefix wins over a JWT shape when somehow both match.
fn embedded_credential(trimmed: &str) -> Option<Redaction> {
    let bare = trimmed.trim_start_matches(|c: char| !c.is_alphanumeric());
    let provider = provider_prefix(bare);
    let pattern = provider
        .map(|(_, pattern)| pattern)
        .or_else(|| jwt_bearer(bare))?;
    // What precedes the credential inside the run is kept (`usa-` in
    // `usa-ghp_…`). A prefix that starts the run keeps nothing, and `&bare[..0]`
    // is already `""` — the same nothing the JWT case wants — so there is no
    // second case to write here.
    let keep = provider.map_or("", |(at, _)| &bare[..at]);
    // The non-alphanumeric head that `bare` trimmed off is not part of the
    // credential: a quote or a bracket in front of it goes back out.
    let untrimmed = trimmed.len() - bare.len();
    Some(Redaction {
        replacement: format!("{}{keep}***", &trimmed[..untrimmed]),
        pattern: Some(pattern),
        announces: None,
    })
}

/// The order of the detectors, which is behaviour and not detail: an announced
/// value is a value whatever it looks like, a url is read as a url before its
/// scheme can be taken for a field name, and a field name beats the provider
/// prefix its value may carry — that is what makes `DISCORD_TOKEN=ghp_…` a
/// "token field" and not a "github token" in the refusal.
fn redaction_for(trimmed: &str, announced: Option<&'static str>) -> Option<Redaction> {
    announced
        .map(|pattern| pending_value(trimmed, pattern))
        .or_else(|| credentials_in_url(trimmed))
        .or_else(|| secret_field(trimmed))
        .or_else(|| embedded_credential(trimmed))
}

fn scan(s: &str) -> Scan {
    let mut out = String::with_capacity(s.len());
    let mut hit: Option<Hit> = None;
    // The only state the detectors share. D3 can end a token with the separator
    // and no value; the value it names arrives in the NEXT token, where D1
    // collects it. It lives here, in the tokenizer, because a whitespace-only
    // token must not consume it: `password:  hunter2` has two spaces and the
    // announcement has to survive the one in the middle.
    let mut announced: Option<&'static str> = None;
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

        let Some(found) = redaction_for(trimmed, announced.take()) else {
            out.push_str(token);
            continue;
        };

        out.push_str(&found.replacement);
        out.push_str(trailing);
        announced = found.announces;
        // The first hit wins: the offset in the refusal points at the first
        // thing that looked like a credential, not at the last one.
        if hit.is_none()
            && let Some(pattern) = found.pattern
        {
            hit = Some(Hit {
                pattern,
                char_offset: at,
            });
        }
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
    fn a_quoted_key_still_names_the_field_whose_value_must_go() {
        let context = serde_json::json!({"api_key": "canary-9f3a1b2c5d"});
        let pretty = serde_json::to_string_pretty(&context).expect("a Value always serialises");
        assert!(
            pretty.contains("canary-9f3a1b2c5d"),
            "the canary has to be in the text first, or `it is gone afterwards` proves \
             nothing: {pretty}"
        );

        assert_eq!(
            looks_like_secret(&pretty),
            Some("api key field"),
            "in a JSON log context the field name arrives quoted, so what the scan hands over \
             is `\"api_key\"`, not `api_key`. `ends_with` ignores whatever sits in FRONT of \
             the name, so the TRAILING quote is the whole problem: trim it and this is an api \
             key field, keep it and the key is a word ending in a quote that matches nothing \
             and the value walks. The value here carries no provider prefix on purpose — this \
             branch is all that stands between it and the prompt"
        );

        let clean = redact_secrets(&pretty);
        assert!(
            !clean.contains("canary-9f3a1b2c5d"),
            "the value of a quoted secret key went out whole: {clean}"
        );
        assert!(
            clean.contains("\"api_key\": ***"),
            "and the key has to stay, or the reader cannot tell what was removed: {clean}"
        );
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

    #[test]
    fn every_provider_prefix_the_table_lists_is_one_the_scan_actually_matches() {
        for (canary, pattern) in [
            ("gho_canary0123456789", "github token"),
            ("github_pat_canary0123456789", "github token"),
            ("xoxb-canary0123456789", "slack token"),
            ("xoxp-canary0123456789", "slack token"),
        ] {
            let text = format!("el deploy usa {canary} fin");
            assert!(
                text.contains(canary),
                "control: with the canary missing from the text, `the token is gone afterwards` \
                 would also pass on a scan that does nothing at all: {text}"
            );
            assert_eq!(
                looks_like_secret(&text),
                Some(pattern),
                "four of the seven PROVIDER_PREFIXES had no test reaching them: gho_, \
                 github_pat_, xoxb- and xoxp-. While only sk-, ghp_ and AKIA were exercised, a \
                 typo in one of those rows, a dropped row, or a pattern name pasted from the \
                 row above was invisible, and a prefix that matches nothing is the write gate \
                 storing the token: {text}"
            );
            assert_eq!(
                redact_secrets(&text),
                "el deploy usa *** fin",
                "and the same row has to scrub on the way to the LLM, not only refuse on the \
                 way in: both views read this one table and both have to reach it"
            );
        }
    }

    #[test]
    fn a_value_with_no_digits_has_to_be_long_before_it_counts_as_a_secret() {
        let long_enough = "canaryallletters";
        let one_char_short = "canaryallletter";
        assert_eq!(
            (long_enough.chars().count(), one_char_short.chars().count()),
            (16, 15),
            "this test is only about which side of MIN_ALL_LETTER_VALUE_CHARS each value falls \
             on, so a miscounted literal would quietly assert the opposite of what it reads"
        );

        let opaque = format!("api_key={long_enough}");
        assert_eq!(
            looks_like_secret(&opaque),
            Some("api key field"),
            "every other value in this module carries a digit, so the second half of \
             value_is_opaque decided nothing and deleting it left the suite green. An \
             all-letter value at the threshold -- a passphrase, a word-list key -- is the one \
             shape that reaches the write gate through that clause alone: {opaque}"
        );
        assert_eq!(redact_secrets(&opaque), "api_key=***");

        let too_short = format!("api_key={one_char_short}");
        assert!(
            too_short.contains(one_char_short),
            "control: the value has to be in the text before None means the gate looked and \
             declined, rather than the interpolation having eaten it"
        );
        assert_eq!(
            looks_like_secret(&too_short),
            None,
            "and one character below the threshold it has to be let through, or the constant is \
             decorative in the other direction too: a word that short is prose far more often \
             than it is a credential, and refusing prose loses the observation the user \
             believed they had stored: {too_short}"
        );
        assert_eq!(
            redact_secrets(&too_short),
            "api_key=***",
            "the redactor never asks value_is_opaque whether to print ***, only whether to \
             refuse: a named secret field loses its value either way. Same asymmetry as \
             the_redactor_scrubs_more_than_the_write_gate_refuses, one detector down"
        );
    }

    #[test]
    fn a_url_with_a_user_and_no_password_keeps_the_user() {
        let with_password = "https://canary-user:canary-pass-9f@example.invalid/ruta";
        assert_eq!(
            looks_like_secret(with_password),
            Some("credentials in a url"),
            "control: the sibling that does carry a password has to be caught here, or what \
             follows proves only that the url detector never runs on either of them"
        );
        assert_eq!(
            redact_secrets(with_password),
            "https://canary-user:***@example.invalid/ruta",
            "the user survives and only what follows the colon goes: a redaction that ate the \
             user too would cost the reader the one part of the url that says which account"
        );

        let user_only = "https://canary-user@example.invalid/ruta";
        assert_eq!(
            redact_secrets(user_only),
            user_only,
            "userinfo with no colon carries no password, so there is nothing to hide and the \
             url is left whole. This is the branch of the url detector that falls through to \
             the detectors below instead of rewriting, and nothing pinned it: a search for the \
             colon widened from the credentials to the whole token would splice this url at \
             the scheme and hand the reader something that is no longer a url: {user_only}"
        );
        assert_eq!(
            looks_like_secret(user_only),
            None,
            "and it must not be refused either: user@host with no password is the shape a git \
             remote and a docs link have, and a gate that rejects those teaches the user to \
             pass allow_secret by reflex on the writes that really do carry one"
        );
    }

    #[test]
    fn a_jwt_is_exactly_two_dots_and_a_near_miss_is_left_alone() {
        assert_eq!(
            redact_secrets("bearer eyJhbG.eyJzdWI.SflKxw"),
            "bearer ***",
            "control: three segments still go, so what the loop below shows is the dot count \
             deciding, not the eyJ prefix having quietly stopped matching"
        );
        for near_miss in ["eyJhbG.eyJzdWI", "eyJhbG.eyJzdWI.SflKxw.extra"] {
            let text = format!("bearer {near_miss}");
            assert_eq!(
                looks_like_secret(&text),
                None,
                "the dot count was only ever tested at 2, so the comparison could widen to >= \
                 or drift by one and nothing would notice. One dot is a truncated paste and \
                 three is not a JWS: both are base64ish words, and a gate that refuses every \
                 base64ish word is a gate the user turns off: {text}"
            );
            assert_eq!(
                redact_secrets(&text),
                text,
                "and neither view may touch it, or the two have drifted apart: {text}"
            );
        }
    }

    #[test]
    fn the_redactor_scrubs_more_than_the_write_gate_refuses() {
        for (prose, scrubbed) in [
            ("password: canary", "password: ***"),
            (
                "password: sin definir todavía",
                "password: *** definir todavía",
            ),
            (
                "el token: temporal, caduca en 15 minutos",
                "el token: *** caduca en 15 minutos",
            ),
        ] {
            assert_eq!(
                looks_like_secret(prose),
                None,
                "the write gate stays conservative: a separator announces a value, but it only \
                 refuses when the word that arrives is opaque enough to be a credential, \
                 because refusing prose loses a memory the user believed they had stored: \
                 {prose:?}"
            );
            assert_eq!(
                redact_secrets(prose),
                scrubbed,
                "the two views are asymmetric on purpose, and this is the direction. The \
                 separator announces a value, so the redactor replaces the next word whatever \
                 it turns out to be, while the gate refuses only the opaque ones. Each side \
                 fails safe where its own mistake is paid: a scrubbed word costs one word of \
                 prose inside a prompt, a refused write costs the observation. The first row \
                 is why the redactor cannot be made conditional to match the gate: six letters \
                 and no digit is not opaque, so the gate lets that password in, and the \
                 redactor is all that stands between it and the LLM. \
                 the_two_views_of_the_detector_cannot_drift_apart holds the implication that \
                 matters, refused implies redacted; this holds that the converse is NOT held, \
                 so nobody tidies the gap into a leak. Prose: {prose:?}"
            );
        }

        let no_announcement = "la doc dice que api_key es obligatorio";
        assert_eq!(looks_like_secret(no_announcement), None);
        assert_eq!(
            redact_secrets(no_announcement),
            no_announcement,
            "the contrast that makes the rule readable: a secret field name with no separator \
             after it announces nothing, so the next word is prose and stays. The separator is \
             what arms the replacement, not the field name: {no_announcement}"
        );
    }
}
