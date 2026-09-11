use memory_industry::cognitive::nli::{self, Entailment};

const EVIDENCE_RUST: &str = "cuba-memorys es un servidor MCP de memoria escrito en Rust, \
                             con PostgreSQL y pgvector para la búsqueda semántica.";

fn require_nli_model() {
    assert!(
        nli::available() && nli::enabled(),
        "NLI model required by the local merge gate — missing CUBA_NLI_PATH / \
         ~/.cache/cuba-memorys/models-nli. Install with `cuba-memorys models nli` \
         (or `models all`). Soft-skip is forbidden."
    );
}

#[tokio::test]
#[ignore]
async fn true_claim_is_supported() {
    require_nli_model();
    let v = nli::entails(EVIDENCE_RUST, "cuba-memorys está escrito en Rust")
        .await
        .expect("el NLI debe emitir un veredicto, no un error");

    assert_eq!(
        v.label,
        Entailment::Supports,
        "la evidencia dice literalmente que está en Rust; p={:.3}",
        v.confidence
    );
    assert!(
        v.confidence > 0.5,
        "confianza demasiado baja: {}",
        v.confidence
    );
}

#[tokio::test]
#[ignore]
async fn false_claim_is_contradicted() {
    require_nli_model();
    let v = nli::entails(EVIDENCE_RUST, "cuba-memorys está escrito en Java")
        .await
        .expect("el NLI debe emitir un veredicto, no un error");

    assert_eq!(
        v.label,
        Entailment::Contradicts,
        "Rust y Java se excluyen; el coseno no lo veía. p={:.3}",
        v.confidence
    );
}

#[tokio::test]
#[ignore]
async fn unrelated_claim_is_neutral() {
    require_nli_model();
    let v = nli::entails(EVIDENCE_RUST, "la paella es un plato valenciano")
        .await
        .expect("el NLI debe emitir un veredicto, no un error");

    assert_eq!(
        v.label,
        Entailment::Neutral,
        "la evidencia no dice nada sobre paella; p={:.3}",
        v.confidence
    );
}

#[tokio::test]
#[ignore]
async fn english_still_works() {
    require_nli_model();
    let v = nli::entails(
        "The reranker runs as a separate ONNX session and is disabled by default.",
        "The reranker is enabled by default.",
    )
    .await
    .expect("el NLI debe emitir un veredicto, no un error");

    assert_eq!(v.label, Entailment::Contradicts, "p={:.3}", v.confidence);
}

#[tokio::test]
#[ignore]
async fn distractor_clauses_do_not_dilute_the_verdict() {
    require_nli_model();
    let v = nli::entails(EVIDENCE_RUST, "cuba-memorys está escrito en Java")
        .await
        .expect("veredicto");

    assert_eq!(
        v.label,
        Entailment::Contradicts,
        "probs={:?} — la contradicción (0.40) le gana a la entailment (0.19) más de \
         dos a uno; si esto vuelve a dar Neutral es que se leyó el argmax de una \
         cabeza de 3 clases y la masa de 'neutral' se tragó la señal",
        v.probs
    );
    assert!(
        v.decisive,
        "y debe ser un veredicto, no un empate: {:?}",
        v.probs
    );
}

#[tokio::test]
#[ignore]
async fn a_false_claim_is_never_confirmed_on_weak_entailment() {
    require_nli_model();
    let v = nli::entails(
        "The reranker is a cross-encoder based on XLM-RoBERTa.",
        "The reranker is a bi-encoder.",
    )
    .await
    .expect("veredicto");

    assert_ne!(
        (v.label, v.decisive),
        (Entailment::Supports, true),
        "SE ESTÁ CONFIRMANDO UNA AFIRMACIÓN FALSA. probs={:?}. La entailment espuria \
         (0.69) se coló por debajo de SUPPORT_FLOOR y el juez la publicó como \
         `supports` en vez de escalar.",
        v.probs
    );

    if !v.decisive {
        assert_eq!(
            v.label,
            Entailment::Neutral,
            "sin veredicto, la etiqueta no puede ser dirigida: {:?}",
            v.probs
        );
    }
}

#[tokio::test]
#[ignore]
async fn technical_substitutions_are_caught_decisively() {
    require_nli_model();
    let cases = [
        ("El servidor usa PostgreSQL.", "El servidor usa MySQL."),
        ("El índice es HNSW.", "El índice es IVFFlat."),
        (
            "El servidor corre en el puerto 5488.",
            "El servidor corre en el puerto 5491.",
        ),
        (
            "cuba-memorys está escrito en Rust.",
            "cuba-memorys está escrito en Python.",
        ),
        (
            "The reranker runs as a separate ONNX session and is disabled by default.",
            "The reranker is enabled by default.",
        ),
    ];

    for (premise, claim) in cases {
        let v = nli::entails(premise, claim).await.expect("veredicto");
        assert_eq!(
            (v.label, v.decisive),
            (Entailment::Contradicts, true),
            "«{claim}» contradice «{premise}» y debe detectarse sin titubeos; probs={:?}",
            v.probs
        );
    }
}

#[tokio::test]
#[ignore]
async fn a_verdict_costs_milliseconds_not_seconds() {
    require_nli_model();
    let _ = nli::entails(EVIDENCE_RUST, "cuba-memorys usa PostgreSQL").await;

    let t0 = std::time::Instant::now();
    for _ in 0..5 {
        nli::entails(EVIDENCE_RUST, "cuba-memorys está escrito en Rust")
            .await
            .expect("veredicto");
    }
    let per_call = t0.elapsed() / 5;

    eprintln!("NLI: {:?} por veredicto", per_call);
    assert!(
        per_call < std::time::Duration::from_millis(500),
        "un veredicto tardó {per_call:?}; el juez LLM al que reemplaza tardaba ~20 s, \
         así que <500 ms es el listón mínimo para que esto valga la pena"
    );
}
