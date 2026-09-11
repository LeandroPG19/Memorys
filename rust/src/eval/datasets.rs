use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EvaluationSample {
    pub query: String,
    pub relevant_ids: HashSet<String>,
    pub relevant_markers: Vec<String>,
    pub expected_answer: Option<String>,
    pub ability: Option<String>,
    pub abstain: bool,
    pub gold_entities: Vec<String>,
    pub question_class: Option<String>,
}

impl EvaluationSample {
    pub fn scored_by_id(&self) -> bool {
        !self.relevant_ids.is_empty()
    }

    pub fn relevant_count(&self) -> usize {
        if self.scored_by_id() {
            self.relevant_ids.len()
        } else {
            self.relevant_markers.len().max(1)
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonlRow {
    query: String,
    #[serde(default)]
    relevant_ids: Vec<String>,
    #[serde(default)]
    relevant_markers: Vec<String>,
    #[serde(default)]
    relevant: Vec<String>,
    #[serde(default)]
    expected_answer: Option<String>,
    #[serde(default)]
    ability: Option<String>,
    #[serde(default)]
    question_type: Option<String>,
    #[serde(default)]
    abstain: bool,
    #[serde(default)]
    gold_entities: Vec<String>,
    #[serde(default)]
    question_class: Option<String>,
}

pub fn builtin_retrieval_set() -> Vec<EvaluationSample> {
    vec![
        EvaluationSample {
            query: "error conexión postgres".into(),
            relevant_ids: HashSet::new(),
            relevant_markers: vec!["postgres".into(), "conexión".into()],
            expected_answer: None,
            ability: Some("information-extraction".into()),
            abstain: false,
            gold_entities: Vec::new(),
            question_class: Some("factoid".into()),
        },
        EvaluationSample {
            query: "decisión arquitectura MCP".into(),
            relevant_ids: HashSet::new(),
            relevant_markers: vec!["MCP".into(), "arquitectura".into()],
            expected_answer: None,
            ability: Some("information-extraction".into()),
            abstain: false,
            gold_entities: Vec::new(),
            question_class: Some("factoid".into()),
        },
    ]
}

pub fn load_jsonl_dataset(path: &str) -> Result<Vec<EvaluationSample>, io::Error> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("dataset not found: {path}"),
        ));
    }
    let file = File::open(p)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    let mut legacy = 0usize;

    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: JsonlRow = serde_json::from_str(trimmed).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {}: {e}", line_no + 1),
            )
        })?;

        let markers = if !row.relevant_markers.is_empty() {
            row.relevant_markers
        } else {
            row.relevant
        };
        let ids: HashSet<String> = row.relevant_ids.into_iter().collect();

        if ids.is_empty() && markers.is_empty() && !row.abstain {
            continue;
        }
        if ids.is_empty() && !row.abstain {
            legacy += 1;
        }

        out.push(EvaluationSample {
            query: row.query,
            relevant_ids: ids,
            relevant_markers: markers,
            expected_answer: row.expected_answer,
            ability: row.ability.or(row.question_type.clone()),
            abstain: row.abstain,
            gold_entities: row.gold_entities,
            question_class: row.question_class.or(row.question_type),
        });
    }

    if legacy > 0 {
        eprintln!(
            "eval: AVISO — {legacy} muestra(s) sin `relevant_ids`, puntuadas por coincidencia \
             de substring. Ese criterio cuenta como acierto cualquier documento que MENCIONE \
             el término, responda o no, y sus cifras NO son comparables con las de un dataset \
             puntuado por id."
        );
    }

    Ok(out)
}

pub fn load_locomo_dataset(path: &str) -> Result<Vec<EvaluationSample>, io::Error> {
    if path.is_empty() {
        return Ok(builtin_retrieval_set());
    }
    load_jsonl_dataset(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn factoid_answer_path() -> String {
        format!(
            "{}/eval-datasets/factoid-answer.jsonl",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn factoid_answer_jsonl_loads_as_id_scored_factoids() {
        let samples = load_jsonl_dataset(&factoid_answer_path())
            .expect("factoid-answer.jsonl must exist next to the other eval datasets");
        assert!(
            !samples.is_empty(),
            "factoid-answer.jsonl must not be empty"
        );
        for (i, sample) in samples.iter().enumerate() {
            assert_eq!(
                sample.question_class.as_deref(),
                Some("factoid"),
                "row {i} must be question_class=factoid"
            );
            assert!(
                sample.scored_by_id(),
                "row {i} must score by observation UUID, not substring markers"
            );
            assert!(
                (1..=3).contains(&sample.relevant_ids.len()),
                "row {i} gold must be 1–3 observations whose content answers the question, not a top-N importance list; got {}",
                sample.relevant_ids.len()
            );
            for id in &sample.relevant_ids {
                Uuid::parse_str(id)
                    .unwrap_or_else(|_| panic!("row {i} relevant_id {id} is not a UUID"));
            }
        }
    }
}
