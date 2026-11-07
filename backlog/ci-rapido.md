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
