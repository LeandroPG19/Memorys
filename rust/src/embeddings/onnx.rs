use crate::search::cache::TtlLruCache;
use anyhow::{Context, Result};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const EMBEDDING_DIM: usize = 384;

pub fn embedding_dim() -> usize {
    std::env::var("CUBA_EMBEDDING_DIM")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&d| d > 0)
        .unwrap_or(EMBEDDING_DIM)
}

fn query_prefix() -> String {
    std::env::var("CUBA_QUERY_PREFIX").unwrap_or_else(|_| "query: ".to_string())
}

fn passage_prefix() -> String {
    std::env::var("CUBA_PASSAGE_PREFIX").unwrap_or_else(|_| "passage: ".to_string())
}

fn use_cls_pooling() -> bool {
    std::env::var("CUBA_POOLING")
        .map(|v| v.eq_ignore_ascii_case("cls"))
        .unwrap_or(false)
}

const DEFAULT_MODEL: &str = "multilingual-e5-small";

pub fn current_model() -> String {
    std::env::var("CUBA_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

static CACHE: OnceLock<std::sync::Mutex<TtlLruCache<Vec<f32>>>> = OnceLock::new();

static ONNX_SEMAPHORE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

const ONNX_DEFAULT_CONCURRENCY: usize = 1;

pub fn embed_concurrency() -> usize {
    std::env::var("CUBA_EMBED_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(ONNX_DEFAULT_CONCURRENCY)
}

fn get_semaphore() -> &'static tokio::sync::Semaphore {
    ONNX_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(embed_concurrency()))
}

static MODEL_STATUS: OnceLock<ModelStatus> = OnceLock::new();

static ONNX_SESSION: OnceLock<std::sync::Mutex<Session>> = OnceLock::new();

static TOKENIZER: OnceLock<tokenizers::Tokenizer> = OnceLock::new();

enum ModelStatus {
    Loaded,
    Fallback,
}

fn get_cache() -> &'static std::sync::Mutex<TtlLruCache<Vec<f32>>> {
    CACHE.get_or_init(|| std::sync::Mutex::new(TtlLruCache::new()))
}

pub(crate) fn locate_onnxruntime() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("ORT_DYLIB_PATH") {
        let p = PathBuf::from(explicit);
        return p.exists().then_some(p);
    }

    let lib = if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };

    let cache_lib = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| {
            let cache = PathBuf::from(h).join(".cache");
            let preferred = cache.join("memory-industry").join("onnxruntime");
            let legacy = cache.join("cuba-memorys").join("onnxruntime");
            if preferred.exists() || !legacy.exists() {
                preferred
            } else {
                legacy
            }
        });

    let search: Vec<PathBuf> = cache_lib
        .into_iter()
        .chain(
            std::env::var("LD_LIBRARY_PATH")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
        )
        .chain(
            [
                "/usr/lib",
                "/usr/local/lib",
                "/usr/lib/x86_64-linux-gnu",
                "/usr/lib64",
                "/opt/homebrew/lib",
            ]
            .iter()
            .map(PathBuf::from),
        )
        .collect();

    let found = search
        .into_iter()
        .map(|d| d.join(lib))
        .find(|p| p.exists())?;

    if std::env::var("ORT_DYLIB_PATH").is_err() {
        unsafe { std::env::set_var("ORT_DYLIB_PATH", &found) };
    }
    Some(found)
}

fn cache_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
    else {
        return Vec::new();
    };
    let cache = PathBuf::from(home).join(".cache");
    let preferred = cache.join("memory-industry");
    let legacy = cache.join("cuba-memorys");
    if preferred.exists() {
        vec![preferred, legacy]
    } else if legacy.exists() {
        vec![legacy, preferred]
    } else {
        vec![preferred, legacy]
    }
}

/// Prefer explicit ONNX_MODEL_PATH; otherwise the cache installed by `models embed`.
fn resolve_model_dir() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ONNX_MODEL_PATH") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    for root in cache_roots() {
        let dir = root.join("models");
        let quantized = dir.join("model_quantized.onnx");
        if quantized.exists() {
            return Some(dir);
        }
        let plain = dir.join("model.onnx");
        if plain.exists() {
            return Some(dir);
        }
    }
    None
}

fn get_model_status() -> &'static ModelStatus {
    MODEL_STATUS.get_or_init(|| {
        let Some(model_dir) = resolve_model_dir() else {
            tracing::info!(
                "no ONNX embedder found — set ONNX_MODEL_PATH or run `memory-industry models embed`"
            );
            return ModelStatus::Fallback;
        };

        if locate_onnxruntime().is_none() {
            tracing::error!(
                "encontré el modelo en {} pero no la librería de ONNX Runtime. \
                 `ort` la carga dinámicamente: sin ella el servidor NO falla — se cuelga en el \
                 primer embedding. Degradando al fallback de hash.\n\
                 Corré `memory-industry models runtime` o definí ORT_DYLIB_PATH.",
                model_dir.display()
            );
            return ModelStatus::Fallback;
        }

        let model_file = if model_dir.is_dir() {
            let quantized = model_dir.join("model_quantized.onnx");
            if quantized.exists() {
                quantized
            } else {
                model_dir.join("model.onnx")
            }
        } else {
            model_dir.clone()
        };

        if !model_file.exists() {
            tracing::warn!(path = %model_file.display(), "ONNX model file not found — using fallback");
            return ModelStatus::Fallback;
        }

        match init_onnx_session(&model_file, &model_dir) {
            Ok(()) => {
                tracing::info!(path = %model_file.display(), "ONNX model loaded successfully");
                ModelStatus::Loaded
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load ONNX model — using fallback");
                ModelStatus::Fallback
            }
        }
    })
}

fn intra_threads() -> usize {
    if let Some(n) = std::env::var("CUBA_EMBED_INTRA_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 4))
        .unwrap_or(2)
}

fn init_onnx_session(model_file: &std::path::Path, model_dir: &std::path::Path) -> Result<()> {
    let builder = Session::builder()
        .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
        .with_intra_threads(intra_threads())
        .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
        .with_memory_pattern(false)
        .map_err(|e| anyhow::anyhow!("memory pattern: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow::anyhow!("optimization level: {e}"))?;
    let session = crate::gpu::configure(builder, crate::gpu::Workload::Embedder)?
        .commit_from_file(model_file)
        .map_err(|e| anyhow::anyhow!("load model: {e}"))?;

    ONNX_SESSION
        .set(std::sync::Mutex::new(session))
        .map_err(|_| anyhow::anyhow!("ONNX session already initialized"))?;

    let tokenizer_dir: std::path::PathBuf = if model_dir.is_dir() {
        model_dir.to_path_buf()
    } else {
        model_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let tokenizer_path = tokenizer_dir.join("tokenizer.json");

    if tokenizer_path.exists() {
        let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

        let truncation = tokenizers::TruncationParams {
            max_length: 512,
            ..Default::default()
        };
        tokenizer
            .with_truncation(Some(truncation))
            .map_err(|e| anyhow::anyhow!("failed to set truncation: {e}"))?;

        let padding = tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::BatchLongest,
            ..Default::default()
        };
        tokenizer.with_padding(Some(padding));

        TOKENIZER
            .set(tokenizer)
            .map_err(|_| anyhow::anyhow!("Tokenizer already initialized"))?;
    } else {
        return Err(anyhow::anyhow!(
            "tokenizer.json not found at {}",
            tokenizer_path.display()
        ));
    }

    Ok(())
}

pub async fn embed(text: &str) -> Result<Vec<f32>> {
    embed_with_prefix(text, &query_prefix()).await
}

pub async fn embed_passage(text: &str) -> Result<Vec<f32>> {
    embed_with_prefix(text, &passage_prefix()).await
}

pub async fn embed_passage_contextual(
    text: &str,
    entity_type: &str,
    entity_name: &str,
) -> Result<Vec<f32>> {
    let contextualized = format!("[{}:{}] {}", entity_type, entity_name, text);
    embed_with_prefix(&contextualized, &passage_prefix()).await
}

pub fn model_fingerprint() -> String {
    std::env::var("CUBA_EMBED_MODEL").unwrap_or_else(|_| "default".to_string())
}

async fn embed_with_prefix(text: &str, prefix: &str) -> Result<Vec<f32>> {
    let cache_key = format!(
        "{}|{}|{}{}",
        model_fingerprint(),
        embedding_dim(),
        prefix,
        text
    );
    if let Ok(mut cache) = get_cache().lock()
        && let Some(cached) = cache.get(&cache_key)
    {
        return Ok(cached);
    }

    let _permit = get_semaphore()
        .acquire()
        .await
        .map_err(|_| anyhow::anyhow!("ONNX semaphore closed"))?;

    let prefixed = format!("{}{}", prefix, text);
    let embedding = tokio::task::spawn_blocking(move || compute_embedding(&prefixed))
        .await
        .context("embedding task panicked")??;

    if let Ok(mut cache) = get_cache().lock() {
        cache.put(cache_key, embedding.clone());
    }

    Ok(embedding)
}

fn compute_embedding(text: &str) -> Result<Vec<f32>> {
    match get_model_status() {
        ModelStatus::Loaded => compute_onnx_embedding(text),
        ModelStatus::Fallback => compute_hash_embedding(text),
    }
}

fn compute_onnx_embedding(text: &str) -> Result<Vec<f32>> {
    let session_lock = ONNX_SESSION.get().context("ONNX session not initialized")?;
    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("session lock poisoned: {e}"))?;
    let tokenizer = TOKENIZER.get().context("Tokenizer not initialized")?;

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

    let ids = encoding.get_ids();
    let attention_mask = encoding.get_attention_mask();
    let token_type_ids = encoding.get_type_ids();

    let seq_len = ids.len();

    let input_ids: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
    let attn_mask: Vec<i64> = attention_mask.iter().map(|&m| m as i64).collect();
    let type_ids: Vec<i64> = token_type_ids.iter().map(|&t| t as i64).collect();

    let shape = vec![1i64, seq_len as i64];

    let input_ids_tensor = ort::value::Tensor::from_array((shape.clone(), input_ids))
        .context("failed to create input_ids tensor")?;

    let attn_mask_tensor = ort::value::Tensor::from_array((shape.clone(), attn_mask.clone()))
        .context("failed to create attention_mask tensor")?;

    let wants_token_type = session
        .inputs()
        .iter()
        .any(|i| i.name() == "token_type_ids");

    let outputs = if wants_token_type {
        let type_ids_tensor = ort::value::Tensor::from_array((shape, type_ids))
            .context("failed to create token_type_ids tensor")?;
        session.run(ort::inputs! {
            "input_ids" => input_ids_tensor,
            "attention_mask" => attn_mask_tensor,
            "token_type_ids" => type_ids_tensor,
        })
    } else {
        session.run(ort::inputs! {
            "input_ids" => input_ids_tensor,
            "attention_mask" => attn_mask_tensor,
        })
    }
    .map_err(|e| anyhow::anyhow!("inference failed: {e}"))?;

    if outputs.len() == 0 {
        return Err(anyhow::anyhow!("ONNX model returned no output tensors"));
    }
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("extract tensor: {e}"))?;

    let dim = embedding_dim();
    if shape.len() != 3 || shape[2] as usize != dim {
        return Err(anyhow::anyhow!(
            "unexpected output shape: {:?}, expected [1, {}, {}] — check CUBA_EMBEDDING_DIM matches the loaded model",
            shape,
            seq_len,
            dim
        ));
    }

    let mut sum_embedding = vec![0.0f32; dim];
    if use_cls_pooling() {
        sum_embedding.copy_from_slice(&data[0..dim]);
    } else {
        let mut sum_mask = 0.0f32;
        for (t, &mask_raw) in attn_mask.iter().enumerate().take(seq_len) {
            let mask_val = mask_raw as f32;
            sum_mask += mask_val;
            let offset = t * dim;
            for d in 0..dim {
                sum_embedding[d] += data[offset + d] * mask_val;
            }
        }
        let sum_mask = sum_mask.max(1e-9);
        for v in sum_embedding.iter_mut() {
            *v /= sum_mask;
        }
    }

    let norm: f32 = sum_embedding
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(1e-9);
    for v in sum_embedding.iter_mut() {
        *v /= norm;
    }

    Ok(sum_embedding)
}

fn compute_hash_embedding(text: &str) -> Result<Vec<f32>> {
    let dim = embedding_dim();
    let mut embedding = vec![0.0f32; dim];

    let text_lower = text.to_lowercase();
    let words: Vec<&str> = text_lower.split_whitespace().collect();

    for (i, word) in words.iter().enumerate() {
        let hash = word
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        let idx = (hash as usize) % dim;
        embedding[idx] += 1.0 / (1.0 + i as f32);
    }

    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in embedding.iter_mut() {
            *v /= norm;
        }
    }

    Ok(embedding)
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

pub fn is_model_loaded() -> bool {
    matches!(get_model_status(), ModelStatus::Loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_dimension() {
        let emb = compute_hash_embedding("hello world").unwrap();
        assert_eq!(emb.len(), embedding_dim());
    }

    #[test]
    fn test_embedding_normalized() {
        let emb = compute_hash_embedding("test sentence for normalization").unwrap();
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "should be L2 normalized: got {norm}"
        );
    }

    #[test]
    fn test_embedding_deterministic() {
        let a = compute_hash_embedding("same text").unwrap();
        let b = compute_hash_embedding("same text").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = compute_hash_embedding("hello world").unwrap();
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity_different() {
        let a = compute_hash_embedding("rust programming language").unwrap();
        let b = compute_hash_embedding("cooking recipes mediterranean").unwrap();
        let sim = cosine_similarity(&a, &b);
        assert!(sim < 0.8);
    }

    #[test]
    fn test_fallback_mode() {
        let emb = compute_hash_embedding("test").unwrap();
        assert_eq!(emb.len(), embedding_dim());
    }

    #[test]
    fn embed_concurrency_defaults_to_one_and_is_configurable() {
        unsafe { std::env::remove_var("CUBA_EMBED_CONCURRENCY") };
        assert_eq!(
            embed_concurrency(),
            1,
            "the ONNX session sits behind a single Mutex during inference — \
             advertising more concurrency than that just queues a second \
             blocking thread behind the first"
        );

        unsafe { std::env::set_var("CUBA_EMBED_CONCURRENCY", "3") };
        assert_eq!(embed_concurrency(), 3);

        unsafe { std::env::set_var("CUBA_EMBED_CONCURRENCY", "0") };
        assert_eq!(embed_concurrency(), 1, "0 must fall through to the default");

        unsafe { std::env::remove_var("CUBA_EMBED_CONCURRENCY") };
    }
}
