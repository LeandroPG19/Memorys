use serde::Serialize;

use super::harness::EvalReport;

#[cfg(test)]
fn report_with_warmup(warmup_ms: f64) -> EvalReport {
    EvalReport {
        sample_count: 5,
        k: 10,
        ndcg_at_k: 0.5,
        mrr: 0.5,
        precision_at_k: 0.5,
        recall_at_k: 0.5,
        mean_exact_match: 0.0,
        mean_f1: 0.0,
        mean_response_tokens: 100.0,
        max_response_tokens: 200,
        per_ability: Vec::new(),
        abstention_accuracy: None,
        false_abstention_rate: None,
        ndcg_ci95: (0.4, 0.6),
        minimum_detectable_effect: 0.369,
        per_query_ndcg: vec![0.4, 0.5, 0.6, 0.5, 0.5],
        scored_by_id: true,
        latency_p50_ms: 1500.0,
        latency_p95_ms: 1700.0,
        warmup_ms,
        missing_relevant_ids: 0,
        questions_with_missing_ids: 0,
        unmeasurable_questions: 0,
        mean_graph_tokens: 0.0,
        mean_hop_ball_tokens: 0.0,
        graph_token_ratio: 0.0,
        per_query_ability: Vec::new(),
    }
}

#[derive(Serialize)]
pub struct JsonReport<'a> {
    pub version: &'static str,
    pub samples: usize,
    pub k: usize,
    pub metrics: &'a EvalReport,
}

pub fn generate_json_report(report: &EvalReport, samples: usize, k: usize) -> String {
    let payload = JsonReport {
        version: env!("CARGO_PKG_VERSION"),
        samples,
        k,
        metrics: report,
    };
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}

pub fn summary_line(report: &EvalReport) -> String {
    let (lo, hi) = report.ndcg_ci95;
    let mut s = format!(
        "nDCG@{}={:.4} [95% CI {:.3}–{:.3}] MRR={:.4} P@{}={:.4} R@{}={:.4} (n={})",
        report.k,
        report.ndcg_at_k,
        lo,
        hi,
        report.mrr,
        report.k,
        report.precision_at_k,
        report.k,
        report.recall_at_k,
        report.sample_count
    );
    s.push_str(&format!(
        " | tokens: mean={:.0} max={}",
        report.mean_response_tokens, report.max_response_tokens
    ));
    if report.mean_hop_ball_tokens > 0.0 || report.mean_graph_tokens > 0.0 {
        s.push_str(&format!(
            " | grafo: paths={:.0}tok bola={:.0}tok ratio={:.2}",
            report.mean_graph_tokens, report.mean_hop_ball_tokens, report.graph_token_ratio
        ));
    }
    if report.latency_p50_ms > 0.0 {
        s.push_str(&format!(
            " | latencia: p50={:.0}ms p95={:.0}ms",
            report.latency_p50_ms, report.latency_p95_ms
        ));
    }
    if report.warmup_ms > 0.0 {
        s.push_str(&format!(
            " | warm-up (fuera de la latencia): {:.0}ms",
            report.warmup_ms
        ));
    }

    if let Some(abst) = report.abstention_accuracy {
        s.push_str(&format!("\nabstention={:.0}%", abst * 100.0));
        if let Some(fa) = report.false_abstention_rate {
            s.push_str(&format!(
                " (falsas abstenciones sobre lo respondible={:.0}%)",
                fa * 100.0
            ));
        }
    }
    for a in &report.per_ability {
        s.push_str(&format!(
            "\n  [{}] n={} nDCG@{}={:.4} R@{}={:.4}",
            a.ability, a.count, report.k, a.ndcg_at_k, report.k, a.recall_at_k
        ));
    }
    s.push_str(&limitations_block(report));
    s
}

const LABEL_WIDTH: usize = 17;
const WRAP: &str = "\n                      ";

fn limitation(label: &str, body: &str) -> String {
    format!("\n  · {label:<width$} {body}", width = LABEL_WIDTH)
}

fn limitations_block(report: &EvalReport) -> String {
    if report.sample_count == 0 {
        return String::new();
    }

    let mut bullets = String::new();

    if report.missing_relevant_ids > 0 {
        bullets.push_str(&limitation(
            "techo alcanzable",
            &format!(
                "{} id(s) relevante(s) del dataset ya no están en el corpus,{WRAP}\
                 y afectan a {} pregunta(s). Esas preguntas no pueden llegar{WRAP}\
                 a 1,0 por bien que ordene la búsqueda: el techo de esta{WRAP}\
                 corrida NO es 1,0, y comparar su nDCG contra uno publicado{WRAP}\
                 sobre un corpus íntegro no significa nada.",
                report.missing_relevant_ids, report.questions_with_missing_ids,
            ),
        ));
    }

    if report.unmeasurable_questions > 0 {
        bullets.push_str(&limitation(
            "preguntas mudas",
            &format!(
                "{} pregunta(s) perdieron TODOS sus ids y quedaron fuera del{WRAP}\
                 promedio: no miden nada. El n que vale es el de la línea del{WRAP}\
                 efecto mínimo detectable, no el samples= de arriba.",
                report.unmeasurable_questions,
            ),
        ));
    }

    if report.minimum_detectable_effect.is_finite() {
        bullets.push_str(&limitation(
            "tamaño de muestra",
            &format!(
                "con n={} puntuadas el efecto mínimo detectable es {:.3} nDCG{WRAP}\
                 (80% poder, α=.05): por debajo de eso esta corrida no{WRAP}\
                 distingue una mejora del ruido. Es la COTA DEL PEOR CASO, la{WRAP}\
                 de dos muestras independientes; entre dos corridas del mismo{WRAP}\
                 dataset el test es pareado y resuelve mucho menos, así que{WRAP}\
                 usá --compare sobre per_query_ndcg, no este número.",
                report.per_query_ndcg.len(),
                report.minimum_detectable_effect,
            ),
        ));
    }

    if !report.scored_by_id {
        bullets.push_str(&limitation(
            "el criterio",
            &[
                "puntuado por SUBSTRING, no por id: cuenta como acierto",
                "cualquier documento que MENCIONE el término, responda o no.",
                "No es comparable con un dataset con relevant_ids.",
            ]
            .join(WRAP),
        ));
    }

    bullets.push_str(&limitation(
        "el corpus",
        &[
            "es la memoria del desarrollador, no un conjunto público. Mide",
            "ESTE corpus con ESTAS preguntas, así que el número no es",
            "comparable con el de ningún otro proyecto ni con uno publicado.",
        ]
        .join(WRAP),
    ));

    if report.warmup_ms > 0.0 {
        bullets.push_str(&limitation(
            "la latencia",
            &format!(
                "p50/p95 excluyen los {:.0}ms de carga de los modelos, que se{WRAP}\
                 pagan una vez por proceso. Un cliente en frío SÍ los ve.",
                report.warmup_ms,
            ),
        ));
    }

    format!("\n\nLO QUE ESTA CORRIDA NO ESTABLECE — leelo antes de citar el número:{bullets}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_line_reports_the_model_load_separately_from_the_search_latency() {
        let report = report_with_warmup(70180.0);
        let line = summary_line(&report);

        assert!(
            line.contains("warm-up (fuera de la latencia): 70180ms"),
            "the cold-start model load must be visible, not silently folded into p50/p95: {line}"
        );
    }

    #[test]
    fn summary_line_omits_the_warmup_note_when_the_models_were_already_warm() {
        let report = report_with_warmup(0.0);
        let line = summary_line(&report);

        assert!(!line.contains("warm-up"), "{line}");
    }

    #[test]
    fn json_report_exposes_warmup_ms_as_its_own_field() {
        let report = report_with_warmup(70180.0);
        let json = generate_json_report(&report, report.sample_count, report.k);

        assert!(json.contains("\"warmup_ms\": 70180.0"), "{json}");
    }

    fn report_with_deleted_ground_truth(
        missing_ids: usize,
        affected: usize,
        unmeasurable: usize,
    ) -> EvalReport {
        EvalReport {
            missing_relevant_ids: missing_ids,
            questions_with_missing_ids: affected,
            unmeasurable_questions: unmeasurable,
            ..report_with_warmup(0.0)
        }
    }

    #[test]
    fn the_report_declares_that_the_ceiling_is_below_one_when_relevant_documents_were_deleted() {
        let line = summary_line(&report_with_deleted_ground_truth(6, 6, 0));

        assert!(
            line.contains("techo alcanzable")
                && line.contains("6 id(s) relevante(s) del dataset ya no están en el corpus")
                && line.contains("afectan a 6 pregunta(s)")
                && line.contains("el techo de esta"),
            "a run whose ground truth was partly deleted must say so with the counts, or its \
             nDCG gets compared against a published number as if both could reach 1,0: {line}"
        );
    }

    #[test]
    fn the_report_claims_no_lowered_ceiling_when_every_relevant_document_still_exists() {
        let line = summary_line(&report_with_deleted_ground_truth(0, 0, 0));

        assert!(
            !line.contains("techo"),
            "declaring a ceiling nobody lowered is noise, and noise is what makes the block \
             stop being read: {line}"
        );
    }

    #[test]
    fn the_report_names_the_questions_it_dropped_for_having_no_resolvable_ground_truth() {
        let line = summary_line(&report_with_deleted_ground_truth(4, 3, 2));

        assert!(
            line.contains("preguntas mudas")
                && line.contains("2 pregunta(s) perdieron TODOS sus ids"),
            "questions excluded from the mean change what the mean is over; hiding the \
             exclusion is how a shrinking n passes for a stable one: {line}"
        );
    }

    #[test]
    fn the_report_stays_silent_about_dropped_questions_when_it_dropped_none() {
        let line = summary_line(&report_with_deleted_ground_truth(4, 3, 0));

        assert!(
            line.contains("techo alcanzable") && !line.contains("preguntas mudas"),
            "missing ids and unmeasurable questions are different facts: a question that lost \
             one of two ids is still scored, and must not be reported as dropped: {line}"
        );
    }

    #[test]
    fn the_report_states_the_smallest_effect_this_sample_size_can_resolve() {
        let line = summary_line(&report_with_warmup(0.0));

        assert!(
            line.contains("con n=5 puntuadas el efecto mínimo detectable es 0.369"),
            "the limit has to carry the computed number: 'underpowered' in the abstract is \
             what let a 0.03 difference be reported as a regression: {line}"
        );
    }

    #[test]
    fn the_report_says_the_corpus_is_private_so_the_number_is_nobody_elses_baseline() {
        let line = summary_line(&report_with_warmup(0.0));

        assert!(
            line.contains("el corpus")
                && line.contains("no es")
                && line.contains("comparable con el de ningún otro proyecto"),
            "this project already published an nDCG that was read as a public benchmark score; \
             every run must say the corpus is the developer's own: {line}"
        );
    }

    #[test]
    fn the_report_flags_substring_scoring_only_for_the_datasets_that_use_it() {
        let by_substring = EvalReport {
            scored_by_id: false,
            ..report_with_warmup(0.0)
        };

        assert!(
            summary_line(&by_substring).contains("puntuado por SUBSTRING, no por id"),
            "substring scoring counts any document that MENTIONS the term, so its numbers are \
             not the same metric as an id-scored run"
        );
        assert!(!summary_line(&report_with_warmup(0.0)).contains("SUBSTRING"));
    }

    #[test]
    fn an_empty_run_gets_no_limitations_block_because_it_has_no_number_to_qualify() {
        let empty = EvalReport {
            sample_count: 0,
            minimum_detectable_effect: f64::INFINITY,
            per_query_ndcg: Vec::new(),
            ..report_with_warmup(0.0)
        };

        assert!(
            !summary_line(&empty).contains("LO QUE ESTA CORRIDA NO ESTABLECE"),
            "there is nothing to caveat when nothing was measured"
        );
    }

    #[test]
    fn every_wrapped_line_of_the_limitations_block_stays_under_the_text_column() {
        let report = report_with_deleted_ground_truth(6, 6, 2);
        let line = summary_line(&EvalReport {
            scored_by_id: false,
            warmup_ms: 70180.0,
            ..report
        });
        let block = line
            .split_once("LO QUE ESTA CORRIDA NO ESTABLECE")
            .expect("the block must be present with every limitation switched on")
            .1;

        let mut bullets = 0;
        for l in block.lines().skip(1).filter(|l| !l.is_empty()) {
            let text_starts_at = if l.starts_with("  · ") {
                bullets += 1;
                l.chars()
                    .enumerate()
                    .skip(21)
                    .find(|(_, c)| *c != ' ')
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            } else {
                l.chars().take_while(|c| *c == ' ').count()
            };
            assert_eq!(
                text_starts_at, 22,
                "a limitation whose lines do not line up under one text column reads as filler \
                 and gets skipped; this one starts at column {text_starts_at}: {l:?}"
            );
        }
        assert_eq!(
            bullets, 6,
            "this fixture switches every limitation on, so a bullet that stopped rendering \
             would otherwise pass unnoticed"
        );
    }
}
