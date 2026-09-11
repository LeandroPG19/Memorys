use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;

use super::datasets::EvaluationSample;
use super::metrics::{
    bootstrap_ci, minimum_detectable_effect, mrr, ndcg_at_k, precision_at_k, recall_at_k,
};
use crate::eval::metrics::{calculate_exact_match, calculate_f1_score};

const BOOTSTRAP_ITERATIONS: usize = 2000;

#[derive(Debug, Clone)]
pub struct EvalConfig {
    pub k: usize,
    pub associative: bool,
    pub abstain: bool,
    pub rerank: bool,
    pub format: String,
    pub max_tokens: i64,
}

const UNLIMITED_TOKENS: i64 = i64::MAX;

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            k: 10,
            associative: false,
            abstain: false,
            rerank: false,
            format: "verbose".to_string(),
            max_tokens: UNLIMITED_TOKENS,
        }
    }
}

pub fn faro_args(query: &str, cfg: &EvalConfig, k: usize) -> Value {
    serde_json::json!({
        "query": query,
        "mode": "hybrid",
        "limit": k,
        "max_tokens": cfg.max_tokens,
        "enable_bm25": true,
        "rerank": cfg.rerank,
        "diversify": false,
        "associative": cfg.associative,
        "abstain_ood": cfg.abstain,
        "format": cfg.format,
        "track_access": false
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub sample_count: usize,
    pub k: usize,
    pub ndcg_at_k: f64,
    pub mrr: f64,
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub mean_exact_match: f32,
    pub mean_f1: f32,
    pub mean_response_tokens: f64,
    pub max_response_tokens: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_ability: Vec<AbilityScore>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstention_accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub false_abstention_rate: Option<f64>,

    pub ndcg_ci95: (f64, f64),
    pub minimum_detectable_effect: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_query_ndcg: Vec<f64>,
    pub scored_by_id: bool,
    #[serde(default)]
    pub latency_p50_ms: f64,
    #[serde(default)]
    pub latency_p95_ms: f64,
    #[serde(default)]
    pub warmup_ms: f64,
    #[serde(default)]
    pub missing_relevant_ids: usize,
    #[serde(default)]
    pub questions_with_missing_ids: usize,
    #[serde(default)]
    pub unmeasurable_questions: usize,
    #[serde(default)]
    pub mean_graph_tokens: f64,
    #[serde(default)]
    pub mean_hop_ball_tokens: f64,
    #[serde(default)]
    pub graph_token_ratio: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_query_ability: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbilityScore {
    pub ability: String,
    pub count: usize,
    pub ndcg_at_k: f64,
    pub recall_at_k: f64,
}

pub struct BenchmarkHarness {
    dataset: Vec<EvaluationSample>,
    k: usize,
}

impl BenchmarkHarness {
    pub fn new(dataset: Vec<EvaluationSample>) -> Self {
        Self { dataset, k: 10 }
    }

    pub fn with_k(mut self, k: usize) -> Self {
        self.k = k.clamp(1, 50);
        self
    }

    pub async fn run(&self, pool: &PgPool) -> Result<EvalReport> {
        let cfg = EvalConfig {
            k: self.k,
            ..EvalConfig::default()
        };
        run_faro_eval(pool, &self.dataset, &cfg).await
    }
}

pub async fn run_faro_eval(
    pool: &PgPool,
    samples: &[EvaluationSample],
    cfg: &EvalConfig,
) -> Result<EvalReport> {
    let k = cfg.k.clamp(1, 50);
    if samples.is_empty() {
        return Ok(EvalReport {
            sample_count: 0,
            k,
            ndcg_at_k: 0.0,
            mrr: 0.0,
            precision_at_k: 0.0,
            recall_at_k: 0.0,
            mean_exact_match: 0.0,
            mean_f1: 0.0,
            latency_p50_ms: 0.0,
            latency_p95_ms: 0.0,
            warmup_ms: 0.0,
            mean_response_tokens: 0.0,
            max_response_tokens: 0,
            per_ability: Vec::new(),
            abstention_accuracy: None,
            false_abstention_rate: None,
            ndcg_ci95: (0.0, 0.0),
            minimum_detectable_effect: f64::INFINITY,
            per_query_ndcg: Vec::new(),
            scored_by_id: false,
            missing_relevant_ids: 0,
            questions_with_missing_ids: 0,
            unmeasurable_questions: 0,
            mean_graph_tokens: 0.0,
            mean_hop_ball_tokens: 0.0,
            graph_token_ratio: 0.0,
            per_query_ability: Vec::new(),
        });
    }

    let mut relevance_lists: Vec<Vec<bool>> = Vec::with_capacity(samples.len());
    let mut ndcg_scores: Vec<f64> = Vec::with_capacity(samples.len());
    let mut ndcg_sum = 0.0;
    let mut prec_sum = 0.0;
    let mut recall_sum = 0.0;
    let mut em_sum = 0.0_f32;
    let mut f1_sum = 0.0_f32;

    let all_by_id = samples
        .iter()
        .filter(|s| !s.abstain)
        .all(|s| s.scored_by_id());

    let mut token_sum = 0usize;
    let mut token_max = 0usize;

    use std::collections::BTreeMap;
    let mut per_ability: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
    let mut abstain_total = 0usize;
    let mut abstain_correct = 0usize;
    let mut answerable_total = 0usize;
    let mut false_abstentions = 0usize;
    let mut qa_count = 0usize;

    let surviving_ids = surviving_relevant_ids(pool, samples).await?;
    let audit = audit_ground_truth(samples, &surviving_ids);

    let warmup_started = std::time::Instant::now();
    crate::embeddings::onnx::embed("warm up")
        .await
        .context("embedder warm-up failed during eval")?;
    if cfg.rerank {
        crate::search::rerank::warm_up().await;
    }
    let warmup_ms = warmup_started.elapsed().as_secs_f64() * 1000.0;

    let mut latencies_ms: Vec<f64> = Vec::new();
    let mut graph_tok_sum = 0.0;
    let mut hop_tok_sum = 0.0;
    let mut per_query_ability: Vec<String> = Vec::new();
    for sample in samples {
        let args = faro_args(&sample.query, cfg, k);
        let started = std::time::Instant::now();
        let response = crate::handlers::faro::handle(pool, args)
            .await
            .context("faro handle failed during eval")?;

        latencies_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let cost = crate::search::budget::count_tokens(&response.to_string());
        token_sum += cost;
        token_max = token_max.max(cost);
        graph_tok_sum += response
            .get("graph_context_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        hop_tok_sum += response
            .get("graph_hop_ball_tokens")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let ranked = extract_ranked(&response);
        let rels: Vec<bool> = ranked.iter().map(|hit| hit.is_relevant(sample)).collect();

        if sample.abstain {
            abstain_total += 1;
            if ranked.is_empty() {
                abstain_correct += 1;
            }
            let e = per_ability
                .entry(
                    sample
                        .ability
                        .clone()
                        .unwrap_or_else(|| "abstention".into()),
                )
                .or_insert((0, 0.0, 0.0));
            e.0 += 1;
            continue;
        }

        let total_rel = if sample.scored_by_id() {
            resolvable_relevant_count(sample, &surviving_ids)
        } else {
            sample.relevant_count()
        };
        if total_rel == 0 {
            continue;
        }

        answerable_total += 1;
        if ranked.is_empty() {
            false_abstentions += 1;
        }

        let s_ndcg = ndcg_at_k(&rels, k, total_rel);
        let s_recall = recall_at_k(&rels, total_rel, k);
        ndcg_sum += s_ndcg;
        ndcg_scores.push(s_ndcg);
        per_query_ability.push(
            sample
                .ability
                .clone()
                .or_else(|| sample.question_class.clone())
                .unwrap_or_default(),
        );
        prec_sum += precision_at_k(&rels, k);
        recall_sum += s_recall;
        relevance_lists.push(rels);

        if let Some(ability) = &sample.ability {
            let e = per_ability.entry(ability.clone()).or_insert((0, 0.0, 0.0));
            e.0 += 1;
            e.1 += s_ndcg;
            e.2 += s_recall;
        }

        if let Some(expected) = &sample.expected_answer {
            let top = ranked.first().map(|h| h.content.as_str()).unwrap_or("");
            em_sum += calculate_exact_match(top, expected);
            f1_sum += calculate_f1_score(top, expected);
            qa_count += 1;
        }
    }

    let scored = ndcg_scores.len().max(1);
    let n = samples.len();

    let ability_scores: Vec<AbilityScore> = per_ability
        .into_iter()
        .map(|(ability, (count, nd, rc))| {
            let denom = count.max(1) as f64;
            AbilityScore {
                ability,
                count,
                ndcg_at_k: nd / denom,
                recall_at_k: rc / denom,
            }
        })
        .collect();

    let (_, lo, hi) = bootstrap_ci(&ndcg_scores, BOOTSTRAP_ITERATIONS, 0.95);
    let mde = minimum_detectable_effect(&ndcg_scores);

    Ok(EvalReport {
        sample_count: n,
        k,
        ndcg_at_k: ndcg_sum / scored as f64,
        mrr: mrr(&relevance_lists),
        precision_at_k: prec_sum / scored as f64,
        recall_at_k: recall_sum / scored as f64,
        mean_exact_match: if qa_count > 0 {
            em_sum / qa_count as f32
        } else {
            0.0
        },
        mean_f1: if qa_count > 0 {
            f1_sum / qa_count as f32
        } else {
            0.0
        },
        mean_response_tokens: token_sum as f64 / n as f64,
        max_response_tokens: token_max,
        per_ability: ability_scores,
        abstention_accuracy: if abstain_total > 0 {
            Some(abstain_correct as f64 / abstain_total as f64)
        } else {
            None
        },
        false_abstention_rate: if answerable_total > 0 {
            Some(false_abstentions as f64 / answerable_total as f64)
        } else {
            None
        },
        ndcg_ci95: (lo, hi),
        minimum_detectable_effect: mde,
        latency_p50_ms: percentile_ms(&latencies_ms, 0.50),
        latency_p95_ms: percentile_ms(&latencies_ms, 0.95),
        warmup_ms,
        per_query_ndcg: ndcg_scores,
        scored_by_id: all_by_id,
        missing_relevant_ids: audit.missing_ids,
        questions_with_missing_ids: audit.affected_questions,
        unmeasurable_questions: audit.unmeasurable_questions,
        mean_graph_tokens: graph_tok_sum / n as f64,
        mean_hop_ball_tokens: hop_tok_sum / n as f64,
        graph_token_ratio: if hop_tok_sum > 0.0 {
            graph_tok_sum / hop_tok_sum
        } else {
            0.0
        },
        per_query_ability,
    })
}

async fn surviving_relevant_ids(
    pool: &PgPool,
    samples: &[EvaluationSample],
) -> Result<HashSet<Uuid>> {
    let wanted: Vec<Uuid> = samples
        .iter()
        .filter(|s| !s.abstain)
        .flat_map(|s| s.relevant_ids.iter())
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();
    if wanted.is_empty() {
        return Ok(HashSet::new());
    }

    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM brain_observations WHERE id = ANY($1)")
        .bind(&wanted)
        .fetch_all(pool)
        .await
        .context("resolving which relevant_ids still exist in the corpus")?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

fn resolves(id: &str, surviving: &HashSet<Uuid>) -> bool {
    Uuid::parse_str(id).is_ok_and(|parsed| surviving.contains(&parsed))
}

fn resolvable_relevant_count(sample: &EvaluationSample, surviving: &HashSet<Uuid>) -> usize {
    sample
        .relevant_ids
        .iter()
        .filter(|id| resolves(id.as_str(), surviving))
        .count()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GroundTruthAudit {
    missing_ids: usize,
    affected_questions: usize,
    unmeasurable_questions: usize,
}

fn audit_ground_truth(samples: &[EvaluationSample], surviving: &HashSet<Uuid>) -> GroundTruthAudit {
    let mut missing: HashSet<&str> = HashSet::new();
    let mut audit = GroundTruthAudit::default();

    for sample in samples.iter().filter(|s| !s.abstain && s.scored_by_id()) {
        let gone: Vec<&str> = sample
            .relevant_ids
            .iter()
            .filter(|id| !resolves(id.as_str(), surviving))
            .map(String::as_str)
            .collect();
        if gone.is_empty() {
            continue;
        }
        audit.affected_questions += 1;
        if gone.len() == sample.relevant_ids.len() {
            audit.unmeasurable_questions += 1;
        }
        missing.extend(gone);
    }

    audit.missing_ids = missing.len();
    audit
}

struct Hit {
    id: Option<String>,
    content: String,
}

impl Hit {
    fn is_relevant(&self, sample: &EvaluationSample) -> bool {
        if sample.scored_by_id() {
            return self
                .id
                .as_ref()
                .is_some_and(|id| sample.relevant_ids.contains(id));
        }
        is_relevant_by_marker(&self.content, &sample.relevant_markers)
    }
}

fn extract_ranked(response: &Value) -> Vec<Hit> {
    let mut out = Vec::new();
    let results = response
        .get("results")
        .or_else(|| response.get("observations"))
        .and_then(|v| v.as_array());
    if let Some(arr) = results {
        for item in arr {
            let id = item.get("id").and_then(|v| v.as_str()).map(str::to_string);
            let content = item
                .get("content")
                .or_else(|| item.get("c"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            out.push(Hit { id, content });
        }
    }
    out
}

fn is_relevant_by_marker(content: &str, markers: &[String]) -> bool {
    let lower = content.to_lowercase();
    markers
        .iter()
        .any(|m| !m.is_empty() && lower.contains(&m.to_lowercase()))
}

pub fn percentile_ms(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eval_scores_the_whole_ranking_not_what_fits_in_a_token_budget() {
        let args = faro_args("cualquier consulta", &EvalConfig::default(), 10);

        let budget = args
            .get("max_tokens")
            .and_then(|v| v.as_i64())
            .expect("faro lee max_tokens con as_i64: un float o un ausente cae al default de 5000");

        assert!(
            budget > 5000,
            "con el default de faro (5000) las filas que no caben se puntúan como fallos \
             de ranking cuando sólo son fallos de longitud; medido: verbose pesa 5251"
        );
    }

    #[tokio::test]
    async fn empty_dataset_short_circuits_before_any_warmup() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://eval-test:unused@localhost/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network");
        let cfg = EvalConfig::default();

        let report = run_faro_eval(&pool, &[], &cfg)
            .await
            .expect("an empty dataset must not touch the pool or the models");

        assert_eq!(report.sample_count, 0);
        assert_eq!(report.warmup_ms, 0.0);
    }

    fn sample_by_id(ids: &[&str]) -> EvaluationSample {
        EvaluationSample {
            query: "¿en qué lenguaje está escrito?".into(),
            relevant_ids: ids.iter().map(|s| s.to_string()).collect(),
            relevant_markers: vec![],
            expected_answer: None,
            ability: None,
            abstain: false,
            gold_entities: Vec::new(),
            question_class: None,
        }
    }

    fn marker_sample() -> EvaluationSample {
        EvaluationSample {
            query: "error conexión postgres".into(),
            relevant_ids: HashSet::new(),
            relevant_markers: vec!["postgres".into()],
            expected_answer: None,
            ability: None,
            abstain: false,
            gold_entities: Vec::new(),
            question_class: None,
        }
    }

    fn hit(id: &str, content: &str) -> Hit {
        Hit {
            id: Some(id.into()),
            content: content.into(),
        }
    }

    const ALIVE: &str = "11111111-1111-4111-8111-111111111111";
    const ALSO_ALIVE: &str = "22222222-2222-4222-8222-222222222222";
    const DELETED: &str = "33333333-3333-4333-8333-333333333333";
    const ALSO_DELETED: &str = "44444444-4444-4444-8444-444444444444";

    fn corpus(ids: &[&str]) -> HashSet<Uuid> {
        ids.iter()
            .map(|id| Uuid::parse_str(id).expect("fixture ids must be UUIDs"))
            .collect()
    }

    #[test]
    fn a_deleted_relevant_document_leaves_the_benchmark_ceiling_below_one() {
        let sample = sample_by_id(&[ALIVE, DELETED]);
        let found_everything_that_still_exists = vec![true, false, false];

        let honest = ndcg_at_k(
            &found_everything_that_still_exists,
            10,
            resolvable_relevant_count(&sample, &corpus(&[ALIVE])),
        );
        let against_the_dataset = ndcg_at_k(&found_everything_that_still_exists, 10, 2);

        assert!(
            against_the_dataset < 0.99,
            "this fixture only proves anything if counting the deleted document caps the score \
             below 1,0 — it scored {against_the_dataset:.4}"
        );
        assert!(
            (honest - 1.0).abs() < 1e-9,
            "a run that returned every relevant document STILL IN THE CORPUS must score 1,0; \
             scoring {honest:.4} blames the search for a row somebody deleted"
        );
    }

    #[test]
    fn ids_that_are_not_uuids_are_treated_as_gone_not_as_present() {
        let sample = sample_by_id(&["not-a-uuid"]);

        assert_eq!(
            resolvable_relevant_count(&sample, &corpus(&[ALIVE])),
            0,
            "an id the database cannot even be asked about is unresolvable, and counting it \
             would put back the very ceiling this resolution removes"
        );
    }

    #[test]
    fn the_audit_counts_distinct_missing_ids_the_questions_they_hit_and_the_ones_left_blind() {
        let samples = vec![
            sample_by_id(&[ALIVE]),
            sample_by_id(&[ALIVE, DELETED]),
            sample_by_id(&[DELETED, ALSO_DELETED]),
            sample_by_id(&[ALSO_ALIVE, ALSO_DELETED]),
        ];

        let audit = audit_ground_truth(&samples, &corpus(&[ALIVE, ALSO_ALIVE]));

        assert_eq!(
            audit.missing_ids, 2,
            "two rows are gone, and the fourth question reuses one of them: counting id \
             occurrences instead of distinct ids would report 4"
        );
        assert_eq!(audit.affected_questions, 3);
        assert_eq!(
            audit.unmeasurable_questions, 1,
            "only the third question lost ALL its ground truth; the others can still be scored \
             against what survives"
        );
    }

    #[test]
    fn an_abstention_question_is_not_audited_for_missing_ground_truth() {
        let mut abstainer = sample_by_id(&[DELETED]);
        abstainer.abstain = true;

        assert_eq!(
            audit_ground_truth(&[abstainer], &corpus(&[ALIVE])),
            GroundTruthAudit::default(),
            "an abstention question is scored by whether the search returned nothing at all, \
             so its relevant_ids are not a denominator and cannot deflate one"
        );
    }

    #[test]
    fn a_substring_scored_dataset_is_audited_without_a_single_database_row() {
        let samples = vec![marker_sample()];

        assert_eq!(
            audit_ground_truth(&samples, &HashSet::new()),
            GroundTruthAudit::default(),
            "a dataset with no relevant_ids has no ids to discount: reporting a lowered ceiling \
             for it would be inventing a limitation"
        );
    }

    #[tokio::test]
    async fn resolving_ground_truth_does_not_dial_the_database_when_there_are_no_ids_to_resolve() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(50))
            .connect_lazy("postgres://eval-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network");

        let surviving = surviving_relevant_ids(&pool, &[marker_sample()])
            .await
            .expect("a substring-scored dataset must not cost a query at all");

        assert!(surviving.is_empty());
    }

    #[test]
    fn id_scoring_does_not_reward_merely_being_on_topic() {
        let sample = sample_by_id(&["the-answer"]);

        assert!(hit("the-answer", "cuba-memorys está escrito en Rust").is_relevant(&sample));

        assert!(
            !hit("some-other-row", "cuba-memorys usa PostgreSQL con pgvector").is_relevant(&sample),
            "an on-topic document that is not the answer must not count as relevant"
        );
    }

    #[test]
    fn marker_scoring_survives_for_old_datasets() {
        let legacy = marker_sample();
        assert!(!legacy.scored_by_id());
        assert!(hit("x", "fallo de conexión postgres en docker").is_relevant(&legacy));
        assert!(!hit("y", "todo ok").is_relevant(&legacy));
    }

    #[test]
    fn relevant_count_is_the_size_of_the_ground_truth() {
        assert_eq!(sample_by_id(&["a", "b", "c"]).relevant_count(), 3);
    }

    #[test]
    fn ids_are_extracted_from_verbose_and_compact() {
        let verbose = serde_json::json!({
            "results": [{"id": "abc", "content": "texto largo", "entity_name": "e"}]
        });
        let compact = serde_json::json!({
            "results": [{"id": "abc", "c": "texto largo", "e": "e"}]
        });

        for (shape, response) in [("verbose", verbose), ("compact", compact)] {
            let hits = extract_ranked(&response);
            assert_eq!(hits.len(), 1, "{shape}");
            assert_eq!(hits[0].id.as_deref(), Some("abc"), "{shape}: falta el id");
            assert_eq!(
                hits[0].content, "texto largo",
                "{shape}: falta el contenido"
            );
        }
    }
}

#[cfg(test)]
mod wiring_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn a_run_whose_ground_truth_was_deleted_reports_a_lowered_ceiling() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        let pool = crate::db::create_pool(&url)
            .await
            .expect("connect to test database");

        let ghost = Uuid::new_v4();
        let samples = vec![EvaluationSample {
            query: "una consulta cualquiera sobre nada en particular".into(),
            relevant_ids: std::iter::once(ghost.to_string()).collect(),
            relevant_markers: vec![],
            expected_answer: None,
            ability: None,
            abstain: false,
            gold_entities: Vec::new(),
            question_class: None,
        }];

        let report = run_faro_eval(&pool, &samples, &EvalConfig::default())
            .await
            .expect("the eval must run even when its ground truth is gone");

        assert_eq!(
            report.missing_relevant_ids, 1,
            "the run has to notice that the only document it was scored against does not \
             exist. Reverting the call site alone leaves every unit test green, because \
             they exercise the counting function and not its use — this is the wiring"
        );
        assert_eq!(report.unmeasurable_questions, 1);
        assert!(
            report.per_query_ndcg.is_empty(),
            "a question whose ground truth is gone must be left OUT of the scores, not \
             scored as a zero. Counting it as a miss punishes retrieval for a document \
             nobody could have returned, and this is the assertion that catches the call \
             site reverting while the counting function stays correct — the audit fields \
             above do not, because they come from a different pass"
        );
        assert!(
            crate::eval::reporters::summary_line(&report).contains("techo"),
            "and the report has to say so out loud: a benchmark whose ceiling is not 1.0 \
             produces a number nobody can compare against a published one"
        );
    }
}
