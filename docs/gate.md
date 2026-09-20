# Local merge gate

The only merge judge is `./scripts/merge-gate.sh` (aliases: `./scripts/como-el-ci.sh todo`, `./scripts/memory-industry-test.sh all`) exiting 0 **in this turn**, with a clean log: no `SKIPPED`, no soft alerts, no “should pass”.

GitHub Actions is not the merge judge. A green badge alone does not make a change mergeable.

Second judge, chained, not a substitute: `./scripts/quality-gate.sh` (lizard + `cargo mutants` on `rust/src` in the diff). See [AGENTS.md](../AGENTS.md).

## What it runs

| Step | What it proves |
|---|---|
| Postgres :5488 | The cluster is up before anything mutates |
| Optional backup | Live corpus is snapshotted unless `SKIP_BACKUP=1` |
| `scripts/run-all-tests.sh` | fmt, clippy `-D warnings`, unit + ignored integration on throwaway `brain_gate` / peer, e2e MCP + live (no soft-skip), eval smoke |
| `--features docs` | clippy + tests of the docs feature |
| `cargo deny` | licenses, bans, sources |
| npm wrapper smoke | `npm/install.test.js` |
| `cargo audit` | known Rust advisories |
| `codigo-muerto` | dead code / unused deps |
| `scripts/crap-gate.sh` | coverage floor (`CRAP_MIN_LINE_COV`, default 15%) |
| `scripts/mutants-gate.sh` | kill-rate floor on `src/search/{mmr,rrf,cache}.rs` |
| `scripts/quality-gate.sh` | **not** in the SIL — second judge: lizard + mutants of the `rust/src` diff |

Oracles and fixtures decide green — not an AI opinion.

## What it does **not** check

Read this before trusting a green run (copied from the gate banner):

- **Reranker in E2E.** `e2e_all_tools.py` points `CUBA_RERANKER_PATH` at an empty dir so calls exercise the identity fallback. `mcp_live_session_test.py` inherits your shell and **does** use the real reranker. Both suites must stay.
- **GPU placement.** A CUDA build can still run work on CPU; nothing fails if it does.
- **Retrieval quality.** Eval is a smoke run. It proves the harness executes; it asserts no nDCG threshold.
- **Other platforms.** The judge runs on the merge machine (typically Linux x64).
- **Migrations on old DBs.** The throwaway database is created from scratch. A migration that only works on an existing schema is still not covered.

## Fixtures

- Mutating tests write only to throwaway databases (`brain_gate` / peer). Never the live corpus.
- Eval smoke may read the live corpus **read-only**.
- Published migrations through **0060** are frozen (SHA-384). Wrong shipped SQL gets a **new** migration. Do not edit `0017`–`0060`.

## Required machine

Missing any of these is **FAIL**, never `SKIPPED`:

- ONNX embed + ORT (`memory-industry models all`)
- NLI + reranker
- Generative LLM: `memory-industry llm set …` or `MEMORY_INDUSTRY_LLM_BASE_URL` or authenticated `claude`/`gemini` or MCP sampling
- `cargo-deny`, `cargo-machete`, `cargo-llvm-cov`, `cargo-audit`

## Isolation extras (0.26)

`rust/tests/v041_isolation.rs` (`--ignored`) is the contract for:

- `cuba_jornada start` B while A is still open, under `cuba_app` (session is the control plane)
- `cuba_faro scope=errors` returns only errors
- `cuba_decreto query` on the exact title has `count >= 1`
- leftover `project_id NULL` rows stay hidden unless `include_unscoped=true`
- the same entity name can exist in two projects
- `Mcp-Session-Id` splits two chats that share `Mcp-Client-Id`
- quarantined observations do not surface in faro

E2E asserts `cuba_decreto` query `count >= 1` after record. That used to pass with count 0.

## Programming rules (this repo)

Versioned under `.cursor/rules/` (never a junction to `~/.cursor/rules`). Six-pack + TDD + two judges. Comments stay. Plant rules (Playwright, React Query, guardian-planta) are not copied here.

Handoffs: `.cursor/handoffs/*.yml`, validated by `scripts/validar-handoff.sh`.
