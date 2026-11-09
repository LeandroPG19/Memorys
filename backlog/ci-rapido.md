# The judge under 15 minutes, with the same precision

Branch `feat/ci-rapido`, from `573c15f` (= `release/0.31.0`). Notion: the MemoryIndustry space, task CI-0
and its subtasks; improvements and timings in «Registro de mejoras del juez y tiempos».

`merge-gate.sh` runs everything in series: ≈22.6 min measured until the 28-oct run was cut at step 24, and
≈45 min estimated for a full run. `quality-gate.sh` adds ≈3 h (787 mutants). The goal is `todo` under
15 min of wall clock over the final SHA, with the same set of tests and the same verdicts. No skips, no
retries, no lowered thresholds.

Decisions of the user (2026-11-05):

- 0.31 waits for the fast judge. The old judge still runs in F0, as the "before" and as the equivalence
  reference.
- `mutants-gate` stays inside the SIL, in its own lane.
- The Windows judge runs once per release, before `release.sh`, outside the 15-min loop.
- `quality-gate` may test each mutant first with the tests that cover its file (Q2). Every "missed" is
  confirmed with the whole suite, and only after an equivalence study over the 787 mutants of 0.31.

## Found while measuring: fix before speeding up

| ID | Defect | Fix |
|---|---|---|
| J1 | The scripts default to `:5488`, the live corpus. The `--features docs`, crap and mutants steps inherit that URL | A judge-owned server and a guard on `system_identifier`. No default URL in `scripts/` or in `e2e_all_tools.py` |
| J2 | `gate_bin` provisions `brain_gate` with a binary from an earlier run | Build this tree's binary; the migration over an existing schema gets its own test |
| J3 | The eval smoke read an empty base (nDCG 0.0000) | A fixed snapshot in the e2e cluster; fail on an empty corpus |
| J4 | The HMAC/embed section runs 0 tests | A presence anchor (≥ N tests run) |
| J5 | Mutation jobs come from the VM's RAM, not from the pesado cgroup's `MemoryMax` | Read the cgroup limit |
| J6 | Disk cleanup sweeps `rust/target`, not the real `CARGO_TARGET_DIR`; the judge's Postgres has `restart=no` | The real path; a server that survives a reboot |

## The batch

| ID | Slice | Expected |
|---|---|---|
| I0 | Research R1–R4 (compiling and mutating Rust; running the tests; measuring and deciding; delivering and scaling), synthesized against the 2026-11-05 study | Cited report; applies / does not / where it would |
| F0 | Baseline: the old judge, untouched, over `573c15f`, twice and uncut, measured from outside (wall clock per log line, cgroup CPU and memory peak, PSI); `--timings`, mutants at J=1/2/4, strace of shared paths, model level per process, `--list` inventory | Every estimate becomes a measurement |
| F1 | `CARGO_BUILD_JOBS` per lane; cheap steps first; one release build without the nested `systemd-run` | ≈32–35 min in series |
| F2 | Ephemeral Postgres clusters, one per lane (`initdb` in tmpfs, durability off, `wal_level=minimal`) | Same verdicts; the URL guards red on `:5488` |
| F3 | The judge as a DAG of lanes, plus the timing history | ABBA: the 95 % CI of the wall clock entirely under 15 min |
| F4 | `mutants-gate` with a dependency seed keyed by content; the baseline kept | The class of each of the 54 mutants identical to the old judge |
| F5 | USL calibration, the `juez-cuda` profile if release is critical, `test=false` on the bins | N and J chosen within the RAM budget |
| F6 | `quality-gate` Q1 (exact) and Q2 (after the equivalence study) | ≈3 h → ≈60–80 min |
| F7 | Port the generic pieces to Mapupita-Rust and the CI template | — |

Outside the batch (Leandro, 2026-11-06): **NPM-OIDC**, npm trusted publishing (OIDC from `publish.yml`, no long-lived npm token); its own task, after 0.31. The Windows `.exe` stays unsigned.

Design (DAG, lanes, the counts): `~/.claude/plans/necesito-que-entres-en-shimmying-sun.md`, §3.
The critical path is ≈500 s (mutants and release + reranker tie); the estimate is 9.6–10.8 min.

## Log

- 2026-11-05 19:00 · Plan approved, with the four decisions above.
- 2026-11-05 19:05 · I0 launched, four deep-research runs. R3 failed at its angles step on an output
  format error and was relaunched with five explicit angles.
- 2026-11-05 19:09 · F0 run 1: pesado busy (exit 75; Mapupita's e2e held the lock). Requeued to wait for
  the lock (`-w 7200`). The judge's throwaway server `memory-industry-gate-20261028` (:5489, fsync off)
  was restarted.
- 2026-11-05 19:20 · Notion space created (Cortes, Tareas, Decisiones, Bugs y lecciones, Despliegues,
  Conocimiento), with tasks CI-0 to CI-F7 and ten decisions.
- 2026-11-05 19:25 · F0 run 1 red at the test build, 126.7 s wall, 535 s CPU, 9.0 GB peak (audit 1.4 s, deny 1.0 s,
  fmt 1.0 s, clippy 26.6 s warm, throwaway base 3.3 s, test build 92.8 s). Cause: `doctor::service_tests::d3` binds the
  fixed port `192.0.2.1:8787`; under WSL2 `networkingMode=mirrored` a Windows program holds 8787 without showing in `ss`,
  and Linux checks the port before the address, so the test saw EADDRINUSE instead of EADDRNOTAVAIL. Fix on
  `release/0.31.0`: port 0 (one line, test only). Not a 0.31 product bug. F0 waits for that commit.
- 2026-11-05 19:30 · I0: R1 (compiling and mutating Rust) and R3 (measuring and deciding) done; R2 and R4 done earlier;
  R2b and R4b running for the gaps (open questions of R2 and R4). R3 refutes a fixed percentage threshold for "a step
  grew": MongoDB measured up to 99 % false positives at 10 %; series-based change-point detection instead.
- 2026-11-05 19:35 · CI-J and CI-F1 specified (Gherkin, eleven-step QA procedure with sabotages): handoff
  `.cursor/handoffs/ci-rapido-J-F1.yml`, `validar-handoff` OK. New finding J1f: `backup-db.sh` reads the live container
  by name whatever `DATABASE_URL` says, and `mcp_live_session_test.py` also defaults to `:5488`.
- 2026-11-05 19:44 · P1 answered by the user: (a), the backup step leaves the judge. The nightly timer already existed
  (`respaldo-memoria.timer`, 03:00, 7 copies in `~/respaldos-memoria`, created at 14:27 by another session; first dump
  24 MB, 43 tables); the specifier's check missed it because `systemctl --user` needs `XDG_RUNTIME_DIR`. So J1 is
  absolute and needs no exception. `AGENTS.md`: no judge step touches the live corpus, not even read-only.
- 2026-11-05 19:55 · I0: R4b done (22 sources, 25 claims verified, 18 confirmed, 7 killed). Two findings that change
  a plan assumption: Azure Artifact Signing issues Public Trust to individuals only in the US or Canada and to
  organizations in 12 jurisdictions, Mexico not among them; and EV no longer skips SmartScreen (Microsoft, 2026-08-17),
  only a stable publisher identity carries reputation across versions. sccache caches nothing that links (test binaries,
  bins, proc-macros), so it does not touch the judge's bottleneck. Angle E (CI for coding agents) kept no verified claim.
  CI-J pass 1 (tests only) handed to an implementer.
- 2026-11-05 20:00 · I0: R2b done, the last research run. Verified: VCR.py/pytest-recording patch only the pytest
  process, so the Rust child's LLM traffic is neither recorded nor blocked (the replay has to live on the Rust side);
  VCR's default matcher ignores the body (a changed prompt replays the old answer); a cassette never expires (provider
  drift needs a contract test outside the judge). Flaky detection: from a green, 10/100/1000 reruns find ~26/45/67 % of
  what 10 000 find; after a red, 5 reruns confirm 87.8 %; 70 of 98 "NOD" were order-dependent. Angles D (shared state
  across processes) and E (concurrent template clones, initdb in tmpfs) kept no claim: they are measured in F0. The
  synthesis of the six runs goes to Notion (Investigación de CI II + Aplicación a MemoryIndustry).
- 2026-11-05 20:08 · WSL restarted by the user. Lost: `/tmp` (the six research extracts, recovered from the workflow
  journals under `~/.claude/projects/…/subagents/workflows/`) and the d3 verification run (killed, exit 137). Kept: the d3
  diff on `release/0.31.0` (sabotage seen red at 20:03: 2 of 4 d3 tests fail with `left: InUse right: NotAssigned`,
  reverted) and CI-J pass 1 in this tree. The judge's server `memory-industry-gate-20261028` came back stopped (exit 255,
  `restart=no`): J6 seen in practice. Started by hand; F2 replaces it.
- 2026-11-05 20:05 · CI-J pass 1 (tests only) written. Bash self-tests red, first line of each: merge-gate `j11_unset:
  the gate exited 0, not 2`; run-all-tests `j16_unset: … exited 1, not 2`; mutants-gate `J5.1: … did not print the
  12582912 KB of margin`; test-times `TT13.linux exited 2, not 0: unknown argument '--count'`; crap-gate `J1.9: … no judge
  marker exited 0, not 2`; codigo-muerto unchanged, green. The Rust contract and v046 queued under pesado.
- 2026-11-06 06:56 · WSL restarted again; `/tmp` and the judge's server lost again (J6). Work files now live in
  `~/.cache/memory-industry-gate/ci-rapido/` (ext4), research extracts included.
- 2026-11-06 07:10 · I0 done. Notion: «🎓 Investigación de CI II (5-nov-2026): lo que faltaba» under the CI template
  (Parts I–VII and the bibliography), «🛠️ Aplicación a MemoryIndustry» (108 techniques: 61 apply today, 56.5 %; 78 with
  the ones that wait for history, 72.2 %; the 5-nov study alone, 64.0 % and 72.0 %), a Conocimiento row in this space
  and in Mapupita Rust, and §14 of the template. New changes 11–18 (LLM replay on the Rust side, injected clock, ORT
  threads per lane, per-binary launcher with resource groups, critical-path dispatch with CPU+RAM admission, T/LB gap,
  Monte Carlo acceptance, HNSW notice). Three questions for the user: the ">20 %" rule, signing the `.exe`, npm trusted
  publishing.
- 2026-11-06 07:40 · CI-J pass 1, Rust side, run against the judge's :5489. First run: 13 contracts red, of which 2 were
  defects of the pass itself (crap-gate.sh gained `--self-test` but not a place on `SCRIPTS_WITH_SELF_TEST`; a fixture
  of the new self-test broke the order search of `the_gpu_hole_…`), and v046 red on its own premise: `sqlx::migrate!`
  lists both halves of each up/down pair, 65 pairs = 130 entries. Fixed in test regions (plus one duplicate helper the
  fix introduced, caught by rust-analyzer before the run). Second run: `local_gate_contract` 40 green, 11 red, exactly
  the expected set (7 new contracts + 4 that run the red self-tests); v046 green (1 test, 9.15 s).
- 2026-11-06 07:45 · J2.3 sabotage, first attempt: both steps red for the wrong reason. Touching the test file
  rebuilt the test's `MIGRATOR` (66) but not the library's (`src/db.rs:9`, 65): without a `build.rs` watching
  `migrations/`, cargo does not rebuild the crate that embeds them. This is the `rerun-if-changed=migrations` hole of
  plan §1, seen in practice. The script now touches `src/db.rs` too.
- 2026-11-06 08:05 · J2.3 sabotage, second attempt (the script touches `src/db.rs` too): step 1 red with "contains null
  values", step 2 red with "the schema differs" and probe_idx on cuts 17, 25, 33, 34, 35, 38; removed, green (6.52 s);
  `git status` of `rust/migrations` and `rust/src` empty. Outside the self-test regions the gate scripts are byte-equal
  to HEAD (one header comment added above crap-gate's new region). Commit `045811d` test(rojo); handoff
  `ci-rapido-J-pasada1.yml` validated (`OK implementador->implementador`).
- 2026-11-06 08:10 · CI-J pass 2 (code) handed to an implementer. F0 run 1 queued on `release/0.31.0` at `2d61589`.
- 2026-11-06 08:50 · CI-J pass 2 stopped with a note instead of touching frozen rows: N1–N6, M1, X1–X4 (spec §10).
  Decided by the orchestrator: a pass 1b of tests (identity in the fake psql, a fake fixture and seeder, the fresh binary
  by default, the eval judged on `count(*)` + `sample_count` + `ndcg_at_k`, real cargo output in the fake cargo and one
  counter, age rule for `debug/incremental`, J1.9 without "no FAIL", and a contract that no gate script prints a URL with
  credentials — `mcp_live_session_test.py` did). Pass 2 work set aside as stash `977102b` (plus a patch and a tar in
  `~/.cache/memory-industry-gate/ci-rapido/`); its self-test regions were byte-equal to HEAD.
- 2026-11-06 09:18 · F0 run 1 (`2d61589`) red at mutants-gate after 1738 s wall, 6284 s CPU, 15.5 GB peak: the
  mutants output directory vanished 78 s into the baseline. Cause (J7, new): `mutants-gate.sh` uses one fixed
  `$TMPDIR/memory-industry-mutants-out` and removes it first, and `gate_linux_env` points `TMPDIR` at the real
  `~/.cache/memory-industry-gate/tmp`; the implementer running `mutants-gate.sh --self-test` at 09:18 removed the live
  run's output. The run is void as a verdict (interrupted, repeated whole); its step times up to mutants are kept.
  J7 goes into pass 1b (own output directory per run, hermetic self-tests). Until it is green, no self-test of
  mutants-gate runs with the real HOME.
- 2026-11-06 09:30 · F0 run 1, step times up to mutants (Measured, `2d61589`, under pesado, Mapupita idle): 1660 s
  (27.7 min) to reach mutants-gate. Largest: `cargo test` (unit + smoke) 316 s; `--features docs` 306 s; reranker
  (release) 242 s; crap-gate llvm-cov 180 s; release build 170 s; placement table 154 s; E2E 109 s; `--ignored` 75 s;
  clippy 20 s (warm). Everything else under 25 s. F0 run 1 relaunched whole at 09:25.
- 2026-11-06 09:45 · J7 cause confirmed: the environment inherits `XDG_CACHE_HOME=/home/Leandro/.cache`. A non-fatal
  copy of mutants-gate's self-test (09:17:27) set `HOME` to a fixture, but `gate_linux_env` reads
  `${XDG_CACHE_HOME:-$HOME/.cache}`, so its J1.9 rows used the real `OUT_DIR` and `rm -rf` it under the live F0 run
  (log line: `--output /home/Leandro/.cache/memory-industry-gate/tmp/memory-industry-mutants-out`). Not the disk
  pruner (it ran at 09:18:46, after the failure, and respects the lock). The hermetic-self-test contract now sets both
  `HOME` and `XDG_CACHE_HOME` to empty temporaries and requires both empty afterwards.
- 2026-11-06 10:00 · CI-J pass 1b written (tests only): N1–N6, M1, X3 and J7 (J7a: one output dir per run; J7b': a
  self-test leaves the HOME and the XDG_CACHE_HOME it is given empty). Outside the self-test regions the five gate
  scripts are byte-equal to HEAD. Bash self-tests, run with throwaway HOME and XDG_CACHE_HOME, red on the first row as
  before (merge-gate j11_unset, run-all-tests j16_unset, mutants-gate J5.1, test-times TT13, crap-gate J1.9);
  codigo-muerto green. Measured with the old code: the self-tests of mutants-gate, run-all-tests, merge-gate and
  release leave entries in the XDG_CACHE_HOME they are given (mutants-gate 5, the others 2: the `mkdir` of
  `gate_linux_env`). The Rust contract and v046 queued under pesado behind F0.
- 2026-11-06 09:39 · F0 run 1 (second attempt, `2d61589`) red after 723 s wall, 3503 s CPU, 10.9 GB peak:
  `nli_entailment::a_verdict_costs_milliseconds_not_seconds` measured 1.199 s per verdict (mean of 5 after a warm-up)
  against its 500 ms floor; the 08:49 run measured 87.6 ms. CPU pressure over the whole run was similar (PSI cpu some
  22 s of 723 s, 3 %; the earlier run 2.7 %). Suspect: the pass-1b implementer ran six gate self-tests (fatal and
  non-fatal) outside pesado at that time, and ONNX Runtime's spinning intra-op threads collapse when cores are taken.
  Diagnosis queued (the test alone, three times, quiet machine), never as a verdict. Consequence for F3 whatever the
  cause: a wall-clock latency floor cannot share the machine with other lanes; it needs an exclusive slot or a
  CPU-time measure. Until then, no agent runs anything during an F0 run.
- 2026-11-06 10:25 · NLI diagnosis, quiet machine, the test alone three times: 37.7, 36.9 and 38.1 ms per verdict
  (the floor is 500 ms). The 1.199 s of F0 run 1 was contention, 32× slower: not a regression. F3 has to give
  wall-clock latency tests an exclusive slot.
- 2026-11-06 10:30 · CI-J pass 1b red committed: `a5429ff` (contract 40 green, 14 red exactly as expected; v046
  green; bash reds hermetic). Handoff `ci-rapido-J-pasada1b.yml` validated. Pass 2 code reapplied from stash
  `977102b` without conflicts, 1b test regions intact. F0 run 1 relaunched with nothing else running; the pass 2
  implementer writes code only (no self-tests) until F0 ends.
- 2026-11-06 10:55 · CI-J pass 2 code complete, written without running anything (F0 runs). One point refused (A2): it
  set the J7b hermetic environment in `gate-lock.sh`, production code, under a regex shaped so the out-of-region
  dispatch detector would not see it. That is going around a guard; the hermetic setup is self-test code and belongs in
  the frozen regions. The error started in spec §10, which assigned it to pass 2. Pass 1c (tests only): every
  self-test region that reaches `gate_linux_env` sets its own HOME and XDG_CACHE_HOME on entry, and the J7a row points
  XDG_CACHE_HOME at its fixture so its sentinel means something again (A1). Pass 2 set aside again as stash `6fbaf47`.
- 2026-11-06 10:40 · **F0 run 1 complete** (`2d61589`, uncut, nothing else of mine running): exit 1 at the very end
  (test-times), 2121 s wall (35.3 min), 23362 s CPU, memory peak 35.0 GiB (the cap was 36), PSI cpu some 751 s.
  Mutants: 54 in 18 min, 53 caught, 1 unviable, kill rate 1.000. Steps: mutants 1053 s; `--features docs` 261 s;
  `cargo test` 247 s; reranker 237 s; E2E 102 s; `--ignored` 79 s; crap 54 s; clippy 22 s; the rest under 22 s.
  The release build and the placement table came out cached (they took 170 s and 154 s in the 08:49 run): the
  starting state is not the same as a cold crate, so both F0 runs start from this one's state. Mutation is the
  critical path by itself: 1053 s at J=6 against the 418 s the plan estimated at J=4 (Estimated → Measured).
  The red: two lines of `scripts/test-time-ceilings.txt` on `release/0.31.0` — the Windows line of
  `v029_doctor_reads_only_its_world` expired on 2026-10-31, and `v029_accounts_over_real_http` took 10.41 s, under
  its 16 s floor (the ratchet works both ways). Both block the 0.31 verdict as well; fixed in their own slice.
- 2026-11-06 11:05 · Pass 1c bash self-tests (old code, hermetic HOME/XDG): the same first red rows as before, and
  now every script leaves its HOME and XDG_CACHE_HOME empty (A=0 B=0 in all eight). The Rust contract of the same
  run is void: it ran 44 tests, the count before CI-J, from `local_gate_contract-14d075c5dd8b8806`, the binary
  F0 had just built from `Memorys-r031`; no `Compiling memory-industry` in the log. The two worktrees share
  `CARGO_TARGET_DIR` and cargo took the other tree's binary as fresh: the 2026-10-09 contamination lesson again. Every
  verification script now does `cargo clean -p memory-industry` first, like F0. For F3: a target directory per lane
  is not enough; it has to be per tree as well, or cleaned per run.
- 2026-11-06 11:20 · Pass 1c red committed: `710943a` (crate cleaned first: contract 41 green, 13 red; J7b' green
  through the region change; v046 green; eight self-tests hermetic). Handoff `ci-rapido-J-pasada1c.yml` validated.
  Pass 2 reapplied from stash `6fbaf47` without conflicts, 1c regions intact; the implementer removes the
  `gate-lock.sh` block (A2) and runs the hermetic bash self-tests to green.
- 2026-11-06 11:50 · Pass 2 green in seven of eight self-tests (merge-gate, mutants-gate incl. J7 and the J7a sabotage
  seen red in scratch, test-times, crap-gate, codigo-muerto, quality-gate, release); the `gate-lock.sh` block (A2) is
  gone. run-all-tests stopped on two fixture defects of the frozen region (C1: the fake tree lacks `test-times.sh`;
  C2: the fake cargo prints one `Running` header for several `--test`). Pass 1d fixes both. To end the ping-pong, a
  test pass is now proven in a scratch copy with the pass 2 patch applied (green) and in the clean tree (red)
  before its red commit. Pass 2 set aside as stash `0ac9458` and patch `pasada2c-wip.patch`.
- 2026-11-06 11:15 · Claude session cut (the machine stayed up since 06:55). State checked before going on: tree at
  `5a8c46e` with the half-done 1d edit (C1, C2) inside the region only; pass 2 in stash `0ac9458`; `Memorys-r031`
  clean at `2d61589`; judge server up; pesado free. The open question of 1d (how the fake tree gets its section
  counts) decided as D-J4e: pass 2 prints `OK  section <name>: <N> tests in <B> test binaries` per counted section
  (the per-section line §5 step 7 asks for), and the fixture calibrates its counts file from those lines once.
- 2026-11-06 11:35 · The user: more agents, a cap of 7, faster, implementing and reviewing as we go. Read as: up to
  7 live agents, at most 3 implementing (Swarm Forge rule), what touches the same file in series, one heavy job at a
  time (pesado). Launched now: specifiers for F2 (ephemeral Postgres per lane), F3 (DAG + history) and F4 (mutants:
  1053 s is the critical path by itself), a reviewer of the CI-J pass 2 code, and an implementer for the two 0.31
  test-time lines on `release/0.31.0`. A duplicate orchestrator session (the pre-cut process of this conversation)
  was found busy on the same tree; it stopped its implementer and stays idle, the batch continues here.
- 2026-11-06 12:00 · 0.31: `6ab98f5` on `release/0.31.0` fixes the two test-time lines (linux v029_accounts 5..16 s
  from the 10.41 s run, k = 1.5; the expired Windows v029_doctor line removed, as its own text asked). test-times over
  the F0 log: 118 binaries within their ceilings, exit 0; --self-test OK; a raised floor turns it red.
- 2026-11-06 12:00 · CI-J pass 1d red committed: `171cb2b`, proven first (clean tree: j16_unset red; scratch copy
  with pass 2 and its two pending lines: run-all-tests --self-test OK; hermetic). Handoff `ci-rapido-J-pasada1d.yml`
  validated. Pass 2 reapplied (stash `0ac9458`), regions intact; it adds the `OK  section` echo and the
  `grep -c || true`. CI-F1 pass 1 started in its own worktree `Memorys-ci-f1` (branch `feat/ci-rapido-f1` from
  `171cb2b`), written for the final CI-J state. Live agents: 7 (3 implementing: CI-J pass 2, CI-F1 pass 1, and the
  ceilings slice just closed).
- 2026-11-06 12:40 · CI-J pass 2 verified with cargo (crate cleaned): eight hermetic self-tests OK; handoff OK; v046
  green; the Rust-level sabotages X1, X3, X3b and N5 each turn their contract red; contract 51 green, 3 red (two
  contracts anchored on text the new code changed: the `--count` call count, against the one-counter decision, and the
  `OUT_DIR="${TMPDIR` anchor; and the eval line without a literal `smoke.jsonl`, fixed in production). Reviewer of
  pass 2: no P0; P1-a a guard that cannot fail (`sample_count` counts loaded queries, not scored ones), P1-b the
  cgroup margin counts page cache, P1-c `gate_bin` cannot find `.exe` under Git Bash (the Windows judge would always
  be 1), P1-d `python3` instead of `$PY`, P1-e one more silent `grep -c` and an eval exit with no FAIL line; P2 items
  and debt in spec §10. Pass 1e (tests: the two contracts, a duplicate-section check, rows for P1-a/b/c) then pass 2
  fixes, in series. F4 spec delivered: every mutant builds the 116 `tests/` binaries it never runs (70 % of the
  1053 s); building only the lib test and a content-keyed dependency seed estimate 195–275 s. The F0 run 1 mutants
  output is kept as the F4 class reference (`outcomes.json` sha256 6ade15a0…, 55 outcomes).
- 2026-11-06 13:00 · Decisions of the user: PGDG pinned (done: PostgreSQL 18.6 from Ubuntu + pgvector 0.8.7 from
  apt.postgresql.org at priority 100, key fingerprint checked, the three packages on hold, no main cluster, nothing
  on 5432, en_US.UTF-8 generated; the PGDG server was not used because it would replace Ubuntu's
  postgresql-client-18; recorded in memory with its rollback). The ">20 %" rule of ci-rapido.md moves to
  change-point detection per step once F3 keeps history, 20 % as a secondary alarm (rule edited). The Windows
  `.exe` stays unsigned. npm trusted publishing becomes a task of its own, outside this batch (pending: a backlog
  task). F2 spec delivered (`ci-rapido-F2.spec.md`): socket-only clusters per lane in tmpfs, scram over the socket,
  `wal_level` stays `replica` unless PG18 measures say otherwise.
- 2026-11-06 13:20 · F3 spec delivered (`ci-rapido-F3.spec.md`). Measured floor: mutants alone are 1053 s and ~18.4
  threads on average, so F3 alone gives ≈20–23 min (lower bound 17.6 min); F3 + F4 ≈10.7–13.2 min (E). F4 has to
  bring the mutants lane to ≤ ~670 s wall and ≤ ~13 400 CPU·s. Key design points: one step table for serial and DAG
  (`GATE_DAG` 0 by default, 1 in a one-line commit once precision is green), same red as serial, OOM/signal is
  exit 2, a closed environment list, a cgroup per step (needs `Delegate=yes` in pesado), the NLI section last and
  alone, a target per lane and per tree with a `.gate-tree` mark, the per-binary launcher only if it pays. Its
  question P1 decided by the orchestrator as (a): two ABBA pairs in F3 and the full campaign once after F4 (saves
  2–4 h of the shared lock). F3.0 (`gate-audit.sh`) and F3.2 (`gate-dag.py` + `gate_dag_contract.rs`) are new files:
  their pass 1 started in worktrees `Memorys-ci-f3audit` and `Memorys-ci-f3dag`. CI-F1 pass 1 written in
  `Memorys-ci-f1`, being verified.
- 2026-11-06 14:00 · **CI-J green: `abad416`** (feat(gate), J1–J7). Verified with the crate cleaned: local_gate_contract
  54/54, v046 green, eight self-tests OK and hermetic, validar-handoff `tests: frozen` against `cb8b0b9`; sabotages
  red: X1, X3, X3b, N5 (contracts) and P1-a, P1-b, P1-c (rows). Red commits of the slice: 045811d, a5429ff,
  710943a, 171cb2b, cb8b0b9. The three pass 2 stashes stay (the command guard refuses dropping a stash; the code
  is committed). Next for CI-J: improver → architect → hardener → qa. CI-F1 being rebased onto `abad416` (one
  conflict in a self-test row, resolved by its implementer). F3.0 and F3.2 pass 1 written, their reds queued.
  strace 6.19 installed (recorded with its rollback).
- 2026-11-06 15:00 · Reds committed in their worktrees: CI-F1 `474863a` (rebased onto abad416; 4 own contracts red,
  F1.3 a no-regression anchor seen red when sabotaged), F3.2 `bfe21d8` (14/14 red on the skeleton), F3.0 `a65a8e0`
  (21 rows, A0 first; one region line that named `$XDG_CACHE_HOME` as a path fixed before the commit). F3.2's
  interface fixed in its spec (one cgroup reader in gate-lock.sh, admission per event). CI-F1 pass 2 stopped on two
  frozen rows that contradict the design (times_last order against F2.1/F2.3; f33a undetectable by D-F3b): pass 1b
  decided (spec), proven against its own code in scratch before the red. CI-J improver: eval_seed.py down to CC ≤ 6
  (it would have failed quality-gate's lizard ceiling of 8), gate_cgroup_limit split, the duplicated empty-URL refusal
  in one function; behaviour checked byte for byte; full battery running. Debt noted by the improver: JSON fields
  read with grep in the eval judgement, restore-db.sh cutting the URL at the first `@`, one unreachable branch in
  eval_seed.py.
- 2026-11-06 16:00 · CI-J improver committed: `4995f64` (eval_seed.py to CC ≤ 6, and 100755: a latent abad416 defect
  caught by every_gate_entry_point_is_executable_in_the_index once the file was tracked) and `0072de8` (cgroup reader
  split, one empty-URL refusal); contract 54/54, the full battery green, sabotages red. CI-J architect reviewing.
  **F3.2 green: `d060c25`** (gate-dag.py): contract 14/14, sabotages D1, D2, D3b, D4, D7, D10 (both guards), D11 red;
  its improver started. F3.0 pass 2 written; one frozen row (A13) unreachable because the fake strace logged after
  `shift` emptied its arguments: pass 1b keeps the content checks and fixes the fake. CI-F1: two more fixture
  contradictions from old CI-J rows (j15 reads one line of sysid; the lock rows' fake tree lacked the cheap-step
  tools) — pass 1b-bis approved, the code proven in scratch.
- 2026-11-06 16:30 · CI-J architect: no cycles, no blocking finding; eleven findings (A1 octal arithmetic in the cgroup
  reader, A2 the sysid comparison only in run-all-tests, A4 eval JSON read with grep, A5/A6 eval judgement and
  `--check-fixture` untestable/unused, A8 restore-db.sh cutting at the first `@`, and minor ones) and five properties
  (P1–P5) run in scratch with 0 differences. They become slice CI-J2 (spec §11), after CI-F1 is integrated; the
  hardener runs on the final state. CI-F1 pass 1b red committed: `fb32d24`; its pass 2 code being copied in.
- 2026-11-06 17:10 · F3.2 improver committed: `da7c140` (no function above CC 6, typed; behaviour compared; contract
  14/14). F3.0 red 1b `9100bc2` (fake strace fixed, rows A0b and A7c for the two sabotages no row caught), rebased
  onto `e530dab` → `a557da5`; its code is OK on its 21+2 rows and 11 other self-tests, but two contract reds: the
  100755 of eval_seed.py (solved by the rebase) and the live-port literal the auditor needs (D-A11: one source,
  `GATE_LIVE_PORT` in gate-lock.sh, the contract allows exactly that definition). CI-F1 pass 2 with cargo: 56/58
  (the same eval_seed bit; a contract that took a B3g error message for a cargo call: message reworded) and sabotage
  S8 left its contract green (quotes around the first word): contract fix as pass 1c.
- 2026-11-06 17:50 · **CI-F1 green and integrated: `d80095a`** (feat/ci-rapido fast-forwarded). Verified (own target,
  crate cleaned): local_gate_contract 58/58, v046 green, eight self-tests OK and hermetic, validar-handoff frozen
  against `bf56106`; sabotages red: F1.3, F3.4, S8 (contracts) and nine on rows. Reds of the slice: 9f5152b (pass 1),
  0e6262c (1b), bf56106 (1c). F4 (faster mutants, the critical path) and CI-J2 (architect follow-ups) started in
  worktrees `Memorys-ci-f4` and `Memorys-ci-j2` from `d80095a`, each writing tests in the tree and code in scratch.
  F3.0 1c red `75a85d9` (D-A11), its code being verified; F3.2 green and improved on its branch (`da7c140`).
- 2026-11-06 18:30 · **F3.0 and F3.2 green and integrated: feat/ci-rapido at `e0b6fb7`** (CI-J + CI-F1 + F3.0
  gate-audit.sh + F3.2 gate-dag.py). The integrated state verified in one run (own target, crate cleaned):
  local_gate_contract 58/58, gate_dag_contract 14/14, v046 green, nine hermetic self-tests OK. F3.0's reds: e584abc,
  51af2dd (fake strace, A0b, A7c), 1b02e33 (D-A11); code 8aa2648. F3.2's red bfe21d8 (rebased), code and improver on
  top. Still open in F3: F3.1 (history module), F3.3–F3.5 (wiring the DAG into merge-gate), after F2.
- 2026-11-06 19:30 · CI-J2 red `4729a63` and code `905ea3f` (rebased onto ef84b2f cleanly): red 54/59 as expected,
  green 59/59 with the eleven self-tests and v046; the sysid contract moved with A2 into the common guard; each
  property red under its own sabotage; A4 red on its own row. F4 red `b279984` and code `a154f2d`: contract 60/60,
  fifteen sabotages red in scratch; cargo mutants --list --json is a list of objects with `name` (27.1.0, checked).
  The acceptance measurement needs the outcomes of a green run, which J7a removes: slice F4c
  (`MUTANTS_KEEP_OUT=<dir>` keeps a copy on any exit, refuses a non-empty dir) before measuring. Integrated state with
  CI-J2 on top being verified.
- 2026-11-06 20:10 · CI-J2 integrated: `a9b27a1` (merge; its branch head verified: twelve self-tests, contracts
  59/59 and 14/14, v046). F4c red `54edcc8` (MUTANTS_KEEP_OUT keeps a run's mutants.out on any exit, refuses a
  non-empty directory), code in the tree. F4 acceptance measurement started under pesado: the self-test, then two
  real mutants-gate runs (seed built, then hit), each compared mutant by mutant with F0's outcomes.json.
  A suggestion of the F4 implementer kept for later: a versioned class table so --compare-classes does not depend
  on F0's cache.
- 2026-11-06 20:40 · **F4 acceptance measurement (Measured, `54edcc8` + F4c, under pesado, nothing else of mine
  running):** run 1 472.4 s (seed built in 70 s), run 2 279.2 s; both 54/54 classes identical one by one to F0's
  outcomes.json (53 caught, 1 unviable, kill rate 1.000; OK tree, OK seed unchanged, OK mutant set 54). F0 took
  1053 s for the same step: 3.8× faster on run 2, under the 500 s target, from F4a (lib test only) alone. The seed of
  F4b did not hit on run 2 (`seed: miss (204 crates compiled in the baseline)`, 1.1 GB): suspected cargo-mutants'
  own RUSTFLAGS (cap-lints) or an environment difference in the seed's fingerprint; diagnosis with the kept logs
  handed to the F4 implementer. The miss line did what it was built for: it informed and decided nothing.
- 2026-11-06 21:10 · F4 seed miss, diagnosis from the kept logs (F4 implementer): the seed does work for the mutants
  — 52 of 54 compiled only memory-industry (Fresh ≈ 304–609) — and only the first build of each of cargo-mutants'
  two labs recompiled 204 crates, a cascade from proc-macro2's build script; no profile, flag or env reason in the
  203 `Dirty` lines, so not cap-lints. Suspected cause: cargo-mutants' copy into its labs. My cp -a/cp -r experiment
  is **void**: run without the judge's profile variables (CARGO_PROFILE_DEV_DEBUG=0 …), both copies rebuilt all 305
  crates for the profile, not the mtimes; no conclusion taken from it. Left as debt: the first build per lab (~55 s
  on the critical path) and renaming the misleading `seed: miss` line (it counts the labs' first builds). F4 accepted
  as measured: 279 s, 54/54 classes. F4 rebased onto 0349cb9; integrated verification queued.
- 2026-11-06 21:40 · **F4 integrated: `eb91db1`** (merge; branch head verified integrated: twelve self-tests,
  local_gate_contract 61/61, gate_dag_contract 14/14, v046). feat/ci-rapido now carries CI-J, CI-J2, CI-F1, F3.0,
  F3.2 and F4. F2 started in worktree `Memorys-ci-f2` from `a9fd82c`. A new marked throwaway judge server for the
  CI-J guard: container `memory-industry-gate-20261106` on 127.0.0.1:5490, pgvector:pg18,
  cluster_name=memory-industry-gate, durability off, restart unless-stopped (the old :5489 one stays for the old
  judge's F0 run 2). First full run of the new judge in series (F1+F4, before F2/F3 wiring) started under pesado,
  measured like F0.
- 2026-11-07 03:39 · **F2 split** (my decision; reverted by merging the two halves back into one task): **F2a** =
  `scripts/gate-cluster.sh` standalone (library + hermetic self-test, its own rows B5 with an injectable ready
  timeout so no row waits 30 s, D1, D2, D5, D6, A3, the long `sun_path`), entering `SCRIPTS_WITH_SELF_TEST`
  ([&str; 13]); **F2b** = the wiring into merge-gate/run-all-tests, contracts C1–C5/E1/E2 and the rewrite of the
  frozen CI-J rows j11_unset/j11_empty/j16_unset. The implementer's four decisions accepted: pg_config required,
  `--pwfile PATH`, `PGPORT=5432` in socket URLs, `source` without dispatch (`BASH_SOURCE == $0`). Lessons recorded
  in memory: a bare `wait` also waits for a postmaster child; a parent killed by `set -e` leaves orphan postmasters
  (trap + dead-owner sweep); `yes -- -D`. The full new-judge run (F1+F4) is still queued behind Mapupita's
  e2e-c2b under pesado (waiting since 02:57).
- 2026-11-07 03:39 · Notion synced: CI-F2 and CI-F3 «En curso» (F3.0 and F3.2 commits and evidence), CI-F4 «Hecha» 2/2
  with «Qué se hizo», CI-J2 appended to CI-J; Decisiones rows for PGDG, the change-point rule, the unsigned .exe, npm
  OIDC and the F2 split; Bugs y lecciones rows for J7, the shared target, the NLI latency under contention (open, F3)
  and the bash/postgres lessons. NPM-OIDC registered (batch table, outside the batch) and in Notion. **F3.1 started**
  in worktree `Memorys-ci-f31` (from 5c71048), in parallel with F2a: gate_history.py with one writer (gate-dag.py
  imports it in pass 2, its frozen contract unchanged), `test-times.sh --units`, contracts in
  gate_history_contract.rs; the merge-gate wiring and its J7b' row move to F3.3. A refuter reads cargo-mutants'
  lab copy (no compiling) for the 204-crate first build per lab.
- 2026-11-07 03:39 · F4 seed miss, the refuter's reading (cargo-mutants v27.1.0 `src/copy_tree.rs`, the kept logs of
  f4-medida-144115): not a path and not a flag. cargo-mutants copies the stage into each lab with `fs::copy` (reflink
  fails on this ext4: `os error 95`; with reflink it would still set mtime = now), so the seed's target lands with
  fresh, unordered mtimes; 68 independent roots (`the dependency X was rebuilt (T1, Δ after …)`, all inside the copy
  window), different between runs (an ordering race), 135 transitive; lab 2 is copied again from the stage. Cost per
  run ≈100–120 s (first build per lab 75/52 s and 55/50 s, others ≈5–8 s). Rivals refuted: path in the fingerprint
  (406 units Fresh under the new path), cap-lints/RUSTFLAGS (none), baseline in the original tree. Proposed fix: a
  cargo shim for cargo-mutants that sets one mtime on a lab's target before its first build, plus D-F4j counting
  every lab's first build. Side finding: the seed key does not record `CARGO_PROFILE_DEV_DEBUG=0` (passed as a
  prefix, not in the environment where the key is computed). Three-arm experiment (fs::copy-like / one mtime /
  cp -a) queued under pesado before any change.
- 2026-11-07 03:39 · **F2a delivered** (implementer): gate-cluster.sh pass 1 in `Memorys-ci-f2` (red: `A1 …
  gate_cluster_check: command not found`), pass 2 in scratch `f2copy` (OK); `SCRIPTS_WITH_SELF_TEST` → 13. New rows B5
  (`GATE_CLUSTER_READY_TIMEOUT_S=1`, ~1 s), D1, D2 (`gate_cluster_down_all` in a TERM trap), long path (>107 B socket =
  exit 2 before anything), D6 (no row creates the default root). Two rows fixed that gave a false green (D4 took the exit
  2 of «owner from another host»; D6 did not count the root itself). The library no longer sets `set -euo pipefail` on its
  caller. 17 sabotages red; the 12 other hermetic self-tests OK on a full copy. **A3 and D5 move to F2b** (they exercise
  merge-gate/run-all-tests, not the library), together with a boundary row at exactly 107 bytes. Verification queued
  under pesado (`verif-f2a.sh`); a reviewer reads pass 2 (kill/rm paths first).
- 2026-11-07 03:39 · **F3.1 delivered** (implementer): pass 1 in `Memorys-ci-f31` (gate_history_contract.rs, 9 tests,
  all expected red in their first assert; test-times row U1, red `unknown argument '--units'`), pass 2 in scratch `f31copy`
  (gate_history.py; gate-dag.py imports it, its own writer removed, same fields, corridas.jsonl only gains fields;
  `--units` with a new parse_log class `N` that --count/--judge ignore). 33 Python checks of the contract bodies, 0 fails;
  H1–H6 sabotages red through that harness. Gap noted: no planner fixture for H2 (cpu_s null under a fake cgroup without
  cpu.stat) → F3.3. Verification queued under pesado (`verif-f31.sh`: red, 13 self-tests, three contracts, clippy); a
  reviewer reads pass 2.
- 2026-11-07 03:39 · F3.1 review (reviewer, reproduced in /tmp/rev-f31): **P0 latent** (from F3.2, copied by F3.1): a
  non-UTF-8 byte in an old jsonl makes `load_history` raise and the planner exit 1 without running a step; `registrar`
  the same. **P1:** an unbounded `flock` hangs the planner; `registrar` writes 0.0 where nothing was measured and the
  planner takes it as a sample; the flake mark does not compare the step id (lib and lib-ignored share a key). Clean:
  no verdict change with the root a file, read-only, `.candado` a dir or bad-but-UTF-8 JSON; the same 18 step fields;
  `--count`/`--judge` byte-identical on 33 real logs. Decision (spec «F3.1 · correcciones de la revisión»): H7–H11 plus
  three P2 (no `__pycache__`, one bounds formula, `units()` tolerant) go in now as pass 1b/2b; the huella that also
  hashes untracked files, the remaining D-F3s run fields and units of a signal-killed binary go to F3.3. The queued
  verif-f31 was stopped (it would verify a superseded pass 2).
- 2026-11-07 03:39 · F2a review (reviewer, reproduced): no confirmed P0. **P1:** `down` can send QUIT/KILL to a recycled
  pid (bash reaps background children at once; no identity check as in the sweep); a live owner in another PID namespace
  looks dead to the sweep; six self-test rows green for another cause (the sweep's -D and exe checks not tested apart,
  B4 not reached, D3 hangs instead of failing, D1/D3 go through the 10 s KILL, D6 blind to deletions, D1 code 1 reuses a
  log). P2: empty pid taken as alive, inherited PGPORT/PGHOSTADDR redirect the server, the ready wait counts loops not
  time, root/lane/owner not validated. Clean: identity of the sweep on real PG 18.6, Docker postgres (uid 70/999) never
  killable, no bare wait, the password never printed. Decision: spec «F2a · correcciones de la revisión» K1–K10 as pass
  1b/2b; the password in psql's argv in gate-lock.sh goes to F2b. The queued verif-f2a was stopped.
- 2026-11-07 03:54 · The full new-judge run did not run: when it got pesado, medir-juez.sh refused (exit 2) because I
  had appended to this backlog in the same tree (a measurement needs the exact tree). Fixed by measuring from a detached,
  clean worktree `Memorys-ci-medir` at `4f34314` (F1+F4, same code as `5c71048`); requeued under pesado.
- 2026-11-07 03:54 · F3.1 pass 1b/2b delivered: 14 new contract tests (23 total; the 9 old ones untouched), H7–H11 and
  the three P2, plus a per-process broken-lock flag (after one timeout a process skips later writes). Python harness:
  13/14 new tests red against the old pass 2 (the 14th is a guard), 14/14 green with 2b; 12 sabotages red; the planner's
  exit, normalized output and step fields identical to F3.2 (GATE_DAG 0 and 1, green and red, first and learning run).
  h8 with the lock held: 1.06 s with the flag, 4.08 s without (limit 2.5 s, timeout injected at 1 s). Verification
  queued under pesado (verif-f31.sh).
- 2026-11-07 03:54 · **mtime experiment (three arms, same seed, the seed's profile env): the refuter's diagnosis
  holds.** A (copy without mtimes, like cargo-mutants' fs::copy): 78 Compiling before it stopped, 12 roots «the
  dependency X was rebuilt (T1, Δ after …)», 0 other Dirty reasons (it then failed on rustls' build script: `cp
  --no-preserve=all` drops the exec bit, an artifact of the arm, not of cargo-mutants). B (same copy, one mtime for the
  whole target): **Compiling=1, Fresh=304**. C (cp -a): **Compiling=1, Fresh=304**. So one mtime on a lab's target
  before its first build removes the ≈100–120 s per run; the fix (cargo shim for cargo-mutants, D-F4j over every lab)
  becomes slice **F4d**, after F2/F3.1 free the implementers.
- 2026-11-07 03:54 · **The first full run of the new judge stopped at 3.8 s on `cargo fmt --check`** (exit 1, the cheap
  step first, as designed): local_gate_contract.rs (16 hunks) and v046 (4) were never formatted — every verification
  of the batch ran the contracts but never fmt. Fixed in `13c4443` (rustfmt only; token-identical except whitespace,
  commas and closure braces). The verification scripts now check rustfmt of the new contracts. Lesson: a slice's own
  checks include fmt and clippy of what it touched (trabajo-en-lotes), and I skipped them. Measuring tree moved to
  `13c4443`; F3.1 verification and the measurement queued back to back under pesado.
- 2026-11-07 04:04 · F2a pass 1b/2b delivered (K1–K10): 32 rows red against the old pass 2, the copy OK in **7 s**
  (was 33 s: no row waits for the 10 s QUIT→KILL any more); every K sabotage red with its own line; 13 hermetic
  self-tests OK. Stand-ins are small ELF binaries built with cc per run (the identity check needs exe = bindir/postgres).
  **Rejected:** D6 creating a sentinel in the real `/tmp/memory-industry-gate-<uid>` (under F3 lanes it would race real
  clusters and write outside its temporaries) → the default root becomes `${GATE_CLUSTER_TMP:-/tmp}/…` and the region
  points it at its own temporary; cluster logs go under `${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/clusters`
  and the region leaves HOME/XDG empty (J7b'); one hand check of K8 on real PG 18.6. Back to the implementer.
  **F4d** (one mtime per mutants lab) handed to an especificador.
- 2026-11-07 04:04 · **F3.1 verified with cargo:** red on the worktree (gate_history_contract exit 101, 20 FAILED; U1
  `unknown argument '--units'`); on the copy gate_dag_contract 14/14, local_gate_contract 61/61, clippy clean, 12
  hermetic self-tests rc 0 with HOME/XDG empty, **gate_history_contract 22/23**: `la_marca_de_intermitente_compara_sha_y_huella`
  finds no flake (`[]`), likely the H10 id comparison against the fixture; rustfmt finds diffs in the new contract.
  Back to the implementer (cause first; a frozen test is not changed without my decision).
- 2026-11-07 04:04 · **Second full new-judge run stopped at 107 s** (exit 101): lib test
  `doctor::d3_the_real_check_tells_a_held_port_from_an_address_nobody_owns` (192.0.2.1:8787 held under mirrored
  networking) — the bug already fixed on release/0.31.0 (`2d61589`), which the batch did not carry. Merged
  release/0.31.0 into the batch: `4905e85` (d3 on port 0 and the 0.31 test-time lines). Lib before that: 1192 passed,
  1 failed. Measuring tree moved to `4905e85`.
- 2026-11-07 04:04 · F2a pass 1c/2c delivered: default root `${GATE_CLUSTER_TMP:-/tmp}/memory-industry-gate-<uid>` (one
  line), the region on its own temporary with a live sentinel (D6 by name and mtime), K11 computes the default without
  creating it, logs under `${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/clusters` (K7 logs); HOME/XDG empty in all
  13 self-tests; 20 sabotages red; self-test 8 s. K8 on real PG 18.6 by hand: with PGPORT=5488 and PGHOSTADDR=127.0.0.1
  inherited, the lane listens only on `.s.PGSQL.5432`, no TCP, `select 1` through the URL, up in 491 ms (initdb 323,
  ready 168), down leaves nothing. F2a verification and then the third full run queued under pesado.
- 2026-11-07 04:11 · F3.1: the failing flake test was the fixture: its three seeds had no step `id`, which H10 now
  compares and every real `units()` record carries. Approved (still pass 1, no red commit yet): `"id": "tests"` in the
  three seeds; rustfmt applied (only token change: the `use` order rustfmt imposes). Harness: 0 fails on 2b, 13/14 red on
  the old pass 2. Cargo re-verification queued.
- 2026-11-07 04:11 · **F4d specified** (spec F4 §«F4d · un mtime por laboratorio», handoff ci-rapido-F4d.yml,
  validar-handoff OK): direct `cargo-mutants mutants …` with `CARGO=<shim>` (D-F4m; `--list` stays without it), the shim
  as a production mode `--cargo-shim` acting only in a lab once (D-F4n, D-F4q, D-F4r), **epoch = the seed's newest
  mtime, not the judge's start** (D-F4p: the start would make freshly extracted registry files look newer; reverted in
  one line), D-F4j → `--lab-builds` per lab (`lab builds: hit|miss|unknown`), `CARGO_PROFILE_DEV_DEBUG=0` from one source
  that also enters the seed key (D-F4t; the next run rebuilds the seed once, ≈70 s). QA: 1 Compiling per lab, 0 Dirty,
  54/54 classes, run 2 < 230 s. Implementer started in worktree `Memorys-ci-f4d` (from 2aa31e9).
- 2026-11-07 04:15 · **F3.1 committed** on `feat/ci-rapido-f31`: red `45c5765` (gate_history_contract exit 101, 20 of 23
  FAILED; U1 `unknown argument '--units'`), code `39ac8f4`. Verified on a copy with those files: gate_history_contract
  **23/23**, gate_dag_contract 14/14, local_gate_contract 61/61, clippy clean, rustfmt clean, 12 hermetic self-tests rc 0
  with HOME/XDG empty. Handoff validated (`tests: frozen`, commit 39ac8f4). Improver (CC ≤ 6, types, no duplicates)
  started before the merge into the batch.
- 2026-11-07 04:19 · **Third full new-judge run (`4905e85`) stopped at 171 s** (exit 101) in local_gate_contract, 59/61:
  run-all-tests.sh --self-test («alone» row) and mutants-gate.sh --self-test (J1.9) refused by the common sysid guard —
  inside the judge, `cargo test` inherits `GATE_SERVER_SYSTEM_ID` (the real :5490) and the rows' fake psql answers
  another id. No per-slice verification could see it (they all run the contract outside the judge). New slice **CI-J3**
  (spec J-F1 §12): a closed list of what the judge exports to children; a contract runs every self-test with each set to
  a foreign value, and a second contract keeps the list complete both ways against the `export` lines; the fix lives in
  the regions (test code only). Implementer started in worktree `Memorys-ci-j3` (from cf07332).
- 2026-11-07 04:19 · **F2a verified with cargo:** red on the worktree (row A1 `gate_cluster_check: command not found`;
  contract 59/59 there), copy: gate-cluster self-test OK in 7 s with HOME/XDG empty and no postgres left, 12 other
  self-tests OK, **local_gate_contract 58/59**: `no_judge_script_prints_a_database_url_with_its_credentials` flags
  `gate_cluster_url`'s printf of the URL with its password → it assigns through `printf -v` into a caller-named
  variable (pass 1d/2d). verif-f2a's loop missed gate-audit/gate-cluster (wrong pattern): fixed.
- 2026-11-07 04:26 · F3.1 improver: gate_history.py max CC 23 → 5 (lower_bounds split; the «null, not zero» rule
  written once), gate-dag.py max 7 → 6 (bound 7 → 2, is_step_sample 4 → 2, dead `rounded` removed); one JSONL reader,
  one root, one REPO; full annotations (ParamSpec on the no-verdict decorator); ruff F,B,UP,TC clean. Harness identical
  before/after (0 fails, ALL SAME) and an in-process equivalence of 12 000 seeded cases EQUIV-IDENTICAL, six sabotages
  EQUIV-DIFFERENT. Seen, left for F3.3: `registrar` loses the run line on a unit naming an unknown step (no verdict
  change). Both contracts queued under pesado before the refactor commit.
- 2026-11-07 04:27 · **F2a committed** (`feat/ci-rapido-f2`): red `87256df` (row A1 `gate_cluster_check: command not
  found`), code `df50fe4`; verified on a copy: local_gate_contract 59/59, the 13 hermetic self-tests OK with HOME/XDG
  empty (gate-cluster 7–8 s), no postgres left. Handoff validated (implementador→mejorador, frozen). **F3.1 improver
  committed** `bff10b4` (gate_history_contract 23/23, gate_dag_contract 14/14 on it). **Both merged into the batch:
  `f71cffb` (F3.1), `9e04a47` (F2a).** Seen: the contract's `python3 -c 'import gate_history'` leaves
  `scripts/__pycache__` in the tree (gitignored; to F3.3: PYTHONDONTWRITEBYTECODE in the contract's launcher). F2a's
  improver/architect pass goes with F2b.
- 2026-11-07 04:38 · CI-J3 delivered (no cargo yet): `run_self_test(script, tag, env)` behind J7b''s launcher; a
  `JUDGE_ENVIRONMENT` of **19** names, measured (the spec's 10 were incomplete: run-all-tests also exports ONNX_MODEL_PATH,
  CUBA_RERANKER_PATH, CUBA_NLI_PATH and CUBA_EMBEDDING_DIM, which break its own self-test when inherited, plus five that
  do not), one exclusion (TMPDIR: a missing one breaks every mktemp, not a foreign value); the region fixes in three
  scripts (crap-gate too, by the same sysid), `unset` at the start of each region; bash table: before, crap-gate,
  mutants-gate and run-all-tests red under the foreign environment, after, 12/12 OK. Decision (mine): TMPDIR stays
  excluded; whether the judge should refuse a missing TMPDIR is a production change left out (gate-lock.sh keeps an
  inherited TMPDIR on disk without checking it exists) — noted as debt. Red/green with cargo queued.
- 2026-11-07 04:44 · **CI-J3 committed and merged:** red `4b79cd6` (every_self_test… FAILED: crap-gate and mutants-gate
  J1.9 with sysid 1 inherited, run-all-tests with the reranker at /nonexistent-ajeno; the list contract green, the list
  being complete), fix `4fb2b10` (region lines only), local_gate_contract **63/63**; merged `058cb55`. Fourth full run of
  the new judge started on `058cb55` (F1+F4+F2a lib+F3.1+J3, serial).
- 2026-11-07 04:44 · F4d delivered (pass 1 in `Memorys-ci-f4d`, pass 2 in scratch): rows F4d.1–F4d.7 with doubles of cargo
  and cargo-mutants, F4b.2's DEBUG arm now «same key» plus an OPT_LEVEL arm, F4b.8 into F4d.4, the contract locator to
  `cargo-mutants mutants`; red `F4b.2 CARGO_PROFILE_DEV_DEBUG: a debug setting from outside built another seed`, green
  38 s; 12 of 17 sabotages red by their own row, 4 caught first by another row, 1 (renamed cargo-mutants double) aborts
  the self-test before F4d.7. Found: the self-test wrote seeds into the hermetic cache since F1.1 (nobody checked).
  Verification queued. **F2b started** (worktree `Memorys-ci-f2b` from 058cb55): wiring in serial mode, rows A3, D5,
  C1–C5, E1, E2, the 107-byte boundary, the caller's D2, rewrites of j11_unset/j11_empty/j16_unset, contracts, the
  password out of psql's argv in gate-lock.sh.
- 2026-11-07 04:49 · F2b too large for one round: the implementer delivered only part of pass 1 (merge-gate j11_unset/
  j11_empty rewritten per D-F2c/D-F2k, row A3, contracts E2 and C4, GATE_LANE added to JUDGE_ENVIRONMENT; red
  `j11_unset: no FAIL line names initdb and docs/gate.md`). Split (my decision; reverted by merging them back):
  **F2b-1** = the caller's rows only (A1 order, C1, C5, D1, D2, E1; run-all-tests j16 → D5) with an injected fake
  `gate_cluster_up` instead of new PG fixtures, plus the code in merge-gate/run-all-tests/gate-lock (PGPASSWORD,
  D-F2i) and the docs part E2 needs; **F2b-2** = the real-cluster contract (B1–B3), the rest of docs and AGENTS.md. The
  library rows already in F2a's self-test (A2, D3, D4/K4, K5–K10, the long path) are not duplicated in the caller.
- 2026-11-07 04:51 · **Fourth full run (`058cb55`): 412.9 s, exit 101** — past fmt, clippy (16 s), the throwaway
  brain_gate (fresh, 43 tables), cargo test unit + smoke (≈295 s), then red in the `--ignored` step: lib test
  `sync::chunk::tests::a_deleted_fact_leaves_a_tombstone_even_though_its_key_is_not_called_id` → «connect to test
  database: failed to run sqlx migrations … duplicate key value violates unique constraint "_sqlx_migrations_pkey"»
  (17 of 18 passed). Looks like a first-time race of migrations under parallel tests on the new :5490 server (the old
  judge's :5489 passed it in F0). A debugger reads it (no rerun to approve: a race between two connections is the bug).
- 2026-11-07 04:57 · Debugger on the `--ignored` red: **not the server, a latent race since 12-ago in lib tests.**
  `db::tests::a_database_behind_the_binary_is_refused_instead_of_dying_mid_import` (db.rs:1116) holds
  GLOBAL_STATE_GUARD while it deletes and re-inserts the newest `_sqlx_migrations` row (outside sqlx's advisory lock)
  and sets CUBA_SKIP_MIGRATIONS=1; the `test_pool()` helpers of sync/chunk.rs and protocol.rs open a pool without the
  guard, so they can migrate inside that window (23505 `_sqlx_migrations_pkey`, 2.8 ms after the db.rs test ended) or
  read the SKIP. Order-dependent, three faces of one cause. Slice **T-GUARD** on `release/0.31.0` (worktree
  `Memorys-guard`, spec `guard-chunk-protocol.spec.md`): a deterministic interleaving test (two connections, an
  observable barrier, no sleep), then the guard in both helpers; merged into the batch afterwards.
- 2026-11-07 04:58 · F4d verified with cargo: red on the worktree (local_gate_contract 57/60: the locator
  `each_mutant_builds_only_the_lib_test_it_runs`, plus two that run the red mutants self-test), green copy: 12 hermetic
  self-tests OK with HOME/XDG empty, contract **60/61**: `no_script_looks_for_the_cache_where_the_binary_does_not` flags
  the F4d.7 row's `find "$HOME" "$XDG_CACHE_HOME"` line (mutants-gate.sh:2565). Back to the implementer (the row uses
  its own variable; the contract stays).
- 2026-11-07 05:03 · T-GUARD pass 1 stopped on the no-skips hook (`#[ignore]` cannot be added; not worked around).
  Decision (mine): the interleaving test runs in the **normal** lib `cargo test` (the judge's unit step already has
  DATABASE_URL, run-all-tests.sh:1585), fails if DATABASE_URL is missing, and opens the dangerous window only on its
  **own throwaway database** (DATABASE_URL switched under GLOBAL_STATE_GUARD, dropped at the end), so the parallel lib
  tests never see it. No section count changes (the unit step is not a `run_counted` section). Reverted by moving the
  test back under the `--ignored` convention once the hook allows it.

- 2026-11-07 05:10 · **Correction:** the 7-nov entries from «04:00» on were stamped with invented times (up to «11:30»);
  each one now carries the time of the commit that recorded it (git blame), which is when it was written.- 2026-11-07 05:21 · **T-GUARD closed:** red `1469a6c` (the interleaving test, `panicked at src/sync/chunk.rs:420 …
  _sqlx_migrations_pkey`, 0.60 s, deterministic), fix `1d10db6` (green 2.57 s; the whole lib 1194 passed, 18 ignored;
  no private database left); fast-forwarded into release/0.31.0 and merged into the batch (`2a41261`). Fifth full run
  queued on 2a41261. F4d's green copy had run a **stale contract binary** (its CARGO_MANIFEST_DIR pointing at an older
  copy: verif-f4.sh never cleaned before the green build) → clean added, F4d re-verification queued.
- 2026-11-07 05:21 · **Incident:** the F2b implementer wrote its pass 1 into `Memorys-ci` (the batch's integration
  tree) instead of its worktree `Memorys-ci-f2b`, and reported that I had told it to (I had not). Nothing lost: the diff
  was saved as a patch, the implementer consolidated everything into f2b (five files byte-identical), and `Memorys-ci`
  was put back to its HEAD (status clean). The T-GUARD merge into Memorys-ci touched only rust/src, so it did not mix.
  F2b-1 delivered: caller rows A1, C1, C5, D1, D2, A3, j11 and j16 rewritten (D5), a fake gate-cluster library in the
  regions, PGPASSWORD in gate_require_judge_server, D-F2i, docs «The judge's servers»; 13 self-tests OK under CI-J3's
  environment; 7 sabotages red. Verification queued.
- 2026-11-07 05:25 · **Fifth full run (`2a41261`): 104.9 s, exit 101** in the unit step: lib 1193 passed, 1 failed —
  `cognitive::judge::parse_tests::an_error_reply_is_empty_and_its_redacted_cause_is_logged` («logged: ""»: the warning
  was not captured). It passed in the fourth run: intermittent, suspected the global callsite-interest cache of
  tracing against a per-thread capture subscriber. A debugger reads it; no rerun to approve.
- 2026-11-07 05:25 · **F4d committed** on `feat/ci-rapido-f4d`: red `08d3f43` (self-test F4b.2; contract 59/61), code
  `5ff2aee` (self-test OK 34.6 s, 12 hermetic self-tests OK, local_gate_contract **61/61** with a clean green build).
  QA (two real mutants runs, MUTANTS_JOBS=2, outcomes kept, classes against F0) queued after F2b-1's verification.
- 2026-11-07 05:30 · Debugger on the fifth run's red: **a race inside tracing-core 0.1.36, latent since 13-oct**: with the
  capture subscriber the only live dispatcher (`has_just_one`), another libtest thread that first reaches the same
  `warn!` (judge.rs:203) is asked through `Rebuilder::JustOne` with ITS thread's default (NoSubscriber) and caches
  `Interest::never` for the whole process; the capturing test's warning is then dropped (the neighbour finished 0.2 ms
  before). Not T-GUARD (no judge.rs or tracing change in that diff). Slice **T-TRACE** on release/0.31.0 (worktree
  `Memorys-trace`): a test on its own callsite plus a child-process rerun that is red every time, then a global no-op
  subscriber answering `Interest::sometimes` installed once per process, called from the three capture helpers.
- 2026-11-07 05:42 · **F4d QA passed and merged** (`f4de3a8`): two real mutants-gate runs with
  MUTANTS_JOBS=2 — run 1 **265.9 s** (seed rebuilt once in 48 s, its key now carries the profile), run 2 **219.4 s**
  (target < 230 s); `lab builds: hit in 2 of 2 labs (each first build compiled 1 crate)`, mtime shim 24–29 ms per lab;
  54 mutants, 53 caught, 1 unviable, kill rate 1.000; **54/54 classes identical to F0**. The mutants step: 1053 s (F0)
  → 279 s (F4) → 219 s (F4d). Seed directory 2.1 GB (the two newest keys).
- 2026-11-07 05:42 · F2b-1 verified with cargo: red as expected on the worktree (j11_unset, j16_unset; contract
  60/65); green copy: 13 hermetic self-tests OK, no postgres left, contract **62/65** — three contracts on pass 2
  code: two DATABASE_URL assignments in merge-gate (one verified value only), gate-lock.sh:457 builds the DSN with
  printf of ${url…} (the credentials detector), a new cargo call before the cheap block (fmt after the first
  compile). Back to the implementer.
- 2026-11-07 05:46 · **T-TRACE closed:** red `640f430` (both envs::trace_tests FAILED; the child run 0 passed, 1
  failed — deterministic), fix `652e777` (both green; the whole lib 1196 passed, 18 ignored); fast-forwarded into
  release/0.31.0, merged into the batch `484e4ad`. Sixth full run started on 484e4ad (F1, F4, F4d, F2a lib, F3.1,
  CI-J/J2/J3, T-GUARD, T-TRACE; serial, no lanes yet).
- 2026-11-07 06:03 · **Sixth full run (`484e4ad`): 960.8 s, exit 1, the furthest yet** — fmt/clippy/cheap block in
  19 s, unit + smoke 297 s, --ignored 73 s, model sections, release build 150 s, reranker 209 s, E2E 116 s, live session
  12 s, then red in the **eval harness smoke**: `FAIL: the eval seeding exited 1: FAIL: fixture line 2: no se insertó
  (dedup o refuerzo de otra fila)`. First full run ever to reach the seeded eval CI-J built (only stand-ins had run it).
  Peak memory 10.5 GB, CPU 2453 s. A debugger reads it. In series this head should land near 25 min with docs, crap and
  mutants still to come; the lanes (F2b + F3.3) are what take it under 15.
- 2026-11-07 06:08 · **F2b-1 committed and merged:** red `8e9af29` (merge-gate j11_unset, run-all-tests j16_unset; contract
  60/65), code `f039e3d` (contract **65/65**, 13 hermetic self-tests OK under CI-J3's environment, no postgres left); merged
  into the batch `343df5f` (clean merge with F4d's mutants-gate and contract changes). Integrated verification of the head
  queued (fmt, the three contract binaries, clippy --all-targets). Note: with an external DATABASE_URL the judge keeps
  the old single-server path (C5); the lanes run only with DATABASE_URL empty — the next full run is made that way.
- 2026-11-07 06:12 · Debugger on the eval red: **the fixture, not the parsing.** The 24 lines of smoke-corpus.jsonl
  come in pairs sharing an entity; with the real embedder loaded, `cuba_cronica add`'s semantic dedup (same entity,
  cosine > 0.92, cronica.rs:707-729) reinforces line 2 against line 1 and answers `reinforced_id`; deterministic; the
  self-tests' double said «inserted» to everything. Slice **CI-J4** (spec J-F1 §13, worktree `Memorys-ci-j4`): a
  rust/tests binary that seeds every line through the real binary and model (unit step, no `#[ignore]`), then one entity
  per line (no content change; the judge's seeded = fixture = count guard untouched). **F3.3 started** in parallel
  (worktree `Memorys-ci-f33` from 97563fb): the DAG wired into merge-gate/run-all-tests with `GATE_DAG=0` by default.
- 2026-11-07 06:15 · F3.3 does not fit one round (the implementer said so before writing anything). Split (spec F3
  «F3.3 · corte y decisiones»): **F3.3a** table + `run-all-tests.sh --lane` with the series identical, **F3.3b** merge-gate
  from the table in series + history + F3.1's «for F3.3» fixes, **F3.3c** GATE_DAG=1. Release lanes decided: `rel` (B3→R1→R2)
  and `e2e` (E), on F2b's run_in_lane. F3.3a started.
- 2026-11-07 06:18 · **Integrated batch head `343df5f` verified** (F2b-1 + F4d + F3.1 + J3 + T-GUARD + T-TRACE): fmt
  clean, local_gate_contract **65/65**, gate_dag_contract 14/14, gate_history_contract 23/23, clippy --all-targets clean.
  CI-J4 delivered (a rust/tests binary seeding the 24 lines through the real binary over MCP stdio, as eval_seed.py does;
  fixture one entity per line; eval_seed.py tells deduplicated from reinforced_id); its verification with the real
  model queued. F3.3a pass 1 delivered (four contracts on the table, the lanes, the graph and gate-audit), pass 2 in
  progress; B3 stays its own lane (E depends on B3, so E and R run in parallel after it).
- 2026-11-07 06:21 · **CI-J4 closed:** red `6ba8570` (fixture line 4: reinforced_id — real binary, real model, 2.33 s),
  fix `36f5cb3` (24 lines in, 2.67 s; eval_seed.py --check-fixture OK; run-all-tests self-test OK); merged `ead5aee`.
  **Seventh full run started in lanes mode** (DATABASE_URL empty: each test step on its own ephemeral PG 18.6 cluster,
  serial), on ead5aee — the first run of F2b's lanes against the real tree.
- 2026-11-07 06:26 · F3.3a delivered (pass 1: four contracts and the `--lane` self-test rows; pass 2 as a patch:
  `scripts/gate-dag.txt` with 8 steps CL, D1, D2, NLI, D3, B3, R, E; eight `lane_<id>()` wrapping the existing blocks
  without moving code; `DAG_LANES` in idx order; an unknown or empty `--lane` is exit 2 before the lock). Red: `F3.3a:
  --lane NOPE exits 1, not 2`. 13 self-tests OK under CI-J3's environment; the contracts imitated in python green; four
  contract sabotages and one row sabotage red. Accepted: E depends on B3 **and D1** (the doctor uses brain_gate).
  Verification with cargo queued behind the seventh run.
- 2026-11-07 06:37 · **Seventh full run, the first in lanes mode (`ead5aee`, DATABASE_URL empty): 889.6 s, exit
  101.** Every test step on its own ephemeral PG 18.6 cluster; **the eval smoke passed** (CI-J4) at 807–812 s; peak 14.0 GB,
  CPU 2704 s. Red in the `--features docs` round: CI-J4's test «falta el modelo: ONNX_MODEL_PATH ("")» — merge-gate runs
  that round without the model env run-all-tests chooses. Slice **CI-J5** (spec J-F1 §14, worktree `Memorys-ci-j5`): one
  `gate_model_env` in gate-lock.sh for both rounds, a contract that it is the only place, a merge-gate row. Step times in
  lanes: unit 318 s, --ignored 68 s, models 48 s, release build + reranker 217 s, E2E 101 s, live 12 s, eval 5 s.
- 2026-11-07 06:57 · **F3.3a committed and merged:** red `d463d02` (`F3.3a: --lane NOPE exits 1, not 2`; contract 64/69),
  code `e3a22b7` (contract **69/69**, gate_dag 14/14, gate_history 23/23, 13 hermetic self-tests OK). **CI-J5 committed and
  merged:** red `0703cd9` (merge-gate J5 row; contract the_model_environment_is_set_in_one_place FAILED), fix `53a1c0b`
  (contract **66/66**). Both merged clean into the batch: `b9fe4e0`. Debt noted: F3.3a's red row reached the old judge
  server :5489 through the unpatched script (a throwaway server, never :5488; the green exits 2 before the lock);
  CI-J5's `demo.sh` exception has a reason but no owner or expiry (same format as CI-J3's). Eighth full run (lanes mode)
  started on b9fe4e0.
- 2026-11-07 07:01 · F3.3b does not fit: run-all-tests drops brain_gate in its on_exit, so one process per table step
  breaks D2/NLI/D3, and B3→R→E share REL_BIN/REL_SHA through the environment. Decision (spec F3 «F3.3b · ciclo de vida»):
  **the process is the chain** (run-all-tests takes several --lane in one call; merge-gate launches dbg = CL..D3, b3, rel =
  R, e2e = E, each on its own cluster); only files cross processes (the release binary; REL_SHA recomputed; E provisions
  its own brain_gate through the one provisioning function). Split: **F3.3b-0** (F3.1's history fixes, Python) now,
  **F3.3b-1** (merge-gate wiring, table rows P, B1, DC, TB, CV, M, joined log, serial history, T/LB) next.
- 2026-11-07 07:17 · **Eighth full run (lanes, `b9fe4e0`): 1134.7 s, exit 2.** Everything through the `--features
  docs` round passed (CI-J5 fixed it; the docs round took 330 s), then crap-gate in lane `cov`: `FAIL: lane cov has a URL
  that is not a socket under no root (D-F2i)` — run_in_lane hands the child DATABASE_URL, GATE_LANE and the sysid but
  not GATE_CLUSTER_ROOT, so the child's guard has no root. Slice **F2b-fix** (worktree `Memorys-ci-f2bfix`): the root in
  run_in_lane's env, in JUDGE_ENVIRONMENT, D-F2i not relaxed. Peak 13.8 GB, CPU 3652 s. Serial estimate for the whole
  judge once crap (≈60 s) and mutants (≈220 s) run: ≈24 min; the DAG (F3.3b-1 + F3.3c) is what takes it under 15.
- 2026-11-07 07:24 · **F3.3b-0 committed and merged:** red `6510adc` (gate_history_contract 24/33, U2 exited 2), fix
  `75d863e` (gate_history 33/33, gate_dag 14/14, local_gate 70/70, 13 self-tests OK); merged `72917cb`. Open note: the
  `__pycache__` test is not red in pass 1 (the launcher is test code); its red was seen only in the implementer's harness.
  F3.3b-1 waits for F2b-fix (both touch merge-gate's run_in_lane).
- 2026-11-07 07:38 · **F2b-fix committed and merged:** red `700a5c2` (merge-gate row «the step of lane cov did not see
  GATE_CLUSTER_ROOT»; contract 68/70), fix `f6a7277` (contract 70/70, gate_dag 14/14, gate_history 23/23); merged `c8dcdba`.
  Ninth full run (lanes) started on c8dcdba; F3.3b-1 started in worktree `Memorys-ci-f33b1` from c8dcdba.
- 2026-11-07 07:54 · F3.3b-1 delivered, cut (accepted): run-all-tests takes several `--lane` in one call (validated
  before the lock), R and E re-read the release binary (REL_BIN/REL_SHA recomputed), E provisions its own brain_gate only
  when missing; merge-gate runs four chains on their clusters — dbg (CL D1 D2 NLI D3), b3, rel (R), e2e (E) — then docs,
  cov and mut as before; a contract that the chains cover every lane of the table once, in idx order. Found and fixed in
  the copy: lane_B3's release CARGO_BUILD_JOBS leaked into E's provisioning (restored), check_section_lines per chain.
  Red: `--lane CL --lane D1 did not run CL and then D1`, merge-gate C1. **F3.3b-1b** (to come): table rows P, B1, DC, TB,
  CV, M, the joined log, the serial history and the T/LB summary. Verification queued.
- 2026-11-07 08:04 · **Ninth full run (lanes, `c8dcdba`): the first that ran the whole judge — 1493.1 s (24.9 min,
  serial, F0 was 2121 s), exit 1 only at the last step, test times:** local_gate_contract 220.26 s over its ceiling of 25 s
  (the batch's contracts now run every self-test several times, twice over: normal and docs rounds), gate_dag_contract
  15.14 s over the default 10 s. Every other step green: crap 45 s, **mutants 126 s** (F4d), docs round 330 s. Peak 18.7 GB,
  CPU 5729 s. Slice **CI-J6**: the two lines re-set by the file's own rule (one run alone: k = 1.5 → local_gate_contract
  110/331, gate_dag_contract 7/23), owner and expiry; plus a read of why the contract costs 220 s (it sits on the dbg
  chain, the DAG's critical path).
- 2026-11-07 08:07 · **CI-J6 committed and merged:** `dcfef60` (linux local_gate_contract 110/331, gate_dag_contract
  7/23, by the file's rule from the ninth run; red before = test-times --judge over that log exits 1; after: «OK test times
  on linux: 122 binaries within their ceilings», self-test OK, both checked by me); merged `8a9c4cc`. Next full run after
  F3.3b-1's verification, on the head with the chains.
- 2026-11-07 08:09 · Why local_gate_contract costs 110 s a round (read, not run): J3 runs the 13 self-tests in series
  inside one test (90–112 s) and J7b' runs 8 in series (85–107 s); ten single tests run each self-test again with the
  inherited environment — 31 launches, 250–314 CPU-s per round, twice (normal and docs). Slice **CI-J7** (spec J-F1 §15,
  worktree `Memorys-ci-j7`): parallel launches inside J3 and J7b' with a fixed cap of 6 threads (U3) and one run per
  script in J3's foreign environment shared by OnceLock with the ten single tests (U1, plus an own TMPDIR); J7b' is not
  merged into J3 (U2 would blind it to the default-under-HOME leaks). Estimate ≈40–45 s a round; the ceiling line is
  re-set from the measure (two-way: the 110 s floor would turn a 45 s round red). Skipping the docs round for this binary
  (U4) is not done: it is Leandro's call under ci-rapido.md.
- 2026-11-07 08:12 · **F3.3b-1 committed and merged:** red `8835d91` (run-all-tests «--lane CL --lane D1 did not run CL
  and then D1», merge-gate C1; contract 67/71), code `e2dafb2` (contract 71/71, gate_dag 14/14, gate_history 33/33, 13
  self-tests OK); merged `d093eec` with CI-J6's ceilings. Tenth full run (lanes, chains) started on d093eec. F3.3b-1b
  started in worktree `Memorys-ci-f33b1b`.
- 2026-11-07 08:15 · **Tenth full run (lanes, chains, `d093eec`): 105.3 s, exit 101** in the unit step:
  `doc_contract::the_gate_writes_its_own_exit_code_where_a_reader_can_find_it` — «scripts/merge-gate.sh runs
  "$ROOT/scripts/run-all-tests.sh"»: the contract finds merge-gate's first test step by the literal call line, which
  F3.3b-1 turned into chained `--lane` calls. My slice verifications ran local_gate, gate_dag and gate_history only;
  five other test binaries read the judge scripts (ci_contract, doc_contract, v025, v027, v045). From now on every
  judge-slice verification also runs ci_contract and doc_contract. Slice **CI-J8**: the locator finds the first
  run-all-tests call however it is written (the intent — the trap armed before the first step that can fail — kept).
- 2026-11-07 08:16 · F3.3b-1b did not fit as designed (no chain column in the table, P/DC/CV/M/TB are not lanes,
  serial history and T/LB would need new bash). Decision (spec F3 «F3.3b-1b · la fila de la tabla es la cadena»): **a table
  row is a chain** (P, DBG, B3, REL, E2E, DC, CV, M, TB), its command `merge-gate.sh --step <id>`, and merge-gate runs
  the table **through the F3.2 planner in both modes** — the series (GATE_DAG=0) then writes its history and its T/LB
  summary through what F3.1/F3.2/F3.3b-0 already built, and GATE_DAG=1 needs no new wiring. B1 out; start values from the
  ninth run of this tree. Restarted on top of CI-J7 once that is merged (both touch local_gate_contract.rs).
- 2026-11-07 08:23 · CI-J7 measured: **local_gate_contract 108.6 s → 41.9 s a round** (same 70 tests, list identical,
  clippy and rustfmt clean), but 69/70: its per-launch base path (uuid + script name + own TMPDIR) pushes gate-cluster's
  socket to 139 bytes, over the 107 of sun_path — gate-cluster's guard right, the path shortened (st-<pid>-<n>, TMPDIR
  base/t, margin computed). (The base run's one red, every_gate_entry_point_is_executable_in_the_index, is an artifact of
  measuring it from a git archive: not a repository.)
- 2026-11-07 08:24 · **CI-J8 committed and merged** (`8417979`, doc_contract 26/26, ci_contract 10/10;
  red = the tenth run): merged `1c8eda7`. Eleventh full run (lanes, chains) next, on this head.
- 2026-11-07 08:47 · **ELEVENTH FULL RUN: THE FIRST GREEN OF THE NEW JUDGE — 1303.3 s (21.7 min), exit 0, «MERGE GATE
  PASSED», receipt for 1c8eda7** (lanes mode, chains, still serial; peak 18.7 GB, CPU 4481 s). F0, the old judge on the same
  machine: 2121 s and red. Step starts: cheap block + clippy 0–20 s, throwaway db 20, unit + smoke 40→350 (310 s), --ignored
  350→418, model sections 418→461, release build 461→462 (warm), reranker 463→684 (221 s), E2E 684→780, live 780→793, eval
  793→797, docs round 797→1122 (325 s), crap 1122→1167 (45 s), mutants 1167→1303 (136 s), test times OK (122 binaries).
  DAG estimate from these numbers (chains as rows): critical path P 20 + DBG 421 + E2E 113 ≈ 554 s (9.2 min), the other
  chains under it (B3+REL 222, DC 325, CV 45, M 136); with κ = 1.2 ≈ 11 min; with CI-J7 (−66 s per contract round)
  ≈ 10 min. What remains: F3.3b-1b (the table through the planner), F3.3c (GATE_DAG=1, per-chain targets), ABBA.
- 2026-11-07 08:49 · **CI-J7 committed and merged:** `3ef45c2` (the contract: one run per script, six launches at a time;
  70/70 in 44.18 s, list identical, clippy clean) and `78949a3` (linux local_gate_contract 44/133 from that measure);
  merged `ddf9662`. Twelfth full run (serial, lanes) started on ddf9662 to confirm it whole. F3.3b-1b restarted with the
  redesign (rows are chains, the planner runs the series) in worktree `Memorys-ci-f33b1b2` from ddf9662.
- 2026-11-07 08:53 · F3.3b-1b stopped before writing: a step the planner launches is not merge-gate's direct child, so
  `inherit_gate_lock` (PPID) refuses it. Decided: D-F3k's minimum enters now (a one-use token per step, the owner alive
  and an **ancestor** of the process, reuse = exit 2; the serial PPID inheritance unchanged); TB leaves the table
  (merge-gate runs test-times after the planner, D-F3b); emit_block flushes; deps and start values fixed from the eleventh
  run. Allowed cut if it does not fit: token first, then table + --step + planner.
- 2026-11-07 08:56 · **Twelfth full run (`ddf9662`, with CI-J7): 354.7 s, exit 101** in lib-ignored:
  `handlers::zafra::tests::prune_plans_before_it_deletes_and_stays_inside_the_active_project` — «a dry run must not change
  the row count at all», 14 rows before, 6 after: another test deleted rows of the shared brain_gate between its two whole-
  table counts. Passed in the eleventh run on the same tree except CI-J7 (test file of the contract only): order-dependent.
  A debugger reads it (no rerun to approve).
- 2026-11-07 09:00 · F3.3b-1b-i delivered (the lock token; the cut accepted): rust/tests/gate_lane_token_contract.rs
  (7 tests with real bash owners and descendants: own token inherits once, a foreign token, a dead owner, a non-descendant
  and a reused token refused with exit 2 without taking the lock, no token = the direct-child inheritance as before), a
  planner test that a step under the lock gets a one-use ticket; code: gate-lock.sh (token branch, /proc ancestry,
  ticket renamed .used.<pid>), gate-dag.py writes the ticket and passes CUBA_GATE_LANE_TOKEN, run-all-tests uses
  inherit_or_acquire, merge-gate exports GATE_LOCK. Lesson: bash execs the last command of `bash -c`, so a test owner
  ending in `bash -c "$CONSUMER"` becomes its own direct child — ancestry tests end with `:`. Note: every token refusal is
  exit 2 (D-F3k said 1 for a second presentation; the spec text follows the decision). F3.3b-1b-ii (the 8-row table,
  --step, the planner in series, flush, TB out) next. Verification queued.
- 2026-11-07 09:02 · **Debugger on the zafra red: a real hole in project isolation, not a test race.** In sqlx-core
  0.8.6 `before_acquire` runs only on connections taken from the idle queue; a connection **opened** inside an acquire runs
  only `after_connect`, which db.rs sets to `app.current_project = ''` — so during a request with an active project,
  every new pool connection sees **all projects** through `tenant_isolation` (cuba_app has no bypass). The two counts of
  the zafra test fell on a new and a reused connection (14 vs 6). The test meant to guard it
  (`released_connection_does_not_leak_app_current_project`) uses `max_connections(1)` and never opens a second one.
  Handlers that also filter by project_id in SQL were safe; anything relying on RLS alone was not. Slice **T-RLS** on
  release/0.31.0 (worktree `Memorys-rls`): a deterministic test (one connection held as the barrier, the second must open;
  plus a row-level check as cuba_app on its own database), then `after_connect` sets `rls_scope()` and propagates errors.
- 2026-11-07 09:07 · F3.3b-1b-i verified with cargo: red as expected (gate_dag 14/15 on the ticket test); green copy:
  gate_dag 15/15, gate_history 33/33, doc 26/26, ci 10/10, but local_gate 70/71 (GATE_LOCK exported and not in
  JUDGE_ENVIRONMENT) and gate_lane_token 6/7 (a refused token does not exit 2, and says nothing on stderr). Back to the
  implementer.
- 2026-11-07 09:11 · T-RLS verified with cargo: scenario 1 (a new connection carries the active project) **red on the
  worktree** (db.rs:1157) and green with the fix; the whole lib 1198 passed, 18 ignored. But scenario 2 (row level, as
  cuba_app) is **green against the hole** — it does not discriminate, and that questions the reach of the hole ("sees all
  projects"). Back to the implementer: read the real tenant_isolation policy, say what a '' scope sees, make scenario 2
  red against the hole or remove it with the reason. Not committed until then.
- 2026-11-07 09:16 · **F3.3b-1b-i committed and merged:** red `38479d9` (gate_lane_token_contract 1/7 — the one guard
  green; gate_dag 14/15), code `5cf5753` (token 7/7, gate_dag 15/15, local_gate 71/71, gate_history 33/33, doc 26/26, ci
  10/10); merged `bf678f4`. Spec: every token refusal is exit 2. F3.3b-1b-ii (the 8-row table, merge-gate --step, the
  planner in series, flush, TB out) started in worktree `Memorys-ci-f33b1b3` from bf678f4.
- 2026-11-07 09:16 · T-RLS: the policy text (0017:31-40) confirms the reach — with a '' or unset scope cuba_app sees
  every project's rows (the RESTRICTIVE principal layer does not restrict while brain_principals is empty). Scenario 1 red
  again on the hole (left "" / right the project), green with the fix; whole lib 1198. Scenario 2 still green on the hole:
  likely PgPoolOptions::connect warms one idle connection that before_acquire then scopes; it gets the same barrier as
  scenario 1 (connect_lazy, one held, the count on the second, size() == 2), or it goes.
- 2026-11-07 09:19 · **T-RLS closed (security fix in the product):** red `39c883f` — both tests red on the hole: a new
  pool connection's `app.current_project` is "" instead of the active project, and **as cuba_app it counts 1 row of another
  project instead of 0** (the leak shown at row level); fix `fbe6632` (after_connect sets rls_scope() and fails closed) —
  both green, the whole lib 1198 passed. Fast-forwarded into release/0.31.0, merged into the batch `40771e4`. The test
  that should have caught it used max_connections(1); the pool's `connect_with` warms an idle connection that hid it too.
- 2026-11-07 09:44 · **Thirteenth full run (`40771e4`, with CI-J7 and T-RLS): green again — 1415.2 s, exit 0,
  receipt for 40771e4.** Unit + smoke 41→289 (**248 s**, was 310: CI-J7's contract is 86.60 s for both rounds, within
  44/133), --ignored 289→407, **release build cold 407→561 (154 s: T-RLS changed rust/src)**, reranker 563→792, E2E
  792→907, docs 907→1234, crap 48 s, mutants 133 s. The cold release build explains the +112 s against the eleventh run.
- 2026-11-07 09:49 · F3.3b-1b-ii delivered (8-row table P..M, `merge-gate --step <id>` checking the ticket and
  leaving it to the lane to spend, the flow wrapped in gate_main, the planner in the background with wait so a signal
  still stops the run, emit_block flush, SIGTERM in the planner exit 2; red: «the planner did not start the rows of
  gate-dag.txt in idx order»; 13 self-tests green on the copy; 47 contract checks imitated). Decisions 1–10 accepted.
  **One precision risk closed before cargo:** with the end-of-timed-steps marker after the whole planner, test-times
  would also count crap's llvm-cov and mutants' `finished in` lines (a second lib:memory_industry, new binaries) — the
  timed set must stay today's (the run-all-tests rounds and docs). CV and M become untimed rows, with a contract, a row
  and a sabotage.
- 2026-11-07 10:14 · **F3.3b-1b-ii committed and merged:** red `8699c92` (merge-gate planner row; local_gate 65/74,
  lane token 7/10, gate_dag 15/17), code `8d201a0` (local_gate **74/74**, lane token 10/10, gate_dag 17/17, gate_history
  33/33, doc 26/26, ci 10/10, 13 self-tests OK); merged `6b22e40`. The judge now runs its table through the planner in
  series. Fourteenth full run started on 6b22e40: the first through the planner, and the first that seeds the serial
  history the DAG needs before it parallelises (D8).
- 2026-11-07 10:15 · **Fourteenth full run (`6b22e40`, the first through the planner): 4.5 s, exit 2.** P passed, then
  DBG: «GATE_OWNER is empty, so no lane can say whose it is» — `merge-gate --step` is a new process, gate-lock.sh leaves
  GATE_OWNER empty and only gate_main sets it after taking the lock; the self-test's fake cluster library did not demand
  an owner as the real one does. The planner then stopped B3..M as the serial order demands and printed its summary
  (`dag: T=4.11 … mode=serie`). Handed to the F3.3c implementer as its first item: the fake library demands an owner
  like the real one, a --step DBG row, --step sets GATE_OWNER from the lock's owner, and a list of what else gate_main
  prepares that a fresh --step process lacks.
- 2026-11-07 10:23 · F3.3c delivered as **F3.3c-1** (cut accepted): --step sets GATE_OWNER from the lock's owner after
  the ticket check (the fake cluster library now demands an owner as the real one does; rows step_owner, step_dead), and
  D-F3g — a target per row and per tree (`<CARGO_TARGET_DIR>/<tree>-<12 hex of its realpath>/<T_x>`, a `.gate-tree`
  marker, two trees never share, a marker naming another tree refused before the step), DC → T_docs, CV → T_cov (both
  now start cold: decision 3), B3/REL/E2E share T_rel. Checked what a fresh --step lacks: only GATE_OWNER. Next:
  **F3.3c-2** (D-F3c's `fresh` and CARGO_BUILD_JOBS per chain within the cgroup) and the DAG activation trial. Disk now
  621 GB free; up to four targets per tree ≈ 60–100 GB. Verification queued.
- 2026-11-07 10:50 · **F3.3c-1 committed and merged:** red `c023ebc` (merge-gate A1 «GATE_OWNER is empty»; local_gate
  72/75; gate_dag 18/22), code `f6462df` (local_gate **75/75**, gate_dag 22/22, lane token 10/10, gate_history 33/33,
  doc 26/26, ci 10/10). A foreign CARGO_TARGET_DIR (CI-J3's /nonexistent-ajeno) made target creation raise; now exit 2
  with its reason; GATE_OWNER is read from the lock's record and crossed with CUBA_GATE_LOCK_OWNER (a foreign inherited
  owner refused: row step_judge). Merged `b91aa8e`. Fifteenth full run (through the planner, serial) started on it.
- 2026-11-07 10:54 · **Fifteenth full run (`b91aa8e`): 191.7 s, exit 101** — DBG's unit step: local_gate_contract 4 red
  inside the real judge: the contract's self-tests inherit **CUBA_GATE_LANE_TOKEN** from the DBG step (the planner hands
  it), so their inner gates take the token path and are refused («this lane does not descend from the gate that holds
  the lock», «no unused ticket … holds this token»), and D2 left '0'. CI-J3's list-completeness contract scans bash
  exports only; the planner's per-step environment (D-F3j) escaped it. Slice **CI-J9**: the token unset by the process
  that spends it, CUBA_GATE_LANE_TOKEN in JUDGE_ENVIRONMENT and unset in the regions that take the lock, and the
  completeness contract extended to the planner's closed list.
- 2026-11-07 11:10 · **CI-J9 committed and merged:** red `c8c8c0e` (gate_lane_token 11/12), fix `a74683a`
  (local_gate 76/76 incl. J3 with JUDGE_ENVIRONMENT at 27 names, lane token 12/12, gate_dag 22/22, gate_history 33/33,
  doc 26/26, ci 10/10); merged `f4d0e83`. Sixteenth full run (through the planner, serial) started.
- 2026-11-07 11:13 · **Sixteenth full run (`f4d0e83`): 91.3 s, exit 101** — DBG's unit step, local_gate_contract 74/76:
  merge-gate's self-test row D2 «a gate stopped by INT left '0' in its exit file, not 2» (also under J3). Green in every
  per-slice verification; red only inside the real judge. Reading: F3.3b-1b-ii runs the planner as a **background job**
  of merge-gate; a non-interactive bash starts background jobs with SIGINT and SIGQUIT **ignored**, the ignore is
  inherited by every step and below, and a shell cannot trap a signal ignored at its entry — so the self-test's inner
  gate never sees its INT. Slice **CI-J10**: the planner gives its steps the default disposition of INT and QUIT, with a
  test that a step under an ignoring parent sees them default (/proc SigIgn), and the D2 row run under an ignored INT.
- 2026-11-07 11:19 · F3.3c-2 delivered: per-row CARGO_BUILD_JOBS and ORT pools in the table (none existed: every step
  asked nproc = 24 jobs), the planner's CPU cap at C instead of 1.25·C, NLI split out of DBG into an **exclusive** row
  (its 500 ms clock assertion runs alone), MUTANTS_JOBS=6 for M. Decisions D1–D7 accepted (spec F3 «F3.3c-2»). The
  implementer's admission simulation: GATE_DAG=1 ≈ 614 s (≈10 min), critical path P → DBG → E2E 554 s, CPU bound
  ≈509 s — not measured. Residual clock risk: v029_accounts, v029_auto_host_beacon, v042 stay in DBG. Verification queued.
- 2026-11-07 11:28 · F3.3c-2 verified with cargo: red as expected; green copy gate_dag 24/24, lane token 12/12,
  history/doc/ci green, but local_gate 73/78 and merge-gate's self-test red (F1.2 expects the gate's jobs where the row now
  sets them; two F3.3b-1b-ii contracts do not skip the NAME=value tokens or count `exclusive` as timed; DAG_LANES still
  lists NLI before D3). Decision: NLI moves after D3 in DAG_LANES (same tests, two independent sections reordered), and
  NLI stays a timed row — its block is emitted before the boundary in idx order even if it runs last. Back to the
  implementer.
- 2026-11-07 11:36 · **CI-J10 committed and merged:** red `3a64b56` (gate_dag 22/24), fix `e4cb6a7` (gate_dag 24/24,
  local_gate 76/76, the rest green); merged `f0cf37d`. Seventeenth full run (serial, through the planner) started.
- 2026-11-07 11:46 · F3.3c-2 corrections delivered (the two F3.3b-1b-ii contracts skip NAME=value tokens and count
  `exclusive` as timed, NLI's block emitted before the boundary in idx order — checked in flush(); DAG_LANES = CL D1 D2 D3
  NLI B3 R E; F1.2 accepts each row's jobs and stays red on an unset one). The implementer saw merge-gate's D2_INT row fail
  2 of 3 on the unpatched base 75b2a7a too — the same INT-ignored mechanism CI-J10 just fixed in the planner (that base
  does not carry CI-J10); not retried to pass. Re-verification queued; the seventeenth full run (with CI-J10) will tell
  whether D2 is green inside the real judge.
- 2026-11-07 12:03 · **Seventeenth full run (`f0cf37d`, serial through the planner, with CI-J9 and CI-J10): every step
  green — D2 included — and red only at test times:** `v017_relation_scan took 8.47 s, under the floor of 9 s of its line`
  (the two-way ceiling doing its job: the generative-LLM test got faster). 1520.3 s (DC and CV started cold in their own
  targets), peak 18.0 GB. The planner's own summary: **LB_run = 578.77 s (critical path P → DBG → E2E)** — the DAG's
  lower bound on this tree, 9.6 min; T/LB_run 2.63 in series. v017_relation_scan over the four whole runs: 19.18, 9.45,
  11.24, 8.47 s → by the file's rule (≥ 3 runs, k = 1.3) floor 4, ceiling 25. Slice CI-J11 for that line.

- **7-nov 12:08 · CI-J11 fusionada** (`60a8725`, merge `bb86e37`). `linux v017_relation_scan` pasa de 9/33 a **4/25** por la regla (cuatro corridas completas bajo `pesado`: 19,18 / 9,45 / 11,24 / 8,47 s). Evidencia: `test-times --judge` sobre el log de la 17.ª corrida sin marcas → rc 0 (123 binarios dentro); con el fichero anterior → rc 1 (suelo 9). `test-times --self-test` rc 0. Hallazgo: en `log-con-hora.txt` de la 17.ª las marcas llegan en ráfagas, porque el planificador vuelca el log de cada fila al terminarla; por eso el 8,47 no se puede comprobar por marcas (el suelo sale 4 igual con 9,45). Es un artefacto de `medir-juez.sh`, no del juez: el juez lee su log crudo.

- **7-nov 12:12 · F3.3c-2, verde en el sitio.** El parche de la pasada 2 se aplicó sobre el rojo `2456b7d` + el merge `06a0530` (conserva el `preexec_fn` de CI-J10), y las regiones de autotest no cambian. Contratos: gate_lane_token 12/12, gate_dag 26/26, gate_history 33/33, doc 26/26, ci 10/10, rustfmt OK. **Rojo:** `local_gate_contract` 76/78 y el autotest de merge-gate, ambos por la misma causa: la expectativa literal `"P DBG B3 REL E2E DC CV M "` del autotest (merge-gate.sh ≈1507) no se actualizó en la pasada 1, y el planificador arranca bien en orden de idx `P DBG NLI B3 …`. Se corrige con una enmienda de la pasada 1 (un implementador, un commit `test(rojo)` aparte), no tocando el código.

- **7-nov 12:18 · F3.3c-2 fusionada.** Commits: `764e57f`, la enmienda `test(rojo)` (orden literal `P DBG NLI B3 …`); `0b70677`, el código; merge `ec30fb9`. Verde: local_gate_contract 78/78, gate_dag 26/26, gate_lane_token 12/12, gate_history 33/33, doc 26/26, ci 10/10, y los autotests de merge-gate, run-all-tests y gate-lock con rc 0. Sabotaje: NLI con idx 35 deja rojo el autotest de merge-gate. Decisión D8 en la spec: NLI comparte `T_dbg` porque depende de DBG y es exclusiva. Siguiente paso: la 18.ª corrida completa en serie sobre la cabeza, para sembrar el historial (D8 de F3), y después la primera con `GATE_DAG=1`.

- **7-nov 12:19 · Notion sincronizado**: CI-F3 (commits F3.1–F3.3c-2, evidencia de la 17.ª corrida y de F3.3c-2, criterios 2/4), CI-J (J3–J11) y CI-F2 (F2b-1, F2b-fix, criterios 2/3).

- **7-nov 12:20 · F2b-2 abierta** (worktree `Memorys-ci-f2b2`, rama `feat/ci-rapido-f2b2` desde `51b82c3`). Pasada 1 con un implementador: contrato con clúster real `a_lane_cluster_is_the_judges_own_throwaway_server` (B1–B3, E1) y D6, la raíz por omisión de los autotests. E2 y C4 ya existen. Después vienen docs/gate.md y AGENTS.md (orquestador), y la retirada del contenedor viejo (D-F2m) tras dos verdes del juez de F2.

- **7-nov 12:26 · 18.ª corrida, roja en NLI** (`a6b4932`, en serie, 350 s hasta el rojo, pico 6,6 GB). P y DBG en verde (DBG completo: CL, D1, D2, D3 y `cluster dbg down`). La fila NLI sale con exit 2, `FAIL: DATABASE_URL is not set`, porque `step_NLI` llama a run-all-tests sin `run_in_lane` y run-all-tests exige siempre servidor. El autotest lo tenía codificado como correcto. **CI-J12 abierta** (spec en ci-rapido-F3.spec.md; worktree `Memorys-ci-j12`): NLI con su propio carril de clúster, `nli`, y el stand-in de run-all-tests que exige `DATABASE_URL` como el real. Decisión: un clúster propio y no una excepción en la guarda del servidor (protege `:5488`); coste ≈0,5 s. Se revierte con la llamada directa.

- **8-nov 09:48 · F2b-2: el contrato nuevo se colgó 21 h** (desde el 7-nov 12:39). Leía `.lines().take(5)` del driver, que imprime 4 líneas y espera en `read`. Lo maté y comprobé que no quedaba ningún postmaster ni `/tmp/f2b-*`. Arreglo pedido al implementador: parar al tener las cuatro claves, y plazo en la lectura y en el `wait` (un test colgado no da veredicto). Antes, en el mismo contrato: `GATE_OWNER` se perdía porque `gate-lock.sh:55` lo vacía al cargarse con `source`; ahora se asigna después del `source` con `owner_record`. Hallazgo para deuda: `pesado -w` es un tope de tiempo, no una espera por el candado, y cuando el candado está ocupado no hace cola (sale enseguida).

- **8-nov 09:52 · CI-J12 fusionada.** Commits: `08e590a`, el rojo (autotest de merge-gate con A1 y NLI saliendo con 2; local_gate_contract 75/78); `f2aa677`, el código; merge `1e275a6`. Verde: los autotests de merge-gate y run-all-tests en OK; local_gate_contract 78/78, gate_dag 26/26, gate_lane_token 12/12, doc 26/26, ci 10/10, rustfmt OK. docs/gate.md: nueve filas, DBG sin NLI y por qué NLI tiene clúster. Deuda anotada: B3 llama a `run_in_lane b3` pero su fila no declara `cluster:` (el planificador ignora `cluster:` hasta F3.4), y gate-audit.sh tiene fixtures con filas que ya no existen. Siguiente: verde de F2b-2 y después la 19.ª corrida completa en serie.

- **8-nov 14:36 · F2b-2, tercera verificación.** Ya no se cuelga (lectura con plazo y `release` con plazo). Rojo del propio test: `no reactor running` en `local_gate_contract.rs:4781`, porque `tokio::time::timeout` se construye fuera del runtime; arreglo pedido. J7b' ampliado con `GATE_CLUSTER_TMP`: en ok, clippy rc 0. **El tiempo no vale:** local_gate_contract tardó 110,4 s (antes 44,8). Mientras tanto, otra sesión corría el juez `todo` de Mapupita bajo `pesado` (carga 59, PSI de CPU 54 %), y mi cronometraje suelto de los autotests también cayó encima de ese juez. No se toma como regresión: se repite con la máquina libre.

- **8-nov 15:09 · F2b-2, cuarta verificación** (máquina libre, PSI de CPU al 3 %). El contrato funciona y los tres sabotajes caen cada uno en su aserto: B1 con `fsync` en on; B2 «SQLSTATE connected, not 28P01»; B3 con el descriptor 1 en `/dev/null`. Queda un rojo sin sabotaje, y es del test: PG 18.6 responde `UTC` a `show timezone` aunque arranca con `-c timezone=Etc/UTC`. Arreglo pedido: comprobar que la zona es UTC (el nombre en {UTC, Etc/UTC} y un offset de 0), no la cadena exacta. local_gate_contract tardó 55,5 s con la máquina libre (antes 44,8 s, +24 %): queda por explicar cuando salga verde. **19.ª corrida completa en serie lanzada** sobre `220355b` (lleva CI-J12).

- **8-nov 15:47 · 19.ª corrida** (`220355b`, en serie, todo por el planificador): **todos los pasos en verde**, T=1157 s (la 17.ª, 1520 s), LB_run=575,9 s (P→DBG→E2E), T/LB_run=2,01, pico de memoria 17,6 GB, PSI de CPU 45 s en toda la corrida. Solo falla test-times, con 3 binarios: (1) `v017_rerank_gpu` 100,8 s, por debajo del suelo de 106. La causa está en el código: `rerank.rs` usa 2 hilos por omisión cuando `wants_gpu(Reranker)` (build con cuda) aunque corra en CPU, y la fila REL le fija ahora 8, que es lo que da el plan de producción en CPU; antes salía 235–247 s. (2) `nli_cost` 24,3 s (antes 4,5–5,0), sin línea: con 4 hilos antes y ahora; las inferencias, ≈1,5 veces más lentas; los otros ≈19 s caen fuera de lo medido (la carga del modelo o la sesión). Se reproduce aparte antes de fijar nada. (3) `v046_migrations_apply_over_an_existing_schema` 12,9 s (antes 4,2–8,8), sin línea. No se toca ningún techo con una sola corrida (regla de ≥ 3).

- **9-nov 07:16 · Reanudación.** La quinta verificación de F2b-2 y el experimento de `nli_cost` no llegaron a correr (la sesión se cerró con los dos en cola); se relanzan encadenados bajo `pesado`. **Pendiente de Notion:** el MCP de Notion no está conectado en esta sesión. Queda por subir: CI-F3 (la 19.ª corrida, todo verde por el planificador, T=1157 s) y CI-F2 (F2b-2 en verificación). Análisis de `v046_migrations_apply_over_an_existing_schema`: el juez suma sus dos pasadas, DBG y DC. En la 17.ª fueron 4,45 + 4,35 = 8,80 s; en la 19.ª, 4,70 + 8,24 = 12,94 s. La suma lleva cinco corridas en 8,5–8,8 s, pegada al tope de 10 s por omisión, y lo que subió es la pasada de DC (una sola observación). Se fija su línea con ≥ 3 corridas de la configuración nueva.

- **9-nov 07:43 · F2b-2, quinta verificación:** local_gate_contract **79/79 en verde**. Los sabotajes B1 de fsync, B2 de trust y B3 de stdio caen cada uno en su aserto. **El sabotaje «quitar `-c timezone`» sale verde.** Causa comprobada en el fuente: sqlx manda `TimeZone=UTC` en el arranque de cada conexión (`sqlx-postgres-0.8.6/src/connection/establish.rs:33`), así que `show timezone` por sqlx da siempre UTC. **Corrección de la nota del 8-nov:** PG 18.6 no cambia `Etc/UTC` por `UTC`; quien pone `UTC` es sqlx. A mano, con el sabotaje y `TZ=America/Mexico_City`, el servidor queda en America/Mexico_City. Arreglo pedido: leer la zona del servidor con psql por el socket, sin PGTZ. Pendiente aparte: local_gate_contract tardó 123,5 s (J7b' por encima de 60 s; con la máquina libre fueron 55,5 s). El experimento de nli_cost no corrió porque `pesado` lo tenía otra sesión (e2e-n8).

- **9-nov 10:39 · Decisión de Leandro (AskUserQuestion):** (1) **todo el plan entra antes de la 0.31**: F5 (USL, perfil juez-cuda, test=false), F6 (quality-gate Q1+Q2) y F7 (portar a Mapupita y a la plantilla). (2) **Primero se implementa todo, y el juez completo corre al final**, una vez, para ahorrar tiempo de máquina. Se acabaron las corridas completas y las medidas intermedias; cada tajada conserva sus comprobaciones baratas (test en rojo, verde acotado, sabotaje). Se revierte volviendo a medir por fases. Lo que esto cambia: los techos de tiempo (3 líneas), el ABBA y las dos vueltas del veredicto salen de las corridas finales en paralelo.

- **9-nov 10:41 · Lo que falta del plan, en tajadas.** F2b-2 (verificación en cola; docs del orquestador tras el merge), **F3.4**, F3.5 (condicional por D-F3w, se decide con el historial de las corridas finales), **F5**, **F6**, **F7**, la deuda anotada y el six-pack final (arquitecto, endurecedor, qa). Lanzados cuatro especificadores en paralelo (no compilan): F3.4 (huecos frente a F3.3c-2, la desviación de D-F3m en el reranker y el diagnóstico barato de nli_cost), F5 (herramienta USL alimentada por el historial, perfil juez-cuda, `test=false` en los bins duplicados), F6 (Q1 exacto, Q2 por cobertura con estudio de equivalencia en la corrida final, Q3) y F7 (lo genérico a Mapupita y a la plantilla; en este lote solo lo mínimo, el resto como tareas del backlog de Mapupita).

- **9-nov 10:44 · CI-J13 abierta** (deuda; spec `.cursor/handoffs/ci-rapido-J13.spec.md`; worktree `Memorys-ci-j13`): D1, toda excepción con dueño y caducidad (`MODEL_NAMES_ELSEWHERE`, `NOT_HANDED_DOWN`); D2, una sola fuente para `ALLOWED_ENV`; D3, las fixtures de gate-audit con la tabla real; D4, el juez barre los targets de árboles borrados. Lo motivó medir el disco: borré **42 targets de verificaciones ya fusionadas (≈92 GB)**, y se pasó de 537 a 630 GB libres; solo quedan el target del juez, el de cargo-lote y el de f2b2. Fuera del lote, como tarea de producto para después de la 0.31: la conexión LISTEN de `protocol.rs` no lleva ámbito RLS.

- **9-nov 10:56 · Leandro permite 12 agentes hoy.** Mi lectura: 12 vivos en total, y hasta 6 implementadores a la vez (no compilan) siempre que no toquen los mismos ficheros; el trabajo pesado sigue siendo uno a la vez con `pesado`. Lanzada la revisión del lote antes de la corrida final, que será la primera en paralelo: un revisor para gate-dag.py y gate_history.py (carreras, señales, esperas en modo GATE_DAG=1), otro para los guiones de shell (aislamiento entre carriles, que nada llegue a `:5488`) y el arquitecto (valores duplicados, dirección de dependencias, property tests y si compensa partir local_gate_contract.rs). En marcha: 9 agentes.

- **9-nov 11:06 · Spec de F6 lista** (`.cursor/handoffs/ci-rapido-F6.spec.md`): tres tajadas en serie (F6a Q1, F6b Q2+Q3, F6c el estudio). Hallazgos: (1) **el build se lleva la mayor parte de las ≈3 h**, porque quality-gate compila los 122 targets por mutante; con el laboratorio de F4 (≈4,6 s de mediana por build) Q1 bajaría a ≈1,2–2,2 h y Q2+Q3 a ≈35–50 min (estimado). (2) cargo-mutants 27.1.0 no elige tests por mutante: se resuelve con el shim de F4d, que filtra por mutante con su marcador. (3) **`CARGO_BUILD_JOBS` no acota bajo cargo-mutants** (el jobserver heredado manda; leído en el fuente, sin medir): lo que acota es `--jobserver-tasks`. (4) No hay ningún outcomes.json guardado de quality-gate: los 787 mutantes y las 3 h vienen del plan. Q2 solo se adopta si el estudio (`--study legacy`, ≈3,5–4,5 h en la corrida final) da cero cambios de clase; mientras tanto el modo por omisión es `full`. **F6a lanzada** (worktree `Memorys-ci-f6`). Pregunta pendiente para Leandro (en una ronda): cuándo corre la auditoría nocturna de quality-gate.

- **9-nov 11:16 · `pesado` con `Delegate=yes`** (autorizado por Leandro con AskUserQuestion; copia en `~/.local/bin/pesado.antes-delegate`, cambio atómico con mv). Causa comprobada: sin delegación, el scope solo tiene los controladores memory y pids; `delegate()` (gate-dag.py:330-343) falla al escribir `+cpu`, y con `GATE_DAG=1` el planificador corre **en serie** sin medir nada por paso (por eso `cpu_s` y `anon_max` salían null en todo el historial). Lo vieron los especificadores de F3.4 y F5; lo confirmé con systemd-run. **Corrección de la nota de ayer:** `pesado -w` sí espera el candado (`flock -w`); el «ocupado» venía de la verificación colgada 21 h. **Specs listas:** F3.4 (ci-rapido-F3.4.spec.md: admisión por MemAvailable, semáforo de modelos, comprobación del nivel de modelos, gate-latency.txt, E2E sin compilar, sha cruzado con B3, `cluster:C_b3`; adopta los 8 hilos del reranker en REL), F5 (release no es crítico: 32 % de holgura; F5c `test=false`, F5a herramienta USL) y F7 (sin código en este lote; L2–L7). **CI-J13** hecha por el implementador; además destapó un bug: `traced_id` de gate-audit tenía los ids viejos y no trazaba DBG, REL ni E2E. **En marcha:** F6a, F5c y F5a.

- **9-nov 11:20 · Revisión del planificador** (antes de la primera corrida en paralelo): ningún verde falso. Seis problemas que fallarían en paralelo; A4 lo cubre F3.4 y los demás van a la spec **CI-J14** (`.cursor/handoffs/ci-rapido-J14.spec.md`): (a) se graba el modo efectivo (hoy el DAG no llegaría a activarse nunca); (b) ningún paso sobrevive a SIGHUP ni a TERM en la ventana de Popen; (c) la RAM admitida cuenta el tmpfs de los clústeres (anon+shmem); (d) con un DATABASE_URL externo se corre en serie; (e) un fallo interno sale con 2, no con 1, y vuelca los bloques; (f) una línea a medias del historial no se come la siguiente. Va después de F3.4. **F7 L7 hecha:** `linux.md` (rules-wsl, compartido) dice que la zona del servidor se lee con psql, porque sqlx fija UTC por sesión.

- **9-nov 11:26 · Revisión de los guiones de shell:** nada crítico y ningún camino a `:5488`. A CI-J14 entran: (g) TERM antes que KILL, para que una fila matada corra su trap y limpie su clúster y su stage; (h) `on_exit` baja los clústeres de su propio dueño; (i) `timeout 60` para `claude -p` y `gemini -p`, y el psql de la guarda sin las variables `PG*` y con `PGCONNECT_TIMEOUT`. Después de F6 (toca mutants-gate.sh): la poda de corridas viejas (D-F3i) y de los .tmp de la semilla. **Hallazgo de producto, fuera del lote:** con `DATABASE_URL` vacía, el binario cae a `127.0.0.1:5488` (`setup.rs:13-31`); ningún test del juez llega a ese camino hoy, y se le llevará a Leandro como tarea para después de la 0.31. Lección para memoria: una limpieza que vive solo en un trap desaparece con SIGKILL (ya pasó en F2b-fix y CI-J12).

- **9-nov 11:29 · Revisión del arquitecto.** H1 (alta): **gate-audit.sh --run no puede ejecutar ninguna fila de la tabla real, y su autotest sale verde** (trata los tokens NAME=value como orden, falta el billete, la clave de árbol difiere del planificador —medido: 82dd… frente a 46cf…— y busca las raíces de clúster con el nombre equivocado). H2 (nombres de clúster) ya está en F3.4. Al planificador (CI-J14): H3 (camino crítico escrito dos veces), H7 (clave del historial sin comentarios), H8 (NaN e inf) y las propiedades P1–P4. **CI-J15** nueva (spec `ci-rapido-J15.spec.md`, después de CI-J13): H1, H4 (hilos de ORT leídos de resources.rs), H5, H6, H9 y H10. No se parte local_gate_contract.rs: costaría unos 130 s por juez.

- **9-nov 11:48 · F5a de vuelta** (worktree `Memorys-ci-f5a`): `gate_usl_contract.rs` con 19 tests (U1–U18) y el parche con `gate_usl.py` y `usl_point`. Comprobada con espejos en Python: recupera σ, κ y N* de una USL conocida (σ̂ 0,059994, κ̂/κ 1,0003), y cada sabotaje se vio en rojo en su espejo. Riesgo: que el contrato pase el techo de 10 s (≈60 procesos de Python). Aviso del implementador: con la tabla real, los 24 núcleos son el cuello y el simulador puede proponer «se conserva» para DBG. Va al segundo lote de verificación, con F6a y F3.4. El primer lote (F2b-2, J13, F5c) sigue en cola de `pesado`, detrás del `todo-final-4` de Mapupita y de `diag-cama`.

- **9-nov 11:55 · F3.4 de vuelta** (worktree `Memorys-ci-f34`): 9 tests nuevos en gate_dag_contract.rs (P1–P8), 9 en local_gate_contract.rs (L1–L5, T1–T3) y R1–R3 en el autotest de run-all-tests. **Rojo visto de verdad** en el autotest: «F3.4 R1: B3 left no .gate-release.sha256…». Sabotajes X6, X6b y T3b vistos en rojo en el autotest; los del planificador y los contratos, en espejos de Python. Parche: gate-dag.py (+196), gate-dag.txt, gate-latency.txt (nuevo) y run-all-tests.sh. Aviso: posibles conflictos de contexto con CI-J13 en gate-dag.py y al final de los contratos. Ojo: L3 hace que una línea `pendiente` de gate-latency.txt caduque el 2026-11-30, y L5 prohíbe GATE_DAG=1 por omisión mientras quede alguna; las corridas finales en paralelo las pasan a `dag-verde:2`. **Segundo lote de verificación** (F3.4 y F5a, con el guion general `verif-tajada.sh`) en cola de `pesado`, detrás del primero.

- **9-nov 12:13 · Tajadas apiladas para no esperar a `pesado`.** F6a de vuelta: el rojo, visto en los dos autotests; el parche toca mutants-gate.sh, quality-gate.sh y docs/gate.md. Tercer lote de verificación (F6a) en cola. **F6b** arranca sobre una base `wip(base)` con F6a aplicada (rama feat/ci-rapido-f6b). **CI-J14** arranca sobre una base con CI-J13 y F3.4 aplicadas (rama feat/ci-rapido-j14). El único choque al juntarlas fue el final de local_gate_contract.rs (las dos añadían tests al final); lo reconstruí de forma determinista, con el fichero de J13 más la cola exacta de F3.4 (el resto de F3.4 coincide con HEAD; rustfmt OK). Cada `wip(base)` se descarta al rebasar cuando entren las tajadas reales. CI-J15 sigue en espera (toca los mismos ficheros que CI-J14).

- **9-nov 12:46 · Primer lote verificado.** **CI-J13 fusionada** (rojo `2544b2a`, código `8d38cab`, merge `91e3a84`): autotests de merge-gate y gate-audit en OK; gate_dag_contract 27/27, doc 26/26, ci 10/10; local_gate_contract 82/83. **F2b-2 fusionada** (`1cbe843`, merge `b0935e2`): los cuatro sabotajes caen cada uno en su aserto (fsync; trust → «SQLSTATE connected»; stdio → descriptor 1 en /dev/null; sin -c timezone → America/Mexico_City y −21600 s); 78/79. **El único rojo en los dos** es la fila B5 del autotest de gate-cluster.sh, un aserto de reloj de menos de 3 s que bajo carga dio 3–5 s (local_gate_contract tardó 438–615 s, con la carga de las otras sesiones y de los autotests de mis agentes). No viene de ninguna de las dos tajadas: falla igual en el árbol de F2b-2, que no lleva J13. **CI-J16 abierta:** los siete asertos de reloj del autotest de gate-cluster.sh pasan a un tope entre el tiempo normal y el saboteado, y cada sabotaje se ve en rojo. **F5c:** su rojo fue el esperado, pero mi guion contó también la línea «test result: FAILED» y no hizo el commit; va en el lote siguiente.

- **9-nov 12:52 · F6b de vuelta:** rojo visto en los dos autotests (mutants-gate en «F6b.5: the real cargo did not get … -- --exact t_a1 t_a2»; quality-gate en «F6b.1: --cover-index did not write the select.tsv»); verde en la copia; parche aplicable. **Hallazgo confirmado en el fuente:** cargo-mutants 27.1.0 serializa el timeout como `"Timeout"` (`src/outcome.rs:331-338`), y `mutants-gate.sh` cuenta `"TimeoutMutant"` (≈576 y ≈1913): un timeout real no se cuenta nunca. La equivalencia de F4 sigue en pie: sus 54 mutantes no tuvieron ninguno (53 CaughtMutant, 1 Success, 1 Unviable en el outcomes.json de F0). Se arregla dentro de **F6c**, que compara clases con esa herramienta (lanzada sobre una base `wip(base)` con F6b). **Cuarto lote de verificación** (F6b y F5c) en cola.

- **9-nov 13:05 · CI-J16 de vuelta** (solo tests: la región de autotest de gate-cluster.sh). Los siete sabotajes se vieron en rojo, cada uno en su fila. Topes medidos con carga 24–27, ≥ 3 veces el tiempo normal y ≤ la mitad del saboteado: B5 10000 ms (saboteado 31269), «B5 never» 15000 (31294), B5b 15000 (32110). D1 mide solo el `down` (3000 frente a 10909). **D3 deja el reloj y comprueba el estado de salida del postgres** (0 si lo terminó QUIT, 137 si el barrido lo mató): no depende de la carga. D2 y K2 no cambian (ya tenían margen). Quinto lote de verificación (CI-J16) en cola.

- **9-nov 13:08 · Segundo lote:** F3.4 dio un rojo bueno (7 tests de gate_dag_contract con sus mensajes), pero mi guion tomó «error: test failed» por un error de compilación y no hizo el commit; además cargo se paraba en el primer binario rojo. **Guion corregido:** `--no-fail-fast`, y solo cuenta como error de compilación un `error[…]` o `error:` que no sea de tests (probado con las dos formas). F3.4 reencolada (lote 6). **F5a no compila** (`gate_usl_contract.rs:1016`, E0308: pasa `&mut Command` donde se espera `Command`); devuelta a su implementador. Primera vez que el test se compiló de verdad.

- **9-nov 13:14 · CI-J14 de vuelta** (sobre la base con CI-J13 y F3.4): 15 tests en gate_dag_contract, 7 en gate_history_contract (P1–P4 con semilla fija) y filas de autotest en merge-gate.sh (J14h, psql) y run-all-tests.sh (timeouts). Rojo de los autotests visto de verdad; los contratos, en espejos de Python (38 de 51 asertos en rojo; P1–P3 son verdes ya antes y su valor está en el sabotaje). Sabotajes de J14h, J14i y P1–P3 ejecutados. **Hallazgo que afecta a F3.4:** su exigencia de una línea `resource plan` en el log de E2E dejaba rojo el autotest de merge-gate, cuyo stand-in no la imprime. Apliqué a la pasada 1 de F3.4 (región de autotest de merge-gate.sh) el trozo de fixture que escribió J14, y `merge-gate.sh` entra en su commit rojo; el lote 6 aún no había arrancado. Cambio de clave del historial (J14k): invalida el historial actual una vez. CI-J14 encolada (lote 8). CI-J15 se lanza ahora, sobre la base de J14.

- **9-nov 13:40 · F6c de vuelta** (sobre las bases de F6a y F6b): `quality-gate.sh --study legacy|full DIR` (referencia → cover → `--compare-classes` → `study.tsv`; sale 0 solo con la referencia verde y 0 diferencias) y `compare_classes` cuenta `"Timeout"`. Rojo en los dos autotests; **8 sabotajes corridos de verdad, todos en rojo**. Riesgo para el humo final: si el argv de test de legacy (`-- --lib`) y el de cover (`--cargo-arg=--lib`) difieren, el comparador dará «test command changed» en masa. Comandos de la corrida final: humo `--study full` (≈8–15 min) y estudio `QG_BASE=<base de la 0.31> … --study legacy` (≈3,5–4,5 h). Noveno lote (F6c) en cola. Pendiente fuera del lote: el temporizador de la auditoría nocturna (D-F6r), que necesita a Leandro (horario).

- **9-nov 14:00 · F6a fusionada** (rojo `2f2280d`, código `a612a75`, merge `48855de`): autotests de mutants-gate y quality-gate en OK; local_gate_contract 80/81, doc 26/26, ci 10/10; rustfmt y shellcheck limpios. El único rojo es la fila B5b del autotest de gate-cluster.sh (aserto de reloj, 5 s bajo carga con tope de 5 s), que corrige CI-J16 (lote 5). Ese aserto de reloj ya es la causa del único rojo de tres tajadas (J13, F2b-2 y F6a), siempre con local_gate_contract tardando 438–615 s por la carga.

- **9-nov 14:20 · CI-J15 de vuelta** (sobre las bases de J13+F3.4 y J14). Rojo real en tres autotests: gate-audit (A11, A13, A15–A19), gate-cluster (J15c, J15d y J15e mayor) y merge-gate (J15e despacho, J15d). Quince sabotajes corridos de verdad. **Tres fallos de gate-audit fuera de la spec, arreglados:** (1) la raíz privada de cada fila es la del `mkdtemp` del planificador, que ahora sale de la traza (A18); (2) strace cortaba los argv a 32 caracteres (`-s 1024`); (3) el analizador no leía el resultado alineado de strace (`vfork()    = N`), visto con strace de verdad (A19). **Orden de fusión:** el contrato de fixtures de J13 sale rojo en cuanto entra F3.4 (la tabla pasa a C_rel, reader y C_b3), y J15 lo deja verde, así que el orden es F3.4 → J14 → J15. Decisión del implementador: `gate-audit --run` traza un juez entero (GATE_DAG=0) en lugar de lanzar filas sueltas. Décimo lote (J15) en cola. **Todas las tajadas implementables del plan están escritas**; quedan las verificaciones en cola y la integración en orden.

- **9-nov 14:23 · Decisiones de Leandro (AskUserQuestion):** (1) la **campaña final corre esta noche** (≈6–7 h de `pesado` en exclusiva: barridos USL, un juez en serie y dos en paralelo, el humo de quality-gate y el estudio de Q2); (2) la **auditoría de quality-gate con la suite entera es semanal, el domingo a las 02:00** (el temporizador D-F6r, tajada aparte); (3) **el arreglo de la URL por defecto entra en la 0.31**: con `DATABASE_URL` vacía, el binario se conecta a `127.0.0.1:5488` (setup.rs:13-31). Es producto: va por el six-pack sobre `release/0.31.0`, como T-RLS, y después se integra en feat/ci-rapido (tarea **T-DBURL**).

- **9-nov 14:37 · F6r de vuelta:** `scripts/qg-weekly-audit.sh` (nuevo; `SCRIPTS_WITH_SELF_TEST` pasa de 13 a 14). Hace `quality-gate --study full` sobre un HEAD nuevo de la rama de integración, bajo `pesado -m 36G -w 3600` y con un tope de 6 h; guarda veredicto y study.tsv, se salta un HEAD que ya tiene veredicto y borra el worktree desechable con un trap. Rojo visto; **24 sabotajes corridos, todos rojos**. Decisión clave: la base del diff es `QG_AUDIT_BASE` o el HEAD de la última auditoría (sin base, quality-gate juzgaría nada). **Instalación de las unidades** (`qg-auditoria-semanal.service/.timer`, domingo 02:00): después de la 0.31, porque lanzan el guion del clon principal, que hoy está en `feat/0.29-seguimientos`. Undécimo lote en cola.

- **9-nov 14:40 · Spec de T-DBURL lista** (`Memorys-dburl/.cursor/handoffs/t-dburl.spec.md`): con `MEMORY_INDUSTRY_REQUIRE_DATABASE_URL` no vacía (cualquier valor, también `0`, para que un error de tecleo siga protegiendo), el binario sin `DATABASE_URL` sale con 2 antes de llamar a docker o de probar el puerto. El orden: la URL dada, después el modo nube (sale con 1, como hoy), después el interruptor, y por último el camino de siempre. Función pura `url_source` con una tabla de 42 casos; el test de comportamiento v047, con un doble de docker; una guarda estática sobre las siete cadenas de test que quitan la variable; el juez exporta el interruptor en merge-gate.sh y run-all-tests.sh. Al integrarla en feat/ci-rapido, JUDGE_ENVIRONMENT pasa de 27 a 28. Implementación lanzada.

- **9-nov 14:53 · T-DBURL de vuelta** (rama fix/t-dburl, desde release/0.31.0). Pasada 1: la tabla de 42 casos en `setup.rs`, el test de comportamiento v047 (un doble de docker, `env_clear`, tope de 10 s, credenciales tapadas en los mensajes), el contrato de los scripts y el interruptor en las siete cadenas. Pasada 2: `url_source` antes de cualquier llamada a docker o al puerto, el `export` en los dos jueces y la fila del README. Autotests de los dos .sh en verde en la copia. La guarda estática y el contrato se emularon en Python, y la emulación cazó dos fallos del propio implementador, ya corregidos. Duodécimo lote en cola (el rojo de comportamiento con v047 y el contrato; la tabla unitaria, en el verde porque antes no compila; clippy -D warnings). Nota para el endurecedor: quality-gate excluía `setup.rs log` porque nada lo juzgaba, y v047 empieza a juzgarlo.

- **9-nov 15:05 · F6b y F5c fusionadas.** F6b: rojo `2a81114`, código `be426ea`, rebasada sobre feat/ci-rapido sin su `wip(base)` (guiones y doc idénticos byte a byte a los verificados), merge `79c2528`. Autotests en OK; local_gate_contract 81/82, doc y ci en verde. F5c: rojo `5da0034`, código `efccf6e`; `cargo metadata` da los dos bins con test=false; 83/84. Al fusionar F5c chocó el final de local_gate_contract.rs (las dos ramas añadían tests al final); lo resolví de forma determinista: el fichero de la rama más la cola exacta de F5c (286 líneas; el resto coincide con su base); rustfmt OK. En las dos, el único rojo es otra vez el aserto de reloj de gate-cluster.sh (B5 8 s, B5b 6 s), que corrige CI-J16.

- **9-nov 15:30 · CI-J16 fusionada** (`300c5b0`, merge). Autotest de gate-cluster.sh en OK y **local_gate_contract 78/78, el primero sin ningún rojo** (819 s con la máquina cargada por otras sesiones). Con esto cae el único rojo que arrastraban J13, F2b-2, F6a, F6b y F5c. La sesión se reinició a las 15:2x; la cola de `pesado` (lotes 6–12) sobrevivió y el lote 6 (F3.4) ya corre.

- **9-nov 15:54 · F3.4 (lote 6):** rojo bueno (commit `fbb3a13`; R1 del autotest y 7 tests de gate_dag_contract), parche aplicado, regiones congeladas, autotests de run-all-tests y merge-gate en OK. En verde, dos rojos: (1) el reloj B5b de gate-cluster.sh (8 s), que ya corrige CI-J16 (fusionada); (2) `no_more_than_two_rows_with_models_are_alive_at_once`: **el semáforo funciona** (su primer aserto, nunca tres vivas, pasó), pero el test daba por hecho que la que espera es m3, y el planificador ordena por camino crítico con las duraciones del historial, así que puede ser cualquiera de las tres. Es un fallo del test: enmienda pedida (exactamente una de las tres espera, con motivo `models`), con su commit `test(rojo)` de enmienda. CI-J14 lleva la misma copia del test en su base: se arregla al rebasar.

- **9-nov 15:56 · F5a (lote 7):** rojo bueno (`2d46746`; los 19 tests rojos con su mensaje), parche aplicado, regiones congeladas. **Verde:** gate_usl_contract 19/19 en 2,86 s (por debajo del techo de 10 s por omisión, que era el riesgo), gate_history_contract 33/33, rustfmt OK. `recomendar` sobre el historial real sale con 3 y pide `barrido --perfil debug` y `--perfil bin`, como se esperaba. **Clippy -D warnings, rojo** con dos avisos del test (type_complexity y manual_is_multiple_of): enmienda de forma pedida, antes del commit del código. Pendiente de esta noche: los dos barridos USL.

- **9-nov 16:14 · Cesión de `pesado` a Mapupita.** La sesión l24-ci-e6 (Mapupita-Rust, TASK-260) pidió turno: dice que Leandro eligió allí «Mapupita primero» y que sus tandas esperaban desde las 15:10. Detuve mis lotes 9 a 14, que esperaban sin haber empezado, y dejé terminar el lote 8 (CI-J14). Se relanzan cuando esa sesión avise de que terminó (≈1 h: su tanda y el todo+extra de Mapupita). Le avisé de la campaña de esta noche. **Pendiente de relanzar:** 9 F6c, 10 J15, 11 F6r, 12 T-DBURL, 13 F3.4 de nuevo y 14 F5a de nuevo.

- **9-nov 16:16 · Lote 8 (CI-J14) detenido a mitad, a las ≈16:15**, por la sesión del simulador (`systemctl --user stop` de su scope), por decisión de Leandro en esa sesión: «detener todos los mi-lote». Lo que llegó a hacer: **rojo bueno** (commit `5576c5b`: 14 tests de gate_dag_contract y los dos autotests, J14h y J14i), parche aplicado, regiones congeladas, autotests de merge-gate y run-all-tests en verde (gate-cluster cae en el reloj B5b: su rama aún no tiene CI-J16). La fase de cargo en verde no terminó. Worktree revisado: sin restos ni huérfanos. Guion de solo verde preparado (`verif-lote8b.sh`). Todo parado hasta que Mapupita avise.

- **9-nov 17:08 · Orden en la máquina** (regla nueva `maquina-compartida.md`; aviso de la sesión de Mapupita: el disco estaba a 5 GB del umbral). Borrados los targets de las tajadas integradas (j16, f5c, f6a, f6b, j13, f2b2) y `memory-industry-lote`: el disco sube de 605 a 623 GB libres. **Cerrados 32 worktrees de este lote ya integrados y limpios** (`git worktree remove`, que se niega si hay cambios; las ramas siguen a salvo). No se tocan F6r ni T-DBURL (trabajo sin commit), ni los worktrees de lotes anteriores o de agentes. Las tajadas que faltan comparten desde ya **un solo target** (`memory-industry-tajadas`). **Lección propia (está en la regla nueva):** fui yo quien encoló diez tandas con esperas de hasta 32 h. Desde ahora: una cadena por llamada a `pesado`, `-w` de minutos para una tanda y de 1–2 h para un juez, y mirar la cola antes de encolar.

- **9-nov 17:24 · Skill `ci-minuto-uno` aplicada** (encargo de Leandro, transmitido por la sesión l24-ci-e6: «impleméntalo en este proyecto y en el de memorys también»). Pasos que no compilan, hechos: medida del día, lectura de la toolchain y de las órdenes de cargo, máquina y constancia. Documento: `docs/ci/plantilla-aplicada.md`. Números: juez en serie 1157 s con LB 575,9 s; B3 206 s en frío frente a 1,7 s en caliente; el juez usa 3,8 núcleos de 24 y 17,6 GB de pico; **hoy el proyecto esperó 14,2 h en cola por 4,5 h de candado**, por encolar una entrada por tajada. **Tareas nuevas:** **CI-K1**, fijar la toolchain (no hay `rust-toolchain.toml`; todo compila con `stable` por omisión); **CI-K2**, medir en la campaña final los criterios de cierre de los pasos 2 y 3 (segunda orden con 0 crates propios recompilados; juez tras 2 h parado en menos de 30 s); **CI-K3**, agrupar los 119 binarios de test sin estado global; **CI-K4**, `CARGO_INCREMENTAL=0`, sin debuginfo y con strip en el juez, y `--timings` guardado en cada corrida. Lo que compila (K1–K4) va después del juez de Mapupita; K1 y K4 entran en este lote antes de la campaña si da tiempo.
  *Decisión:* `docs/ci/plantilla-aplicada.md` se queda local (sin commit): `.gitignore:78` declara `docs/*` como ficheros internos que no se publican (salvo gate.md y despliegue-lan.md), y el repo es público en npm, PyPI y GitHub. La regla del repo gana a la skill. La tabla, el estado y las tareas K1–K4 quedan en esta entrada del backlog, que sí tiene commit. Se revierte añadiendo `!docs/ci/` al .gitignore, si Leandro lo quiere publicado.

- **9-nov 17:25 · CI-K1 aplazada a después de la 0.31** (decisión técnica). Fijar `channel = "1.99.0"` mantendría el compilador (`stable` es hoy 1.99.0, y cargo identifica la caché por la versión del compilador), pero la toolchain `1.99.0` instalada **no trae `llvm-tools`** (lo necesitan crap-gate y el modo cover de quality-gate) **ni los targets windows-msvc y musl** que trae `stable`. Además, `publish.yml` y `ci.yml` usan `dtolnay/rust-toolchain@stable`, y un `rust-toolchain.toml` les cambiaría la toolchain. Fijarla hoy rompería la campaña y tocaría la cadena de publicación justo antes de la 0.31. Al hacerla: la toolchain con sus componentes y targets, los workflows leyendo el fichero, y un contrato. CI-K4 (incremental, strip y timings) se hace después de integrar J14 y J15, que tocan los mismos guiones.

- **9-nov 17:46 · Traspaso.** La sesión `optimizar-juez-merge-gate` (por encargo de Leandro, la sesión principal de MemoryIndustry) toma el lote: la integración de lo que falta y la campaña final de esta noche. Le pasé el estado exacto: F3.4, F5a y J14 con su rojo en commit y la pasada 2 aplicada sin commit; F6c, J15, F6r y T-DBURL con su pasada 1 y su parche; la cadena `verif-cadena-final.sh`; el orden de integración F3.4 → J14 → J15; los commits wip(base) por quitar con `--onto`; clippy sobre el árbol integrado antes de la campaña; y las decisiones de Leandro de hoy. Esta sesión deja de tocar Memorys-ci* y `pesado`.
