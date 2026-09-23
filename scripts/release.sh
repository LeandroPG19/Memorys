#!/usr/bin/env bash
# The only way to publish a release.
#
#   ./scripts/release.sh vX.Y.Z              gate, tag, push the tag
#   ./scripts/release.sh --dry-run vX.Y.Z    gate, print the tag message, create nothing
#   ./scripts/release.sh --self-test         every guard against the fixture that has to stop it
#   ./scripts/release.sh --verify-receipt TAG SHA
#                                            what .github/workflows/publish.yml runs
#
# Publishing used to depend on GitHub: publish.yml asked `gh run list` whether
# ci.yml had passed on the tagged commit and refused otherwise. That made the
# badge the judge of a release while AGENTS.md says the badge is not the judge
# of anything: ci.yml excludes MODEL_OR_CLI_ONLY and has no ONNX, NLI, reranker
# or generative LLM. The judge is merge-gate.sh on this machine, so this script
# runs it on exactly the commit main holds and writes its verdict into the one
# object that travels with the release: an annotated tag. publish.yml reads the
# receipt back from that tag and refuses a tag that does not carry it.
#
# Every guard runs before anything is created. A failure anywhere leaves no tag,
# local or remote.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RECEIPT_PREFIX="local-gate: MERGE GATE PASSED"
ONLY_WAY="The only way to publish is ./scripts/release.sh vX.Y.Z: it runs ./scripts/merge-gate.sh on the commit origin/main holds and writes the receipt into an annotated tag."

die() {
  echo "REFUSING TO RELEASE: $*" >&2
  exit 1
}

usage() {
  sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# vX.Y.Z, with an optional pre-release suffix the way Cargo spells it.
version_of_tag() {
  local tag="$1"
  [[ "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$ ]] \
    || die "'$tag' is not a release tag. Expected vX.Y.Z, for example v0.27.0"
  printf '%s\n' "${BASH_REMATCH[1]}"
}

# The version under [package], not the first `version =` in the file: a
# dependency table written inline further down also has one.
cargo_version() {
  tr -d '\r' <"$ROOT/rust/Cargo.toml" | awk '
    /^\[/ { section = $0; next }
    section == "[package]" && /^version[[:space:]]*=/ {
      sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print; exit
    }'
}

# Step 1: what is about to be judged is what main holds, and nothing else.
# Untracked files count: the DB integration step discovers rust/tests/*.rs by
# glob, so a stray file changes what the gate runs without being in the commit.
check_the_tree() {
  local tag="$1" head main remote
  [[ -z "$(git status --porcelain)" ]] \
    || die "the working tree is not clean, so the gate would judge something that is not a commit:
$(git status --porcelain)"
  # --no-tags: a plain fetch would auto-follow a tag somebody already pushed
  # for this version, and the refusal would blame this clone for it.
  git fetch --quiet --no-tags origin \
    || die "git fetch origin failed, so there is no telling whether HEAD is what main holds"
  head="$(git rev-parse HEAD)"
  main="$(git rev-parse --verify -q refs/remotes/origin/main)" \
    || die "there is no origin/main to compare HEAD with"
  [[ "$head" == "$main" ]] \
    || die "HEAD is $head and origin/main is $main. What gets published has to be what main holds: push or pull first"
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    die "tag $tag already exists here. A release is never re-tagged: bump the version"
  fi
  remote="$(git ls-remote --tags origin "refs/tags/$tag")" \
    || die "could not list the tags on origin, so there is no telling whether $tag is already taken"
  [[ -z "$remote" ]] \
    || die "tag $tag already exists on origin. A release is never re-tagged: bump the version"
}

# Step 2: the tag and the code say the same version.
check_the_version() {
  local tag="$1" wanted have
  wanted="$(version_of_tag "$tag")"
  have="$(cargo_version)"
  [[ -n "$have" ]] || die "no version under [package] in rust/Cargo.toml"
  [[ "$have" == "$wanted" ]] \
    || die "the tag says $wanted and rust/Cargo.toml says $have. A release is published under the version its code declares"
}

# Step 3, second half: exit 0 is necessary and not sufficient. AGENTS.md
# defines mergeable as exit 0 with a clean log. The banner prints
# "missing = FAIL, never SKIPPED" on every run, so that phrase is the one
# SKIPPED that does not count; any other is a hole the exit code hid.
judge_the_log() {
  local log="$1" code="$2" clean skipped failed
  (( code == 0 )) || die "merge-gate.sh exited $code. Log: $log"
  clean="$(tr -d '\r' <"$log")"
  skipped="$(grep -n 'SKIPPED' <<<"$clean" | grep -v 'never SKIPPED' || true)"
  [[ -z "$skipped" ]] || die "merge-gate.sh exited 0 with SKIPPED in its log, which is not a pass:
$skipped"
  failed="$(grep -n 'test result: FAILED' <<<"$clean" || true)"
  [[ -z "$failed" ]] || die "merge-gate.sh exited 0 with a failed test run in its log:
$failed"
  GATE_PASSED_LINE="$(grep -m1 'MERGE GATE PASSED' <<<"$clean" | sed 's/║//g; s/^[[:space:]]*//; s/[[:space:]]*$//' || true)"
  [[ -n "$GATE_PASSED_LINE" ]] || die "merge-gate.sh exited 0 without printing MERGE GATE PASSED. Log: $log"
  GATE_KILL_LINE="$(grep -m1 'kill_rate=' <<<"$clean" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' || true)"
  [[ -n "$GATE_KILL_LINE" ]] || die "merge-gate.sh exited 0 and mutants-gate.sh never reported a kill_rate. Log: $log"
}

# The whole message is built from the sha, the clock and two lines the gate
# printed. Nothing from the environment goes in: no path, no URL, no user.
receipt_message() {
  local tag="$1" sha="$2"
  printf 'MemoryIndustry %s\n\n' "$tag"
  printf '%s %s\n' "$RECEIPT_PREFIX" "$sha"
  printf 'local-gate-date: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'local-gate-summary: %s\n' "$GATE_KILL_LINE"
  printf 'local-gate-summary: %s\n' "$GATE_PASSED_LINE"
}

release() {
  local dry_run="$1" tag="$2" sha log code msg
  cd "$ROOT"
  version_of_tag "$tag" >/dev/null
  check_the_tree "$tag"
  check_the_version "$tag"
  sha="$(git rev-parse HEAD)"

  log="$HOME/.cache/cuba-gate/release-$tag.log"
  mkdir -p "$(dirname "$log")"
  echo "=== merge-gate.sh on $sha (log: $log) ==="
  set +e
  "$BASH" "$ROOT/scripts/merge-gate.sh" 2>&1 | tee "$log"
  code="${PIPESTATUS[0]}"
  set -e
  judge_the_log "$log" "$code"

  # The gate takes minutes; the receipt names a commit, so that commit must be
  # the one that was judged and the tracked files must still be its files.
  [[ "$(git rev-parse HEAD)" == "$sha" ]] || die "HEAD moved while the gate ran; the receipt would name a commit nobody judged"
  git diff --quiet HEAD || die "tracked files changed while the gate ran; the receipt would describe a tree that is not $sha"

  msg="$(receipt_message "$tag" "$sha")"
  if [[ "$dry_run" == 1 ]]; then
    echo ""
    echo "dry run: the annotated tag $tag on $sha would carry:"
    echo "----"
    printf '%s\n' "$msg"
    echo "----"
    echo "dry run: no tag created, nothing pushed"
    return 0
  fi

  git tag -a "$tag" --cleanup=verbatim -F - "$sha" <<<"$msg" \
    || die "git tag -a $tag failed"
  if ! git push origin "refs/tags/$tag"; then
    git tag -d "$tag" >/dev/null
    die "pushing $tag failed. The local tag was removed so the next run starts clean"
  fi
  echo "OK  $tag pushed with the local gate's receipt for $sha. publish.yml takes it from here."
}

# What publish.yml runs. A lightweight tag, a tag on another commit, or an
# annotated tag without the exact receipt line for this commit is refused.
verify_receipt() {
  local tag="$1" sha="$2" kind target
  refuse() {
    echo "REFUSING TO PUBLISH: $*" >&2
    echo "$ONLY_WAY" >&2
    exit 1
  }
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || refuse "'$sha' is not a full commit sha"
  kind="$(git cat-file -t "refs/tags/$tag" 2>/dev/null)" || refuse "there is no tag $tag in this clone"
  [[ "$kind" == tag ]] \
    || refuse "$tag is a lightweight tag (it names a $kind directly), so it carries no receipt"
  target="$(git rev-parse "refs/tags/$tag^{commit}")"
  [[ "$target" == "$sha" ]] || refuse "$tag points at $target and this run would publish $sha"
  git cat-file -p "refs/tags/$tag" | sed '1,/^$/d' | tr -d '\r' | grep -qxF "$RECEIPT_PREFIX $sha" \
    || refuse "$tag has no line '$RECEIPT_PREFIX $sha' in its message: nothing says the local gate passed on this commit"
  echo "OK  $tag is annotated and carries the local gate's receipt for $sha"
}

# --- self-test: each guard gets the fixture that has to stop it ---------------
# A throwaway origin (bare) and clone in a temp dir, with this script and a
# stand-in merge-gate.sh committed into it. The stand-in never builds anything:
# FIXTURE_GATE picks what it prints and how it exits, and it leaves a marker so
# the fixtures can tell whether a guard refused before the gate or after it.
# Git runs with a scratch global config, so nobody's signing or hooks settings
# reach the fixture repos.
self_test() {
  local tmp origin work other sha out
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  export HOME="$tmp/home" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$tmp/gitconfig"
  mkdir -p "$HOME"
  : >"$GIT_CONFIG_GLOBAL"
  git config --global user.name fixture
  git config --global user.email fixture@example.invalid
  git config --global init.defaultBranch main
  git config --global core.autocrlf false
  git config --global advice.detachedHead false

  origin="$tmp/origin.git"
  work="$tmp/work"
  git init -q --bare "$origin"
  git init -q "$work"
  mkdir -p "$work/scripts" "$work/rust"
  tr -d '\r' <"$ROOT/scripts/release.sh" >"$work/scripts/release.sh"
  printf '%s\n' '#!/usr/bin/env bash' \
    ': >"$HOME/gate-ran"' \
    'echo "WHAT IT REQUIRES (missing = FAIL, never SKIPPED):"' \
    'case "${FIXTURE_GATE:-green}" in' \
    '  red) echo "test result: FAILED. 1 passed; 1 failed"; exit 101 ;;' \
    '  skipped) echo "SKIPPED: tests that need the NLI model" ;;' \
    '  failed) echo "test result: FAILED. 1 passed; 1 failed" ;;' \
    'esac' \
    '[ "${FIXTURE_GATE:-green}" = nokill ] || echo "mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800"' \
    '[ "${FIXTURE_GATE:-green}" = nobanner ] || echo "║  MERGE GATE PASSED — safe to merge (local CI 100%)       ║"' \
    'exit 0' >"$work/scripts/merge-gate.sh"
  printf '[package]\nname = "fixture"\nversion = "0.27.0"\n\n[dependencies]\nserde = { version = "1.0.0" }\n' \
    >"$work/rust/Cargo.toml"
  git -C "$work" add -A
  git -C "$work" commit -q -m fixture
  git -C "$work" remote add origin "$origin"
  git -C "$work" push -q origin main 2>/dev/null
  git -C "$work" fetch -q origin

  run() { (cd "$work" && "$BASH" scripts/release.sh "$@") >"$tmp/out" 2>&1; }
  no_tag_anywhere() {
    ! git -C "$work" rev-parse -q --verify refs/tags/v0.27.0 >/dev/null \
      || self_fail "$1: a tag was created locally"
    ! git -C "$origin" rev-parse -q --verify refs/tags/v0.27.0 >/dev/null \
      || self_fail "$1: a tag reached origin"
  }
  # A guard of steps 1-2 refuses, says why, creates nothing and never gets as
  # far as the gate.
  refused_before_the_gate() {
    local what="$1" says="$2"
    shift 2
    rm -f "$HOME/gate-ran"
    if run "$@"; then self_fail "$what was released: $(cat "$tmp/out")"; fi
    grep -q "$says" "$tmp/out" || self_fail "$what was refused without saying '$says': $(cat "$tmp/out")"
    [[ ! -e "$HOME/gate-ran" ]] || self_fail "$what reached the gate before being refused"
    no_tag_anywhere "$what"
  }
  refused_after_the_gate() {
    local what="$1" says="$2"
    rm -f "$HOME/gate-ran"
    if FIXTURE_GATE="$3" run v0.27.0; then self_fail "$what was released: $(cat "$tmp/out")"; fi
    grep -q "$says" "$tmp/out" || self_fail "$what was refused without saying '$says': $(cat "$tmp/out")"
    [[ -e "$HOME/gate-ran" ]] || self_fail "$what: the fixture never reached the gate, so it proves nothing"
    no_tag_anywhere "$what"
  }

  # Step 1.
  echo x >"$work/stray.rs"
  refused_before_the_gate "an untracked file" "working tree is not clean" v0.27.0
  rm -f "$work/stray.rs"
  echo "# edited" >>"$work/rust/Cargo.toml"
  refused_before_the_gate "an edited tracked file" "working tree is not clean" v0.27.0
  git -C "$work" checkout -q -- rust/Cargo.toml

  git -C "$work" commit -q --allow-empty -m "not pushed"
  refused_before_the_gate "a HEAD ahead of origin/main" "What gets published has to be what main holds" v0.27.0
  git -C "$work" reset -q --keep origin/main

  # origin moved and this clone has not fetched: its stale origin/main still
  # equals HEAD, so only a guard that fetches can see the difference.
  other="$tmp/other"
  git clone -q "$origin" "$other"
  git -C "$other" commit -q --allow-empty -m "landed on main elsewhere"
  git -C "$other" push -q origin main 2>/dev/null
  [[ "$(git -C "$work" rev-parse HEAD)" == "$(git -C "$work" rev-parse origin/main)" ]] \
    || self_fail "the fixture for a stale origin/main is not stale"
  refused_before_the_gate "a HEAD behind an origin/main it never fetched" "What gets published has to be what main holds" v0.27.0
  git -C "$work" merge -q --ff-only origin/main

  git -C "$work" tag v0.27.0
  rm -f "$HOME/gate-ran"
  if run v0.27.0; then self_fail "a tag that already exists here was released again"; fi
  grep -q "already exists here" "$tmp/out" || self_fail "an existing local tag was refused without saying so: $(cat "$tmp/out")"
  [[ ! -e "$HOME/gate-ran" ]] || self_fail "an existing local tag reached the gate"
  git -C "$work" tag -d v0.27.0 >/dev/null

  git -C "$other" pull -q --ff-only 2>/dev/null
  git -C "$other" tag v0.27.0
  git -C "$other" push -q origin refs/tags/v0.27.0 2>/dev/null
  rm -f "$HOME/gate-ran"
  if run v0.27.0; then self_fail "a tag that already exists on origin was released again"; fi
  grep -q "already exists on origin" "$tmp/out" || self_fail "a tag on origin was refused without saying so: $(cat "$tmp/out")"
  [[ ! -e "$HOME/gate-ran" ]] || self_fail "a tag on origin reached the gate"
  git -C "$origin" tag -d v0.27.0 >/dev/null
  git -C "$work" tag -d v0.27.0 >/dev/null 2>&1 || true

  # Step 2.
  refused_before_the_gate "a tag whose version rust/Cargo.toml does not declare" \
    "the tag says 0.28.0 and rust/Cargo.toml says 0.27.0" v0.28.0
  refused_before_the_gate "a tag without its v" "is not a release tag" 0.27.0

  # Step 3.
  refused_after_the_gate "a gate that exited 101" "merge-gate.sh exited 101" red
  refused_after_the_gate "a gate that exited 0 over a SKIPPED line" "SKIPPED in its log" skipped
  refused_after_the_gate "a gate that exited 0 over a failed test run" "failed test run" failed
  refused_after_the_gate "a gate that never printed its verdict" "without printing MERGE GATE PASSED" nobanner
  refused_after_the_gate "a gate whose mutation step never reported" "never reported a kill_rate" nokill

  sha="$(git -C "$work" rev-parse HEAD)"

  # --dry-run judges everything and creates nothing.
  run --dry-run v0.27.0 || self_fail "a green dry run was refused: $(cat "$tmp/out")"
  grep -qxF "$RECEIPT_PREFIX $sha" "$tmp/out" || self_fail "the dry run did not print the receipt it would write: $(cat "$tmp/out")"
  grep -q "no tag created, nothing pushed" "$tmp/out" || self_fail "the dry run did not say it created nothing"
  no_tag_anywhere "a dry run"

  # The real thing, with a local branch that must not travel with the tag.
  git -C "$work" branch local-only
  run v0.27.0 || self_fail "a green release was refused: $(cat "$tmp/out")"
  [[ "$(git -C "$origin" cat-file -t refs/tags/v0.27.0 2>/dev/null)" == tag ]] \
    || self_fail "origin did not receive an annotated v0.27.0"
  [[ "$(git -C "$origin" for-each-ref --format='%(refname)' refs/heads)" == refs/heads/main ]] \
    || self_fail "the release pushed a branch, not only its tag: $(git -C "$origin" for-each-ref refs/heads)"
  [[ "$(git -C "$origin" rev-parse refs/heads/main)" == "$sha" ]] \
    || self_fail "the release moved origin's main"
  out="$(git -C "$origin" cat-file -p refs/tags/v0.27.0)"
  grep -q "kill_rate=0.900" <<<"$out" || self_fail "the receipt lost the gate's kill_rate line: $out"
  grep -q "^local-gate-date: " <<<"$out" || self_fail "the receipt has no date: $out"
  if grep -qF -e "$tmp" -e "$HOME" -e "$origin" <<<"$out"; then
    self_fail "the tag message carries a local path: $out"
  fi

  # What publish.yml does, in a clone the way actions/checkout leaves one:
  # the tag ref points at the commit (lightweight) until it is fetched again.
  git clone -q "$origin" "$tmp/runner" 2>/dev/null
  git -C "$tmp/runner" update-ref refs/tags/v0.27.0 "$sha"
  if (cd "$tmp/runner" && "$BASH" "$work/scripts/release.sh" --verify-receipt v0.27.0 "$sha") >"$tmp/out" 2>&1; then
    self_fail "a lightweight tag passed as a receipt"
  fi
  grep -q "lightweight" "$tmp/out" || self_fail "a lightweight tag was refused without saying so: $(cat "$tmp/out")"
  grep -qF "$ONLY_WAY" "$tmp/out" || self_fail "the refusal does not name scripts/release.sh as the way to publish"
  git -C "$tmp/runner" fetch -q --no-tags --force origin "refs/tags/v0.27.0:refs/tags/v0.27.0"
  (cd "$tmp/runner" && "$BASH" "$work/scripts/release.sh" --verify-receipt v0.27.0 "$sha") >"$tmp/out" 2>&1 \
    || self_fail "the receipt release.sh wrote is not one --verify-receipt accepts: $(cat "$tmp/out")"

  # And what it must refuse on a runner.
  if (cd "$tmp/runner" && "$BASH" "$work/scripts/release.sh" --verify-receipt v0.27.0 "$(printf '%040d' 0)") >"$tmp/out" 2>&1; then
    self_fail "a receipt was accepted for a commit the tag does not point at"
  fi
  git -C "$work" tag -a v0.0.1 -m "annotated by hand, no gate" "$sha"
  if (cd "$work" && "$BASH" scripts/release.sh --verify-receipt v0.0.1 "$sha") >"$tmp/out" 2>&1; then
    self_fail "an annotated tag without a receipt passed"
  fi
  git -C "$work" tag -a v0.0.2 -m "$RECEIPT_PREFIX $(printf '%040d' 1)" "$sha"
  if (cd "$work" && "$BASH" scripts/release.sh --verify-receipt v0.0.2 "$sha") >"$tmp/out" 2>&1; then
    self_fail "a receipt for another commit passed"
  fi
  # A receipt line copied by hand into a tag on some other commit.
  git -C "$work" tag -a v0.0.3 -m "$RECEIPT_PREFIX $sha" "$sha~1"
  if (cd "$work" && "$BASH" scripts/release.sh --verify-receipt v0.0.3 "$sha") >"$tmp/out" 2>&1; then
    self_fail "a tag on another commit passed on a receipt line copied into it"
  fi

  echo "OK  self-test: a dirty tree, a HEAD ahead of or behind origin/main, a tag taken"
  echo "    here or on origin and a version Cargo.toml does not declare are refused before"
  echo "    the gate; a red gate and an exit 0 over SKIPPED, a failed run or a missing"
  echo "    verdict are refused after it; nothing is tagged in any of them. The receipt a"
  echo "    green run writes is the one --verify-receipt accepts once the annotated tag is"
  echo "    fetched, and it refuses the lightweight tag checkout leaves, another commit and"
  echo "    an annotated tag without the line"
}

case "${1:-}" in
  --self-test) self_test ;;
  --verify-receipt)
    [[ $# -eq 3 ]] || usage
    verify_receipt "$2" "$3"
    ;;
  --dry-run)
    [[ $# -eq 2 ]] || usage
    release 1 "$2"
    ;;
  ""|-h|--help) usage ;;
  *)
    [[ $# -eq 1 ]] || usage
    release 0 "$1"
    ;;
esac
