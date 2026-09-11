<!-- mcp-name: io.github.LeandroPG19/memory-industry -->
# MemoryIndustry

Formerly **cuba-memorys**. Same daemon, same `cuba_*` MCP tools, new product name.

[![CI](https://github.com/LeandroPG19/Memorys/actions/workflows/ci.yml/badge.svg)](https://github.com/LeandroPG19/Memorys/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/memory-industry?logo=pypi&logoColor=white&label=PyPI)](https://pypi.org/project/memory-industry/)
[![npm](https://img.shields.io/npm/v/memory-industry?logo=npm&logoColor=white&label=npm)](https://www.npmjs.com/package/memory-industry)
[![MCP Registry](https://img.shields.io/badge/MCP_Registry-published-8A2BE2)](https://registry.modelcontextprotocol.io)
[![Rust](https://img.shields.io/badge/rust-1.93+-orange?logo=rust&logoColor=white)](https://rust-lang.org)
[![PostgreSQL](https://img.shields.io/badge/PostgreSQL-18-336791?logo=postgresql&logoColor=white)](https://postgresql.org)
[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue)](https://www.apache.org/licenses/LICENSE-2.0)

**Long-term memory for AI coding agents.** An MCP server that gives your agent a knowledge graph it can search, reason over, and be corrected by — so it stops forgetting your codebase between sessions.

Written in Rust. Backed by PostgreSQL + pgvector. **31 MCP tools** (32 with `CUBA_DOCS=1`), **23 CLI commands**, and every number below measured on a benchmark that — as of v0.12 — actually measures what it claims to. (The previous one did not. See [Measured](#measured--and-the-benchmark-that-was-lying).)

<p align="center">
  <img src="assets/demo.gif" alt="MemoryIndustry terminal demo — hybrid search, claim verification with an LLM judge, procedural memory, and the CLI" width="760" />
</p>

---

## Install

```bash
pip install memory-industry        # or: npm install -g memory-industry
claude mcp add memory-industry -- memory-industry

# Previous names still install the same binary:
#   pip install cuba-memorys
#   npm install -g cuba-memorys
```

That is the whole setup. On first run it provisions a PostgreSQL 18 + pgvector container via Docker and initializes the schema. **[Docker](https://docs.docker.com/get-docker/) must be running.** The `cuba-memorys` command remains a binary alias.

<details>
<summary><b>Cursor / Windsurf / VS Code / Zed</b></summary>

```json
{
  "mcpServers": {
    "memory-industry": {
      "command": "memory-industry"
    }
  }
}
```

No `DATABASE_URL` needed. Or run `cuba-memorys setup` (or `memory-industry setup`) and it writes the config for every client it finds — then `cuba-memorys setup check` audits them for disagreement, which is the failure that actually bites (two configs, two embedding dimensions, one silently broken search).
</details>

<details>
<summary><b>Bring your own PostgreSQL</b></summary>

```json
{
  "mcpServers": {
    "memory-industry": {
      "command": "memory-industry",
      "env": { "DATABASE_URL": "postgresql://user:pass@localhost:5432/brain" }
    }
  }
}
```
Needs the `vector` and `pg_trgm` extensions. `cuba-memorys doctor` will tell you if anything is missing.
</details>

<details>
<summary><b>One shared daemon instead of one process per client</b></summary>

stdio gives every client its own process, and every process loads its own copy of the models — embeddings, reranker and NLI together are several GB. Three editor windows meant three copies, and on a 16 GB laptop that is the whole machine.

`serve` loads them once and answers every client over loopback HTTP, which is also the shape the [2026-07-28 MCP specification](https://blog.modelcontextprotocol.io/posts/2026-07-28/) settled on: no session handshake, every request self-describing.

```bash
cuba-memorys serve                      # 127.0.0.1:8787 by default
cuba-memorys serve 127.0.0.1:9000       # or pick the address
```

`memory-industry serve` is the same command. Point every client at it, and give each one its own `Mcp-Client-Id` so their sessions stay separate — without it `jornada start` in one window becomes the active session of the next:

```json
{
  "mcpServers": {
    "memory-industry": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp",
      "headers": { "Mcp-Client-Id": "editor-window-1" }
    }
  }
}
```

`GET /health` reports uptime, database reachability and the clients seen so far. `CUBA_HTTP_ADDR` overrides the address; `CUBA_HTTP_TOKEN` requires `Authorization: Bearer`, and is mandatory if you bind anything other than loopback — the daemon serves the entire graph with no authentication by default.

Models load in the background *after* the port opens, so a client that connects during startup waits on its first search instead of timing out the connection. Under stdio that timeout was how you ended up with abandoned multi-GB processes: the client gives up at 30 s but never closes stdin, so the server sat there holding every model it had loaded. Stdio now exits if no handshake arrives within `CUBA_HANDSHAKE_TIMEOUT_SECS` (60 s, `0` disables).
</details>

<details>
<summary><b>Semantic embeddings & models (recommended)</b></summary>

Without a model, embeddings are hash-based: deterministic, and semantically meaningless. Search still works through the lexical and BM25 branches, but nothing understands *meaning*.

One command installs the models and the ONNX runtime, on any OS — no shell scripts, no manual `ORT_DYLIB_PATH`:

```bash
cuba-memorys models all          # embeddings + NLI + reranker + runtime
cuba-memorys models embed        # just the embeddings model (~113 MB)
cuba-memorys models all --gpu    # GPU runtime, if you have one
cuba-memorys doctor              # confirms what loaded
```

Everything lands in `~/.cache/cuba-memorys/` and is found automatically. `models` downloads only when you run it — nothing is fetched behind your back.

**bge-m3 (1024-d) is better than e5-small** for Spanish, though the size of the gap is no longer claimed (the old +21 nDCG figure came from a broken benchmark). It needs a dimension migration (`scripts/migrate-embedding-dim.sh 1024`) and `CUBA_EMBED_MODEL=bge-m3 CUBA_POOLING=cls`.
</details>

<details>
<summary><b>Modes: local · red · completo</b></summary>

`CUBA_MODE` is a preset that sets the database, the models, and outbound network together, so you pick one name instead of lining up a dozen env vars:

| `CUBA_MODE` | Database | Capabilities | Network out |
|---|---|---|---|
| `local` (default) | Docker on this machine | embeddings + NLI as installed | none |
| `red` | shared managed Postgres (set `DATABASE_URL` with `sslmode=require`) | + provenance per node, real-time sync between machines | none |
| `completo` | whatever `DATABASE_URL` implies | **+ reranker (GPU if present) + `cuba_docs`** | `cuba_docs` |

**Two machines, one memory.** Point both at the same managed Postgres (Neon or Supabase free tier both have pgvector and fit the 36 MB corpus many times over), give each a name with `CUBA_NODE_NAME`, and `CUBA_MODE=red`. What one writes, the other reads; every memory records which machine it came from (`origin_node`). Without a shared database, `cuba_sync` does the same job through a git repository — see [Sync between machines](#sync-between-machines-through-git). Do **not** expose your own Postgres port to the internet — use a managed provider's TLS, or a private network like [Tailscale](https://tailscale.com).

**Real isolation when you share.** A shared database is where row-level security stops being decorative. Run `cuba-memorys secure` once (as the admin role) to create a non-superuser `cuba_app` with RLS and append-only audit actually enforced, then point the runtime at it with `CUBA_SKIP_MIGRATIONS=1`. `cuba-memorys doctor` reports whether the runtime role is a superuser (which bypasses all of it) or not.

**Maximum capability.** `CUBA_MODE=completo` turns on the cross-encoder reranker (+93% nDCG) and `cuba_docs`. **The reranker no longer needs that mode when the machine can actually run it**: a build with a GPU provider that finds a working device turns it on by itself, because that is where it fits its budget. On CPU it stays off by default — the table below is why — and `cuba-memorys doctor` says which of the three reasons applies. Asking for `rerank: true` in the call still overrides everything. On CPU `faro` time-boxes it and falls back to the RRF ranking (`CUBA_RERANK_TIMEOUT_SECS`, default 20 s), so a slow machine still answers. GPU binaries ship with CUDA (NVIDIA) and, on Windows, DirectML (any GPU) — `cuba-memorys models runtime --gpu` fetches the accelerated runtime.

Fetching the GPU runtime is only half of it: **the binary itself has to be built with `--features cuda`**, or `gpu::configure()` registers no provider and the reranker runs on CPU. That is not a hypothetical — it is what a 50-candidate rerank costs on a 6-core laptop, measured with `cargo run --release --example rerank_bench`:

| build | 50 candidates, mixed lengths | inside the 20 s budget? |
|---|---|---|
| CPU, `with_intra_threads(2)` | 106,9 s | no — scores computed, then discarded |
| CPU, physical cores | 61,0 s | no |
| **`--features cuda`** | **4,1 s** | **yes** |

Same ranking either way — CPU and GPU agree candidate for candidate, differing only in the fifth decimal of the score. Run `rerank_bench` on any machine to see whether the reranker fits its budget there or is silently throwing the work away, and `cuba-memorys doctor` reports whether this build has a GPU provider at all.

**This section used to say "every model quietly runs on CPU", implying all three would run on the GPU once you built with `--features cuda`. Only the reranker ever did.** The embedder ships dynamically quantised to INT8, which means 96 `DynamicQuantizeLinear` feeding 144 `MatMulInteger` — and the CUDA provider registers no kernel for either, so ONNX Runtime partitions them onto the CPU no matter what you build. Registering CUDA for that session bought nothing and cost a VRAM arena the model never computed in: **374 MiB held while all 544 MB of weights sat in host RAM.** The NLI cross-encoder has the opposite problem — it is FP32 and stuck there, because mDeBERTa is documented upstream as not supporting FP16 and the INT8 build returns confident false entailments.

So placement is now decided per model rather than once per process, and only the reranker asks for the GPU. On the 6 GB card this was measured on, the daemon went from **5228 MiB of VRAM to 2950 MiB while searching, and 0 while idle** — and down to 1460 MiB with the two opt-in steps in [Footprint](#footprint) below.

Individual env vars (`CUBA_DOCS`, `CUBA_RERANKER_PATH`, …) always override the preset.
</details>

---

## What it actually does

Most memory servers are a key-value store with an embedding bolted on. This one models four kinds of memory, because the psychology literature says they are four different things and they decay differently:

| | What it holds | How it strengthens |
|---|---|---|
| **Semantic** | Facts about entities — "all endpoints are async" | Access (Hebbian/BCM, Oja 1982) |
| **Episodic** | Events with actors and time — "we shipped v2 on Tuesday" | Power-law decay (Tulving 1972, Wixted 2004) |
| **Procedural** | How things are *done* here — recipes with a track record | **Success**, not access (ACT-R) |
| **Working** | Scratch notes bound to the current session | Cleared with the session |

Procedural memory is a separate table rather than a ninth observation type for a specific reason: ACT-R separates declarative memory (reinforced by *access*) from procedural (reinforced by *success*). As an observation, a recipe consulted constantly *because it keeps failing* would climb in importance. It is ranked by **Wilson lower bound**, so 1/1 successes scores 0.21 and 47/50 scores 0.84 — a lucky first try does not outrank a track record.

### Retrieval

Hybrid RRF fusion (k=60, Cormack 2009) over three signals — full-text, BM25 (`ts_rank_cd`), and pgvector HNSW — with entropy-routed weighting that shifts from keyword-heavy to semantic as the query's Shannon entropy rises.

Answers arrive in **`compact` by default**: abbreviated keys, content truncated at 1200 chars. **30% fewer tokens, and a slightly *better* nDCG** — measured on the 221 id-scored questions, +0.0090 with a paired 95% interval of [+0.0024, +0.0166]. The format genuinely cannot change which documents rank; what it changes is how many of them survive the response token budget before they are scored. Verbose at the default 5000-token budget weighs 5286 tokens and loses its tail; compact weighs 3723 and keeps it. Pass `"format": "verbose"` for the full per-branch score breakdown.

### Verification that actually verifies

`cuba_faro mode=verify` checks a claim against what is stored. It used to score claims by **cosine similarity to the retrieved evidence**, and that does not work — similarity measures what a text is *about*, not what it *asserts*. "cuba-memorys is written in Rust" and "…in Java" are nearly the same vector. Measured on the live corpus, the **false claim scored 0.61 and the true one 0.59.**

Entailment is a different question from similarity, and it needs something that *reads*. A local cross-encoder now judges each piece of evidence — `supports` / `contradicts` / `unrelated` — and confidence is derived from the verdicts, each weighted by that evidence's similarity. Same corpus, after:

| Claim | Before (cosine) | Now |
|---|---|---|
| "written in Rust" (true) | 0.59 | **0.995 · verified** |
| "written in Java" (false) | **0.61** | **0.00 · contradicted** |
| "the best paella uses saffron" (unrelated) | 0.45, with 10 "evidence" items | **0.00 · unknown**, no evidence |

Being on-topic is not support, and `unrelated` counts for **neither** side.

The judge is **mDeBERTa-v3-base-xnli** running locally on ONNX: 100 languages, ~50 ms per verdict, no API key, no network, no cost. That matters here — about 75% of this corpus is Spanish, and the English-only NLI models everyone reaches for first would have silently failed on three memories out of four. Install it with `cuba-memorys models nli`; `cuba-memorys doctor` will tell you whether it loaded.

Without it, verification falls back to an LLM (your MCP client's own model via sampling, a local `claude` CLI, or the Anthropic API) — and with none of those, to an honest `unknown` rather than an invented verdict.

Two things it will not do. It will not **confirm** a claim on weak evidence: entailment must clear 0.80 while contradiction needs only 0.60, because confirming a false memory and doubting a true one are not errors of equal cost. And when it cannot tell, it says so instead of returning whichever number came out largest — an argmax over a 3-way head will happily publish `supports` for a claim that is flatly false, and did.

### Calibrated abstention

The out-of-distribution gate rejects queries the corpus cannot answer. The threshold is **not** a magic constant: Ledoit-Wolf covariance shrinkage plus a conformal quantile, calibrated against your own corpus with `cuba-memorys calibrate --dataset <questions.jsonl> --apply` and persisted (the dataset is required — without it the command refuses). (The theoretical χ² threshold rejected **100% of answerable queries.** Distribution-free calibration is not a nicety here.)

### Sync between machines, through git

`CUBA_MODE=red` puts two machines on one database. `cuba_sync` is the other route, for machines that never see each other: the graph is written out as JSON you can commit, and read back on the other side.

```bash
cuba-memorys sync export            # write the bundle under .cuba-memorys/
cuba-memorys sync import            # read one back in
cuba-memorys sync diff              # entities on disk vs entities in the database
cuba-memorys sync status            # which bundles this machine has already imported
cuba-memorys hook install           # export after every commit, import after every checkout
```

The same four actions are `cuba_sync action=export|import|diff|status`. A bundle is one JSON file per entity with its observations inside, plus `episodes/YYYY-MM/`, `errors/`, `decisions/`, `relations.json`, `projects.json`, `tombstones.json` and a `manifest.json` — the active project and anything not bound to a project, unless you pass `--scope all`. Embeddings stay out unless you ask for them (`--with-embeddings`): they are most of the bytes and they can be recomputed. A bundle imports once, and the manifest hash covers the contents of every file in it — so an unchanged bundle is skipped, and a hand-edited entity file is a new bundle rather than a silent no-op.

**A deletion travels now, and stops where it would take something with it.** Deleting a row records a tombstone, and the receiving side deletes exactly the ids that were named. Before this, a delete was not slow to arrive — it was undone: the peer still had the row, exported it, and it came back on the next round trip. The **entity** tombstone is the dangerous one, because deleting an entity cascades to everything hanging off it. It is applied only when this machine has no observations or episodes under that entity that the sender never named; otherwise it is withheld and reported in `tombstones_withheld`. A tombstone for an entity with three children there must not take three hundred here.

**And a bundle cannot quietly wipe you.** If the tombstones in it would delete at least 25 rows *and* more than 10% of the observations on this machine, the import refuses and asks for `confirm=true`. A remote wipe and a large legitimate cleanup look identical; the only difference is whether you meant it. The floor matters as much as the ratio: on a database with a single observation a pure percentage demanded confirmation to delete that one, and a guard that trips on ordinary curation is one everybody learns to pass `confirm=true` through — and then it guards nothing.

**`conflict=merge` does not merge content, and now says so.** `merge` and `skip` are one policy: rows that are missing here arrive, and where a row already exists with different content, the one that was here first wins and the incoming text is dropped. What changed is the silence — the import counts those rows and reports them as `diverged`, with their ids and a note saying what it did. `conflict=overwrite` takes the incoming version and **keeps the one it replaced** in `previous_versions` (the newest 20 are kept), and clears the embedding when the content changed, so a row stops being retrievable by a meaning it no longer carries.

**Counters do merge, under either policy.** `importance` and `access_count` on an entity, and `strength` on a relation, are not values one side copies from the other: each machine grows its own, from its own reinforcement and its own traversals. The higher of the two wins, which is idempotent — importing the same bundle twice inflates nothing. (Summing would be more faithful to "both machines counted", and would double on a re-import, so it loses to a rule that cannot corrupt the number.)

**Which machine is which.** Each installation generates a uuid in its own database on first migration — one row, stable across restarts, unique by construction — and the manifest carries it, so a bundle can say which machine produced it. `CUBA_NODE_NAME` keeps meaning what it always meant: a human-readable label stored in `origin_node`. It is not the identity and could not be one, because two machines both called `pop-os` is the likeliest outcome there is.

**The clock ticks for what a peer needs, and stays still for local noise.** An observation's `version` advances when its content, type, trust, evidence level or tags actually change, and for nothing else. Decay moves `importance` and `last_accessed`; `reembed` replaces vectors. If either woke the clock, every export would ship a graph that had not changed and the two machines would never stop talking to each other about nothing. Rewriting a row with the same content does not tick it either, so an idempotent re-import does not invent a conflict out of agreement.

**Older bundles still import.** The format is `SCHEMA_VERSION` 2: `version`, `updated_at`, `origin_node`, `previous_versions`, `evidence`, `verification` and `trust` travel now, because a conflict rule that compares clocks needs the clock to be in the file. Bundles written before that still import — every new field defaults, and a v1 observation lands as `asserted`, which is the honest reading of a file that never claimed anything stronger.

Anything in an incoming bundle that looks like a credential is stored `quarantined` instead of trusted — withheld from `cuba_faro` and `cuba_expediente` until you promote it with `cuba_eco` — because an import reads JSON out of a repository anyone with push access can write to.

**A peer that only ever reads.** `CUBA_PEER_TOKEN` reaches five more verbs and nothing else. `pull` returns the bundle in the response instead of writing it anywhere, paged by file (`limit`, `offset`) up to a 3 MB budget per page — abort if `manifest_hash` changes between pages, because that means this node was written to mid-transfer and the pages describe two different states. `notify` is the one write a peer token may make: a short `summary` (at most 2000 characters) saying the other machine learned something, tagged with `node_id`/`node_name`, surfaces at the next `cuba_jornada start` and in `status`, and closes itself when a bundle carrying its `manifest_hash` is imported — it never enters the graph itself. `conflicts` lists the rows two machines disagree about with both texts, and `resolve id=… keep=ours|theirs|both` closes one: `keep=both` (the default) keeps this machine's text current and files the other in `previous_versions`, discarding nothing, while `theirs` also clears the embedding because it described text that is no longer here. `fetch` is the other half and runs on the local machine: it pages a peer's `pull` over HTTP, lands the files, imports them with the same validation as any bundle, and records the peer's manifest hash so the next `fetch` stops before opening a transaction when nothing changed. Embeddings are omitted by default on export and included by default on `pull` — a peer that receives text without vectors cannot search what it just received until it re-embeds, which on a machine without a GPU is slow and sequential — and a bundle whose model or dimension does not match this machine is refused rather than silently filling the index with vectors from another space.

### And it tells you when it is broken

```
$ cuba-memorys doctor
[  ok  ] migrations           49 aplicadas, ninguna dirty
[  ok  ] embedding_dim        runtime 1024-d == columna vector(1024)
[  ok  ] runtime_role         'cuba_app' sin superuser — RLS y audit efectivos
[ warn ] binary_freshness     4 proceso(s) MCP corren un binario más viejo que el de disco
```

This exists because the failure mode of a hybrid search engine is not a crash — it is a vector branch dying and the search quietly becoming lexical, with no symptom. The server now **refuses to start** on an embedding-dimension mismatch, and search sets `degraded: true` in the response when a branch fails.

---

## The CLI: your memory without an LLM in the middle

Twenty-three commands. `memory-industry --help` lists them all.

| | |
|---|---|
| **`serve`** | One shared HTTP daemon for every client, instead of one process (and one copy of the models) per editor window |
| `search <query>` · `save` · `delete` · `export` | Read and write the brain from a shell |
| `dashboard` | A self-contained HTML view of what is in there |
| **`doctor`** | Health check: schema, dimensions, config coherence, stale processes |
| `recall` | Session-start context injection — wire it with `setup hook` |
| `reembed` | Re-encode what needs it (default: only stale rows, not all of them) |
| `calibrate` | Recompute the abstention threshold from your corpus |
| `link` | Auto-link entities by NPMI co-occurrence |
| **`dedupe`** | Entities that are the same thing under different names — see below |
| **`sync`** · `hook` | Write the graph out as committable JSON and read it back on another machine — see [Sync between machines](#sync-between-machines-through-git). `hook install` wires it to git |
| `skills <dir>` | Export procedures as Claude Code Skills |
| `eval` | Retrieval benchmark — nDCG@10 with confidence intervals, MRR, recall, token cost |
| `setup` | Wire this into your MCP clients; `setup check` audits them |

### `dedupe` — because a different string is a different entity

`cuba_alma create` inserts with `ON CONFLICT (name)`. So one project fragments into `Mapupita-Web`, `Mapupitta-Web` (typo), `Mapupita Web`, `mapupita`… and searching one finds none of the others. On a real 266-entity graph, **158 of them (59%) had not a single relation** — for PageRank and multi-hop retrieval, they did not exist.

What decides a merge is **not** the embedding centroid. That was the obvious idea and it is wrong: `M-Codes Reference Guide` and `G-Codes Reference Guide` sit at **0.811 cosine** between centroids. On a corpus about one domain, centroid similarity measures the *domain*, not the *entity* — a 0.80 threshold would have merged two different CNC guides, irreversibly.

So `--apply` merges only what is **provable** (identical after normalizing case and separators). Typos and near-matches are shown, and judged one at a time with `--judge`. The old name is written to `brain_entity_aliases`, so nothing is lost: looking it up still resolves.

---

## The 31 tools

Named after Cuban culture. `cuba-memorys` advertises all of them, or set `CUBA_TOOL_PROFILE=lean` to advertise an everyday core of 13 plus `cuba_tools` + `cuba_call` — **15 of 31, a 49% smaller catalogue with zero functions lost**, the rest reachable on demand.

**Knowledge graph** — `cuba_alma` (entities) · `cuba_cronica` (observations, episodes, timeline) · `cuba_puente` (typed relations, traversal, link prediction) · `cuba_ingesta` (bulk import)

**Search** — `cuba_faro` (hybrid RRF, verification, MMR diversification, OOD abstention)

**Error memory** — `cuba_alarma` (report) · `cuba_remedio` (resolve) · `cuba_expediente` (search past errors; warns if an approach failed before)

**Sessions & decisions** — `cuba_jornada` (session lifecycle, diff) · `cuba_decreto` (architecture decisions) · `cuba_proyecto` (per-project isolation) · `cuba_pre_compact` (survive `/compact`)

**Procedural** — `cuba_receta` (recipes ranked by Wilson lower bound)

**Cognition** — `cuba_reflexion` (gap detection) · `cuba_hipotesis` (abductive inference) · `cuba_contradiccion` (semantic conflicts) · `cuba_juez` (LLM judge) · `cuba_centinela` (prospective triggers) · `cuba_calibrar` (Bayesian calibration, source credibility)

**Maintenance** — `cuba_zafra` (decay, prune, merge, PageRank, Leiden communities) · `cuba_eco` (RLHF feedback) · `cuba_vigia` (health, drift, centrality) · `cuba_forget` (GDPR erasure) · `cuba_archivo` (CFR-21 hash-chain audit log) · `cuba_pizarra` (working memory) · `cuba_sync` ([git-friendly export/import between machines](#sync-between-machines-through-git), with propagated deletions and a remote-wipe guard)

**Meta** — `cuba_tools` (discover) · `cuba_call` (invoke)

---

## Configuration

| Variable | Default | What it does |
|---|---|---|
| `CUBA_MODE` | `local` | `local` / `red` (shared cloud DB) / `completo` (everything + GPU). A preset for the rest. |
| `CUBA_NODE_NAME` | `$HOSTNAME` / `$COMPUTERNAME` | A human-readable label for this machine, written into `origin_node`. The fallback is `$HOSTNAME`, which a shell does not export to child processes, so on Linux `origin_node` stays empty unless you set this. It is **not** this installation's identity: that is a uuid generated in its own database, because two machines can easily choose the same name |
| `DATABASE_URL` | auto (Docker) | PostgreSQL connection. Set it (external + TLS) for `red` mode. |
| `ONNX_MODEL_PATH` + `ORT_DYLIB_PATH` | auto (`~/.cache`) | Semantic embeddings. `cuba-memorys models` sets these up for you. |
| `RUST_LOG` | `cuba_memorys=info` | Log level, read by `tracing`'s `EnvFilter`. Logs go to **stderr** — on stdio transport, stdout is the JSON-RPC channel and anything else printed there breaks the client. `cuba_memorys=debug` for per-handler detail, `sqlx=debug` to see every query. |
| `CUBA_EMBED_MODEL` · `CUBA_EMBEDDING_DIM` · `CUBA_POOLING` | `multilingual-e5-small` · `384` · `mean` | Set to `bge-m3` · `1024` · `cls` for the stronger Spanish model |
| `CUBA_QUERY_PREFIX` · `CUBA_PASSAGE_PREFIX` | `query: ` · `passage: ` | Instruction prefixes prepended before tokenising. E5 was trained with them; `bge-m3` was not — set both to the empty string when you switch, or every vector is computed on text the model never saw that way |
| `CUBA_CHUNK_THRESHOLD_CHARS` · `CUBA_CHUNK_CHARS` | `1800` · `1400` | Content longer than the threshold is split into chunks of this many characters (200-char overlap). `CUBA_CHUNK_CHARS` is floored at 200. A value that is not a positive integer falls back to the default |
| `CUBA_EMBED_CONCURRENCY` | `1` | Permits on the semaphore around the ONNX embedding session. Sized once, on first use |
| `CUBA_TOOL_PROFILE` | `full` | `lean` → 15 tools of 31, 49% smaller catalogue, nothing lost. The thirteen are the everyday core (incl. whoami/artefacto/contexto); the rest stay reachable through `cuba_call` |
| `CUBA_JUDGE` · `MEMORY_INDUSTRY_JUDGE` | `auto` | `nli` / `mcp_sampling` / `claude_cli` / `heuristic` / named OpenAI-compat providers. `MEMORY_INDUSTRY_*` is the preferred name; `CUBA_*` still works |
| `CUBA_JUEZ_CLI` · `MEMORY_INDUSTRY_LLM_CLI` · `CUBA_JUEZ_MODEL` · `MEMORY_INDUSTRY_LLM_MODEL` | `claude` · `claude-haiku-4-5` | The CLI the offline judge shells out to, and the model it asks for. `CUBA_JUEZ_CLI` / `MEMORY_INDUSTRY_LLM_CLI` also decide the automatic path: if that name is not on `PATH` there is no CLI judge and the choice falls through |
| `CUBA_JUEZ_TIMEOUT_SECS` · `MEMORY_INDUSTRY_LLM_TIMEOUT_SECS` | `30` | Budget for one judgement, CLI and API alike. Anything that does not parse as an integer leaves the default |
| `CUBA_JUEZ_MAX_PAIRS` · `MEMORY_INDUSTRY_LLM_MAX_PAIRS` | `5` | Candidate pairs `cuba_juez` sends per call |
| `MEMORY_INDUSTRY_LLM_PROVIDER` · `CUBA_LLM_PROVIDER` | unset | Named cloud/local preset (`deepseek`, `qwen`, `ollama`, …) or `openai_compat` |
| `MEMORY_INDUSTRY_LLM_BASE_URL` · `CUBA_LLM_BASE_URL` | unset | OpenAI-compatible `/v1` base URL (Ollama, vLLM, vendor gateways) |
| `MEMORY_INDUSTRY_LLM_API_KEY` · `CUBA_LLM_API_KEY` | unset | Bearer for that base. Vendor fallthroughs: `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `DASHSCOPE_API_KEY` (and other preset keys) |
| `CUBA_NLI_PATH` | `~/.cache/cuba-memorys/models-nli` | Local entailment model (`cuba-memorys models nli`) |
| `CUBA_NLI_ESCALATE` · `MEMORY_INDUSTRY_NLI_ESCALATE` | off | Send claims the NLI could not decide to an LLM. Buys recall, costs ~12 s each |
| `MEMORY_INDUSTRY_GRAPH_DB` · `CUBA_GRAPH_DB` | `off` | Optional graph projection: `falkor` / `neo4j` / `off`. Postgres remains source of truth |
| `MEMORY_INDUSTRY_GRAPH_URL` · `CUBA_GRAPH_URL` | unset | Graph endpoint (`redis://…` for FalkorDB) |
| `MEMORY_INDUSTRY_GRAPH_NAME` · `CUBA_GRAPH_NAME` | `memory_industry` | Falkor/RedisGraph graph key. The gate keeps this unset/`off` so throwaway writes never land on the live graph |
| `MEMORY_INDUSTRY_ENTITY_FACTOID` | `on` | Extra RRF leg for factoid queries that mention an entity. Set `off` / `0` / `false` to keep the hybrid ranking unchanged |
| `CUBA_RERANKER_PATH` · `CUBA_RERANK_TIMEOUT_SECS` | `~/.cache/…/reranker` · `20` | Cross-encoder reranker (+93% nDCG); on CPU it falls back to RRF past the budget |
| `CUBA_RERANK_INTRA_THREADS` | physical cores (2 on GPU) | ONNX threads per rerank inference. Past the physical core count it gets *slower* — measure with `rerank_bench` before raising it |
| `CUBA_RERANK_LENGTH_BUCKETING` | on (off under fixed shape) | Batch similar-length candidates so padding does not become compute. Scores are unchanged |
| `CUBA_RERANK_CHUNK` | `16` | Candidates per forward pass. Under fixed shapes every batch pads to 512 tokens, making this the main lever on the GPU arena: `16` → 2938 MiB, `4` → 2364 MiB. Scores are unchanged — a verbose search at 16 and at 4 came back byte-identical |
| `CUBA_RERANK_CONCURRENCY` | `1` | Permits on the semaphore around the reranker session. The session is a mutex, so raising this queues callers rather than parallelising them |
| `CUBA_RERANK_BUCKET` | `512` | Rounds the padded sequence length up to a multiple of this. Only `0` or a power of two up to 512 is accepted — anything else leaves the default. `0` pads to the longest candidate instead |
| `CUBA_RERANK_FIXED_SHAPE` | on when the reranker runs on GPU | Pads every batch to the same 512-token shape. `0` / `off` / `false` disables it; any other value enables it. It also flips the default of `CUBA_RERANK_LENGTH_BUCKETING`, which has nothing left to do once every batch is the same size — and it is what makes `CUBA_RERANK_CHUNK` the main lever on VRAM |
| `CUBA_EMBED_DEVICE` · `CUBA_RERANK_DEVICE` · `CUBA_NLI_DEVICE` | `cpu` · `gpu` · `cpu` | Per-model placement. Only the reranker gains from a GPU; the INT8 embedder cannot use one and the FP32 NLI is not worth the VRAM. Set to `gpu`/`cpu` to A/B a placement without rebuilding |
| `CUBA_GPU_MEM_LIMIT_MB` | `2048` | Caps the CUDA arena and pins `arena_extend_strategy` to `SameAsRequested`. The default (`NextPowerOfTwo`) doubles its reservation on every growth, which is how 1,65 GB of weights became 5+ GB of VRAM. The cap is **per session** |
| `CUBA_EMBED_INTRA_THREADS` | half the logical cores, max 4 | ONNX threads per embedding. Measured on 12 threads: 1 → 94,8 ms, 2 → 52,3 ms, **4 → 35,8 ms**, 6 → 68,1 ms, 12 → 155,4 ms per query |
| `CUBA_IDLE_SHUTDOWN_SECS` | `0` (off) | Exit after this long with no request from any client. Pairs with a systemd `.socket` unit so the next call brings the daemon back — see [Footprint](#footprint) |
| `CUBA_WARM_RERANKER` | off | Load the cross-encoder at startup instead of on its first batch. Off, a cold start costs 0,027 s instead of 11 s and holds no VRAM until something actually reranks |
| `CUBA_HTTP_ADDR` · `CUBA_HTTP_TOKEN` | `127.0.0.1:8787` · unset | Address for `serve`, and the bearer token it requires. A token is mandatory to bind anything but loopback |
| `CUBA_PANEL` | unset | Set to `1` and `serve` also answers `GET /panel`: a control page compiled into the binary that reads the daemon's state, connected clients, recent calls and open problems. It carries no data of its own — everything it shows it asks for over `POST /mcp` with the same bearer token as any MCP client, so there is no second endpoint to protect. Off by default |
| `CUBA_PANEL_PUBLIC` | unset | Without it, `/panel` refuses any request carrying a forwarding header (`Forwarded`, `X-Forwarded-For`, `CF-Connecting-IP` and six more) — the signature of an HTTP proxy. The Cloudflare tunnel connects to `127.0.0.1`, so the client address is loopback either way and only the header tells the two apart. **What it does not catch**: a raw TCP forward (`ssh -L`, `socat`, `ngrok tcp`) adds no header and is indistinguishable from a local request, so this stops HTTP proxies rather than proving a request is local. Set to `1` to publish the panel deliberately |
| `CUBA_PEER_URL` | unset | Default address of the other daemon for `cuba_sync action=fetch`, e.g. `https://brain.example.net`. Only a fallback: the address is remembered per peer name after the first successful fetch |
| `CUBA_PEER_TOKEN` | unset | A second bearer token for another machine that syncs with this one. It reaches only the sync verbs — never `cuba_forget`, `cuba_zafra prune` or `cuba_sync import` — so a peer can read what this node knows and cannot write or delete a single row. Must differ from `CUBA_HTTP_TOKEN`, which is also the tunnel's; `serve` refuses to start if they match |
| `CUBA_HANDSHAKE_TIMEOUT_SECS` | `60` | stdio exits if no MCP handshake arrives, instead of holding the models for a client that gave up. `0` disables |
| `CUBA_HANDLER_TIMEOUT_SECS` | `30` | Ceiling on one tool call. It is also the budget the LLM extraction inside `cuba_ingesta` gets, at 60% of this value — raising it lets extraction think longer |
| `CUBA_DOCS` | **off** | `1` enables `cuba_docs`, the only tool that leaves your machine. Unset, it is not even advertised. |
| `CUBA_COMPACT_CHARS` | `1200` | Compact truncation (measured knee) |
| `CUBA_OOD_THRESHOLD` | calibrated | Override the abstention threshold |
| `CUBA_BITEMPORAL` | on | Mirror observations into `brain_facts` |
| `CUBA_AUDIT_KEY` | unset → `~/.cache/cuba-memorys/audit_key` | HMAC key for the `cuba_archivo` hash chain. Without a key the chain is plain SHA-256, which anyone with write access to the table can recompute — the entries stay consistent and the forgery is invisible |
| `CUBA_APP_ROLE` | on | After migrations the pool reconnects as the unprivileged `cuba_app` role. `0` / `off` / `false` keeps the admin connection instead — the superuser stays live for the whole session |
| `CUBA_PROJECT_FILTER` | unset (filter on) | `off` (any case) disables per-project scoping: the RLS scope becomes `*` and every project's memories are visible at once. Any other value leaves the filter on |
| `CUBA_QUARANTINE_INFERENCE` | off | `1` / `on` / `true` stores anything with `source=inference` as `quarantined` instead of `trusted`, unless the caller set the trust level explicitly |
| `CUBA_PG_BIND` | `127.0.0.1` | Host address the managed Postgres container publishes its port on. Anything but loopback exposes the database to the network |
| `CUBA_RANDOM_PAGE_COST` · `CUBA_IO_CONCURRENCY` | `1.1` · `200` | Per-connection planner settings for the pool. Accepted ranges are `0.1`–`10.0` and `≤ 1000`; outside them the default stands |
| `CUBA_REM_AUTOLINK` | on | `0` / `off` / `false` stops the REM cycle from creating NPMI co-occurrence edges between entities |
| `CUBA_GATE_MIN_FREE_GB` | `8` | Free disk the gate demands before it compiles anything. Below it, it refuses to start and says so. A run on a 98%-full partition died as `collect2: fatal error: ld terminated with signal 7 [Bus error]` with three test binaries reported as «could not compile» — nothing in that output mentions disk, so it reads as a code failure |
| `CUBA_GATE_SWEEP_BELOW_GB` | `20` | Free disk under which the gate sweeps build artifacts before running. cargo never removes the binaries of earlier compilations — every edit makes a new hash and the old one stays — so `target/debug/deps` grows without bound; it reached 64 GB here |
| `CUBA_GATE_SWEEP_DAYS` | `7` | How old an artifact has to be for that sweep to take it. By age and not by size on purpose: anything this run needs was written today |
| `CUBA_REM_FIRST_DELAY_SECS` | `300` | How long after start-up the first REM consolidation runs. It used to be `REM_INTERVAL` — four hours — because the loop consumed the interval's first tick, which resolves instantly. Under stdio the process rarely lives that long, so the cycle never ran there at all, and every machine restart put the counter back to zero |
| `CUBA_REM_RELATION_BATCH` | `5` | Entities the REM cycle runs a relation scan over per pass. `0` skips the scan. Left unset it adapts: 20 while 50 or more entities are still waiting, back to 5 once the queue drains — 226 pending at 5 per 4-hour cycle is a week |
| `CUBA_REM_SCAN_TIMEOUT_SECS` | `90` | Budget for one entity's relation scan |
| `CUBA_REM_EXTRACTION_BATCH` | `5` | Observations the REM cycle runs `cuba_ingesta auto_extract` over per pass, right after the relation scan. `0` skips it. What it finds is written `trust=quarantined`, always — this is the graph's only fully unattended writer, so nothing it produces is visible to `cuba_faro` until `cuba_eco action=promote` clears it by hand |
| `CUBA_REM_BACKFILL_LIMIT` | `100` | Observations without an embedding that the REM cycle backfills per pass. `0` disables the backfill; a negative value leaves the default |
| `CUBA_SYNC_DIR` | unset → `.cuba-memorys` under the working directory | Root for `cuba_sync` export/import. It is also the confinement boundary: a `--dir` outside this root is refused, so setting it is how you sync somewhere else instead of escaping with `../` |
| `CUBA_UNDO_DIR` | `~/.cache/cuba-memorys/undo` | Where destructive CLI commands write their undo snapshots |

---

## Footprint

A memory server is infrastructure: it is running when you are not using it. On the 6 GB laptop GPU this was measured on, it used to hold **5228 MiB of VRAM from boot** — 93% of the card — and other GPU programs stopped being able to start. The NVIDIA driver was returning `NV_ERR_NO_MEMORY` on channel creation, which is what a game or a GPU-accelerated terminal fails on.

Two of the numbers below ship as defaults; two need a line of config, and this table keeps them apart rather than quoting the best one as if it came free.

| | before | **0.20.0 defaults** | `CUBA_RERANK_CHUNK=4` | + fused artifact |
|---|---|---|---|---|
| VRAM while searching | 5228 MiB | **2950 MiB** | 2364 MiB | **1460 MiB** |
| VRAM idle | 5228 MiB | **0** — the process is gone | 0 | 0 |
| Cold start to answering | 11,1 s | **0,027 s** | 0,027 s | 0,027 s |
| Search, warm | 5,90 s | 5,25 s | 3,73 s | **1,70 s** |
| Embedding one query | 52,3 ms | **35,8 ms** | 35,8 ms | 35,8 ms |

Everything in the defaults column is code that ships. `CUBA_RERANK_CHUNK=4` is one env var. The last column additionally needs the rebuilt reranker described below. None of it removed a feature.

Four things got it there:

**Placement per model, not per process.** Only the reranker is accelerated by a GPU — the INT8 embedder cannot be, and the FP32 NLI is not worth a gigabyte of VRAM for a judge that runs occasionally and tolerates 150-400 ms. The arena cap is per session, so three sessions asking for CUDA on a 6 GB card is a 3× overcommit waiting to fail.

**A CUDA arena that stops doubling.** `ArenaExtendStrategy::NextPowerOfTwo` is the ONNX Runtime default and it reserves in powers of two rather than what the session asked for.

**The reranker loads on its first batch.** Under socket activation the daemon starts far more often than it reranks, and plenty of those starts only ever answer a `save`.

**A daemon that is not running when nobody is asking.** `CUBA_IDLE_SHUTDOWN_SECS` plus a systemd `.socket` unit: the socket owns the port, the daemon starts on the first real connection and exits after the idle window. It shuts down through the normal path — `serve` returns, the background drain flushes in-flight embedding writes, `sqlx` closes its pool — because exiting the process directly loses those writes silently.

<details>
<summary>The systemd pair</summary>

```ini
# ~/.config/systemd/user/cuba-memorys.socket
[Socket]
ListenStream=127.0.0.1:8787
Accept=no

[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/cuba-memorys.service — no [Install]; the socket starts it
[Unit]
Requires=cuba-memorys.socket

[Service]
Type=exec
ExecStart=%h/.local/bin/cuba-memorys-daemon serve 127.0.0.1:8787
# An idle shutdown exits 0 — Restart=always would bounce it straight back up.
Restart=on-failure
Environment=CUBA_IDLE_SHUTDOWN_SECS=1200
Environment=CUBA_EMBED_DEVICE=cpu
Environment=CUBA_RERANK_DEVICE=gpu
Environment=CUBA_NLI_DEVICE=cpu
```

Both units ship in [`packaging/`](packaging/). `ExecStart` has to name the binary you actually installed — `command -v cuba-memorys` — and the `-daemon` suffix above is only the convention for keeping a GPU build beside a stock one. A wrong path here fails as `status=203/EXEC`.

`serve` adopts the socket systemd passes as fd 3 (`LISTEN_FDS`), so the port is held while the daemon is not running and no client sees a refused connection.

The unit must also bind loopback. With socket activation the `.socket` unit's `ListenStream` decides the address and `CUBA_HTTP_ADDR` is ignored, so `serve` checks the address of the socket it is handed and refuses a routable one unless `CUBA_HTTP_TOKEN` is set.
</details>

### Host RAM: it sizes itself to your machine

VRAM was only half of it. The weights also live in host memory, and that appetite used to be fixed no matter what the machine had. Measured with `cargo run --release --features cuda --example mem_bench`, daemon stopped, on the 6 GB laptop GPU:

| stage | added RSS | VRAM | load |
|---|---|---|---|
| process start | 5,5 MiB | 0 | — |
| + PostgreSQL pool | +1,3 MiB | 0 | — |
| + embedder (bge-m3, CPU) | +862,0 MiB | 0 | 1,72 s |
| + reranker (fused FP16, GPU) | +1034,7 MiB | 1460 MiB | 3,73 s |
| + OOD fit (n=1811, d=1024) | +37,4 MiB | 0 | 11,45 s |
| **peak** | **2677 MiB** | **1460 MiB** | |

Resident settles near 1941 MiB; the peak is 2677 because loading a 1,1 GB ONNX file costs transient memory on top of the weights it leaves behind. The peak is the number that has to fit, not the steady state.

On the machine this was measured on that is fine. On a 4 GB laptop it is not, and under a systemd unit capped at `MemoryHigh=4500M` it has been seen paging **2,56 GiB to swap** — `MemoryHigh` does not kill, it reclaims, and reclaiming is paging.

Two traps worth knowing if you re-run this. `mem_bench` attributes VRAM to its own PID via `nvidia-smi --query-compute-apps`, because reading `memory.used` charges you for every other process on the card — that is how a first attempt showed 3590 MiB "at process start" that belonged to a game and a desktop shell. And run it with the daemon's own environment: with `CUBA_RERANKER_PATH` unset it silently loads the *unfused* artifact and the warm-up goes from 3,7 s to 131 s on CPU.

So the daemon now reads the machine at startup and picks a level. Nothing is invented for this: all three degradations already existed and are tested.

| level | models loaded | host RAM | what you give up |
|---|---|---|---|
| **minimal** | none | ~220 MiB | semantic search. BM25 + full-text + trigram still answer |
| **lean** | embedder | ~1,1 GiB | reranking and local entailment |
| **standard** | embedder + reranker | ~2,2 GiB | the NLI judge, which drops to its own fallback ladder |
| **full** | all three | ~3,3 GiB | nothing |

**How the level is chosen.** The budget is `min(cgroup limit, system available) − 768 MiB` of headroom, and the cgroup has to win. On this machine `/proc/meminfo` reports 7,16 GB available while the daemon's cgroup caps it at 4,39 GiB — believing `/proc` would load 2,6 GiB of weights against a limit where the kernel already starts paging. The reader walks from the cgroup root down to the leaf and takes the tightest `memory.max` or `memory.high` it finds, because the limit is usually set on an ancestor.

Models are then fitted in order of measured value: the embedder first, then the reranker (**+93% nDCG**, so it outranks the judge), then NLI.

**The plan can only take away.** Every knob is capped at the value the daemon already used, so on a machine with room the level is `full` and nothing changes. Degradation only goes downward.

**You always win.** Any of these set by hand is left untouched — the regulator fills gaps, it does not overwrite decisions:

```bash
CUBA_EMBED_INTRA_THREADS   CUBA_RERANK_INTRA_THREADS   CUBA_NLI_INTRA_THREADS
CUBA_RERANK_CHUNK          CUBA_GPU_MEM_LIMIT_MB       CUBA_OOD_FIT_LIMIT
CUBA_DB_MAX_CONNECTIONS
```

To force a model off regardless of the budget, point it at a path that does not exist — `CUBA_RERANKER_PATH=/nonexistent` or `CUBA_NLI_PATH=/nonexistent`. That is the same mechanism the regulator itself uses.

**To see what it decided**, run `cuba-memorys doctor`: it reports the reading and the resulting plan, and warns when the level falls to `minimal`. The plan is also logged at startup with the full machine reading behind it.

### The reranker artifact

The published `bge-reranker-v2-m3` ONNX is converted to FP16 **before** any graph fusion, which leaves 785 `Cast` nodes threaded through it. ONNX Runtime claws some of that back at load time (2023 → 897 nodes, 49 `SkipLayerNormalization`), but it cannot fuse Gelu and it repeats the work on every cold start. Rebuilding from the FP32 export and fusing *first*:

```bash
python -m onnxruntime.transformers.optimizer \
  --input model.onnx --output model.onnx \
  --model_type bert --num_heads 16 --hidden_size 1024 \
  --opt_level 1 --use_gpu --float16
```

| | VRAM | search p50 | load + warm |
|---|---|---|---|
| shipped FP16 | 2364 MiB | 3,73 s | 22,8 s |
| **fused, then FP16** | **1460 MiB** | **1,70 s** | **10,2 s** |

Identical top-10 order on a real search, `fused_score` differing by at most 0,0029; on synthetic logits at the real batch shapes, Pearson ≥ 0,9997 with the same ranking in every batch.

**Attention does not fuse, and that is not fixable here.** `is_fully_optimized: Attention (or MultiHeadAttention) not fused`, at `opt_level` 0, 1, 2 and 99, on both the FP16 artifact and the clean FP32 one. The export builds its Q/K/V reshapes from dynamic shape subgraphs (`Shape → Gather → Unsqueeze → Concat → Reshape`) and `AttentionFusion` needs a `Reshape` with a constant shape to read `num_heads` and `head_size` off it. So flash/efficient attention stays unavailable without a re-export using static shapes — worth knowing before anyone spends an afternoon on it.

---

## Measured — and the benchmark that was lying

Until v0.12 this section carried a line reading *"every number here is measured rather than assumed"*, and every number in it was wrong. The benchmark was broken in three ways, and finding out cost two published conclusions.

**It had ten queries.** A 95% interval of roughly ±0.12; the smallest effect it could detect was ~0.25 nDCG. Any claim about a smaller difference was noise wearing a decimal point.

**Relevance was judged by substring match.** A result counted as correct if its text merely *contained* a marker word — so every observation mentioning "postgres" scored as a right answer to any question about postgres, whether it answered anything or not. That measures keyword presence, not retrieval, and it tilts the whole benchmark toward the lexical branch and against the vector one.

**nDCG normalized against what was retrieved, not what exists.** With 5 relevant documents in the corpus and 2 found, the "ideal" ranking was taken to be those 2 — so a system that missed 60% of the answer scored a perfect **1.0**. (And `R@10 = 3.125` shipped in this file. Recall is a proportion.)

The real number is not 0.894. On 221 id-scored queries it is **nDCG@10 = 0.50** [95% CI 0.44–0.56]. The system did not get worse. It was never 0.894.

### What that cost

- ~~"The cross-encoder reranker earns nothing"~~ — **it had never run.** Three bugs in series: `faro` wrapped the call in `if let Ok(..)` and dropped the error; it fed `token_type_ids` to a model that is XLM-RoBERTa and has none; it read `f16` logits as `f32`. The output was "bit for bit identical" to no reranking not because reranking changed nothing, but because it never happened. Fixed; being measured properly now.

- **Associative retrieval does degrade** — but the old evidence (−0.03 at n=10) could not have shown it. On the new dataset with a **paired bootstrap** (the correct test: same queries in both arms), the interval is **[−0.051, −0.018]** and never touches zero. It improves 0 queries and hurts 23. The decision was right; the reasoning was not. *The power was never in more data — it was in using the right test.*

### What survives, re-measured honestly

| | |
|---|---|
| **`compact` by default** | **−30% tokens, nDCG +0.0090** (paired 95% CI [+0.0024, +0.0166], n=191). The earlier "exactly 0.0000" was measured with a harness that let the 5000-token response budget truncate the ranking before scoring it: verbose lost its tail, compact did not. The old "−40%" came from the broken benchmark. |
| **Conformal abstention** | 100% of out-of-distribution queries caught, 0% false abstentions. |
| **`lean` tool profile** | 15 tools of 31, −49% catalogue, zero functions lost. |
| **bge-m3 over e5-small** | Direction almost certainly right; **the +21.2 nDCG figure is withdrawn** — it came from the broken benchmark and re-establishing it would mean re-embedding the corpus twice. |
| **The benchmark itself** | 221 queries (was 10), relevance by document **id**, bootstrap confidence intervals, and the **minimum detectable effect** printed beside every result — so nobody reads a 3-point difference as a finding again. |

---

## Foundations

| Algorithm | Reference |
|---|---|
| RRF fusion (k=60) | Cormack et al. (2009) |
| Hebbian + BCM metaplasticity | Oja (1982); Bienenstock, Cooper & Munro (1982) |
| Conformal prediction | Vovk (2005); Angelopoulos & Bates (2023) |
| Ledoit-Wolf covariance shrinkage | Ledoit & Wolf (2004) |
| Mahalanobis OOD detection | Lee et al. (NeurIPS 2018) |
| Wilson score interval | Wilson (1927) |
| Declarative vs procedural memory | Anderson & Lebiere (ACT-R) |
| Testing effect | Karpicke & Roediger (Science 2008) |
| Power-law forgetting | Wixted (2004) |
| Episodic vs semantic memory | Tulving (1972) |
| PageRank · Leiden · Brandes | Brin & Page (1998); Traag et al. (2019); Brandes (2001) |
| NPMI co-occurrence | Bouma (2009) |
| MMR diversification | Carbonell & Goldstein (1998) |
| Contextual Retrieval | Anthropic (2024) |
| Prompt-injection spotlighting | Hines et al. (2024) |

---

## Development

```bash
git clone https://github.com/LeandroPG19/Memorys.git
cd Memorys/rust && cargo build --release

# On an NVIDIA machine, build this way instead — without it the reranker spends
# its whole budget for a ranking that gets discarded. It accelerates the
# reranker only; see Footprint for why the other two models stay on the CPU.
cargo build --release --features docs,cuda

./scripts/demo.sh                  # runs on a throwaway Postgres it removes on exit
./scripts/merge-gate.sh            # local CI 100% — sole merge judge (see docs/gate.md)
cargo run --release --example rerank_bench   # does the reranker fit its budget here?
```

Publishing is tag-driven: `v*` on [LeandroPG19/Memorys](https://github.com/LeandroPG19/Memorys) triggers GitHub Release binaries (5 platforms), PyPI wheels (`memory-industry` and the `cuba-memorys` alias), npm (`memory-industry` and the `cuba-memorys` alias), and the MCP Registry (`io.github.LeandroPG19/memory-industry`). A test pins `Cargo.toml`, `package.json`, `pyproject.toml` and `server.json` so those versions cannot drift.

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) — use it, modify it, ship it, sell it, embed it in a closed product. No copyleft obligation. The licence also grants patent rights explicitly, which is the part legal departments care about.

## Author

**Leandro Perez G.** — [@LeandroPG19](https://github.com/LeandroPG19)
