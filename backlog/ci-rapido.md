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
