// The binary hands PostgreSQL the SCRAM-SHA-256 verifier of the application
// role's password instead of the password itself (db::bind_app_role), so the
// password never reaches the statement log or pg_stat_statements. A verifier
// the server cannot check a login against is a daemon that falls back to the
// superuser, so the function is held here to values computed apart from it.
// The v044 tests hold it to a real server: a role given it logs in.
//
// The first row is the example of RFC 7677 §3: password "pencil", salt
// W22ZaJ0SNY7soEsUEjb6gQ==, 4096 iterations. The RFC prints an exchange, not
// the stored keys, so StoredKey and ServerKey were derived with Python's
// hashlib and hmac and checked by recomputing from them the ClientProof
// (p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=) and the ServerSignature
// (v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=) the RFC prints; both
// matched. The other two rows come from the same derivation:
//
//   salted = hashlib.pbkdf2_hmac("sha256", password, salt, 4096)
//   stored = sha256(hmac(salted, b"Client Key")); server = hmac(salted, b"Server Key")
//
// with salts of 17 and 18 bytes, so the salt's Base64 ends in one `=` and in
// none, and the last one carries `+` and `/`.

use memory_industry::db::scram_sha_256_verifier;

#[test]
fn the_verifier_matches_values_computed_apart_from_the_binary() {
    let rows = [
        (
            "pencil",
            "5b6d99689d12358eeca04b141236fa81",
            "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$\
             WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:\
             wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=",
        ),
        (
            "9f8e7d6c5b4a39281706f5e4d3c2b1a0",
            "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0",
            "SCRAM-SHA-256$4096:oKGio6SlpqeoqaqrrK2ur7A=$\
             iP6ReYq60QTiA6v1oUtqUbr34Cxj/t0o8SHXLpuuVcQ=:\
             gJgIJpcmA+buCJhTcT9t17lH680ijTrDS9vMLNdB9jI=",
        ),
        (
            "9f8e7d6c5b4a39281706f5e4d3c2b1a0",
            "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff0001",
            "SCRAM-SHA-256$4096:8PHy8/T19vf4+fr7/P3+/wAB$\
             0abSE1Yt2HJ6erUEpLNqJ64d/I5u90d31A/w4USrHcQ=:\
             KO3xbHCxbnNjsD3+6aWBhnQpYAID6jKviH2X84FF2IE=",
        ),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|&(password, salt_hex, expected)| {
            let salt = hex::decode(salt_hex).expect("the salt of a row is hex");
            let got = scram_sha_256_verifier(password, &salt);
            (got != expected).then(|| {
                format!(
                    "  {password:?} with salt {salt_hex}\n    expected {expected}\n    got      {got}"
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "the SCRAM-SHA-256 verifier differs from the one computed apart from the binary. \
         PostgreSQL stores what it is handed as it is, so a role given this verifier refuses \
         its own password and the daemon falls back to the superuser:\n{}",
        wrong.join("\n")
    );
}
