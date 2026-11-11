# Agent contract — MemoryIndustry

Claude Code / Cursor / CLI: if a global rule (`rust.mdc`, «cero comentarios», plant e2e) clashes with this file, **this repo wins**.

## Mergeable (local CI only)

Mergeable means **`./scripts/merge-gate.sh`** exited 0 in this turn with a clean log: no `SKIPPED`, no soft alerts, no “should pass”.

Aliases of the same SIL: `./scripts/como-el-ci.sh todo`, `./scripts/como-el-ci.sh extra`, `./scripts/memory-industry-test.sh all`. `./scripts/como-el-ci.sh rapido` is not one of them: it is a few seconds of offline checks for the first green and for the stop hook, never a verdict; `parcial` (rows P, B1, CT, DBG; minutes, with a turn) is not a verdict either. Whether a receipt holds for `HEAD` after a documentation-only commit is `bash scripts/recibo-vale.sh`.

GitHub Actions is **not** the merge judge. A green badge is not mergeable. That workflow excludes `MODEL_OR_CLI_ONLY` and has no ONNX / NLI / reranker / generative LLM. `ci.yml` runs only when dispatched by hand.

Publishing is **`./scripts/release.sh vX.Y.Z`** and nothing else. When `merge-gate.sh` already passed on that exact commit over a clean tree, it left `~/.cache/cuba-gate/receipts/<sha>` and `release.sh` tags from that receipt without running the gate a second time; otherwise, or with `--rerun-gate`, it runs `merge-gate.sh` on `origin/main`. Either way it pushes an annotated tag carrying `local-gate: MERGE GATE PASSED <sha>` only on a clean exit 0. `publish.yml` reads that line and asks GitHub's CI nothing.

## Two judges, chained

| Judge | What it is | What it is not |
|---|---|---|
| `./scripts/merge-gate.sh` | The SIL. fmt, clippy `-D warnings`, `--ignored`, e2e (no soft-skip), deny, audit, `codigo-muerto`, `crap-gate` floor, `mutants-gate` on mmr/rrf/cache. | A fraction (`cargo test --lib`). GitHub Actions. |
| `./scripts/quality-gate.sh` | CRAP/lizard + `cargo mutants` on `rust/src` **in the diff**. | A substitute for the SIL. Does not run merge-gate. |

The SIL runs its rows as a DAG by default since 2026-11-10 (about 6.5 min alone, 10 min in a shared turn of `pesado -p`); `GATE_DAG=0` forces the series (16.5 min). The first run of a changed table, a run with an operator's `DATABASE_URL` and a run outside `pesado` are a series by themselves: `docs/gate.md` says why.

On Linux with no `DATABASE_URL` the judge starts its own Postgres for each row that names a `cluster:` in `scripts/gate-dag.txt` (one per lane): PGDG 18.6 with pgvector 0.8.7, socket only, `fsync` off, marked `cluster_name=memory-industry-gate-<lane>`, and taken down when the row ends (CI-F2). Versions, the one-time install and the checks that refuse to start are in `docs/gate.md`, «The judge's servers». The judge also exports `MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1`: a command that loses its `DATABASE_URL` exits 2 instead of falling back on the live corpus (T-DBURL).

The Windows judge (GPU, CUDA) runs once per release, before `./scripts/release.sh`, and not on every change (Leandro, 2026-11-05): `docs/gate.md`, «Running the gate on Windows».

`quality-gate` after the SIL when the change touched `rust/src` or Python. A missing lizard / cargo-mutants is exit 2: the hardener does not close.

## Six-pack

Behaviour change: **especificador → implementador → mejorador → arquitecto → endurecedor → qa**.

Handoffs: `.cursor/handoffs/*.yml`. Judge: `./scripts/validar-handoff.sh`. Example: `.cursor/handoffs/handoff.example.yml`.

A handoff carries the cost of its tests (CI-E6): the specifier's spec has a `Level | Seconds | Lane` table, one row per scenario, and the implementer's handoff has `segundos:` (and `carril:` when the slice touches `scripts/`) inside `evidence: |`. `validar-handoff.sh` judges them; the format is in `.cursor/handoffs/handoff.example.yml`.

It judges shape **and the two-pass protocol**. `commit` must exist, not just look hexadecimal, and `tests: written|frozen` is required whenever `commit != none`: `written` fails if no test region moved since that commit, `frozen` fails if one did. A test region is a `#[cfg(test)]` region of a `.rs`, a whole `.rs` under `tests/`, or in a `.sh` the lines between `# --- self-test begin ---` and `# --- self-test end ---` (column 0, the whole line). Every gate script with a `--self-test` mode wraps all of it in them: the block or the function, its helpers and the `case` arm that dispatches it. A script whose dispatch sits outside a region, or whose markers do not pair, is refused, and `local_gate_contract.rs` checks every script that has the mode (`SCRIPTS_WITH_SELF_TEST`) on every run. With several agents in one tree, list the slice in `paths:` and only the files under it are compared. `./scripts/validar-handoff.sh --self-test` proves each guard still refuses its fixture. Until 0.28 this checked shape only, so the green pass rested on the agent being honest; until 0.31 the self-tests of the gate scripts were frozen by hand with `sed` and `cmp`.

The parent dispatches. The parent **may** write product code when Mapupita asked this chat to implement, or when there is no `Task`. Rules, gate scripts, and `AGENTS.md` are always in-parent.

Do not copy plant roles: no guardian-planta, no Playwright planta, no React Query.

## Comments stay

Comments in this crate are load-bearing (frozen migrations, SHA-384, gate lessons, RLS). Do not strip them. Do not apply Mapupita-Rust «cero comentarios». Rename a comment that only restates the line; keep the why.

## Generative LLM (easy path)

```bash
memory-industry llm list
memory-industry llm set ollama                    # local free
memory-industry llm set deepseek --key sk-...     # or qwen / moonshot / zhipu / …
memory-industry llm status
```

That writes a small config file and loads it automatically on every start. `doctor` reports `generative_llm`.

Also: MCP sampling from the host (Cursor/Claude/Gemini), or any custom `MEMORY_INDUSTRY_LLM_BASE_URL`.

Encode models (embed / NLI / reranker) stay ONNX via `memory-industry models all`.

## Blind gate extras

- E2E must cover tools without soft-skip
- `scripts/crap-gate.sh` — coverage floor (inside the SIL)
- `scripts/mutants-gate.sh` — kill-rate floor on mmr/rrf/cache (inside the SIL). An unviable mutant counts only if rustc refused it with a diagnostic, and a caught one only if its test run exited 1..255 (or overflowed its stack); a build or test run the machine killed fails the run (`--check-builds`, also used by `quality-gate.sh`)
- `~/.cache/cuba-gate/run.exit` — `running <owner>` while a gate runs, then `merge-gate.sh`'s own exit code; a gate killed in the middle leaves `running`
- Oracles / fixtures decide green — not an AI opinion

## Immutable fixtures

Mutating tests use throwaway DBs (`brain_gate` / peer). No judge step reads or writes the live corpus (`:5488`), not even read-only: the eval smoke scores a synthetic fixture (`rust/eval-datasets/smoke-corpus.jsonl`) that `scripts/eval_seed.py` seeds through MCP, by the binary itself, into the judge's own `brain_gate_eval`. The nightly copy of the live corpus is `respaldo-memoria.timer` (systemd user unit, outside the repo), not a judge step.

Published migrations through **0060** are frozen (SHA-384). Wrong shipped SQL gets a **new** migration.

## Product ids

- Display / `serverInfo.name`: **MemoryIndustry**
- Bin / npm / mcpServers key: `memory-industry`
- Crate: `memory_industry`
- Compat: `cuba-memorys` binary (Cargo `[[bin]]` + npm `bin` alias), the `cuba-memorys` package on npm and PyPI, legacy `CUBA_*` env and the `~/.cache/cuba-memorys/` read fallback, through 0.28.x. The systemd units `cuba-memorys.service/.socket` left `packaging/` in 0.28.0; `doctor` still recognises a running `cuba-memorys-daemon`, and nothing uninstalls the old units for the user
- MCP tool names `cuba_*` unchanged this release

## Tracking

- Progress lives in `backlog/` (the source of truth for what is done); git is the source of truth for the code.
- Notion is the management view: the MemoryIndustry space, https://app.notion.com/p/410c9ec4d5d2814ea7e2c981c25ae20b (Cortes, Tareas, Decisiones, Bugs y lecciones, Despliegues, Conocimiento). A task is not opened, moved or closed in one without the other.
- Judge timings as the project grows: when closing a batch, adding tests or steps to the judge, or when a run takes longer than before, apply the `ci-escala` skill. This repo's starting numbers, budgets and guards are in `docs/ci/metodologia-de-escala.md` (internal, kept local like the rest of `docs/*`). Of its guards CI-E1, E4, E6, E7a and E8 are done; CI-E2, E3, E5 and E7b are still open (`backlog/ci-rapido.md`).
