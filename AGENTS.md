# Agent contract — MemoryIndustry

Claude Code / Cursor / CLI: if a global rule (`rust.mdc`, «cero comentarios», plant e2e) clashes with this file, **this repo wins**.

## Mergeable (local CI only)

Mergeable means **`./scripts/merge-gate.sh`** exited 0 in this turn with a clean log: no `SKIPPED`, no soft alerts, no “should pass”.

Aliases of the same SIL: `./scripts/como-el-ci.sh todo`, `./scripts/como-el-ci.sh extra`, `./scripts/memory-industry-test.sh all`.

GitHub Actions is **not** the merge judge. A green badge is not mergeable. That workflow excludes `MODEL_OR_CLI_ONLY` and has no ONNX / NLI / reranker / generative LLM. `ci.yml` runs only when dispatched by hand.

Publishing is **`./scripts/release.sh vX.Y.Z`** and nothing else: it runs `merge-gate.sh` on `origin/main`, and only on a clean exit 0 pushes an annotated tag carrying `local-gate: MERGE GATE PASSED <sha>`. `publish.yml` reads that line and asks GitHub's CI nothing.

## Two judges, chained

| Judge | What it is | What it is not |
|---|---|---|
| `./scripts/merge-gate.sh` | The SIL. fmt, clippy `-D warnings`, `--ignored`, e2e (no soft-skip), deny, audit, `codigo-muerto`, `crap-gate` floor, `mutants-gate` on mmr/rrf/cache. | A fraction (`cargo test --lib`). GitHub Actions. |
| `./scripts/quality-gate.sh` | CRAP/lizard + `cargo mutants` on `rust/src` **in the diff**. | A substitute for the SIL. Does not run merge-gate. |

`quality-gate` after the SIL when the change touched `rust/src` or Python. A missing lizard / cargo-mutants is exit 2: the hardener does not close.

## Six-pack

Behaviour change: **especificador → implementador → mejorador → arquitecto → endurecedor → qa**.

Handoffs: `.cursor/handoffs/*.yml`. Judge: `./scripts/validar-handoff.sh`. Example: `.cursor/handoffs/handoff.example.yml`.

It judges shape **and the two-pass protocol**. `commit` must exist, not just look hexadecimal, and `tests: written|frozen` is required whenever `commit != none`: `written` fails if no `#[cfg(test)]` region moved since that commit, `frozen` fails if one did. `./scripts/validar-handoff.sh --self-test` proves each guard still refuses its fixture. Until 0.28 this checked shape only, so the green pass rested on the agent being honest.

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
- `scripts/mutants-gate.sh` — kill-rate floor on mmr/rrf/cache (inside the SIL)
- Oracles / fixtures decide green — not an AI opinion

## Immutable fixtures

Mutating tests use throwaway DBs (`brain_gate` / peer). Eval smoke may read the live corpus read-only.

Published migrations through **0060** are frozen (SHA-384). Wrong shipped SQL gets a **new** migration.

## Product ids

- Display / `serverInfo.name`: **MemoryIndustry**
- Bin / npm / mcpServers key: `memory-industry`
- Crate: `memory_industry`
- Compat: `cuba-memorys` binary + legacy `CUBA_*` env / cache paths for one release
- MCP tool names `cuba_*` unchanged this release
