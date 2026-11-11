// needs-the-models: it reads the tokenizer.json of the embedder, the reranker and the NLI model and opens the embedder's ONNX session, and the check job of ci.yml has none of them
//
//! CI-D4. The guard that a new `tokenizers` or `tiktoken-rs` moves no token id and no stored vector.
//!
//! An install keeps every embedding in its database and does not keep the tokens that made it, so a
//! library that cut one character differently would mix two spaces of vectors without a word. What is
//! pinned lives in `model_parity/expected.rs` and is what the libraries of the day gave: `tokenizers`
//! 0.21.4, `tiktoken-rs` 0.7.0 (its half is in `search::budget`), ONNX Runtime 1.26.0. This file builds
//! the same inputs again and compares.
//!
//! * The three tokenizers are built here with the parameters the product gives them (512 and
//!   `BatchLongest` for the embedder and the reranker; 512 and no padding for the NLI), which makes
//!   these tests the guard of the LIBRARY. The vectors go through `embeddings::onnx`, which is the guard
//!   of the PRODUCT: prefix, truncation, pooling and norm.
//! * Ids are exact. Vectors are within `MAX_COMPONENT_GAP` per component and `MAX_COSINE_GAP` in
//!   `1 - cos`: the inference does not change with the tokenizer, so what is expected is equality, and
//!   the tolerance is only the room f32 sums need to be reordered.
//! * The vectors depend on three variables a judge or an operator may set (the pooling and the two
//!   prefixes). The judge exports `CUBA_POOLING=cls` (`gate_model_env`, scripts/gate-lock.sh), so the
//!   values are only comparable under the environment they were made in, and that environment is pinned
//!   beside them: a different one is red and names the variable, instead of a vector that "moved".
//! * Without the models these tests fail saying what is missing. They never pass empty and never compare
//!   the vector of the hash fallback.
//! * A table that is empty makes its test fail and print the block to paste over it, in the format
//!   `expected.rs` holds. That is how the values are made, and how they are made again when a library is
//!   deliberately changed.
//! * This process never writes its environment: the embedder reads it on every call.

use std::fmt::Write as _;
use std::path::PathBuf;

#[path = "model_parity/expected.rs"]
mod expected;

const WITHOUT_THE_MODELS: &str = "This is the guard that a new tokenizers or tiktoken-rs moves no token id and no stored vector, and \
     it needs the models and the ONNX Runtime the judge uses (ONNX_MODEL_PATH, CUBA_RERANKER_PATH, \
     CUBA_NLI_PATH and ORT_DYLIB_PATH, installed by `memory-industry models all`). Without them there is \
     nothing to compare, so it fails and does not pass empty";

const WRITING: &str = "writing to a String cannot fail";

/// Each component of a vector is about 0.05 (1/√384). Reordering f32 sums in 12 layers moves one by
/// 1e-6 at most, and a different token in a short text moves components by 1e-3 or more: S1 of the spec
/// (truncating the long text to 511) measures the weakest case, a long text with its last token changed.
/// A tolerance that lets S1 through is too loose and is tightened, never raised.
const MAX_COMPONENT_GAP: f32 = 1e-5;

/// `1 - cos` for a gap of `MAX_COMPONENT_GAP` in every component would be about 2e-8, so this is the same
/// bar seen as a direction and not as a component.
const MAX_COSINE_GAP: f64 = 1e-7;

/// The variables that change a vector of the embedder (`CUBA_EMBED_MODEL` does not: it only names the
/// model in the cache key). `CUBA_EMBEDDING_DIM` is not here either: the vector has to be 384 wide.
const VECTOR_VARIABLES: [&str; 3] = ["CUBA_POOLING", "CUBA_QUERY_PREFIX", "CUBA_PASSAGE_PREFIX"];

const ES_ACENTOS: &str = "¿Dónde quedó la decisión? Ñandú, pingüino y acción: año 2026.";
const CODIGO: &str = "fn main() { let v: Vec<u8> = vec![0xFF; 16]; println!(\"{v:?}\"); }";
const UNICODE: &str = "emoji 🙂, CJK 日本語, árabe مرحبا, e\u{301}";
const ESPACIOS: &str = "  doble  espacio\ttab\nlínea nueva   y  final  ";
const ESPECIALES: &str = "texto con </s> y <mask> y <s> dentro";
const CONSULTA: &str = "¿qué decidimos sobre sqlx 0.9?";

fn largo() -> String {
    "La memoria del agente guarda decisiones, errores y lecciones para la próxima sesión. "
        .repeat(60)
}

fn largo2() -> String {
    "El reranker puntúa cada candidato contra la consulta y los textos largos se recortan antes de llegar al modelo. "
        .repeat(60)
}

/// The texts of the embedder, without the prefix the product puts in front of them.
fn embedder_texts() -> Vec<(&'static str, String)> {
    vec![
        ("es_acentos", ES_ACENTOS.to_string()),
        ("codigo", CODIGO.to_string()),
        ("unicode", UNICODE.to_string()),
        ("espacios", ESPACIOS.to_string()),
        ("especiales", ESPECIALES.to_string()),
        ("largo", largo()),
        ("vacio", String::new()),
    ]
}

fn reranker_pairs() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "consulta_espacios",
            CONSULTA.to_string(),
            ESPACIOS.to_string(),
        ),
        ("consulta_largo", CONSULTA.to_string(), largo()),
        ("largo_largo2", largo(), largo2()),
        (
            "mask_especiales",
            "busca   <mask> aquí".to_string(),
            ESPECIALES.to_string(),
        ),
    ]
}

/// The pairs of the NLI whose ids are pinned. The long-long pair is not here: see the test.
fn nli_pairs() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "espacios_iniciales",
            "  El servidor está escrito en Rust y usa PostgreSQL.".to_string(),
            "Está escrito en Java.".to_string(),
        ),
        (
            "largo_corta",
            largo(),
            "La memoria guarda decisiones.".to_string(),
        ),
        (
            "sep_literal",
            "Usa [SEP] como separador literal.".to_string(),
            "Hay un separador.".to_string(),
        ),
    ]
}

fn model_dir(variable: &str) -> PathBuf {
    let value = std::env::var(variable).unwrap_or_default();
    assert!(
        !value.trim().is_empty(),
        "{variable} is not set. {WITHOUT_THE_MODELS}"
    );
    let dir = PathBuf::from(value.trim());
    assert!(
        dir.join("tokenizer.json").is_file(),
        "{variable} names {}, which has no tokenizer.json. {WITHOUT_THE_MODELS}",
        dir.display()
    );
    dir
}

/// The tokenizer of the model `variable` names, with the parameters the product gives it.
fn tokenizer_of(variable: &str, padded: bool) -> tokenizers::Tokenizer {
    let path = model_dir(variable).join("tokenizer.json");
    let mut tokenizer = tokenizers::Tokenizer::from_file(&path)
        .unwrap_or_else(|e| panic!("{variable}: {} does not load: {e}", path.display()));
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: 512,
            ..Default::default()
        }))
        .unwrap_or_else(|e| panic!("{variable}: the truncation does not apply: {e}"));
    if padded {
        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
    }
    tokenizer
}

struct Row {
    name: String,
    ids: Vec<u32>,
    attention_mask: Vec<u32>,
    type_ids: Vec<u32>,
}

fn row_of(name: &str, encoding: &tokenizers::Encoding) -> Row {
    Row {
        name: name.to_string(),
        ids: encoding.get_ids().to_vec(),
        attention_mask: encoding.get_attention_mask().to_vec(),
        type_ids: encoding.get_type_ids().to_vec(),
    }
}

fn first_difference(got: &[u32], want: &[u32]) -> Option<String> {
    if got == want {
        return None;
    }
    let at = got
        .iter()
        .zip(want)
        .position(|(g, w)| g != w)
        .unwrap_or(got.len().min(want.len()));
    Some(format!(
        "differs at position {at} (got {:?}, want {:?}; lengths {} and {})",
        got.get(at),
        want.get(at),
        got.len(),
        want.len()
    ))
}

/// `values` as the body of an array, `indent` spaces in, wrapped at 100 columns.
fn wrapped<T: std::fmt::Debug>(values: &[T], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut text = String::new();
    let mut line = pad.clone();
    for value in values {
        let item = format!("{value:?},");
        if line.len() > indent && line.len() + 1 + item.len() > 100 {
            text.push_str(&line);
            text.push('\n');
            line.clone_from(&pad);
        }
        if line.len() > indent {
            line.push(' ');
        }
        line.push_str(&item);
    }
    text.push_str(&line);
    text
}

fn encoded_block(constant: &str, rows: &[Row]) -> String {
    let mut block = format!(
        "--- paste into rust/tests/model_parity/expected.rs, over `pub const {constant}` ---\n\
         #[rustfmt::skip]\npub const {constant}: &[Encoded] = &[\n"
    );
    for row in rows {
        writeln!(block, "    (\n        {:?},", row.name).expect(WRITING);
        for numbers in [&row.ids, &row.attention_mask, &row.type_ids] {
            writeln!(block, "        &[\n{}\n        ],", wrapped(numbers, 12)).expect(WRITING);
        }
        block.push_str("    ),\n");
    }
    write!(block, "];\n--- end of {constant} ---").expect(WRITING);
    block
}

/// Compares `got` with the pinned table, row by row and array by array, and names every difference.
fn judge(constant: &str, got: &[Row], want: &[expected::Encoded]) {
    if want.is_empty() {
        println!("\n{}", encoded_block(constant, got));
        panic!(
            "{constant} is empty: the block above is what the libraries of the day give; paste it over \
             the empty table"
        );
    }
    let mut problems = Vec::new();
    for row in got {
        match want.iter().find(|pinned| pinned.0 == row.name) {
            None => problems.push(format!("{}: no pinned row", row.name)),
            Some(pinned) => {
                for (what, now, then) in [
                    ("ids", &row.ids, pinned.1),
                    ("attention_mask", &row.attention_mask, pinned.2),
                    ("type_ids", &row.type_ids, pinned.3),
                ] {
                    if let Some(difference) = first_difference(now, then) {
                        problems.push(format!("{}: {what} {difference}", row.name));
                    }
                }
            }
        }
    }
    for pinned in want {
        if !got.iter().any(|row| row.name == pinned.0) {
            problems.push(format!("{}: pinned, and no input makes it", pinned.0));
        }
    }
    assert!(
        problems.is_empty(),
        "{constant}: the encodings are no longer the pinned ones:\n{}",
        problems.join("\n")
    );
}

fn encode_text(tokenizer: &tokenizers::Tokenizer, name: &str, text: &str) -> Row {
    let encoding = tokenizer
        .encode(text, true)
        .unwrap_or_else(|e| panic!("{name}: does not encode: {e}"));
    row_of(name, &encoding)
}

#[test]
fn the_embedder_tokenizer_still_gives_the_pinned_ids() {
    let tokenizer = tokenizer_of("ONNX_MODEL_PATH", true);
    let mut cases: Vec<(&str, String)> = embedder_texts()
        .into_iter()
        .map(|(name, text)| (name, format!("passage: {text}")))
        .collect();
    cases.push(("consulta", format!("query: {CONSULTA}")));
    let rows: Vec<Row> = cases
        .iter()
        .map(|(name, text)| encode_text(&tokenizer, name, text))
        .collect();

    let long = rows
        .iter()
        .find(|row| row.name == "largo")
        .expect("the case `largo` is in the list");
    assert_eq!(
        long.ids.len(),
        512,
        "a text of more than 512 tokens is cut to 512, the two special ones included"
    );
    assert_eq!(long.ids.first(), Some(&0), "the first id is <s>");
    assert_eq!(long.ids.last(), Some(&2), "the last id is </s>");

    judge("EMBEDDER_IDS", &rows, expected::EMBEDDER_IDS);
}

#[test]
fn the_reranker_tokenizer_still_gives_the_pinned_ids_for_a_batch_of_pairs() {
    let tokenizer = tokenizer_of("CUBA_RERANKER_PATH", true);
    let pairs = reranker_pairs();
    let inputs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(_, query, text)| (query.as_str(), text.as_str()))
        .collect();
    let encodings = tokenizer
        .encode_batch(inputs, true)
        .unwrap_or_else(|e| panic!("the batch does not encode: {e}"));
    let rows: Vec<Row> = pairs
        .iter()
        .zip(&encodings)
        .map(|((name, _, _), encoding)| row_of(name, encoding))
        .collect();

    for row in &rows {
        assert_eq!(
            row.ids.len(),
            512,
            "{}: the four pairs of a batch have the length of the longest, the long-long one, cut to 512",
            row.name
        );
    }

    judge("RERANKER_IDS", &rows, expected::RERANKER_IDS);
}

#[test]
fn the_nli_tokenizer_still_gives_the_pinned_ids_and_splits_a_long_pair_254_255() {
    let tokenizer = tokenizer_of("CUBA_NLI_PATH", false);
    let rows: Vec<Row> = nli_pairs()
        .iter()
        .map(|(name, premise, hypothesis)| {
            let encoding = tokenizer
                .encode((premise.as_str(), hypothesis.as_str()), true)
                .unwrap_or_else(|e| panic!("{name}: does not encode: {e}"));
            row_of(name, &encoding)
        })
        .collect();

    // Two sequences of more than 512 tokens each: the 509 places the three special tokens leave are
    // split in two. Which of the two keeps 255 is the borderline `tokenizers` 0.23 may change (the cut
    // now stops early), and no vector depends on it, so the ids are not pinned: what has to hold in both
    // versions is the sum and the split.
    let (premise, hypothesis) = (largo(), largo2());
    let encoding = tokenizer
        .encode((premise.as_str(), hypothesis.as_str()), true)
        .unwrap_or_else(|e| panic!("the long-long pair does not encode: {e}"));
    assert_eq!(
        encoding.get_ids().len(),
        512,
        "a pair of two long texts is cut to 512 places"
    );
    let sequences = encoding.get_sequence_ids();
    let mut lengths = [Some(0), Some(1)].map(|which| {
        sequences
            .iter()
            .filter(|sequence| **sequence == which)
            .count()
    });
    lengths.sort_unstable();
    assert_eq!(
        lengths,
        [254, 255],
        "512 places minus the three special tokens are 509, and two long sequences share them 254 and 255"
    );

    judge("NLI_IDS", &rows, expected::NLI_IDS);
}

struct Gap {
    max_abs: f32,
    at: usize,
    one_minus_cos: f64,
}

fn gap_between(got: &[f32], want: &[f32]) -> Gap {
    let (at, max_abs) = got
        .iter()
        .zip(want)
        .enumerate()
        .map(|(index, (g, w))| (index, (g - w).abs()))
        .fold(
            (0, 0.0_f32),
            |worst, item| {
                if item.1 > worst.1 { item } else { worst }
            },
        );
    let norm = |vector: &[f32]| {
        vector
            .iter()
            .map(|x| f64::from(*x) * f64::from(*x))
            .sum::<f64>()
            .sqrt()
    };
    let dot: f64 = got
        .iter()
        .zip(want)
        .map(|(g, w)| f64::from(*g) * f64::from(*w))
        .sum();
    Gap {
        max_abs,
        at,
        one_minus_cos: 1.0 - dot / (norm(got) * norm(want)),
    }
}

fn described(value: Option<&str>) -> String {
    value.map_or_else(|| "unset".to_string(), |text| format!("`{text}`"))
}

/// Each variable of `VECTOR_VARIABLES` against what was set when the vectors were made.
fn environment_problems(pinned: &[(&str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    for variable in VECTOR_VARIABLES {
        let now = std::env::var(variable).ok();
        let then = pinned
            .iter()
            .find(|entry| entry.0 == variable)
            .map(|entry| entry.1);
        if now.as_deref() != then {
            problems.push(format!(
                "{variable} is {} here and was {} when the vectors were pinned: the vectors are only \
                 comparable under the environment they were made in (the judge sets it in \
                 `gate_model_env`, scripts/gate-lock.sh)",
                described(now.as_deref()),
                described(then)
            ));
        }
    }
    problems
}

fn environment_block() -> String {
    let set: Vec<String> = VECTOR_VARIABLES
        .iter()
        .filter_map(|variable| {
            std::env::var(variable)
                .ok()
                .map(|value| format!("({variable:?}, {value:?})"))
        })
        .collect();
    format!(
        "--- paste into rust/tests/model_parity/expected.rs, over `pub const EMBEDDER_ENV` ---\n\
         #[rustfmt::skip]\npub const EMBEDDER_ENV: &[(&str, &str)] = &[{}];\n--- end of EMBEDDER_ENV ---",
        set.join(", ")
    )
}

fn vectors_block(rows: &[(String, Vec<f32>)]) -> String {
    let mut block = String::from(
        "--- paste into rust/tests/model_parity/expected.rs, over `pub const VECTORS` ---\n\
         #[rustfmt::skip]\n#[allow(clippy::approx_constant)]\npub const VECTORS: &[Vector] = &[\n",
    );
    for (name, vector) in rows {
        writeln!(
            block,
            "    (\n        {name:?},\n        &[\n{}\n        ],\n    ),",
            wrapped(vector, 12)
        )
        .expect(WRITING);
    }
    block.push_str("];\n--- end of VECTORS ---");
    block
}

/// The environment first, then each vector against its pinned one by component and by direction.
fn judge_vectors(
    got: &[(String, Vec<f32>)],
    pinned_env: &[(&str, &str)],
    pinned: &[expected::Vector],
) {
    if pinned.is_empty() {
        println!("\n{}\n{}", environment_block(), vectors_block(got));
        panic!(
            "VECTORS is empty: the blocks above are what the embedder gives today under this \
             environment; paste them over the empty tables"
        );
    }
    let mut problems = environment_problems(pinned_env);
    for (name, vector) in got {
        let Some(row) = pinned.iter().find(|row| row.0 == name.as_str()) else {
            problems.push(format!("{name}: no pinned vector"));
            continue;
        };
        if row.1.len() != vector.len() {
            problems.push(format!(
                "{name}: {} components and the pinned vector has {}",
                vector.len(),
                row.1.len()
            ));
            continue;
        }
        let gap = gap_between(vector, row.1);
        println!(
            "{name}: max|Δ| = {:e} (component {}), 1 - cos = {:e}",
            gap.max_abs, gap.at, gap.one_minus_cos
        );
        if gap.max_abs > MAX_COMPONENT_GAP || gap.one_minus_cos > MAX_COSINE_GAP {
            problems.push(format!(
                "{name}: max|Δ| = {:e} at component {} (limit {MAX_COMPONENT_GAP:e}), 1 - cos = {:e} \
                 (limit {MAX_COSINE_GAP:e})",
                gap.max_abs, gap.at, gap.one_minus_cos
            ));
        }
    }
    for row in pinned {
        if !got.iter().any(|(name, _)| name == row.0) {
            problems.push(format!("{}: pinned, and no input makes it", row.0));
        }
    }
    assert!(
        problems.is_empty(),
        "the vectors of the embedder are no longer the pinned ones:\n{}",
        problems.join("\n")
    );
}

#[tokio::test]
async fn the_embedder_vectors_through_the_product_path_stay_within_the_noise_of_f32() {
    use memory_industry::embeddings::onnx;

    assert!(
        onnx::model_loaded().await,
        "the embedder did not load, so what it would give is the vector of the hash fallback and a \
         guard over that would be a false one (ONNX_MODEL_PATH = {:?}, ORT_DYLIB_PATH = {:?}). {WITHOUT_THE_MODELS}",
        std::env::var("ONNX_MODEL_PATH").ok(),
        std::env::var("ORT_DYLIB_PATH").ok()
    );
    assert_eq!(
        onnx::failure_reason(),
        None,
        "the embedder found its files and the session did not open"
    );
    assert_eq!(
        onnx::embedding_dim(),
        384,
        "CUBA_EMBEDDING_DIM is set to something other than 384, and the pinned vectors have 384 components"
    );

    let mut vectors: Vec<(String, Vec<f32>)> = Vec::new();
    for (name, text) in embedder_texts() {
        let vector = onnx::embed_passage(&text)
            .await
            .unwrap_or_else(|e| panic!("{name}: embed_passage failed: {e:#}"));
        vectors.push((name.to_string(), vector));
    }
    let query = onnx::embed(CONSULTA)
        .await
        .unwrap_or_else(|e| panic!("consulta: embed failed: {e:#}"));
    vectors.push(("consulta".to_string(), query));

    for (name, vector) in &vectors {
        assert_eq!(vector.len(), 384, "{name}: the vector is 384 wide");
        assert!(
            vector.iter().all(|component| component.is_finite()),
            "{name}: the vector has a component that is not a finite number"
        );
    }

    judge_vectors(&vectors, expected::EMBEDDER_ENV, expected::VECTORS);
}
