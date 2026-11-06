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
