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
