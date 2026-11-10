//! Contract for the lane token of the gate lock (CI-F3.3b-1b-i, D-F3k, minimal form).
//!
//! The planner (`scripts/gate-dag.py`) starts each step as its own child, so a `--lane` run
//! under merge-gate is a grandchild of the lock owner and the direct-parent rule of
//! `inherit_gate_lock` refuses it. A step the planner starts gets a one-use ticket instead:
//! `$GATE_LOCK/lanes/<idx>-<id>` holds a random token, and the token travels in
//! `CUBA_GATE_LANE_TOKEN`. The lane inherits the lock only when the ticket is unused and
//! holds that token, the owner is alive, and the owner is one of its ancestors (read from
//! /proc). Taking the ticket spends it.
//!
//! Without a token nothing changes: a direct child of the owner inherits as it always did.
//! A refused token never falls back to taking the lock: the lane exits 2 (`inherit_or_acquire_gate_lock`).
//!
//! Every scenario is a real bash process tree. The owner (this shell, with its own pid) writes
//! the lock record; the consumer runs `inherit_gate_lock` from `scripts/gate-lock.sh`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// Runs in the owner before its own body: loads the library, makes the lock directory and the
/// ticket `2-D2` with the planted content, and exports the token the lane presents.
const PREFIX: &str = r#"
. "$LIB"
lock="$T/lock"
mkdir -p "$lock/lanes"
printf '%s' "$PLANTED" >"$lock/lanes/2-D2"
export CUBA_GATE_LANE_TOKEN="$TOKEN"
"#;

/// The lane: one `inherit_gate_lock`, reporting its status.
const CONSUMER: &str = r#"
. "$LIB"
lock="$T/lock"
inherit_gate_lock "$lock"
echo "rc=$?"
"#;

/// The lane as the judge's entry point runs it: inherit or acquire, reporting the status.
const CONSUMER_OR_ACQUIRE: &str = r#"
. "$LIB"
lock="$T/lock"
inherit_or_acquire_gate_lock "$lock"
echo "rc=$?"
"#;

fn lib() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("scripts/gate-lock.sh")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-lane-token-{name}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("home")).expect("scratch dir");
    dir
}

struct Out {
    out: String,
    err: String,
    lock: PathBuf,
}

impl Out {
    fn text(&self) -> String {
        format!("stdout:\n{}\nstderr:\n{}", self.out, self.err)
    }
}

/// Runs `body` as the owner process. `token` is what the lane presents, `planted` what the
/// ticket holds. `consumer` is the lane script the body runs.
fn owner(name: &str, body: &str, token: &str, planted: &str, consumer: &str) -> Out {
    let dir = scratch(name);
    // The trailing `:` keeps bash from exec-ing the body's last command: an exec'd owner would be the
    // direct child of its own lane, and the scenarios would test the wrong ancestry.
    let script = format!("{PREFIX}{body}\n:\n");
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("LIB", lib())
        .env("T", &dir)
        .env("TOKEN", token)
        .env("PLANTED", planted)
        .env("CONSUMER", consumer)
        .env("CONSUMER_OR_ACQUIRE", CONSUMER_OR_ACQUIRE)
        .env("HOME", dir.join("home"))
        .env_remove("CUBA_GATE_LOCK_OWNER")
        .stdin(Stdio::null())
        .output()
        .expect("bash owner");
    Out {
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
        lock: dir.join("lock"),
    }
}

/// True when the ticket `2-D2` was spent: renamed to `2-D2.used.<pid>`.
fn ticket_spent(lock: &Path) -> bool {
    fs::read_dir(lock.join("lanes")).is_ok_and(|dir| {
        dir.flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("2-D2.used.")
        })
    })
}

/// The owner's record, written for the owner's own pid, as a run of gate-lock.sh would.
const OWN_RECORD: &str = r#"
record="$(owner_record "$$" owner)"
printf '%s\n' "$record" >"$lock/owner"
export CUBA_GATE_LOCK_OWNER="$record"
"#;

/// A grandchild of the owner, the way the planner starts a step: owner -> planner -> lane.
const VIA_PLANNER: &str = r#"bash -c 'bash -c "$1"; :' _ "$CONSUMER""#;

/// The lane that spends its ticket, then runs a child of its own (CI-J9). The token must be gone from the
/// lane's environment and from that child's.
const CONSUMER_WITH_CHILD: &str = r#"
. "$LIB"
lock="$T/lock"
inherit_gate_lock "$lock"
echo "rc=$?"
echo "lane=${CUBA_GATE_LANE_TOKEN-unset}"
bash -c 'echo "child=${CUBA_GATE_LANE_TOKEN-unset}"'
"#;

/// The step the planner starts: checks its ticket and leaves it for the lane it starts (CI-J9, `--step`).
const STEP_CHECK: &str = r#"
. "$LIB"
lock="$T/lock"
lane_token_check "$lock" "$CUBA_GATE_LOCK_OWNER"
echo "check=$?"
echo "step=${CUBA_GATE_LANE_TOKEN-unset}"
"#;

#[test]
fn a_planner_step_inherits_the_lock_through_its_ticket() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("planner-ok", &body, "tok-a", "tok-a", CONSUMER);
    assert!(
        out.out.contains("rc=0"),
        "a lane with the planted ticket must inherit the lock of its owner ancestor\n{}",
        out.text()
    );
    assert!(
        out.out
            .contains("running under the gate that holds the lock"),
        "an inherited lock is announced as before\n{}",
        out.text()
    );
    assert!(
        ticket_spent(&out.lock),
        "the inherited ticket must be spent (renamed to 2-D2.used.<pid>)\n{}",
        out.text()
    );
}

/// CI-J9. The lane that spends its ticket gives the token up: neither the lane nor a child it starts keeps
/// it, so no test or script under the lane can present a ticket it does not own.
#[test]
fn a_spent_lane_token_does_not_reach_the_lane_or_its_children() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("spent-token", &body, "tok-e", "tok-e", CONSUMER_WITH_CHILD);
    assert!(
        out.out.contains("rc=0"),
        "a lane with the planted ticket must inherit the lock, or nothing below is tested\n{}",
        out.text()
    );
    assert!(
        out.out.contains("lane=unset"),
        "the lane spent its ticket and still holds the token in its own environment\n{}",
        out.text()
    );
    assert!(
        out.out.contains("child=unset"),
        "a child of the lane inherits the spent token, and a test or script it starts would present \
         it as its own\n{}",
        out.text()
    );
    assert!(
        ticket_spent(&out.lock),
        "the inherited ticket must be spent\n{}",
        out.text()
    );
}

/// CI-J9. `--step` checks the ticket and does not spend it: the token stays in the step's environment for the
/// lane the step starts, which is the one that spends it.
#[test]
fn a_step_that_checks_its_ticket_leaves_the_token_for_its_lane() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("step-check", &body, "tok-f", "tok-f", STEP_CHECK);
    assert!(
        out.out.contains("check=0"),
        "a step with its planted ticket must pass the check\n{}",
        out.text()
    );
    assert!(
        out.out.contains("step=tok-f"),
        "the check gave the token up, so the lane the step starts would be refused\n{}",
        out.text()
    );
    assert!(
        !ticket_spent(&out.lock),
        "the check spent the ticket, which only the lane may do\n{}",
        out.text()
    );
}

#[test]
fn a_ticket_that_is_not_the_planted_one_is_refused_by_name() {
    // A direct child of the owner: the old rule would inherit it, so only the token can refuse it.
    let body = format!("{OWN_RECORD}\nbash -c \"$CONSUMER\"\n");
    let out = owner("foreign-ticket", &body, "tok-wrong", "tok-right", CONSUMER);
    assert!(
        !out.out.contains("rc=0"),
        "a lane that presents a token that is not the ticket's must not inherit\n{}",
        out.text()
    );
    assert!(
        out.err.contains("lane token"),
        "the refusal names the lane token, so the log says why the lane took no lock\n{}",
        out.text()
    );
}

#[test]
fn a_dead_owner_refuses_the_ticket_by_name() {
    let body = format!(
        r#"
sleep 0 &
dead=$!
wait "$dead"
record="$(owner_record "$dead" dead-owner)"
printf '%s\n' "$record" >"$lock/owner"
export CUBA_GATE_LOCK_OWNER="$record"
{VIA_PLANNER}
"#
    );
    let out = owner("dead-owner", &body, "tok-c", "tok-c", CONSUMER);
    assert!(
        !out.out.contains("rc=0"),
        "a lock whose owner is gone must not be inherited\n{}",
        out.text()
    );
    assert!(
        out.err.contains("lane token"),
        "the refusal of a dead owner names the lane token\n{}",
        out.text()
    );
}

#[test]
fn a_lane_that_does_not_descend_from_the_owner_is_refused() {
    // The lock's owner is a live process beside this lane, not one of its ancestors.
    let body = r#"
sleep 300 >/dev/null 2>&1 &
other=$!
record="$(owner_record "$other" other-owner)"
printf '%s\n' "$record" >"$lock/owner"
export CUBA_GATE_LOCK_OWNER="$record"
bash -c "$CONSUMER"
kill "$other"
"#;
    let out = owner("not-descendant", body, "tok-d", "tok-d", CONSUMER);
    assert!(
        !out.out.contains("rc=0"),
        "a lane that does not descend from the lock owner must not inherit it\n{}",
        out.text()
    );
    assert!(
        out.err.contains("descend"),
        "the refusal says the lane does not descend from the owner\n{}",
        out.text()
    );
}

#[test]
fn a_reused_ticket_is_refused_the_second_time() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n{VIA_PLANNER}\n");
    let out = owner("reused", &body, "tok-e", "tok-e", CONSUMER);
    let inherited = out
        .out
        .lines()
        .filter(|line| line.starts_with("rc=0"))
        .count();
    assert_eq!(
        inherited,
        1,
        "exactly one lane inherits with one ticket\n{}",
        out.text()
    );
    assert!(
        out.err.contains("lane token"),
        "the second presentation of the same ticket is refused by name\n{}",
        out.text()
    );
}

#[test]
fn a_refused_token_exits_2_and_never_takes_the_lock() {
    // The lock is held by the owner; a refused lane must not fall back to acquiring it.
    let body = format!("{OWN_RECORD}\nbash -c \"$CONSUMER_OR_ACQUIRE\"\n");
    let out = owner(
        "exit-two",
        &body,
        "tok-wrong",
        "tok-right",
        CONSUMER_OR_ACQUIRE,
    );
    assert!(
        out.out.contains("rc=2"),
        "a refused token makes the lane exit 2, as a planner refusal does\n{}",
        out.text()
    );
    assert!(
        !out.err.contains("another gate is running"),
        "a refused token is not reported as a second gate\n{}",
        out.text()
    );
}

#[test]
fn a_direct_child_without_a_token_still_inherits_as_before() {
    let body = format!("{OWN_RECORD}\nbash -c \"$CONSUMER\"\n");
    let out = owner("no-token", &body, "", "", CONSUMER);
    assert!(
        out.out.contains("rc=0"),
        "without a token the direct-parent rule of run-all-tests --lane in series is unchanged\n{}",
        out.text()
    );
}

/// The step's own check, as `--step` runs it before its lane: it reads the ticket and spends nothing.
const CHECK_ONLY: &str = r#"
. "$LIB"
lock="$T/lock"
lane_token_check "$lock" "$CUBA_GATE_LOCK_OWNER"
echo "check=$?"
"#;

/// The step's check, then the lane's inheritance: the lane still inherits, because the check spent nothing.
const CHECK_THEN_LANE: &str = r#"
. "$LIB"
lock="$T/lock"
lane_token_check "$lock" "$CUBA_GATE_LOCK_OWNER" || exit 9
inherit_gate_lock "$lock"
echo "rc=$?"
"#;

// CI-F3.3b-2 (D-F3k, spec F3.3b-1b-ii): `--step` checks the planner's ticket and spends nothing. The lane that
// the step starts is the one that spends it, so the step never takes a ticket its own lane needs.
#[test]
fn a_step_check_reads_the_ticket_and_spends_nothing() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("step-check", &body, "tok", "tok", CHECK_ONLY);
    assert!(
        out.out.contains("check=0"),
        "the step's check refused the ticket the planner planted\n{}",
        out.text()
    );
    assert!(
        !ticket_spent(&out.lock),
        "the step's check spent the ticket its lane has to spend"
    );
}

#[test]
fn a_lane_inherits_after_its_step_checked_the_ticket() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("step-then-lane", &body, "tok", "tok", CHECK_THEN_LANE);
    assert!(
        out.out.contains("rc=0"),
        "the lane was refused after its step checked the ticket\n{}",
        out.text()
    );
    assert!(
        ticket_spent(&out.lock),
        "the lane inherited without spending its ticket"
    );
}

#[test]
fn a_step_check_refuses_a_ticket_that_is_not_the_planted_one_and_spends_nothing() {
    let body = format!("{OWN_RECORD}\n{VIA_PLANNER}\n");
    let out = owner("step-foreign", &body, "other", "tok", CHECK_ONLY);
    assert!(
        out.out.contains("check=1"),
        "the step's check accepted a token that is not planted\n{}",
        out.text()
    );
    assert!(
        out.err.contains("lane token refused"),
        "the refusal does not say it was a lane token: {}",
        out.err
    );
    assert!(
        !ticket_spent(&out.lock),
        "a refused check spent the planted ticket"
    );
}
