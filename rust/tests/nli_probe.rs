use memory_industry::cognitive::nli;

#[tokio::test]
#[ignore = "diagnóstico, no aserción: imprime distribuciones. \
            `cargo test --test nli_probe -- --ignored --nocapture`"]
async fn probe() {
    assert!(
        nli::available() && nli::enabled(),
        "NLI model required by the local merge gate — install with cuba-memorys models nli"
    );

    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".cache/cuba-memorys/models-nli");
    let tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).unwrap();
    let enc = tok
        .encode(
            ("El servidor está escrito en Rust.", "Está escrito en Java."),
            true,
        )
        .unwrap();
    eprintln!("\n=== TOKENIZACIÓN DEL PAR ===");
    eprintln!("tokens   = {:?}", enc.get_tokens());
    eprintln!("type_ids = {:?}", enc.get_type_ids());

    eprintln!("\n=== FRAGMENTOS SIN SUJETO (el peligro de descomponer) ===");
    for (p, h) in [
        (
            "Usa PostgreSQL con pgvector para la búsqueda semántica.",
            "cuba-memorys está escrito en Rust",
        ),
        (
            "cuba-memorys usa PostgreSQL con pgvector para la búsqueda semántica.",
            "cuba-memorys está escrito en Rust",
        ),
        (
            "cuba-memorys es un servidor MCP de memoria escrito en Rust.",
            "cuba-memorys está escrito en Rust",
        ),
    ] {
        match nli::entails(p, h).await {
            Ok(v) => eprintln!(
                "[{:.3} {:.3} {:.3}] {} {:<11} | {}",
                v.probs[0],
                v.probs[1],
                v.probs[2],
                if v.decisive { "->" } else { "~?" },
                v.label.as_verdict(),
                p
            ),
            Err(e) => eprintln!("ERROR: {e:#}"),
        }
    }

    eprintln!("\n=== PREMISA ENTERA, SIN DESCOMPONER (multi-frase) ===");
    const MULTI: &str = "cuba-memorys es un servidor MCP de memoria escrito en Rust. \
                         Usa PostgreSQL con pgvector para la búsqueda semántica.";
    const LARGA: &str = "**cuba-memorys v0.10.0: activar embeddings ONNX e5-small + \
                         onnxruntime estable**. Context: El binario MCP corria en v0.3 \
                         contra una BD ya en v25 -> crash 'failed to run sqlx migrations'. \
                         Chosen: git pull + cargo build --release, descargar modelo \
                         multilingual-e5-small, configurar ONNX_MODEL_PATH.";
    for (p, h, esperado) in [
        (MULTI, "cuba-memorys está escrito en Rust", "supports"),
        (MULTI, "cuba-memorys está escrito en Java", "contradicts"),
        (MULTI, "la paella es valenciana", "unrelated"),
        (
            LARGA,
            "cuba-memorys está escrito en Rust",
            "NO debe contradecir",
        ),
        (LARGA, "cuba-memorys usa el modelo e5-small", "supports"),
    ] {
        match nli::entails(p, h).await {
            Ok(v) => eprintln!(
                "[{:.3} {:.3} {:.3}] {} {:<11} (esperado: {}) | {}",
                v.probs[0],
                v.probs[1],
                v.probs[2],
                if v.decisive { "->" } else { "~?" },
                v.label.as_verdict(),
                esperado,
                h
            ),
            Err(e) => eprintln!("ERROR: {e:#}"),
        }
    }

    eprintln!("\n=== DISTRIBUCIÓN  [entail, neutral, contra] ===");
    const EV: &str = "cuba-memorys es un servidor MCP de memoria escrito en Rust, \
                      con PostgreSQL y pgvector para la búsqueda semántica.";
    let cases: [(&str, &str); 18] = [
        (
            "El servidor está escrito en Rust.",
            "El servidor está escrito en Rust.",
        ),
        (
            "El servidor está escrito en Rust.",
            "El servidor está escrito en Java.",
        ),
        (
            "El servidor está escrito en Rust.",
            "La paella es valenciana.",
        ),
        (
            "The server is written in Rust.",
            "The server is written in Java.",
        ),
        ("A man is eating an apple.", "A man is eating fruit."),
        ("A man is eating an apple.", "Nobody is eating."),
        (EV, "cuba-memorys está escrito en Rust"),
        (EV, "cuba-memorys está escrito en Java"),
        (EV, "la paella es un plato valenciano"),
        (
            "The reranker is a cross-encoder based on XLM-RoBERTa.",
            "The reranker is a bi-encoder.",
        ),
        (
            "The reranker runs as a separate ONNX session and is disabled by default.",
            "The reranker is enabled by default.",
        ),
        (
            "cuba-memorys es un servidor MCP. Está escrito en Rust. Usa PostgreSQL con \
             pgvector para la búsqueda semántica.",
            "cuba-memorys está escrito en Java",
        ),
        (
            "The reranker is a cross-encoder.",
            "The reranker is a bi-encoder.",
        ),
        ("El servidor usa PostgreSQL.", "El servidor usa MySQL."),
        (
            "El servidor corre en el puerto 5488.",
            "El servidor corre en el puerto 5491.",
        ),
        ("El índice es HNSW.", "El índice es IVFFlat."),
        (
            "cuba-memorys está escrito en Rust.",
            "cuba-memorys está escrito en Python.",
        ),
        ("The cat is on the mat.", "The dog is on the mat."),
    ];
    for (p, h) in cases {
        match nli::entails(p, h).await {
            Ok(v) => eprintln!(
                "[{:.3} {:.3} {:.3}] {} {:<11} | {} || {}",
                v.probs[0],
                v.probs[1],
                v.probs[2],
                if v.decisive { "->" } else { "~?" },
                v.label.as_verdict(),
                p,
                h
            ),
            Err(e) => eprintln!("ERROR: {e:#}"),
        }
    }
    eprintln!();
}
