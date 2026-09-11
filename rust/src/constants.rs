use serde_json::Value;
use std::sync::OnceLock;

pub const DEDUP_THRESHOLD: f64 = 0.85;

pub const PRED_ERROR_REINFORCE: f64 = 0.92;
pub const PRED_ERROR_UPDATE: f64 = 0.75;

pub const CACHE_MAX_ENTRIES: usize = 256;
pub const CACHE_TTL_SECS: u64 = 300;

pub const HEBBIAN_ACCESS_BOOST: f64 = 0.01;

pub const BCM_THROTTLE_SCALE: f64 = 0.8;

pub const KILL_SWITCH_ENV: &str = "CUBA_PROJECT_FILTER";

pub const COMPACTION_HINT_HOURS: i64 = 2;
pub const COMPACTION_HINT_OBS_COUNT: i64 = 100;

pub const JUEZ_AMBIGUOUS_LO: f64 = 0.6;
pub const JUEZ_AMBIGUOUS_HI: f64 = 0.8;
pub const JUEZ_DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const JUEZ_DEFAULT_MAX_PAIRS: usize = 5;

pub const VALID_RELATION_TYPES: &[&str] =
    &["uses", "causes", "implements", "depends_on", "related_to"];

pub const VALID_ENTITY_TYPES: &[&str] = &[
    "concept",
    "project",
    "technology",
    "person",
    "pattern",
    "config",
];

pub const VALID_OBSERVATION_TYPES: &[&str] = &[
    "fact",
    "decision",
    "lesson",
    "preference",
    "error",
    "solution",
    "context",
    "tool_usage",
    "superseded",
];

pub const VALID_SOURCES: &[&str] = &[
    "agent",
    "error_detection",
    "user",
    "consolidation",
    "inference",
];

pub fn importance_prior(obs_type: &str, density: f64) -> f64 {
    match obs_type {
        "decision" => 0.8,
        "lesson" => 0.75,
        "error" | "solution" => 0.7,
        "fact" | "preference" => (density * 0.6).clamp(0.1, 0.9),
        "context" | "tool_usage" => (density * 0.4).clamp(0.1, 0.7),
        _ => density.clamp(0.1, 0.8),
    }
}

const ALLOW_SECRET_DESC: &str = "Refused when the text looks like a live credential (token, password, URL with embedded creds). Set true only for a false match — the text is then stored verbatim, in clear, and reachable by search, export and every client.";

static TOOL_DEFS: OnceLock<Vec<Value>> = OnceLock::new();

pub fn tool_definitions() -> &'static Vec<Value> {
    TOOL_DEFS.get_or_init(|| {
        let defs = vec![
        tool_def(
            "cuba_alma",
            "CRUD knowledge graph entities (concepts, projects, technologies, patterns, people). Auto-boosts neighbors on access. For transient info use cuba_cronica instead.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "update", "delete", "get"], "description": "Operation to perform"},
                    "name": {"type": "string", "description": "Entity name (unique identifier)"},
                    "entity_type": {"type": "string", "description": "Type: concept, project, technology, person, pattern, config"},
                    "new_name": {"type": "string", "description": "New name for update action"}
                },
                "required": ["action", "name"]
            }),
        ),
        tool_def(
            "cuba_cronica",
            "Attach facts/lessons/decisions to entities. Also manages episodic memories (specific events with actors/artifacts) via episode_add/episode_list. Timeline view shows chronological history. Auto-creates entity if not found. Dedup gate blocks near-duplicates.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "delete", "list", "batch_add", "episode_add", "episode_list", "timeline"], "description": "Operation to perform. episode_add stores a temporal event; episode_list retrieves events. timeline shows chronological observations+episodes."},
                    "entity_name": {"type": "string", "description": "Entity to attach observation/episode to"},
                    "content": {"type": "string", "description": "Observation or episode text"},
                    "observation_type": {"type": "string", "enum": ["fact", "decision", "lesson", "preference", "context", "tool_usage", "error", "solution"], "description": "Type of observation"},
                    "source": {"type": "string", "enum": ["agent", "user", "error_detection", "consolidation", "inference"], "description": "Who/what created this observation"},
                    "observation_id": {"type": "string", "description": "Observation UUID (for delete action)"},
                    "observations": {"type": "array", "items": {"type": "object"}, "description": "Array of {entity_name, content, observation_type?, source?} objects (for batch_add, max 100)"},
                    "actors": {"type": "array", "items": {"type": "string"}, "description": "People/agents involved in episode (for episode_add)"},
                    "artifacts": {"type": "array", "items": {"type": "string"}, "description": "Files/resources affected in episode (for episode_add)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_faro",
            "Search memory BEFORE answering to ground responses. Returns grounding scores. Mode 'verify' checks claims against evidence (confidence: verified/partial/weak/unknown). Session-aware: boosts results matching active session goals. Supports temporal filtering. Optional MMR diversification, OOD abstention and an exact tiktoken-based token budget.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search text"},
                    "mode": {"type": "string", "enum": ["hybrid", "verify"], "description": "Search mode (default: hybrid). 'verify' checks if claim is grounded."},
                    "scope": {"type": "string", "enum": ["all", "entities", "observations", "errors"], "description": "Where to search (default: all)"},
                    "limit": {"type": "integer", "description": "Max results (default 10, max 50)"},
                    "before": {"type": "string", "description": "ISO8601 datetime — return results created before this time"},
                    "after": {"type": "string", "description": "ISO8601 datetime — return results created after this time"},
                    "format": {"type": "string", "enum": ["verbose", "compact"], "description": "Response format. compact (DEFAULT): abbreviated keys — e=entity, c=content, t=type, i=importance, s=score. 71% fewer tokens (798 vs 2787 at limit=10, measured). verbose: full key names, only when you need every field."},
                    "tags": {"type": "string", "description": "Filter observations by tag keyword (exact match against auto-extracted tags)"},
                    "max_tokens": {"type": "integer", "description": "Token budget for results (default 5000). Counted exactly via tiktoken cl100k_base."},
                    "diversify": {"type": "boolean", "description": "Post-RRF MMR pass that penalizes near-duplicates among top-K. Default false."},
                    "mmr_lambda": {"type": "number", "description": "MMR balance — 1.0 pure relevance, 0.0 pure diversity. Default 0.7."},
                    "abstain_ood": {"type": "boolean", "description": "Abstain (return empty results with abstain_reason) when the query is out-of-distribution via Mahalanobis distance. Default false."},
                    "ood_threshold": {"type": "number", "description": "Mahalanobis distance threshold for abstention. Defaults to sqrt(chi2_0.99(d)), which scales with the embedding dimension (~21.25 for d=384). Override only if you calibrated on your own corpus."},
                    "enable_bm25": {"type": "boolean", "description": "Enable BM25 (ts_rank_cd) as third RRF signal alongside text + vector. Catches queries with rare terms that dense embeddings miss. Default true."},
                    "rerank": {"type": "boolean", "description": "Cross-encoder rerank top-50 → top-K with bge-reranker-v2-m3. Auto-enabled when CUBA_MODE=completo, or when this build has a real GPU provider active (CUDA/DirectML compiled in AND a working device). Off by default everywhere else, even with the model on disk: on CPU it costs 60-110s and blows the search budget. Explicit true/false always wins; run `cuba-memorys doctor` to see which reason applies here."},
                    "associative": {"type": "boolean", "description": "Multi-hop expansion: seeds spreading activation from query-matched entities and pulls in observations on graph-connected entities that no lexical/vector signal surfaced. Additive — never lowers a base hit. Default false."}
                },
                "required": ["query"]
            }),
        ),
        tool_def(
            "cuba_puente",
            "Create edges between entities (uses, causes, implements, depends_on, related_to). 'traverse' explores connections, 'infer' does transitive reasoning (A→B→C), 'predict' suggests missing links via Adamic-Adar. Relations strengthen with use (Hebbian).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "delete", "traverse", "infer", "predict"], "description": "Operation to perform. 'predict' uses Adamic-Adar to suggest missing relations."},
                    "from_entity": {"type": "string", "description": "Source entity name"},
                    "to_entity": {"type": "string", "description": "Target entity name"},
                    "relation_type": {"type": "string", "description": "Relation: uses, causes, implements, depends_on, related_to. Also used by predict+persist to pick the type for the persisted edge (default related_to)."},
                    "bidirectional": {"type": "boolean", "description": "If true, relation goes both ways"},
                    "start_entity": {"type": "string", "description": "Start point for traverse/infer"},
                    "max_depth": {"type": "integer", "description": "Max hops for traverse/infer (default 3, max 5)"},
                    "entity_name": {"type": "string", "description": "Entity name for predict action (Adamic-Adar link prediction)"},
                    "persist": {"type": "boolean", "description": "For predict: write the suggestions to brain_relations as provenance='predicted' (relation_type related_to) instead of only returning them. Default false — read-only."}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_eco",
            "RLHF feedback: positive boosts importance (Oja's rule), negative decreases, correct updates content. Also the quarantine gate: 'pending' lists memories withheld from search because they came from untrusted text, 'promote' makes one retrievable, 'quarantine' withdraws one. The gate covers observations, episodes and errors — pick which with 'kind'.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["positive", "negative", "correct", "promote", "quarantine", "pending"], "description": "Feedback type, or a quarantine transition: promote/quarantine flip one memory's retrievability; pending lists everything currently withheld, in three lists (quarantined, quarantined_episodes, quarantined_errors), each row tagged with its kind."},
                    "kind": {"type": "string", "enum": ["observation", "episode", "error"], "description": "Which table promote/quarantine acts on. Default 'observation'. An import quarantines whatever carried a credential, and cuba_sync writes episodes and errors too: without the matching kind those rows would stay stored and permanently unreachable. Ignored by positive/negative/correct, and by pending, which always returns all three."},
                    "entity_name": {"type": "string", "description": "Target entity"},
                    "observation_id": {"type": "string", "description": "Target observation UUID"},
                    "id": {"type": "string", "description": "Target UUID for promote/quarantine when kind is episode or error. For kind=observation use observation_id."},
                    "correction": {"type": "string", "description": "New content (for correct action)"},
                    "limit": {"type": "integer", "description": "Max rows for the 'pending' listing (default 20, max 200)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_alarma",
            "Report errors immediately. Auto-detects patterns (≥3 similar = warning). Hebbian: similar errors get boosted for easier retrieval.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "error_type": {"type": "string", "description": "Error category: TypeError, ConnectionError, etc."},
                    "error_message": {"type": "string", "description": "Full error message"},
                    "context": {"type": "object", "description": "Context: {file, function, stack_trace, line}"},
                    "project": {"type": "string", "description": "Project name (default: 'default')"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["error_type", "error_message"]
            }),
        ),
        tool_def(
            "cuba_remedio",
            "Mark an error as resolved with solution. Cross-references similar unresolved errors.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "error_id": {"type": "string", "description": "UUID of the error to solve"},
                    "solution": {"type": "string", "description": "Solution that fixed the error"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["error_id", "solution"]
            }),
        ),
        tool_def(
            "cuba_expediente",
            "Search past errors/solutions. Use 'proposed_action' as anti-repetition guard: warns if similar approach previously failed.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search text for errors"},
                    "project": {"type": "string", "description": "Filter by project"},
                    "resolved_only": {"type": "boolean", "description": "Only return errors with solutions"},
                    "proposed_action": {"type": "string", "description": "Anti-repetition: describe what you plan to do. Returns warning if similar approach failed before."}
                },
                "required": ["query"]
            }),
        ),
        tool_def(
            "cuba_jornada",
            "Track working sessions with goals and outcomes. v0.8: optional 'project' arg binds the session to a named project (upserts in brain_projects); subsequent handlers will scope reads/writes to that project.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["start", "end", "list", "current"], "description": "Session action"},
                    "name": {"type": "string", "description": "Session name (for start)"},
                    "goals": {"type": "array", "items": {"type": "string"}, "description": "Session goals (for start)"},
                    "project": {"type": "string", "description": "v0.8: project name to bind this session to (created on first use). Omit to keep session global."},
                    "outcome": {"type": "string", "enum": ["success", "partial", "failed", "abandoned"], "description": "Session outcome (for end)"},
                    "summary": {"type": "string", "description": "What was accomplished (for end)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_decreto",
            "Record and query architecture/design decisions.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["record", "query", "list"], "description": "Decision action"},
                    "title": {"type": "string", "description": "Decision title (for record)"},
                    "context": {"type": "string", "description": "Why this decision was needed"},
                    "alternatives": {"type": "array", "items": {"type": "string"}, "description": "Options considered"},
                    "chosen": {"type": "string", "description": "Option chosen"},
                    "rationale": {"type": "string", "description": "Why this option was chosen"},
                    "query": {"type": "string", "description": "Search text (for query action)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_vigia",
            "Knowledge graph analytics: summary, health, drift, communities, bridges, structural.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "metric": {"type": "string", "enum": ["summary", "health", "drift", "communities", "bridges", "structural"], "description": "summary: counts + token estimate. health: staleness, entropy, DB size. drift: chi-squared on errors. communities: Leiden clustering. bridges: betweenness centrality. structural: harmonic + closeness centrality + k-core ranking (backbone identification)."}
                },
                "required": ["metric"]
            }),
        ),
        tool_def(
            "cuba_zafra",
            "Memory maintenance, scoped to the active project: decay, prune, merge, summarize, pagerank, find_duplicates, export, reembed, decay_episodes.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["decay", "prune", "merge", "summarize", "stats", "pagerank", "find_duplicates", "export", "reembed", "decay_episodes"], "description": "decay: stratified exponential decay by type. prune: deletes low-importance observations, dry-run unless confirm=true. merge: deduplicates similar entities. summarize: replaces an entity's observations with compressed_summary. stats: counts. pagerank: personalized importance ranking. find_duplicates: lists near-duplicate pairs. export: writes a JSON dump. reembed: re-encodes with the current model. decay_episodes: power-law decay on brain_episodes."},
                    "confirm": {"type": "boolean", "description": "prune only: actually delete. Without it, prune returns a dry-run plan (would_prune, by_project) and deletes nothing — read the plan first, the default threshold reaches a large share of a mature corpus."},
                    "entity_name": {"type": "string", "description": "Entity to summarize (for summarize action)"},
                    "compressed_summary": {"type": "string", "description": "Compressed text replacing observations (for summarize)"},
                    "threshold": {"type": "number", "description": "Importance threshold for prune (default 0.1)"},
                    "similarity_threshold": {"type": "number", "description": "Similarity threshold for merge (default 0.8)"},
                    "batch_size": {"type": "integer", "description": "Max observations to re-encode in reembed (default 500)"},
                    "halflife_days": {"type": "number", "description": "Global halflife override for decay (overrides per-type stratification)"},
                    "c": {"type": "number", "description": "Power-law c parameter for decay_episodes (default 0.1)"},
                    "beta": {"type": "number", "description": "Power-law β exponent for decay_episodes (default 0.5)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_forget",
            "GDPR Right to Erasure: cascading hard-delete of an entity and ALL references across observations, relations, errors, and sessions. IRREVERSIBLE. Requires confirm=true.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "entity_name": {"type": "string", "description": "Entity name to erase completely"},
                    "confirm": {"type": "boolean", "description": "Must be true to proceed (safety gate)"}
                },
                "required": ["entity_name", "confirm"]
            }),
        ),
        tool_def(
            "cuba_reflexion",
            "Analyze the knowledge graph for structural gaps. Read-only.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["analyze"], "description": "analyze: the only action. Reports isolated entities, underconnected hubs, type silos, observation gaps (missing decisions/lessons), and statistical density anomalies."}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_hipotesis",
            "Abductive inference: find plausible causes of an observed effect. Read-only.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["explain"], "description": "explain: traverse causal relations backwards from `effect`, ranked by path_strength × importance."},
                    "effect": {"type": "string", "description": "Entity name representing the observed effect"},
                    "max_depth": {"type": "integer", "description": "Max causal chain hops (default 3, max 5)"},
                    "limit": {"type": "integer", "description": "Max hypotheses to return (default 10, max 50)"}
                },
                "required": ["action", "effect"]
            }),
        ),
        tool_def(
            "cuba_contradiccion",
            "Detect semantic contradictions between observations of the same entity. Uses embedding cosine distance + negation heuristics. Read-only.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["scan"], "description": "Contradiction detection action"},
                    "entity_name": {"type": "string", "description": "Entity to scan (omit to scan top entities by observation count)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_centinela",
            "Prospective memory: triggers that fire on entity access, session start, or error match. 'Remember to remind me about X when Y happens.'",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "list", "delete", "check"], "description": "create: define a trigger. list: show triggers. delete: remove one (trigger_id). check: evaluate now."},
                    "entity_pattern": {"type": "string", "description": "Entity name or pattern to match"},
                    "condition_type": {"type": "string", "enum": ["on_access", "on_session_start", "on_error_match"], "description": "When to fire. on_session_start with the other session's name as entity_pattern is also the cross-session note channel: fires once (max_fires) the next time that session starts, carrying who left it."},
                    "message": {"type": "string", "description": "Reminder message to surface when triggered"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC},
                    "max_fires": {"type": "integer", "description": "Max times to fire (default 1, -1 for unlimited)"},
                    "expires_at": {"type": "string", "description": "ISO8601 expiration datetime"},
                    "trigger_id": {"type": "string", "description": "Trigger UUID (for delete)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_calibrar",
            "Bayesian confidence calibration: track verify predictions, mark outcomes, compute P(correct|level). Closes the feedback loop between faro verify and eco correct.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["stats", "history", "resolve", "trust", "metrics"], "description": "stats/history: past predictions. resolve: mark a verify_id correct/incorrect. trust: per-source Beta(α, β) credibility, updated by resolve outcomes. metrics: Brier score + Expected Calibration Error + reliability diagram."},
                    "verify_id": {"type": "string", "description": "Verify log UUID (for resolve)"},
                    "outcome": {"type": "string", "enum": ["correct", "incorrect"], "description": "Whether the verify prediction was right (for resolve)"},
                    "limit": {"type": "integer", "description": "Max results for history (default 20)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_ingesta",
            "Bulk knowledge ingestion. 'ingest': array of {entity_name, content, observation_type} items. 'parse': split long text by paragraphs + heuristic classify. 'auto_extract' (v0.11): the calling client's LLM extracts salient durable facts from a turn/conversation via MCP Sampling ($0, no API key) and ingests them — the automatic-extraction that mem0/Zep have. All routes share the dedup/PE-gating/embedding pipeline; none delete.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["ingest", "parse", "auto_extract"], "default": "ingest", "description": "Ingestion mode. Default 'ingest' is the fast raw path (no LLM). 'parse' splits long text. 'auto_extract' is opt-in LLM extraction via MCP sampling — do not use it as the default write."},
                    "items": {"type": "array", "items": {"type": "object"}, "description": "Array of {entity_name, content, observation_type?} objects (for ingest action, max 200)"},
                    "entity_name": {"type": "string", "description": "Entity to attach parsed observations to (for parse action)"},
                    "text": {"type": "string", "description": "Raw text: paragraphs to split (parse) or a turn/conversation to extract facts from (auto_extract)"},
                    "entity_hint": {"type": "string", "description": "Optional main-subject hint for auto_extract (biases entity_name)"},
                    "supersede_conflicts": {"type": "boolean", "description": "v0.11 (auto_extract): when a new fact replaces/contradicts an existing related one, ask the judge and mark the old observation superseded (knowledge-update; never deletes). Default false."},
                    "untrusted": {"type": "boolean", "description": "Set when the text came from somewhere you do not control (a fetched page, a pasted document, a third party). Everything extracted lands quarantined — stored and inspectable via cuba_eco action=pending, but withheld from cuba_faro until promoted. Default false."},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_proyecto",
            "Project scoping (v0.8): isolate memories per project so multiple projects sharing one DB don't bleed into each other. Active project is bound to the current session (cuba_jornada start --project NAME). Legacy rows with NULL project_id remain visible from every scope.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "current", "switch", "stats", "rename", "merge"], "description": "Project action"},
                    "name": {"type": "string", "description": "Project name (for switch/stats/rename source)"},
                    "to": {"type": "string", "description": "Destination name (for rename/merge)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_pre_compact",
            "Compaction-survival protocol: persist and restore a session summary across /compact.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["snapshot", "restore"], "description": "snapshot: before /compact, persists a dense markdown summary (observations, decisions, unresolved errors, pending embeddings, goals) for the active session. restore: after /compact, retrieves the latest snapshot and re-injects it into context."}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_sync",
            "Git-friendly export/import of the knowledge graph between machines that share no database. export/import/diff/status work on a local directory (default ./.cuba-memorys/); pull/notify/fetch/conflicts/resolve talk to a peer over HTTP with CUBA_PEER_TOKEN.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["export", "import", "diff", "status", "pull", "notify", "fetch", "conflicts", "resolve"], "description": "export/import/diff/status: local bundle round-trip. pull: return the bundle in the response instead of writing it, for a peer to fetch. notify: tell a peer what changed. fetch: pull a peer's bundle over HTTP and import it. conflicts/resolve: list and settle rows two machines disagree about."},
                    "id": {"type": "string", "description": "resolve only: the conflict id from action=conflicts"},
                    "keep": {"type": "string", "enum": ["ours", "theirs", "both"], "description": "resolve only: which text stays current (default 'both', which loses nothing)"},
                    "peer": {"type": "string", "description": "fetch only: which peer to pull from (default 'default')"},
                    "url": {"type": "string", "description": "fetch only: the peer's base address, e.g. https://brain.example.net"},
                    "summary": {"type": "string", "description": "notify only: what changed, in at most 2000 characters"},
                    "node_id": {"type": "string", "description": "notify only: self-asserted id of the sending node"},
                    "node_name": {"type": "string", "description": "notify only: readable name of the sending node"},
                    "manifest_hash": {"type": "string", "description": "notify only: the bundle hash this notice refers to"},
                    "limit": {"type": "integer", "description": "pull only: max files per page"},
                    "offset": {"type": "integer", "description": "pull only: index of the first bundle file to return"},
                    "dir": {"type": "string", "description": "Directory override (default $CUBA_SYNC_DIR or ./.cuba-memorys/)"},
                    "scope": {"type": "string", "enum": ["project", "all"], "description": "Export scope: active project only (default) or all data"},
                    "with_embeddings": {"type": "boolean", "description": "Include embeddings (default false on export, true on pull)"},
                    "conflict": {"type": "string", "enum": ["merge", "skip", "overwrite"], "description": "How to resolve a row that exists on both sides (default merge; merge and skip keep local content, overwrite takes the incoming version)"},
                    "confirm": {"type": "boolean", "description": "Required when the import's tombstones would delete more than 10% of this machine's observations and at least 25 rows"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_archivo",
            "Tamper-evident audit log: append-only, SHA-256 hash chain, UPDATE/DELETE blocked at the PostgreSQL trigger level.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["append", "verify", "tail"], "description": "append: add an event. verify: walk the hash chain and detect tampering. tail: read recent events."},
                    "event_action": {"type": "string", "description": "Event type (for append)"},
                    "payload": {"type": "object", "description": "Arbitrary JSON payload (for append)"},
                    "limit": {"type": "integer", "description": "Limit for verify/tail (default 10000 / 20)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_pizarra",
            "Working memory buffer (v0.9, Baddeley 1992): a TTL-bounded scratchpad orthogonal to episodic and semantic memory. Use for inter-step plan state during long-horizon agent tasks, tentative observations, cross-tool-call reminders inside one session. Auto-expire by ttl_seconds; bulk-purged by cuba_zafra REM cycle.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["write", "read", "clear"], "description": "Working-memory operation"},
                    "content": {"type": "string", "description": "Content to store (for write)"},
                    "tag": {"type": "string", "description": "Optional tag for filtering on read/clear"},
                    "ttl_seconds": {"type": "integer", "description": "Time-to-live in seconds (default 3600)"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
        #[cfg(feature = "docs")]
        tool_def(
            "cuba_docs",
            "Read a library's CURRENT documentation from its official site. Use when you are about to write code against an API you have not verified this session — a renamed function or a changed signature is the most common way generated code fails, and memory cannot save you from it because your memory of the API is the thing that is wrong. `query` filters the page to the paragraphs that mention it. Requires the `docs` Cargo feature; every request is checked against SSRF (private ranges, cloud metadata, redirects).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "library": {"type": "string", "description": "Package name only — `tokio`, `sqlx`, `fastapi`, `react`. Not a URL. Unknown names are assumed to be Rust crates and resolved on docs.rs."},
                    "query": {"type": "string", "description": "What you need to know (e.g. `spawn_blocking`, `Depends`). Filters the page to matching paragraphs; omit for the overview."}
                },
                "required": ["library"]
            }),
        ),
        tool_def(
            "cuba_juez",
            "LLM-judge for semantically-conflicting observations in the ambiguous cosine-similarity band (0.6-0.8), where heuristics miss vocabulary-different conflicts. Verdicts are cached in brain_judgments.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["judge_pair", "scan_entity"], "description": "judge_pair: decide on two given observation ids. scan_entity: pull ambiguous pairs for an entity and judge each."},
                    "observation_a": {"type": "string", "description": "UUID of first observation (for judge_pair)"},
                    "observation_b": {"type": "string", "description": "UUID of second observation (for judge_pair)"},
                    "entity_name": {"type": "string", "description": "Entity to scan (for scan_entity)"},
                    "max_pairs": {"type": "integer", "description": "Max pairs to escalate per call (default 5; controls LLM cost)"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_whoami",
            "Identify this MCP client against the MemoryIndustry daemon: client id, project, session, node, LLM, graph-db, resource plan. Alias: memory_whoami.",
            serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool_def(
            "cuba_artefacto",
            "Shared versioned artifacts between agents on the same daemon (and peers). put requires base_version (optimistic concurrency); lock is a short lease. Alias: memory_artifact.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "get", "put", "patch", "lock", "unlock", "watch_hint"], "description": "Artifact operation"},
                    "path": {"type": "string", "description": "Logical path, e.g. notes/plan.md"},
                    "content": {"type": "string", "description": "Full content for put"},
                    "base_version": {"type": "integer", "description": "Required for concurrent-safe put; omit or 0 to create"},
                    "find": {"type": "string", "description": "patch: substring to replace once"},
                    "replace": {"type": "string", "description": "patch: replacement"},
                    "ttl_seconds": {"type": "integer", "description": "lock lease length (default 60)"},
                    "prefix": {"type": "string", "description": "list: path prefix filter"},
                    "limit": {"type": "integer", "description": "list limit"}
                },
                "required": ["action"]
            }),
        ),
        tool_def(
            "cuba_contexto",
            "Visible context window for the agent: working memory + agent notes (exact Mcp-Client-Id) + artifact index + recall of this session's recent writes (then WM mentions, then importance). Call at the start of a turn; call cuba_cronica add BEFORE editing. Alias: memory_context.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "budget_chars": {"type": "integer", "description": "Max serialized size (default 12000)"}
                }
            }),
        ),
        tool_def(
            "cuba_receta",
            "PROCEDURAL MEMORY: how things are DONE here — bring up the dev services, run the test suite, deploy, migrate. \
             The other tools remember what is TRUE; this one remembers what to DO, so an agent stops rediscovering it every session. \
             Ranked by reliability, not by how often it is read: report the outcome with action='outcome' after running one, or the \
             memory learns nothing. A recipe that keeps failing is worse than none, because it is trusted.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "get", "search", "outcome", "list", "delete"], "description": "search: find by meaning. get: fetch by exact name. add: store/update (re-adding the same name edits it, keeping its track record). outcome: record success/failure — this is what teaches it."},
                    "name": {"type": "string", "description": "Procedure name, e.g. 'levantar el entorno de desarrollo'"},
                    "trigger": {"type": "string", "description": "WHEN this applies — the IF half. e.g. 'cuando hay que levantar los servicios de mapupita-web'"},
                    "steps": {"type": "array", "items": {"type": "object"}, "description": "Ordered steps: [{do: '...', run: 'comando'?, expect: 'qué debe pasar'?}]"},
                    "preconditions": {"type": "string", "description": "What must already be true before starting"},
                    "verification": {"type": "string", "description": "How you know it actually worked"},
                    "success": {"type": "boolean", "description": "For action=outcome: did it work?"},
                    "query": {"type": "string", "description": "For action=search"},
                    "limit": {"type": "integer", "description": "Max results"},
                    "allow_secret": {"type": "boolean", "description": ALLOW_SECRET_DESC}
                },
                "required": ["action"]
            }),
        ),
    ];

        #[cfg(feature = "docs")]
        let defs = if crate::handlers::docs::enabled() {
            defs
        } else {
            defs.into_iter()
                .filter(|t| t.get("name").and_then(Value::as_str) != Some("cuba_docs"))
                .collect()
        };

        defs
    })
}

fn tool_def(name: &str, description: &str, input_schema: Value) -> Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn meta_tool_defs() -> Vec<Value> {
    vec![
        tool_def(
            "cuba_tools",
            "Find cuba-memorys tools and load their schemas ON DEMAND. The server exposes 31 tools; \
             under CUBA_TOOL_PROFILE=lean only the everyday core is pre-loaded and the rest live here. \
             Search by capability ('audit', 'decay', 'contradiction', 'session'), then call what you \
             find with cuba_call. detail='names' is cheapest, 'full' returns the exact argument schema.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Filter by capability — matches tool names and descriptions. Omit to list everything."},
                    "detail": {"type": "string", "enum": ["names", "summary", "full"], "description": "names: just the names. summary (default): name + description. full: the complete JSON Schema, which is what you need to call the tool correctly."}
                }
            }),
        ),
        tool_def(
            "cuba_call",
            "Invoke any cuba-memorys tool by name — including the ones not pre-loaded in this session. \
             Discover them first with cuba_tools (use detail='full' to see the exact arguments). \
             Goes through the same dispatcher as a direct call, so behaviour is identical.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "description": "Tool name, e.g. cuba_zafra"},
                    "args": {"type": "object", "description": "The tool's own arguments, exactly as its schema declares them"}
                },
                "required": ["tool"]
            }),
        ),
    ]
}

const PROFILE_AGENT: [&str; 17] = [
    "cuba_receta",
    "cuba_faro",
    "cuba_cronica",
    "cuba_alma",
    "cuba_puente",
    "cuba_alarma",
    "cuba_remedio",
    "cuba_expediente",
    "cuba_jornada",
    "cuba_decreto",
    "cuba_ingesta",
    "cuba_proyecto",
    "cuba_pre_compact",
    "cuba_pizarra",
    "cuba_whoami",
    "cuba_artefacto",
    "cuba_contexto",
];

const PROFILE_STANDARD_EXTRA: [&str; 6] = [
    "cuba_eco",
    "cuba_reflexion",
    "cuba_hipotesis",
    "cuba_contradiccion",
    "cuba_centinela",
    "cuba_calibrar",
];

const PROFILE_LEAN: [&str; 13] = [
    "cuba_faro",
    "cuba_expediente",
    "cuba_decreto",
    "cuba_jornada",
    "cuba_cronica",
    "cuba_ingesta",
    "cuba_alarma",
    "cuba_remedio",
    "cuba_receta",
    "cuba_pizarra",
    "cuba_whoami",
    "cuba_artefacto",
    "cuba_contexto",
];

pub fn tools_for_profile() -> Vec<Value> {
    tools_with_memory_aliases(tools_for(
        &std::env::var("CUBA_TOOL_PROFILE").unwrap_or_else(|_| "full".to_string()),
    ))
}

/// Announce both `cuba_*` and `memory_*` names for one release (dispatch still canonicalizes).
pub fn tools_with_memory_aliases(tools: Vec<Value>) -> Vec<Value> {
    let mut out = Vec::with_capacity(tools.len() * 2);
    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str).map(str::to_string) else {
            out.push(tool);
            continue;
        };
        out.push(tool.clone());
        if let Some(rest) = name.strip_prefix("cuba_") {
            let alias = match rest {
                "artefacto" => "memory_artifact".to_string(),
                "contexto" => "memory_context".to_string(),
                other => format!("memory_{other}"),
            };
            let mut aliased = tool;
            if let Some(obj) = aliased.as_object_mut() {
                obj.insert("name".into(), Value::String(alias));
                let desc = obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                obj.insert(
                    "description".into(),
                    Value::String(format!("[alias of {name}] {desc}")),
                );
            }
            out.push(aliased);
        }
    }
    out
}

pub fn tools_for(profile: &str) -> Vec<Value> {
    let all = tool_definitions();

    let allowed: Vec<&str> = match profile.to_lowercase().as_str() {
        "lean" => {
            let mut out: Vec<Value> = all
                .iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| PROFILE_LEAN.contains(&n))
                })
                .cloned()
                .collect();
            out.extend(meta_tool_defs());
            return out;
        }
        "agent" => PROFILE_AGENT.to_vec(),
        "standard" => PROFILE_AGENT
            .iter()
            .chain(PROFILE_STANDARD_EXTRA.iter())
            .copied()
            .collect(),
        _ => {
            let mut out = all.clone();
            out.extend(meta_tool_defs());
            return out;
        }
    };

    all.iter()
        .filter(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| allowed.contains(&n))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn every_profiled_tool_actually_exists() {
        let names: Vec<&str> = tool_definitions()
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for t in PROFILE_AGENT.iter().chain(PROFILE_STANDARD_EXTRA.iter()) {
            assert!(
                names.contains(t),
                "el perfil nombra una tool inexistente: {t}"
            );
        }
    }

    #[test]
    fn the_default_hides_nothing() {
        let full = tools_for("full");
        assert_eq!(full.len(), tool_definitions().len() + 2);
        for t in tool_definitions() {
            let name = t.get("name").and_then(Value::as_str).unwrap();
            assert!(
                full.iter()
                    .any(|f| f.get("name").and_then(Value::as_str) == Some(name)),
                "{name} desapareció del perfil full"
            );
        }
    }

    #[test]
    fn an_unknown_profile_falls_back_to_full() {
        let n = tools_for("full").len();
        assert_eq!(tools_for("typo-de-dedo").len(), n);
        assert_eq!(tools_for("").len(), n);
    }

    #[test]
    fn lean_defers_tools_it_does_not_delete_them() {
        let lean = tools_for("lean");
        assert_eq!(lean.len(), PROFILE_LEAN.len() + 2);
        let names: Vec<&str> = lean
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            names.contains(&"cuba_tools"),
            "lean sin cuba_tools deja las demás inalcanzables"
        );
        assert!(
            names.contains(&"cuba_call"),
            "lean sin cuba_call deja las demás inalcanzables"
        );
        assert!(names.contains(&"cuba_faro"));
        assert!(
            names.contains(&"cuba_whoami"),
            "lean debe exponer whoami para onboarding"
        );
        assert!(
            names.contains(&"cuba_artefacto"),
            "lean debe exponer artefacto para coordinación multi-agente"
        );
    }

    #[test]
    fn tools_list_announces_memory_aliases() {
        let listed = tools_with_memory_aliases(tools_for("full"));
        let names: Vec<_> = listed
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"cuba_whoami"));
        assert!(names.contains(&"memory_whoami"));
        assert!(names.contains(&"cuba_artefacto"));
        assert!(names.contains(&"memory_artifact"));
    }

    #[test]
    fn agent_profile_includes_coordination_tools() {
        let names: Vec<String> = tools_for("agent")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        for required in ["cuba_whoami", "cuba_artefacto", "cuba_contexto"] {
            assert!(
                names.iter().any(|n| n == required),
                "agent sin {required} deja la vista compartida inalcanzable"
            );
        }
    }

    #[test]
    fn narrow_profiles_are_strict_subsets() {
        assert_eq!(tools_for("agent").len(), PROFILE_AGENT.len());
        assert_eq!(
            tools_for("standard").len(),
            PROFILE_AGENT.len() + PROFILE_STANDARD_EXTRA.len()
        );
        let full: Vec<String> = tools_for("full")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        for t in tools_for("agent") {
            let name = t.get("name").and_then(Value::as_str).unwrap_or_default();
            assert!(full.contains(&name.to_string()), "{name} no está en full");
        }
    }

    #[test]
    fn a_tool_that_can_destroy_data_declares_its_safety_switch() {
        const GUARDED: [&str; 2] = ["cuba_forget", "cuba_zafra"];

        for name in GUARDED {
            let tool = tool_definitions()
                .iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
                .unwrap_or_else(|| panic!("{name} disappeared from the catalogue"));

            let confirm = tool.pointer("/inputSchema/properties/confirm");
            assert!(
                confirm.is_some_and(|c| c.get("type").and_then(Value::as_str) == Some("boolean")),
                "{name} deletes rows and gates that behind `confirm`, but its schema does not \
                 declare the key. A caller cannot discover a switch that is not in the schema, \
                 and clients that strip undeclared properties would make the guarded path \
                 unreachable"
            );

            let described = confirm
                .and_then(|c| c.get("description"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                !described.trim().is_empty(),
                "{name}: a boolean called `confirm` with no description tells the caller \
                 nothing about what it is confirming"
            );
        }
    }

    #[test]
    fn a_tool_that_stores_free_text_declares_its_secret_override() {
        const GATED: [&str; 11] = [
            "cuba_cronica",
            "cuba_decreto",
            "cuba_alarma",
            "cuba_remedio",
            "cuba_ingesta",
            "cuba_eco",
            "cuba_zafra",
            "cuba_receta",
            "cuba_pizarra",
            "cuba_jornada",
            "cuba_centinela",
        ];

        for name in GATED {
            let tool = tool_definitions()
                .iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
                .unwrap_or_else(|| panic!("{name} disappeared from the catalogue"));

            let allow = tool.pointer("/inputSchema/properties/allow_secret");
            assert!(
                allow.is_some_and(|a| a.get("type").and_then(Value::as_str) == Some("boolean")),
                "{name} refuses to store text that looks like a live credential, and \
                 `allow_secret` is the only way past that refusal. A caller cannot discover a \
                 switch that is not in the schema, and clients that strip undeclared properties \
                 would leave the legitimate write with no way through at all"
            );

            let described = allow
                .and_then(|a| a.get("description"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                described.contains("clear"),
                "{name}: `allow_secret` must say that the text is then stored verbatim and in \
                 clear. A caller who reads the switch as 'store it safely' is the one who will \
                 flip it on a live credential"
            );
        }
    }

    #[test]
    fn ingest_schema_defaults_to_fast_path_not_llm_extract() {
        let tool = tool_definitions()
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some("cuba_ingesta"))
            .expect("cuba_ingesta");
        let action = tool
            .pointer("/inputSchema/properties/action")
            .expect("action property");
        assert_ne!(
            action.get("default").and_then(Value::as_str),
            Some("auto_extract"),
            "auto_extract is the ~18s LLM path and must stay opt-in"
        );
        assert_eq!(
            action.get("default").and_then(Value::as_str),
            Some("ingest"),
            "the contract default is raw ingest"
        );
        let modes: Vec<&str> = action
            .get("enum")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(modes.first().copied(), Some("ingest"));
        assert!(modes.contains(&"auto_extract"));
    }
}
