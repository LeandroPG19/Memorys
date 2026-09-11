# Agent contract — MemoryIndustry

## Mergeable (local CI only)

Mergeable means **`./scripts/merge-gate.sh`** (or `./scripts/memory-industry-test.sh all`) exited 0 in this turn with a clean log: no `SKIPPED`, no soft alerts, no “should pass”.

GitHub Actions is **not** the merge judge.

## Generative LLM (easy path)

For humans and agent installers — **one command**, no hunting env vars:

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
- `scripts/crap-gate.sh` — coverage floor
- `scripts/mutants-gate.sh` — kill-rate floor
- Oracles / fixtures decide green — not an AI opinion

## Immutable fixtures

Mutating tests use throwaway DBs (`brain_gate` / peer). Eval smoke may read the live corpus read-only.

## Product ids

- Display / `serverInfo.name`: **MemoryIndustry**
- Bin / npm / mcpServers key: `memory-industry`
- Crate: `memory_industry`
- Compat: `cuba-memorys` binary + legacy `CUBA_*` env / cache paths for one release
- MCP tool names `cuba_*` unchanged this release
