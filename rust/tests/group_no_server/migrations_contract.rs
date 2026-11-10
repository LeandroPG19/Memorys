use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

fn up_migrations() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(migrations_dir())
        .expect("migrations/ is readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".up.sql"))
        })
        .map(|path| {
            let stem = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("utf8 filename")
                .trim_end_matches(".up.sql")
                .to_string();
            let body = std::fs::read_to_string(&path).expect("readable");
            (stem, body)
        })
        .collect();
    files.sort();
    files
}

const GUARD_MARKERS: [&str; 4] = [
    "DROP TRIGGER IF EXISTS",
    "DROP CONSTRAINT IF EXISTS",
    "information_schema.tables",
    "information_schema.table_constraints",
];

fn unguarded_statements(sql: &str) -> Vec<&'static str> {
    ["CREATE TRIGGER", "ADD CONSTRAINT"]
        .into_iter()
        .filter(|statement| {
            sql.match_indices(*statement).any(|(pos, _)| {
                !GUARD_MARKERS
                    .iter()
                    .any(|marker| sql[..pos].contains(*marker))
            })
        })
        .collect()
}

const ALREADY_APPLIED_WITHOUT_A_GUARD: [(&str, &str); 14] = [
    (
        "0029_bitemporal_check",
        "two ADD CONSTRAINT on brain_facts, no guard",
    ),
    (
        "0035_relation_provenance",
        "ADD CONSTRAINT on brain_relations, no guard",
    ),
    (
        "0037_observation_trust",
        "ADD CONSTRAINT on brain_observations, no guard",
    ),
    (
        "0043_episode_error_trust",
        "two ADD CONSTRAINT, one per table (brain_episodes, brain_errors), no guard",
    ),
    (
        "0044_observation_evidence",
        "four ADD CONSTRAINT on brain_observations, no guard",
    ),
    (
        "0045_tombstones",
        "CREATE TRIGGER inside a DO block's dynamic SQL, looped over six tables, no guard",
    ),
    (
        "0047_sync_clock",
        "CREATE TRIGGER brain_observations_sync_clock, no guard",
    ),
    (
        "0049_portable_layers_and_wider_tombstones",
        "two CREATE TRIGGER, one per new tombstoned table (brain_procedures, brain_facts), no guard",
    ),
    (
        "0050_peer_notices",
        "CREATE TRIGGER brain_peer_notices_cap, no guard",
    ),
    (
        "0052_sync_conflicts",
        "ADD CONSTRAINT brain_sync_conflicts_closed_says_how, no guard",
    ),
    (
        "0053_handler_failures",
        "CREATE TRIGGER brain_handler_failures_cap, no guard",
    ),
    (
        "0056_agent_notes",
        "CREATE TRIGGER brain_agent_notes_cap, no guard",
    ),
    (
        "0057_schema_tightening",
        "ADD CONSTRAINT brain_tombstones_known_table, no guard",
    ),
    (
        "0059_rem_cycles",
        "CREATE TRIGGER brain_rem_cycles_cap, no guard",
    ),
];

#[test]
fn a_new_migration_cannot_ship_a_statement_that_aborts_if_the_database_replays_it() {
    let exceptions: BTreeMap<&str, &str> = ALREADY_APPLIED_WITHOUT_A_GUARD.into_iter().collect();
    assert_eq!(
        exceptions.len(),
        ALREADY_APPLIED_WITHOUT_A_GUARD.len(),
        "ALREADY_APPLIED_WITHOUT_A_GUARD has a duplicate stem — two entries collapsed into one \
         key, which means one of them is not actually excluded"
    );

    let migrations = up_migrations();
    assert!(
        migrations.len() >= 59,
        "the scan found {} up-migrations and there are at least 59; a scan that found almost \
         none proves nothing",
        migrations.len()
    );

    for (name, reason) in &exceptions {
        let Some((_, body)) = migrations.iter().find(|(stem, _)| stem.as_str() == *name) else {
            panic!(
                "ALREADY_APPLIED_WITHOUT_A_GUARD names {name} ({reason}), but no such migration \
                 file exists any more — drop the entry"
            );
        };
        assert!(
            !unguarded_statements(body).is_empty(),
            "ALREADY_APPLIED_WITHOUT_A_GUARD excuses {name} as non-reapplicable ({reason}), but \
             it no longer contains an unguarded CREATE TRIGGER or ADD CONSTRAINT — the exception \
             outlived the thing it excused, drop it"
        );
    }

    let offenders: Vec<String> = migrations
        .iter()
        .filter(|(stem, _)| !exceptions.contains_key(stem.as_str()))
        .filter_map(|(stem, body)| {
            let bad = unguarded_statements(body);
            if bad.is_empty() {
                None
            } else {
                Some(format!("{stem} ({})", bad.join(", ")))
            }
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these migrations run CREATE TRIGGER or ADD CONSTRAINT with nothing guarding it, so \
         replaying sqlx-migrate against a database that already has the object aborts the whole \
         run: PostgreSQL has no CREATE TRIGGER IF NOT EXISTS or ADD CONSTRAINT IF NOT EXISTS. \
         Measured on 0045-0059: 9 of those 15 files abort on a second apply for exactly this \
         reason, and two of the nine (0057, 0059) were added in 0.24.0, after the CHANGELOG had \
         already written the debt down — nothing stopped the pattern from repeating. A guard is \
         either a DROP ... IF EXISTS for the same object right before the CREATE/ADD — 0055 does \
         this for its CREATE POLICY statements, which is why it is NOT in this list — or the \
         whole statement wrapped in an information_schema existence check the way 0016 and 0023 \
         do. Migrations already applied cannot be edited to add one: sqlx hashes every applied \
         file with SHA-384, and changing a single byte refuses to start every database that \
         already ran it (e96df5d broke this, a245f62 undid it). Those belong in \
         ALREADY_APPLIED_WITHOUT_A_GUARD with a reason, which is where the ones on disk today \
         already are. A migration that is new to this branch has no such excuse: {offenders:#?}"
    );
}

const FROZEN_THROUGH: u32 = 60;
const FROZEN_DIGEST: &str = "df5e42237556df4f7e7d32cc3ba55bc8fa61987a3a559eef95827f538b7ea1a27216b0b028f586d42b3ad0f8314e1337";

#[test]
fn a_migration_that_has_already_shipped_cannot_be_edited() {
    use sha2::{Digest, Sha384};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut frozen: Vec<_> = std::fs::read_dir(&dir)
        .expect("the migrations directory is readable")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".up.sql"))
        .filter(|name| {
            name.get(..4)
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n <= FROZEN_THROUGH)
        })
        .collect();
    frozen.sort();

    let mut hasher = Sha384::new();
    for name in &frozen {
        hasher.update(name.as_bytes());
        hasher.update(std::fs::read(dir.join(name)).expect("a listed migration is readable"));
    }
    let digest = format!("{:x}", hasher.finalize());

    assert_eq!(
        digest,
        FROZEN_DIGEST,
        "one of the {} migrations at or below {FROZEN_THROUGH} changed on disk. sqlx hashes \
         every applied file with SHA-384 and refuses to start any database that already ran a \
         different version of it, so editing one of these bricks every existing install — not \
         CI, which always migrates from scratch, but the machines that already have the data. \
         It has happened twice: e96df5d stripped comments from applied migrations and broke \
         the startup of every pre-0.14 database (undone in a245f62), and on 2026-08-16 a \
         one-word fix to a stale comment in 0031 nearly shipped the same way. If a shipped \
         migration is wrong, write a new one that corrects it. If you deliberately added \
         migrations past {FROZEN_THROUGH}, raise FROZEN_THROUGH and update this digest in the \
         same commit. Find what moved with: git diff HEAD -- rust/migrations/",
        frozen.len()
    );
}

#[test]
fn a_file_an_applied_migration_tells_you_to_run_still_exists() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");

    let mut promised: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the migrations directory is readable") {
        let entry = entry.expect("a directory entry is readable");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") {
            continue;
        }
        let body = std::fs::read_to_string(entry.path()).expect("a migration is readable");
        for word in body.split_whitespace() {
            let path = word.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && c != '/' && c != '.' && c != '-' && c != '_'
            });
            if path.starts_with("scripts/") && path.ends_with(".sql") {
                promised.push((name.clone(), path.to_string()));
            }
        }
    }

    assert!(
        !promised.is_empty(),
        "the scan found no script path in any migration, so a green result would prove nothing. \
         0031 and 0055 both tell the reader to run scripts/create-app-role.sql"
    );

    let missing: Vec<&(String, String)> = promised
        .iter()
        .filter(|(_, path)| !root.join(path).exists())
        .collect();

    assert!(
        missing.is_empty(),
        "an applied migration tells the reader to run a file that is not there: {missing:?}. \
         These comments cannot be corrected — sqlx hashes every applied migration and editing \
         one refuses to start every database that already ran it — so the only way to keep them \
         honest is to keep the file where they say it is. That is why the SQL lives at \
         scripts/create-app-role.sql and the binary embeds it from there with include_str!, \
         instead of the reverse. Moving it back under rust/src/ would silently make two \
         permanent comments lie again"
    );
}
