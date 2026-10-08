//! The account protocol: pairing codes, credentials, the MACs both sides check
//! and how a bearer is classified. Pure on purpose: no network, no database
//! and no clock, so every decision here is a function a test can call and a
//! mutant can break. The database lives in `accounts_db`, the wire in `http`
//! and the client in `bridge`.

use anyhow::{Result, bail};
use chrono::Duration;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const CODE_ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const CODE_SYMBOLS: usize = 12;
pub const PAIRING_CODE_TTL: Duration = Duration::hours(24);
pub const MAX_ACTIVE_PAIRING_CODES: usize = 50;
pub const PAIRING_RETAIN: Duration = Duration::days(7);
pub const CREDENTIAL_PREFIX: &str = "mi1";
pub const REQUIRE_ACCOUNTS_REFUSAL: &str = "this daemon only accepts account credentials: the shared token is not accepted here. \
     Pair this device with a code from the host operator: memory-industry bridge pair <code>";

const SYMBOL_BITS: usize = 5;
const SYMBOL_MASK: u64 = 31;
const CODE_GROUP: usize = 4;
const ACCOUNT_NAME_MAX: usize = 32;
const DEVICE_LABEL_MAX: usize = 40;

const DOMAIN_CODE_KEY: &str = "memory-industry/pair-code/v1";
const DOMAIN_PAIR_REQ: &str = "memory-industry/pair-req/v1";
const DOMAIN_PAIR_RESP: &str = "memory-industry/pair-resp/v1";
const DOMAIN_IDENTITY: &str = "memory-industry/identity/v1";
const DOMAIN_FINGERPRINT: &str = "memory-industry/host-fingerprint/v1";

#[derive(Debug, PartialEq, Eq)]
pub enum CodeError {
    Length(usize),
    Symbol(char),
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeError::Length(n) => write!(
                f,
                "a pairing code has {CODE_SYMBOLS} symbols, this one has {n}"
            ),
            CodeError::Symbol(c) => write!(
                f,
                "'{c}' is not a symbol of a pairing code (0/O/1/I are not used, they look alike; \
                 the symbols are the letters A-Z without I and O, and the digits 2-9)"
            ),
        }
    }
}

impl std::error::Error for CodeError {}

/// 32 random bytes the client keeps; only `hash()` ever travels.
pub struct Secret([u8; 32]);

// Debug and Display are written by hand: a derived Debug would put the bytes in
// every `{:?}` of a struct that holds one, and in every panic message.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Secret {
    pub fn generate() -> Self {
        Secret(random_bytes::<32>())
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0).into()
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        lower_hex::<32>(hex).map(Secret)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BearerClass {
    Peer,
    Shared,
    SharedRefusedByPolicy,
    Account { id: Uuid, secret: [u8; 32] },
    Unrecognized,
    NoAuthConfigured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub v: u8,
    pub nc: String,
    pub label: String,
    pub h: String,
    pub mac: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResponse {
    pub v: u8,
    pub host_id: Uuid,
    pub host_name: String,
    pub credential_id: Uuid,
    pub account: String,
    pub label: String,
    pub mac: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityReply {
    pub service: String,
    pub host_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

/// The OS CSPRNG through `Uuid::new_v4` (`getrandom` underneath), without making
/// `getrandom` a direct dependency: `N.div_ceil(16) + 1` v4 UUIDs, 122 random bits
/// each, are hashed in counter mode into `N` bytes.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut seed = Vec::new();
    for _ in 0..=N.div_ceil(16) {
        seed.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    let mut out = [0u8; N];
    for (block, chunk) in out.chunks_mut(32).enumerate() {
        let digest = Sha256::new()
            .chain_update((block as u32).to_be_bytes())
            .chain_update(&seed)
            .finalize();
        chunk.copy_from_slice(&digest[..chunk.len()]);
    }
    out
}

/// `XXXX-XXXX-XXXX`: 12 symbols of 5 bits, 60 bits of the OS CSPRNG.
pub fn generate_code() -> String {
    let bits = u64::from_be_bytes(random_bytes::<8>());
    let symbols: Vec<char> = (0..CODE_SYMBOLS)
        .map(|i| {
            let index = (bits >> (SYMBOL_BITS * i)) & SYMBOL_MASK;
            char::from(CODE_ALPHABET.as_bytes()[index as usize])
        })
        .collect();
    symbols
        .chunks(CODE_GROUP)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// What a person types (any case, spaces and dashes anywhere) reduced to the 12
/// symbols that are hashed. A symbol outside the alphabet is named, before the
/// length is judged, so `0` for `O` is reported as that and not as a length.
pub fn normalize_code(raw: &str) -> Result<String, CodeError> {
    let symbols: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if let Some(bad) = symbols.chars().find(|c| !CODE_ALPHABET.contains(*c)) {
        return Err(CodeError::Symbol(bad));
    }
    match symbols.chars().count() {
        CODE_SYMBOLS => Ok(symbols),
        n => Err(CodeError::Length(n)),
    }
}

pub fn code_key(normalized: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(DOMAIN_CODE_KEY)
        .chain_update([0u8])
        .chain_update(normalized)
        .finalize()
        .into()
}

fn is_alnum(c: char, lowercase_only: bool) -> bool {
    c.is_ascii_digit()
        || if lowercase_only {
            c.is_ascii_lowercase()
        } else {
            c.is_ascii_alphabetic()
        }
}

/// Mirrors the column CHECKs: a first character that is a letter or digit, then
/// letters, digits and the `extra` ones, up to `max` characters in all.
fn has_shape(text: &str, max: usize, extra: &str, lowercase_only: bool) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| is_alnum(c, lowercase_only))
        && text.chars().count() <= max
        && chars.all(|c| is_alnum(c, lowercase_only) || extra.contains(c))
}

pub fn validate_account_name(raw: &str) -> Result<String> {
    let name = raw.to_ascii_lowercase();
    if !has_shape(&name, ACCOUNT_NAME_MAX, "._-", true) {
        bail!(
            "account name {raw:?} must be 1-{ACCOUNT_NAME_MAX} characters: lowercase letters, \
             digits, '.', '_' or '-', starting with a letter or a digit"
        );
    }
    Ok(name)
}

pub fn validate_device_label(raw: &str) -> Result<String> {
    if !has_shape(raw, DEVICE_LABEL_MAX, "._ -", false) {
        bail!(
            "device label {raw:?} must be 1-{DEVICE_LABEL_MAX} characters: letters, digits, \
             '.', '_', '-' or a space, starting with a letter or a digit"
        );
    }
    Ok(raw.to_string())
}

/// Exactly `2 * N` lowercase hex characters. `hex::decode` alone would accept
/// uppercase, a second spelling of the same bytes.
fn lower_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let canonical =
        text.len() == 2 * N && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !canonical {
        return None;
    }
    hex::decode(text).ok()?.try_into().ok()
}

pub fn bearer(credential_id: Uuid, secret: &Secret) -> String {
    format!(
        "{CREDENTIAL_PREFIX}.{}.{}",
        credential_id.simple(),
        secret.to_hex()
    )
}

pub fn parse_bearer(raw: &str) -> Option<(Uuid, [u8; 32])> {
    let mut parts = raw.split('.');
    let (prefix, id, secret) = (parts.next()?, parts.next()?, parts.next()?);
    if prefix != CREDENTIAL_PREFIX || parts.next().is_some() {
        return None;
    }
    Some((
        Uuid::from_bytes(lower_hex::<16>(id)?),
        lower_hex::<32>(secret)?,
    ))
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The transcript every MAC covers: the domain, then each field, every one
/// preceded by its length as a big-endian u32, so a boundary cannot be moved.
pub fn enc(domain: &str, fields: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    put(&mut out, domain.as_bytes());
    for field in fields {
        put(&mut out, field);
    }
    out
}

fn hmac_over(key: &[u8], domain: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(&enc(domain, fields));
    mac.finalize().into_bytes().into()
}

pub fn pair_request_mac(code_key: &[u8; 32], nc: &[u8; 16], label: &str, h: &[u8; 32]) -> [u8; 32] {
    hmac_over(code_key, DOMAIN_PAIR_REQ, &[nc, label.as_bytes(), h])
}

#[allow(clippy::too_many_arguments)]
pub fn pair_response_mac(
    code_key: &[u8; 32],
    nc: &[u8; 16],
    h: &[u8; 32],
    host_id: &Uuid,
    credential_id: &Uuid,
    account: &str,
    label: &str,
    host_name: &str,
) -> [u8; 32] {
    let fields: [&[u8]; 7] = [
        nc,
        h,
        host_id.as_bytes(),
        credential_id.as_bytes(),
        account.as_bytes(),
        label.as_bytes(),
        host_name.as_bytes(),
    ];
    hmac_over(code_key, DOMAIN_PAIR_RESP, &fields)
}

pub fn identity_proof(
    secret_hash: &[u8; 32],
    host_id: &Uuid,
    credential_id: &Uuid,
    nonce: &[u8; 16],
) -> [u8; 32] {
    hmac_over(
        secret_hash,
        DOMAIN_IDENTITY,
        &[host_id.as_bytes(), credential_id.as_bytes(), nonce],
    )
}

pub fn verify_mac(expected: [u8; 32], presented_hex: &str) -> bool {
    lower_hex::<32>(presented_hex).is_some_and(|presented| ct_eq(&expected, &presented))
}

/// Same fold as `http::same_secret`: the length is not a secret, the bytes are.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn fingerprint(host_id: &Uuid) -> String {
    let digest = Sha256::new()
        .chain_update(DOMAIN_FINGERPRINT)
        .chain_update(host_id.as_bytes())
        .finalize();
    let shown = hex::encode_upper(&digest[..8]);
    format!(
        "{}-{}-{}-{}",
        &shown[0..4],
        &shown[4..8],
        &shown[8..12],
        &shown[12..16]
    )
}

fn is_token(bearer: &str, token: Option<&str>) -> bool {
    token.is_some_and(|t| ct_eq(bearer.as_bytes(), t.as_bytes()))
}

/// Peer, then the shared token, then a credential, then nothing. The shared token
/// is compared before the credential shape so a token that happens to start with
/// `mi1.` stays the shared token.
pub fn classify_bearer(
    bearer: &str,
    shared: Option<&str>,
    peer: Option<&str>,
    require_accounts_on_lan: bool,
) -> BearerClass {
    if shared.is_none() && peer.is_none() {
        return BearerClass::NoAuthConfigured;
    }
    if is_token(bearer, peer) {
        return BearerClass::Peer;
    }
    if is_token(bearer, shared) {
        return if require_accounts_on_lan {
            BearerClass::SharedRefusedByPolicy
        } else {
            BearerClass::Shared
        };
    }
    match parse_bearer(bearer) {
        Some((id, secret)) => BearerClass::Account { id, secret },
        None => BearerClass::Unrecognized,
    }
}

/// The knob is read once, here. Only an explicit yes turns it on, and on a loopback
/// bind it does nothing (the shared token is the only way in there), which is said.
pub fn require_accounts_effective(
    addr_is_loopback: bool,
    raw: Option<&str>,
) -> (bool, Option<String>) {
    let asked = raw.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    });
    if asked && addr_is_loopback {
        let warning = "the account-only policy is ignored on loopback: on a loopback bind \
                       the shared token is the only way in";
        return (false, Some(warning.to_string()));
    }
    (asked, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Known-answer vectors below were computed OUTSIDE this crate, when the tests were
    // written: `sha256sum` / `openssl dgst -sha256 -mac HMAC -macopt hexkey:…` / python3
    // `hashlib` + `hmac` + `struct.pack(">I", …)`. None was copied from the implementation.

    fn counting<const N: usize>(from: u8) -> [u8; N] {
        std::array::from_fn(|i| from + i as u8)
    }

    fn mac32(hex_str: &str) -> [u8; 32] {
        hex::decode(hex_str)
            .expect("a vector in this file is valid hex")
            .try_into()
            .expect("a vector in this file is 32 bytes")
    }

    fn host() -> Uuid {
        Uuid::from_u128(0x00112233_4455_6677_8899_aabbccddeeff)
    }

    fn cred() -> Uuid {
        Uuid::from_u128(0xffeeddcc_bbaa_9988_7766_554433221100)
    }

    fn is_dashed_code(s: &str) -> bool {
        let groups: Vec<&str> = s.split('-').collect();
        groups.len() == 3
            && groups
                .iter()
                .all(|g| g.len() == 4 && g.chars().all(|c| CODE_ALPHABET.contains(c)))
    }

    fn is_fingerprint(s: &str) -> bool {
        let groups: Vec<&str> = s.split('-').collect();
        groups.len() == 4
            && groups.iter().all(|g| {
                g.len() == 4
                    && g.chars()
                        .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c))
            })
    }

    // ── P1 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_code_is_typable_unambiguous_and_60_bits() {
        let alphabet: HashSet<char> = CODE_ALPHABET.chars().collect();
        assert_eq!(
            (alphabet.len(), CODE_ALPHABET.chars().count()),
            (32, 32),
            "the alphabet is 32 distinct symbols: 5 bits each, so 12 symbols are exactly 60 bits"
        );
        assert_eq!(
            CODE_SYMBOLS * 5,
            60,
            "CODE_SYMBOLS * 5 == 60: the entropy the threat model budgets for the code"
        );
        for banned in ['0', 'O', '1', 'I'] {
            assert!(
                !CODE_ALPHABET.contains(banned),
                "{banned} is read as another symbol on a printed sheet; a person copying the \
                 code cannot tell them apart"
            );
        }

        let codes: Vec<String> = (0..1000).map(|_| generate_code()).collect();
        for code in &codes {
            assert!(
                is_dashed_code(code),
                "`{code}` must be XXXX-XXXX-XXXX over the alphabet, so it can be read aloud and typed"
            );
            assert!(
                !code.contains(['0', 'O', '1', 'I']),
                "`{code}` contains 0, O, 1 or I: ambiguous when typed from paper"
            );
        }
        let distinct: HashSet<&String> = codes.iter().collect();
        assert_eq!(
            distinct.len(),
            1000,
            "a thousand codes with a repeat means the generator is not drawing from 60 bits"
        );
    }

    #[test]
    fn the_code_normalizer_keeps_twelve_symbols_of_what_the_generator_prints() {
        let code = generate_code();
        let normalized = normalize_code(&code).expect("a generated code must normalize");
        assert_eq!(
            normalized,
            code.replace('-', ""),
            "normalizing drops only the dashes: the 12 symbols are what is hashed and compared"
        );
        assert_eq!(normalized.len(), CODE_SYMBOLS);
    }

    #[test]
    fn the_code_normalizer_accepts_what_a_person_types() {
        for typed in [
            "k7qm 3wxp-9hnr",
            "K7QM-3WXP-9HNR",
            "k7qm3wxp9hnr",
            "  K7QM 3WXP 9HNR  ",
            "K-7QM3WXP9HN-R",
        ] {
            assert_eq!(
                normalize_code(typed).as_deref(),
                Ok("K7QM3WXP9HNR"),
                "`{typed}`: lowercase, spaces and dashes anywhere are typing, not a different code"
            );
        }
    }

    #[test]
    fn the_code_normalizer_rejects_wrong_lengths_and_names_the_ambiguous_symbol() {
        assert!(
            normalize_code("K7QM-3WXP-9HN").is_err(),
            "11 symbols is a truncated code, not a code"
        );
        assert!(
            normalize_code("K7QM-3WXP-9HNRR").is_err(),
            "13 symbols is a pasted extra character, not a code"
        );
        assert!(normalize_code("").is_err(), "nothing typed is not a code");
        assert!(
            normalize_code("K7QM-3WXP-9HN!").is_err(),
            "a symbol outside the alphabet cannot belong to a generated code"
        );

        for banned in ['0', 'O', '1', 'I'] {
            let typed = format!("K7QM-3WXP-9HN{banned}");
            let message = normalize_code(&typed)
                .expect_err("an ambiguous symbol is refused instead of guessed")
                .to_string();
            assert!(
                message.contains(&format!("'{banned}'")),
                "the error for `{typed}` must name the character '{banned}' so the person sees \
                 which one to re-read; got: {message}"
            );
            assert!(
                message.contains("0/O/1/I") && message.contains("not used"),
                "the error must say 0/O/1/I are not used, so the person knows to look for \
                 the lookalike; got: {message}"
            );
        }
        let lowercase = normalize_code("k7qm-3wxp-9hno").expect_err("o is O");
        assert!(
            lowercase.to_string().contains("'O'"),
            "a typed lowercase `o` is judged as the uppercase symbol it would become"
        );
    }

    #[test]
    fn the_code_key_is_sha256_of_domain_nul_and_the_normalized_code() {
        // printf 'memory-industry/pair-code/v1\0K7QM3WXP9HNR' | sha256sum
        let expected = mac32("527dbdde924599069a8123a702f55a1189f4a353eafa0a4563af2f4ee8ae3553");
        assert_eq!(
            code_key("K7QM3WXP9HNR"),
            expected,
            "code_key is SHA-256(\"memory-industry/pair-code/v1\" || 0x00 || code): the server \
             stores only this, and the client derives the same key to prove it knows the code"
        );
        assert_ne!(
            code_key("K7QM3WXP9HNR"),
            code_key("K7QM3WXP9HNS"),
            "one different symbol is a different key"
        );
    }

    // ── P2 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn account_names_are_lowercased_and_bounded() {
        assert_eq!(
            validate_account_name("Ana")
                .expect("Ana is a valid name")
                .as_str(),
            "ana",
            "names are stored lowercase so `Ana` and `ana` are the same account"
        );
        assert!(
            validate_account_name("ana_1.x-y").is_ok(),
            "dot, underscore and dash are part of the closed alphabet"
        );
        assert!(
            validate_account_name(&"a".repeat(32)).is_ok(),
            "32 characters is the limit and is allowed"
        );
        for bad in ["", "ana pc", "-x", "a/b", "año", "ana\n"] {
            assert!(
                validate_account_name(bad).is_err(),
                "{bad:?} must be refused at the boundary: it cannot appear in a path, a log line or a CLI argument unescaped"
            );
        }
        assert!(
            validate_account_name(&"a".repeat(33)).is_err(),
            "33 characters is over the 32 limit the column CHECK also enforces"
        );
        assert!(
            validate_account_name(&"A".repeat(33)).is_err(),
            "lowercasing must not let an over-long name slip past the length check"
        );
    }

    #[test]
    fn device_labels_are_plain_text_without_line_breaks_or_quotes() {
        for good in ["PC lab 1", "pc-02.planta", "x"] {
            assert_eq!(
                validate_device_label(good).as_deref().ok(),
                Some(good),
                "{good:?} is a legitimate label and is kept as typed (labels keep their case)"
            );
        }
        assert!(
            validate_device_label(&"x".repeat(40)).is_ok(),
            "40 characters is the limit and is allowed"
        );
        for bad in ["", " x", "a\nb", "x\n", "a\"b", "a'b", "a\rb"] {
            assert!(
                validate_device_label(bad).is_err(),
                "{bad:?} must be refused: a label is printed in tables and logs and written to a task name"
            );
        }
        assert!(
            validate_device_label(&"x".repeat(41)).is_err(),
            "41 characters is over the 40 limit the column CHECK also enforces"
        );
    }

    // ── P3 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_secret_never_prints() {
        let s = Secret([0xAB; 32]);
        assert_eq!(
            format!("{s:?}"),
            "<redacted>",
            "Debug of a secret ends up in tracing fields and panic messages"
        );
        assert_eq!(
            format!("{s}"),
            "<redacted>",
            "Display of a secret ends up in error messages and CLI output"
        );
    }

    #[test]
    fn the_secret_is_random_and_hashes_with_plain_sha256() {
        let a = Secret::generate();
        let b = Secret::generate();
        assert_ne!(
            a.0, b.0,
            "two generated secrets must differ: they are 256 bits of CSPRNG output"
        );
        assert_ne!(a.0, [0u8; 32], "a generated secret is never 32 zeros");
        assert_ne!(b.0, [0u8; 32], "a generated secret is never 32 zeros");

        // head -c 32 /dev/zero | sha256sum
        assert_eq!(
            Secret([0; 32]).hash(),
            mac32("66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"),
            "hash() is plain SHA-256 of the 32 secret bytes: what the host stores and what the \
             identity proof is keyed with; any other construction makes the two sides disagree"
        );
    }

    #[test]
    fn the_secret_round_trips_through_lowercase_hex() {
        let s = Secret([0xAB; 32]);
        assert_eq!(
            s.to_hex(),
            "ab".repeat(32),
            "the state file holds the secret as 64 lowercase hex characters"
        );
        let back = Secret::from_hex(&s.to_hex()).expect("what to_hex wrote must parse back");
        assert_eq!(back.0, s.0, "to_hex and from_hex are inverses");
        assert!(
            Secret::from_hex("zz").is_none() && Secret::from_hex(&"ab".repeat(31)).is_none(),
            "a state file with a short or non-hex secret is corrupt, not a secret"
        );
    }

    #[test]
    fn the_bearer_has_one_exact_shape_and_parses_back() {
        let id = Uuid::from_u128(0x0a1b2c3d_4e5f_4a6b_8c7d_8e9fa0b1c2d3);
        let secret = Secret([0xAB; 32]);
        let expected = format!("mi1.0a1b2c3d4e5f4a6b8c7d8e9fa0b1c2d3.{}", "ab".repeat(32));
        let b = bearer(id, &secret);
        assert_eq!(
            b, expected,
            "the bearer is mi1.<32 hex id>.<64 hex secret>, lowercase, no dashes in the id"
        );
        assert_eq!(
            parse_bearer(&b),
            Some((id, [0xAB; 32])),
            "parse_bearer is the exact inverse of bearer: it recovers the id and the 32 secret bytes"
        );
    }

    #[test]
    fn parse_bearer_refuses_everything_that_is_not_the_exact_shape() {
        let id = "0a1b2c3d4e5f4a6b8c7d8e9fa0b1c2d3";
        let sec = "ab".repeat(32);
        let ok = format!("mi1.{id}.{sec}");
        assert!(
            parse_bearer(&ok).is_some(),
            "control: the unmodified shape parses, so the refusals below test the change"
        );
        let upper_secret = format!("mi1.{id}.{}", sec.to_uppercase());
        let upper_id = format!("mi1.{}.{sec}", id.to_uppercase());
        let short_secret = format!("mi1.{id}.{}", &sec[..63]);
        let long_secret = format!("mi1.{id}.{sec}0");
        let short_id = format!("mi1.{}.{sec}", &id[..31]);
        let long_id = format!("mi1.{id}0.{sec}");
        let wrong_prefix = format!("mi2.{id}.{sec}");
        let fourth = format!("{ok}.extra");
        let leading = format!(" {ok}");
        let trailing = format!("{ok} ");
        let inner = format!("mi1.{id}. {sec}");
        let dashed_id = "mi1.0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3.".to_string() + &sec;
        for (what, raw) in [
            ("uppercase hex in the secret", upper_secret),
            ("uppercase hex in the id", upper_id),
            ("63 hex characters of secret", short_secret),
            ("65 hex characters of secret", long_secret),
            ("31 hex characters of id", short_id),
            ("33 hex characters of id", long_id),
            ("the prefix mi2.", wrong_prefix),
            ("a fourth segment", fourth),
            ("a leading space", leading),
            ("a trailing space", trailing),
            ("a space inside", inner),
            ("a dashed uuid for the id", dashed_id),
            ("an empty string", String::new()),
            ("the prefix alone", "mi1.".to_string()),
        ] {
            assert_eq!(
                parse_bearer(&raw),
                None,
                "{what}: a second spelling of the same credential would be a second key in any \
                 cache or log keyed by the text; only one spelling is accepted"
            );
        }
    }

    // ── P4 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn enc_prefixes_the_domain_and_every_field_with_a_big_endian_u32_length() {
        // python3: struct.pack(">I", 1) + b"d" + struct.pack(">I", 2) + b"ab" + struct.pack(">I", 1) + b"c"
        let expected = hex::decode("00000001640000000261620000000163").expect("valid hex");
        assert_eq!(
            enc("d", &[b"ab".as_slice(), b"c".as_slice()]),
            expected,
            "domain and each field are length-prefixed with a u32 big-endian: this is the \
             transcript the host and the client must both build byte for byte"
        );
        assert_ne!(
            enc("d", &[b"ab".as_slice(), b"c".as_slice()]),
            enc("d", &[b"a".as_slice(), b"bc".as_slice()]),
            "ab|c and a|bc must not collide: without the length prefix a field boundary can be moved"
        );
        assert_ne!(
            enc("d1", &[b"x".as_slice()]),
            enc("d2", &[b"x".as_slice()]),
            "the domain is part of the transcript: that is the separation between messages"
        );
        assert_ne!(
            enc("d", &[b"".as_slice()]),
            enc("d", &[]),
            "an empty field is still a field: dropping it must change the transcript"
        );
    }

    #[test]
    fn the_pair_request_mac_matches_the_independent_vector_and_binds_every_field() {
        let ck = mac32("527dbdde924599069a8123a702f55a1189f4a353eafa0a4563af2f4ee8ae3553");
        let nc: [u8; 16] = counting(0);
        let h: [u8; 32] = counting(32);
        let base = pair_request_mac(&ck, &nc, "pc-lab-1", &h);
        // python3 hmac.new(code_key, enc("memory-industry/pair-req/v1", [nc, label, h]), sha256);
        // cross-checked with `openssl dgst -sha256 -mac HMAC -macopt hexkey:<code_key>`
        assert_eq!(
            base,
            mac32("b295f9a4d143e4493b0bef04e990dfa5696055299c2f0b8427843c442e5ff535"),
            "pair_request_mac is HMAC-SHA256 keyed with code_key over enc(\"memory-industry/pair-req/v1\", nc, label, h)"
        );

        let mut other_nc = nc;
        other_nc[0] ^= 1;
        let mut other_h = h;
        other_h[31] ^= 1;
        let mut other_key = ck;
        other_key[0] ^= 1;
        let cases = [
            ("nc", pair_request_mac(&ck, &other_nc, "pc-lab-1", &h)),
            ("label", pair_request_mac(&ck, &nc, "pc-lab-2", &h)),
            ("h", pair_request_mac(&ck, &nc, "pc-lab-1", &other_h)),
            (
                "code_key",
                pair_request_mac(&other_key, &nc, "pc-lab-1", &h),
            ),
        ];
        for (field, changed) in cases {
            assert_ne!(
                changed, base,
                "changing {field} must change the request MAC: a field left out of the \
                 transcript is a field an attacker can rewrite in flight"
            );
        }
    }

    #[test]
    fn the_pair_response_mac_matches_the_independent_vector_and_binds_all_eight_inputs() {
        let ck = mac32("527dbdde924599069a8123a702f55a1189f4a353eafa0a4563af2f4ee8ae3553");
        let nc: [u8; 16] = counting(0);
        let h: [u8; 32] = counting(32);
        let (host, cred) = (host(), cred());
        let base = pair_response_mac(&ck, &nc, &h, &host, &cred, "ana", "pc-lab-1", "MADRE");
        // python3, same recipe with enc("memory-industry/pair-resp/v1", [nc, h, host_id.bytes,
        // credential_id.bytes, account, label, host_name]); uuids enter as their 16 raw bytes
        assert_eq!(
            base,
            mac32("238dfe4e144d65aa52cb91b84b68a26135c3384640465c8acfbb4de53177ada9"),
            "pair_response_mac is HMAC-SHA256 keyed with code_key over the response transcript, \
             the uuids as 16 raw bytes and the text as UTF-8"
        );

        let mut other_key = ck;
        other_key[0] ^= 1;
        let mut other_nc = nc;
        other_nc[0] ^= 1;
        let mut other_h = h;
        other_h[0] ^= 1;
        let other_host = Uuid::from_u128(host.as_u128() ^ 1);
        let other_cred = Uuid::from_u128(cred.as_u128() ^ 1);
        let cases = [
            (
                "code_key",
                pair_response_mac(
                    &other_key, &nc, &h, &host, &cred, "ana", "pc-lab-1", "MADRE",
                ),
            ),
            (
                "nc",
                pair_response_mac(&ck, &other_nc, &h, &host, &cred, "ana", "pc-lab-1", "MADRE"),
            ),
            (
                "h",
                pair_response_mac(&ck, &nc, &other_h, &host, &cred, "ana", "pc-lab-1", "MADRE"),
            ),
            (
                "host_id",
                pair_response_mac(&ck, &nc, &h, &other_host, &cred, "ana", "pc-lab-1", "MADRE"),
            ),
            (
                "credential_id",
                pair_response_mac(&ck, &nc, &h, &host, &other_cred, "ana", "pc-lab-1", "MADRE"),
            ),
            (
                "account",
                pair_response_mac(&ck, &nc, &h, &host, &cred, "bea", "pc-lab-1", "MADRE"),
            ),
            (
                "label",
                pair_response_mac(&ck, &nc, &h, &host, &cred, "ana", "pc-lab-2", "MADRE"),
            ),
            (
                "host_name",
                pair_response_mac(&ck, &nc, &h, &host, &cred, "ana", "pc-lab-1", "OTRA"),
            ),
        ];
        for (field, changed) in cases {
            assert_ne!(
                changed, base,
                "changing {field} must change the response MAC: the client trusts exactly what \
                 this MAC covers, and nothing else in the 200 body"
            );
        }
    }

    #[test]
    fn the_identity_proof_matches_the_independent_vector_and_binds_every_input() {
        let secret_hash: [u8; 32] = counting(64);
        let nonce: [u8; 16] = counting(100);
        let (host, cred) = (host(), cred());
        let base = identity_proof(&secret_hash, &host, &cred, &nonce);
        // python3 hmac.new(secret_hash, enc("memory-industry/identity/v1", [host_id.bytes,
        // credential_id.bytes, nonce]), sha256)
        assert_eq!(
            base,
            mac32("5497228058f246ce2e7985c03c4f5d41e83608a5642b5ea7b557e1fca70e7f75"),
            "identity_proof is HMAC-SHA256 keyed with the credential's secret hash over \
             enc(\"memory-industry/identity/v1\", host_id, credential_id, nonce)"
        );

        let mut other_key = secret_hash;
        other_key[0] ^= 1;
        let mut other_nonce = nonce;
        other_nonce[0] ^= 1;
        let cases = [
            (
                "host_id",
                identity_proof(
                    &secret_hash,
                    &Uuid::from_u128(host.as_u128() ^ 1),
                    &cred,
                    &nonce,
                ),
            ),
            (
                "credential_id",
                identity_proof(
                    &secret_hash,
                    &host,
                    &Uuid::from_u128(cred.as_u128() ^ 1),
                    &nonce,
                ),
            ),
            (
                "nonce",
                identity_proof(&secret_hash, &host, &cred, &other_nonce),
            ),
            ("key", identity_proof(&other_key, &host, &cred, &nonce)),
        ];
        for (field, changed) in cases {
            assert_ne!(
                changed, base,
                "changing {field} must change the proof: a proof that ignores the nonce is \
                 replayable, one that ignores the host_id answers for any host"
            );
        }
    }

    #[test]
    fn the_three_macs_differ_on_the_same_numeric_inputs() {
        let key: [u8; 32] = counting(1);
        let nc: [u8; 16] = counting(2);
        let h: [u8; 32] = counting(3);
        let id = host();
        let request = pair_request_mac(&key, &nc, "x", &h);
        let response = pair_response_mac(&key, &nc, &h, &id, &id, "x", "x", "x");
        let proof = identity_proof(&key, &id, &id, &nc);
        assert_ne!(
            request, response,
            "request and response MACs share a key (code_key): only the domain stops one \
             message from being replayed as the other"
        );
        assert_ne!(
            request, proof,
            "the domain separates the pairing request from the identity proof"
        );
        assert_ne!(
            response, proof,
            "the domain separates the pairing response from the identity proof"
        );
    }

    // ── P5 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn verify_mac_accepts_only_the_exact_lowercase_hex() {
        // a vector whose hex has letters, so uppercasing changes it
        let m = mac32("b295f9a4d143e4493b0bef04e990dfa5696055299c2f0b8427843c442e5ff535");
        let good = hex::encode(m);
        assert!(
            verify_mac(m, &good),
            "the MAC's own lowercase hex must verify"
        );
        assert!(
            !verify_mac(m, &good.to_uppercase()),
            "uppercase hex is a second spelling of the same MAC: only lowercase is accepted"
        );
        assert!(
            !verify_mac(m, &good[..63]),
            "63 characters is a truncated MAC"
        );
        assert!(
            !verify_mac(m, &format!("{good}0")),
            "65 characters is a MAC with trailing data"
        );
        assert!(
            !verify_mac(m, &"zz".repeat(32)),
            "64 characters that are not hex cannot verify"
        );
        assert!(!verify_mac(m, ""), "an empty presentation never verifies");
        let mut flipped = m;
        flipped[31] ^= 1;
        assert!(
            !verify_mac(m, &hex::encode(flipped)),
            "a single changed bit must not verify"
        );
    }

    #[test]
    fn ct_eq_compares_lengths_and_every_byte() {
        assert!(ct_eq(&[1, 2, 3], &[1, 2, 3]), "equal slices are equal");
        assert!(ct_eq(&[], &[]), "two empty slices are equal");
        assert!(
            !ct_eq(&[1, 2], &[1, 2, 3]),
            "a prefix is not equal: comparing only the common prefix would accept a truncated secret"
        );
        assert!(
            !ct_eq(&[1, 2, 3], &[1, 2]),
            "the longer side first is just as different"
        );
        assert!(
            !ct_eq(&[1, 2, 3], &[1, 2, 4]),
            "a differing last byte is a different secret"
        );
    }

    // ── P6 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_fingerprint_is_four_groups_stable_and_matches_the_independent_vector() {
        let fp = fingerprint(&host());
        assert!(
            is_fingerprint(&fp),
            "`{fp}` must be four groups of four uppercase hex characters, readable aloud"
        );
        assert_eq!(
            fp,
            fingerprint(&host()),
            "the same host_id always gives the same fingerprint"
        );
        assert_ne!(
            fp,
            fingerprint(&Uuid::from_u128(host().as_u128() ^ 1)),
            "two different hosts must not share a fingerprint"
        );
        // printf '\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff' preceded by
        // "memory-industry/host-fingerprint/v1" (no separator) | sha256sum, first 8 bytes, uppercase
        assert_eq!(
            fp, "66B0-A05F-8A90-DDAF",
            "fingerprint is SHA-256(\"memory-industry/host-fingerprint/v1\" || host_id bytes)[..8] \
             in uppercase hex: 8 bytes, 64 bits, enough to compare by eye and tell two hosts apart"
        );
    }

    // ── P7 ────────────────────────────────────────────────────────────────────────

    fn account_bearer() -> (String, Uuid) {
        let id = Uuid::from_u128(0x0a1b2c3d_4e5f_4a6b_8c7d_8e9fa0b1c2d3);
        (bearer_for_table(id), id)
    }

    // Built by hand, not with `bearer()`, so the classification table does not depend on
    // the function P3 judges.
    fn bearer_for_table(id: Uuid) -> String {
        format!("mi1.{}.{}", id.simple(), "cd".repeat(32))
    }

    #[test]
    fn a_bearer_is_classified_by_the_closed_table_without_the_policy() {
        let (account, id) = account_bearer();
        let class = |raw: &str| classify_bearer(raw, Some("S-token"), Some("P-token"), false);
        assert_eq!(
            class("S-token"),
            BearerClass::Shared,
            "the shared token keeps working exactly as in 0.28"
        );
        assert_eq!(
            class("P-token"),
            BearerClass::Peer,
            "the peer token is its own class: it opens only the sync verbs"
        );
        assert_eq!(
            class(&account),
            BearerClass::Account {
                id,
                secret: [0xCD; 32]
            },
            "a well-formed mi1 credential is an account, carrying the id and the 32 secret bytes"
        );
        for raw in ["", "basura", "mi1.mal"] {
            assert_eq!(
                class(raw),
                BearerClass::Unrecognized,
                "{raw:?} matches nothing: it must reach the failure brake, not be waved through"
            );
        }
    }

    #[test]
    fn the_lan_policy_refuses_the_shared_token_and_nothing_else() {
        let (account, id) = account_bearer();
        let class = |raw: &str| classify_bearer(raw, Some("S-token"), Some("P-token"), true);
        assert_eq!(
            class("S-token"),
            BearerClass::SharedRefusedByPolicy,
            "with REQUIRE_ACCOUNTS on a LAN bind the shared token is recognised but refused, \
             so the daemon can answer with the exact refusal text and not count a failure"
        );
        assert_eq!(
            class("P-token"),
            BearerClass::Peer,
            "the policy must not lock out the peer token: sync verbs still need it"
        );
        assert_eq!(
            class(&account),
            BearerClass::Account {
                id,
                secret: [0xCD; 32]
            },
            "accounts are what the policy is asking for"
        );
        assert_eq!(
            class("basura"),
            BearerClass::Unrecognized,
            "garbage stays garbage under the policy"
        );
    }

    #[test]
    fn without_any_configured_token_nothing_is_classified() {
        let (account, _) = account_bearer();
        for raw in ["anything", "", account.as_str()] {
            assert_eq!(
                classify_bearer(raw, None, None, false),
                BearerClass::NoAuthConfigured,
                "{raw:?}: with no token configured the daemon is open (loopback only), so no \
                 bearer is judged"
            );
        }
    }

    #[test]
    fn a_shared_token_that_looks_like_a_credential_is_still_the_shared_token() {
        let (lookalike, _) = account_bearer();
        assert_eq!(
            classify_bearer(&lookalike, Some(&lookalike), Some("P-token"), false),
            BearerClass::Shared,
            "the shared token is compared before the credential shape: an operator whose \
             token starts with mi1. must not have it silently reinterpreted as an account"
        );
        assert_eq!(
            classify_bearer(&lookalike, Some(&lookalike), Some("P-token"), true),
            BearerClass::SharedRefusedByPolicy,
            "and under the policy it is the shared token that is refused, not an account that is looked up"
        );
    }

    // ── P8 ────────────────────────────────────────────────────────────────────────

    #[test]
    fn the_knob_is_read_once_and_only_a_real_yes_turns_it_on() {
        for yes in ["1", "true", "TRUE", "yes", "on"] {
            assert_eq!(
                require_accounts_effective(false, Some(yes)),
                (true, None),
                "{yes:?} on a LAN bind turns the policy on without a warning"
            );
        }
        for no in ["0", "off", "false", "no", "", "abc"] {
            assert_eq!(
                require_accounts_effective(false, Some(no)),
                (false, None),
                "{no:?} must leave the policy off: a typo must never lock the shared token out"
            );
        }
        assert_eq!(
            require_accounts_effective(false, None),
            (false, None),
            "unset is off"
        );
    }

    #[test]
    fn the_knob_is_ignored_on_loopback_and_says_so() {
        let (on, warning) = require_accounts_effective(true, Some("1"));
        assert!(
            !on,
            "on loopback the knob is ignored: the shared token is the only way in there"
        );
        let warning = warning.expect("an ignored knob must be reported, not swallowed");
        assert!(
            warning.contains("ignored on loopback"),
            "the warning must say the setting is ignored on loopback, so the operator knows it did nothing; got: {warning}"
        );
        for quiet in [Some("0"), Some("abc"), None] {
            assert_eq!(
                require_accounts_effective(true, quiet),
                (false, None),
                "{quiet:?}: nothing was asked for, so there is nothing to warn about"
            );
        }
    }
}
