use crate::search::cache::TtlLruCache;
use anyhow::{Context, Result};
use ort::environment::Environment;
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

/// What a stored vector says made it: the model and the ONNX Runtime that computed it
/// (`multilingual-e5-small@onnxruntime-1.31.1`). Two runtimes do not compute the same numbers
/// (1.26.0 against 1.31.1: up to 1.4e-3 in `1 - cos`, measured), and a vector is only
/// interchangeable with another of the same label. Without a runtime loaded the label is the model
/// alone, which no loaded process writes: a vector is only written after one embedded.
pub fn vector_tag_of(model: &str, runtime: Option<&str>) -> String {
    match runtime {
        Some(runtime) => format!("{model}@onnxruntime-{runtime}"),
        None => model.to_string(),
    }
}

/// The label of a vector this process writes now.
pub fn vector_tag() -> String {
    vector_tag_of(&current_model(), loaded_runtime_version().as_deref())
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

/// What happened the one time this process tried to open the embedder.
///
/// `Fallback` and `Failed` are not the same machine and must not read as the
/// same machine. Three of the four ways the load stops short are "there was
/// nothing to load" — no model directory, no ONNX Runtime library, no model
/// file where one was named — and on those the hash fallback is the supported
/// answer, `Tier::Minimal`. Only a model file that was there, with a runtime
/// that was there, and a session that still would not open, is a fault worth
/// sending somebody to look at.
enum ModelStatus {
    Loaded,
    Fallback,
    /// The session did not open on a machine that had everything it needed.
    /// The reason is kept rather than logged and forgotten: the log line is on
    /// the machine, and whoever is reading `/health` is not.
    Failed(String),
}

fn get_cache() -> &'static std::sync::Mutex<TtlLruCache<Vec<f32>>> {
    CACHE.get_or_init(|| std::sync::Mutex::new(TtlLruCache::new()))
}

/// The runtime path the operator set in `ORT_DYLIB_PATH`, or `None` when the
/// variable is unset, empty or only spaces.
///
/// Empty is not a path: the env file `memory-industry setup service` renders
/// carries `ORT_DYLIB_PATH=` as a placeholder and systemd's `EnvironmentFile`
/// exports it empty, so the daemon saw an operator naming a file that is not
/// there and never searched. Read the way `ONNX_MODEL_PATH` is in
/// `resolve_model_dir`: trimmed, and blank means unset.
pub(crate) fn explicit_runtime_path() -> Option<PathBuf> {
    let value = std::env::var("ORT_DYLIB_PATH").ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

pub(crate) fn locate_onnxruntime() -> Option<PathBuf> {
    if let Some(p) = explicit_runtime_path() {
        return p.exists().then_some(p);
    }

    let lib = if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else {
        "libonnxruntime.so"
    };

    // Not a result: the first candidate of the chain below. With no home the
    // candidate drops out and the search goes on to LD_LIBRARY_PATH and the
    // system directories, which is how a machine with the runtime in /usr/lib
    // and no HOME still loads it.
    let search: Vec<PathBuf> = runtime_cache_dir()
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

    // Not written back into ORT_DYLIB_PATH: this runs on any tokio worker and
    // a set_var while another thread reads the environment is undefined
    // behaviour. `gpu::configure` hands the path to `ort::init_from` instead.
    Some(found)
}

/// Where `models runtime` puts ONNX Runtime under the home: the documented
/// name, or the pre-rename one when only that one is on disk.
///
/// `None` without a home, said quietly: callers read it as «nothing
/// downloaded», which is the supported CPU machine and not a fault.
///
/// `gpu::runtime_dir` used to keep its own copy of this rule, and the copy
/// was also where it stopped looking: it never saw a runtime this search
/// found anywhere else.
pub(crate) fn runtime_cache_dir() -> Option<PathBuf> {
    let cache = crate::envs::home().ok()?.join(".cache");
    let preferred = cache.join("memory-industry").join("onnxruntime");
    // A directory that exists on operators' disks, not the binary's name: a
    // rename sweep that greps for the old name has to leave this one alone or
    // every runtime already downloaded is orphaned.
    let legacy = cache.join("cuba-memorys").join("onnxruntime");
    Some(if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    })
}

fn cache_roots() -> Vec<PathBuf> {
    // An empty list and not an error: every caller reads it as «no cache to
    // look in» and falls through to the hash embedder, which is Tier::Minimal
    // and a supported machine.
    let Ok(home) = crate::envs::home() else {
        return Vec::new();
    };
    let cache = home.join(".cache");
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

/// What the machine has for the embedder, read off the environment and the
/// disk without opening anything.
///
/// The four stops are the ones the load used to find one after the other inside
/// its `get_or_init`. Keeping them here is what lets the guard of
/// `assert_embedding_dim` ask «is there a model to load» at start-up without
/// loading it, and lets the load itself never disagree with the guard.
enum Located {
    /// No model directory named, and nothing under the home cache.
    Absent,
    NoRuntime {
        model_dir: PathBuf,
    },
    NoFile {
        model_file: PathBuf,
    },
    Ready {
        model_file: PathBuf,
        model_dir: PathBuf,
    },
}

/// The one rule for «a model is configured». It logs nothing, opens neither the
/// runtime nor the session, and keeps no answer: every call reads the
/// environment and the disk again.
fn locate_model() -> Located {
    let Some(model_dir) = resolve_model_dir() else {
        return Located::Absent;
    };

    if locate_onnxruntime().is_none() {
        return Located::NoRuntime { model_dir };
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
        return Located::NoFile { model_file };
    }

    Located::Ready {
        model_file,
        model_dir,
    }
}

/// True when a model, the runtime and the model file are all there: the load
/// would open a session. It does not open it, so it is the question for start-up
/// code that must not pay the load.
pub fn model_configured() -> bool {
    matches!(locate_model(), Located::Ready { .. })
}

fn load_model_status() -> ModelStatus {
    let (model_file, model_dir) = match locate_model() {
        Located::Absent => {
            tracing::info!(
                "no ONNX embedder found — set ONNX_MODEL_PATH or run `memory-industry models embed`"
            );
            return ModelStatus::Fallback;
        }
        Located::NoRuntime { model_dir } => {
            tracing::error!(
                "encontré el modelo en {} pero no la librería de ONNX Runtime. \
                 `ort` la carga dinámicamente: sin ella el servidor NO falla — se cuelga en el \
                 primer embedding. Degradando al fallback de hash.\n\
                 Corré `memory-industry models runtime` o definí ORT_DYLIB_PATH.",
                model_dir.display()
            );
            return ModelStatus::Fallback;
        }
        Located::NoFile { model_file } => {
            tracing::warn!(path = %model_file.display(), "ONNX model file not found — using fallback");
            return ModelStatus::Fallback;
        }
        Located::Ready {
            model_file,
            model_dir,
        } => (model_file, model_dir),
    };

    match init_onnx_session(&model_file, &model_dir) {
        Ok(()) => {
            tracing::info!(path = %model_file.display(), "ONNX model loaded successfully");
            ModelStatus::Loaded
        }
        Err(e) => {
            // The only one of the four branches above that is a fault:
            // everything this needed was on the machine and the session
            // still did not open. `{e:#}` rather than `{e}` because the
            // useful half is usually in the context `init_onnx_session`
            // attached — which tokenizer, which builder step — and not in
            // the outermost line.
            let reason = format!("{e:#}");
            tracing::warn!(error = %reason, "Failed to load ONNX model — using fallback");
            ModelStatus::Failed(reason)
        }
    }
}

fn get_model_status() -> &'static ModelStatus {
    MODEL_STATUS.get_or_init(load_model_status)
}

/// Resolves `cell` without holding an executor thread for the load.
///
/// The load runs inside `spawn_blocking` on the cell itself, so it is not
/// cancelled when the caller is: a handler that times out, or a client that
/// leaves, drops the wait and the load still finishes and leaves the cell
/// resolved for the next one. A cancellable async init would start the load
/// again, and the second attempt finds the session already set and leaves the
/// model `Failed` for good. `OnceLock` also makes a caller that arrives during
/// the load wait for it instead of starting its own.
///
/// `None` is a load that panicked. The cell stays unresolved, so the next call
/// tries again.
async fn resolve_off_executor<F>(
    cell: &'static OnceLock<ModelStatus>,
    load: F,
) -> Option<&'static ModelStatus>
where
    F: FnOnce() -> ModelStatus + Send + 'static,
{
    if let Some(status) = cell.get() {
        return Some(status);
    }
    match tokio::task::spawn_blocking(move || cell.get_or_init(load)).await {
        Ok(status) => Some(status),
        Err(e) => {
            tracing::error!(error = %e, "the embedder load panicked — treating it as no model");
            None
        }
    }
}

/// Why the embedder is not loaded, when something already tried to load it.
///
/// `None` means it loaded, or there was nothing to load, or nobody has asked
/// yet (a process that started and has not embedded anything is in that last
/// case: the model opens on first use). Reading the resolved cell rather than forcing it is what makes this
/// callable from `/health`: `is_model_loaded()` resolves the cell that loads
/// the model, and on a cold process that is a multi-second call.
///
/// The sentence names the model file and the tokenizer path, so a caller that
/// answers over a socket puts it through `http::reason_without_a_model_path`
/// first.
pub fn failure_reason() -> Option<String> {
    reason_of(MODEL_STATUS.get()?)
}

/// Only a session that had everything and would not open has a reason worth
/// showing. A machine with no embedding model is a supported tier, and
/// reporting it as broken is how `/health` teaches an operator to stop reading
/// the field.
fn reason_of(status: &ModelStatus) -> Option<String> {
    match status {
        ModelStatus::Loaded | ModelStatus::Fallback => None,
        ModelStatus::Failed(reason) => Some(reason.clone()),
    }
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
    // A factory, not a builder: if the GPU provider refuses to start we
    // need a second, clean builder for the CPU path.
    let make_builder = |environment: &Environment| {
        Session::builder(environment)
            .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
            .with_intra_threads(intra_threads())
            .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
            .with_memory_pattern(false)
            .map_err(|e| anyhow::anyhow!("memory pattern: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {e}"))
    };
    let session = crate::gpu::open(
        make_builder,
        crate::gpu::Workload::Embedder,
        |mut builder| {
            builder
                .commit_from_file(model_file)
                .map_err(|e| anyhow::anyhow!("load model: {e}"))
        },
    )?;

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
    compute_embedding_with(get_model_status(), text)
}

/// Which routine produces the vector, with the status as an argument rather
/// than as the process-wide cell.
///
/// Split off for the same reason `reranker_state_from` was: `get_model_status`
/// resolves a `OnceLock` that loads the model, so a test calling the routing
/// through it would be asserting against whatever that cell happened to
/// settle on first.
///
/// The arms are written out and there is no `_ =>` on purpose. A wildcard is
/// exactly what would have swallowed `Failed` the day it was added, and it
/// would swallow the next variant just as quietly; the compiler refusing to
/// build is the only thing that makes this decision deliberate.
fn compute_embedding_with(status: &ModelStatus, text: &str) -> Result<Vec<f32>> {
    match status {
        ModelStatus::Loaded => compute_onnx_embedding(text),
        ModelStatus::Fallback => compute_hash_embedding(text),
        // A session that did not open still has to produce a vector, and it
        // produces the same one `Fallback` does. The reason it failed travels
        // to `/health`; it never reaches the vector. Reporting a broken model
        // was the change; refusing to embed on it would be a search outage on
        // the machine that most needs search to keep working.
        ModelStatus::Failed(_) => compute_hash_embedding(text),
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

/// Blocks the calling thread while the model loads, when nobody has loaded it
/// yet. Code that runs on the executor asks `model_loaded()` instead.
pub fn is_model_loaded() -> bool {
    matches!(get_model_status(), ModelStatus::Loaded)
}

/// `is_model_loaded()` for async callers: the first one to ask opens the model
/// on a blocking thread and the rest wait for that one load.
pub async fn model_loaded() -> bool {
    matches!(
        resolve_off_executor(&MODEL_STATUS, load_model_status).await,
        Some(ModelStatus::Loaded)
    )
}

/// What the first attempt to load ONNX Runtime answered, and the library and
/// the `Environment` it opened, for the life of the process.
///
/// One attempt and not one per caller: the embedder, the reranker and NLI all
/// reach `load_onnxruntime` from their own threads, and `get_or_init` holds
/// the second and third outside `ort` until the first has an answer. After a
/// failure nobody tries again — `ort` could not take a second library anyway,
/// and the answer to repeat is the one that says why.
///
/// The library is kept here, loaded, so the module `ort::init_from` asks the
/// loader for a moment later is the one this crate already opened and checked.
///
/// The `Environment` is here too, and it is the only one the process will
/// have: since rc.14 every session is built from one, and `build` refuses a
/// second for good (`only one environment is allowed per process`), even
/// after the first is dropped. It lives beside the runtime because this is
/// the one place that already runs once per process, and `gpu::open` hands it
/// to each factory, so a session cannot be built before the runtime is
/// loaded: the type does not let it. It is never dropped, so `ort` does not
/// release it at exit, as the sessions in the statics beside it are not
/// either; CI-E31 and the E2E watch that the process still leaves with 0.
static RUNTIME: OnceLock<std::result::Result<(libloading::Library, Environment, String), String>> =
    OnceLock::new();

/// Loads ONNX Runtime from `path` before any session opens.
///
/// The empty path is refused here because nothing downstream refuses it:
/// `ort` reads an empty path as none and loads the bare `onnxruntime.dll`,
/// which on Windows is the Windows ML build in System32, and `ort::init_from`
/// answers `Ok` for any path once some library is loaded.
///
/// Answers the process's one `Environment`, which every session opens through.
pub(crate) fn load_onnxruntime(path: &std::path::Path) -> Result<&'static Environment> {
    anyhow::ensure!(
        !path.as_os_str().is_empty(),
        "the ONNX Runtime library path is empty: point ORT_DYLIB_PATH at the library file or \
         unset it and run `memory-industry models runtime`"
    );
    match RUNTIME.get_or_init(|| load_onnxruntime_once(path).map_err(|e| format!("{e:#}"))) {
        Ok((_library, environment, _version)) => Ok(environment),
        Err(reason) => Err(anyhow::anyhow!("{reason}")),
    }
}

/// The version string the runtime this process loaded gave (`1.31.1`), or `None` while
/// nothing has loaded one or the load failed. What `doctor` says about the runtime and
/// what `tests/model_parity.rs` compares with the version `models runtime` installs.
pub fn loaded_runtime_version() -> Option<String> {
    let (_library, _environment, version) = RUNTIME.get()?.as_ref().ok()?;
    Some(version.clone())
}

/// Turns the telemetry of ONNX Runtime off for a process that has not loaded it yet,
/// unless the operator decided: a value already set stays, whatever it is.
///
/// From 1.31 the official builds send telemetry to Microsoft over HTTPS by default, with
/// a persistent device identifier. `Environment::with_telemetry(false)` (`load_onnxruntime_once`)
/// stops what it can, but ONNX Runtime may have emitted its first event by then: only
/// this variable, set before the library starts, prevents the uploader and the
/// identifier from being created. `main` calls this next to `resources::apply`, before
/// the tokio runtime exists, because `set_var` is only sound while one thread reads
/// the environment.
pub fn keep_onnx_runtime_telemetry_off() {
    if std::env::var_os("ORT_DISABLE_TELEMETRY").is_none() {
        // SAFETY: called from `main` before the runtime and any thread of ours; the
        // test that exercises it holds `GLOBAL_STATE_GUARD`.
        unsafe { std::env::set_var("ORT_DISABLE_TELEMETRY", "1") };
    }
}

/// Opens and checks the library first, and only hands `ort` a library that
/// passed; then builds the process's one `Environment` from it.
///
/// `ort` 2.0.0-rc.12 could not report a library that did not load: it hung
/// the calling thread forever. `load_dylib_from_path` ran inside the `Once`
/// of `G_ORT_LIB`; when the load failed it built an `ort::Error`, and
/// `Error::new` called `ort::api()`, which called `setup_api`, which called
/// `load_dylib_from_path` again and waited on the same `Once` its own thread
/// held. Measured with cdb on the hung daemon: the embedder or reranker
/// thread in that wait, NLI queued behind it, warm-up never ending. Every
/// early return of that loader went the same way — the file is not a library,
/// it has no `OrtGetApiBase`, or its version is older than `ort` accepts.
/// rc.13 fixed it (pykeio/ort 17ed727): `init_from` answers a
/// `LoadDynamicError` and never calls `ort::api()` on that path. The check
/// stays, for what `ort` does not give: an error that names the file and the
/// way out (`models runtime`, `ORT_DYLIB_PATH`), which is what `/health`
/// redacts and shows, and no library kept by `ort` that this crate did not
/// open first.
///
/// The path is made absolute first so both loads resolve the same file: `ort`
/// reads a relative path against the executable's directory, the search that
/// found it read it against the working directory.
///
/// The environment is built here with telemetry off. ONNX Runtime builds from
/// Microsoft send telemetry events unless told not to (`with_telemetry` in
/// `ort`); this is the one place where that is said, and it is inert on the
/// builds that send none. A failure to build it is an error of the load, like
/// any other: no session opens, and the answer is kept like the rest. Its
/// text names the environment and not the path, so `/health` has no second
/// place to redact.
fn load_onnxruntime_once(
    path: &std::path::Path,
) -> Result<(libloading::Library, Environment, String)> {
    let path = std::path::absolute(path)
        .with_context(|| format!("resolving the ONNX Runtime path {}", path.display()))?;
    let (library, version) = open_runtime_library(&path)?;
    let environment = ort::init_from(&path)
        .map_err(|e| anyhow::anyhow!("loading ONNX Runtime from {}: {e}", path.display()))?
        .with_telemetry(false)
        .build()
        .map_err(|e| anyhow::anyhow!("creating the ONNX Runtime environment: {e}"))?;
    Ok((library, environment, version))
}

/// `OrtGetApiBase` as `ort-sys` declares it.
type GetApiBase = unsafe extern "system" fn() -> *const ort::sys::OrtApiBase;

/// The version of the ONNX Runtime library at `path`, read the way the daemon reads it
/// and without calling into `ort`. The library is closed again before this returns,
/// which on Windows is what lets `models runtime` rename the directory it was read from.
pub(crate) fn runtime_version(path: &std::path::Path) -> Result<String> {
    open_runtime_library(path).map(|(_library, version)| version)
}

/// The library at `path`, opened and checked the way `ort`'s loader is about
/// to check it, without calling into `ort`, and the version it reports.
fn open_runtime_library(path: &std::path::Path) -> Result<(libloading::Library, String)> {
    // SAFETY: the same `Library::new` on the same path that `ort` makes a
    // moment later. Loading runs the library's own initialisation and nothing
    // else of it; a file that is not a library is refused by the platform
    // loader before any of its bytes run.
    let library = unsafe { libloading::Library::new(path) }
        .map_err(|e| anyhow::anyhow!("loading ONNX Runtime from {}: {e}", path.display()))?;
    let version = {
        // SAFETY: the signature `ort-sys` declares for `OrtGetApiBase`, the
        // symbol `ort` resolves and calls next. It takes nothing and returns
        // a pointer to a static table inside the library, which `library`
        // keeps loaded while it is read.
        let get_api_base =
            unsafe { library.get::<GetApiBase>(b"OrtGetApiBase") }.map_err(|_| {
                anyhow::anyhow!(
                    "{} loads but is not ONNX Runtime: it has no `OrtGetApiBase`",
                    path.display()
                )
            })?;
        // SAFETY: as above.
        let base = unsafe { get_api_base() };
        anyhow::ensure!(
            !base.is_null(),
            "{} is not a usable ONNX Runtime: `OrtGetApiBase` returned nothing",
            path.display()
        );
        // SAFETY: `base` is not null and points into the library `library`
        // holds; `GetVersionString` returns a NUL-terminated string the
        // library owns and never frees.
        unsafe { std::ffi::CStr::from_ptr(((*base).GetVersionString)()) }
            .to_string_lossy()
            .into_owned()
    };
    anyhow::ensure!(
        minor_version(&version) >= ort::MINOR_VERSION,
        "the ONNX Runtime at {} is version {version}, and this build needs 1.{} or newer: run \
         `memory-industry models runtime` or point ORT_DYLIB_PATH at a newer library",
        path.display(),
        ort::MINOR_VERSION
    );
    Ok((library, version))
}

/// The minor number of an ONNX Runtime version, read the way `ort` reads it:
/// `1.22.0` is 22, and a string without one is 0, older than anything `ort`
/// accepts.
fn minor_version(version: &str) -> u32 {
    version
        .split('.')
        .nth(1)
        .and_then(|minor| minor.parse().ok())
        .unwrap_or(0)
}

/// The runtime `locate_onnxruntime` found, loaded, and the `Environment` the
/// process opens its sessions through. `ort` keeps the first library it loads
/// for the life of the process, so the calls after the first cost a search and
/// nothing else.
pub(crate) fn load_located_onnxruntime() -> Result<&'static Environment> {
    let runtime = locate_onnxruntime().context(
        "no ONNX Runtime library found: run `memory-industry models runtime` or point \
         ORT_DYLIB_PATH at the library",
    )?;
    load_onnxruntime(&runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envs::{ScopedEnv, scratch_root};

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
    fn a_machine_with_no_model_is_not_a_machine_that_is_broken() {
        assert_eq!(reason_of(&ModelStatus::Loaded), None);
        assert_eq!(
            reason_of(&ModelStatus::Fallback),
            None,
            "three of the four ways the load stops short are «there was nothing to load» — no \
             model directory, no ONNX Runtime, no model file. That machine is Tier::Minimal and \
             this daemon is supported on it; a reason here would put every one of them into \
             `/health` as degraded forever, which is how a field stops being read"
        );

        let tokenizer = "load model: BFCArena::AllocateRawInternal: tokenizer.json not found";
        assert_eq!(
            reason_of(&ModelStatus::Failed(tokenizer.into())),
            Some(tokenizer.to_string()),
            "and the fourth way — everything was there and the session still would not open — \
             is the one an operator has to be sent to look at, word for word, because `doctor` \
             used to guess at this and guessed wrong"
        );
        assert_ne!(
            reason_of(&ModelStatus::Failed(tokenizer.into())),
            Some(String::new()),
            "an empty reason is worse than none: the reader gets a failure with nothing after \
             the colon"
        );
    }

    #[test]
    fn a_model_that_failed_to_open_still_returns_a_vector() {
        const TEXT: &str = "the retrieval pipeline fuses lexical and vector signals";
        let hash = compute_hash_embedding(TEXT).expect("the hash fallback always answers");

        // Positive control. Without it every assertion below would also pass
        // on a fallback that had quietly started returning nothing.
        assert_eq!(
            hash.len(),
            embedding_dim(),
            "the fixture is broken: the hash fallback is not producing a vector at all"
        );

        let broken = compute_embedding_with(
            &ModelStatus::Failed("load model: BFCArena::AllocateRawInternal".to_string()),
            TEXT,
        )
        .expect(
            "a model that would not open must still embed. This whole state was a change to \
             what `/health` SAYS, not to what the embedder DOES — routing this arm to an error \
             would take search down on exactly the machine whose model is already broken",
        );

        assert_eq!(
            broken, hash,
            "and it has to be the same vector the fallback produces, not a different weaker \
             one: two embedding spaces in one index is a ranking bug nobody would trace back \
             to a health field"
        );
        assert_eq!(
            compute_embedding_with(&ModelStatus::Fallback, TEXT)
                .expect("the fallback arm is the one that never changed"),
            hash,
            "the arm this one was cloned from, asserted beside it: if the fallback ever stops \
             going through compute_hash_embedding, the row above stops meaning what it says"
        );
    }

    #[test]
    fn asking_why_the_embedder_failed_does_not_load_the_embedder() {
        // `/health` calls this on every poll and may not force a load:
        // `is_model_loaded()` resolves the cell that reads the model off disk,
        // which on a cold process is seconds inside a monitor's request.
        let resolved_before = MODEL_STATUS.get().is_some();
        let _ = failure_reason();
        assert_eq!(
            MODEL_STATUS.get().is_some(),
            resolved_before,
            "reading why the embedder failed resolved the cell that loads it. A `get_or_init` \
             here turns a health poll into a model load, which is the defect this accessor \
             exists to avoid"
        );
    }

    /// «Is there a model to load» has one answer, and the guard of
    /// `assert_embedding_dim` and the load itself read it from the same place.
    ///
    /// The four rows are the four stops of the load that need no session:
    /// no model directory, a model with no runtime, a runtime with no model
    /// file, and both. Asking is not loading: the cell that holds the session
    /// must be exactly as resolved after the question as before it, or the
    /// guard that runs at start-up opens the model again.
    #[tokio::test]
    async fn the_guard_and_the_loader_read_one_rule_for_configured() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let resolved_before = MODEL_STATUS.get().is_some();

        let root = scratch_root("e16_locate");
        let empty_home = root.join("home");
        let with_model = root.join("with-model");
        let without_model = root.join("without-model");
        for dir in [&empty_home, &with_model, &without_model] {
            std::fs::create_dir_all(dir).expect("the test owns this directory");
        }
        std::fs::write(with_model.join("model.onnx"), b"not a model")
            .expect("temp dir is writable");
        let runtime = root.join(runtime_library_filename());
        std::fs::write(&runtime, b"not a runtime").expect("temp dir is writable");
        let missing_runtime = root.join("no-such-runtime");

        // HOME moves to an empty directory so that the cache of the machine
        // running the test cannot be the model the first row looks for.
        let _home = ScopedEnv::set("HOME", &empty_home.display().to_string());

        {
            let _model = ScopedEnv::cleared("ONNX_MODEL_PATH");
            let _runtime = ScopedEnv::cleared("ORT_DYLIB_PATH");
            assert!(
                matches!(locate_model(), Located::Absent),
                "no ONNX_MODEL_PATH and nothing under the home cache is no model at all"
            );
            assert!(
                !model_configured(),
                "nothing to load, and the guard would read it as a model"
            );
        }

        {
            let _model = ScopedEnv::set("ONNX_MODEL_PATH", &with_model.display().to_string());
            let _runtime = ScopedEnv::set("ORT_DYLIB_PATH", &missing_runtime.display().to_string());
            let Located::NoRuntime { model_dir } = locate_model() else {
                panic!(
                    "a model directory and a runtime path that is not a file must stop at \
                     «no runtime»: the load logs it and falls back, so the guard has to agree"
                );
            };
            assert_eq!(model_dir, with_model);
            assert!(
                !model_configured(),
                "without the runtime the load never opens a session, so there is no model to guard"
            );
        }

        {
            let _model = ScopedEnv::set("ONNX_MODEL_PATH", &without_model.display().to_string());
            let _runtime = ScopedEnv::set("ORT_DYLIB_PATH", &runtime.display().to_string());
            let Located::NoFile { model_file } = locate_model() else {
                panic!(
                    "a runtime and a model directory with neither model.onnx nor \
                     model_quantized.onnx must stop at «no file»"
                );
            };
            assert_eq!(model_file, without_model.join("model.onnx"));
            assert!(
                !model_configured(),
                "a directory with no model file is not a model"
            );
        }

        {
            let _model = ScopedEnv::set("ONNX_MODEL_PATH", &with_model.display().to_string());
            let _runtime = ScopedEnv::set("ORT_DYLIB_PATH", &runtime.display().to_string());
            let Located::Ready {
                model_file,
                model_dir,
            } = locate_model()
            else {
                panic!("the runtime and model.onnx are both there: this is the configured machine");
            };
            assert_eq!(model_file, with_model.join("model.onnx"));
            assert_eq!(model_dir, with_model);
            assert!(model_configured());

            std::fs::write(
                with_model.join("model_quantized.onnx"),
                b"not a model either",
            )
            .expect("temp dir is writable");
            let Located::Ready { model_file, .. } = locate_model() else {
                panic!("adding the quantized file cannot make the machine less configured");
            };
            assert_eq!(
                model_file,
                with_model.join("model_quantized.onnx"),
                "the quantized model is the one the load prefers when both are there"
            );
        }

        assert_eq!(
            MODEL_STATUS.get().is_some(),
            resolved_before,
            "asking whether a model is configured resolved the cell that opens the session. \
             That is the guard turning into a load at start-up again"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// One poll of a future, without waiting for it: `Pending` is the answer
    /// that says «it did not finish inside this call».
    async fn poll_once<F: std::future::Future>(
        waiting: &mut std::pin::Pin<Box<F>>,
    ) -> std::task::Poll<F::Output> {
        std::future::poll_fn(|cx| std::task::Poll::Ready(waiting.as_mut().poll(cx))).await
    }

    /// A loader that counts itself, says when it is inside, and holds the load
    /// until the test lets it go.
    ///
    /// The wait is `recv_timeout`, not `recv`: a load that nobody releases
    /// answers `Failed("never released")` after 10 s instead of hanging the
    /// suite, and the assertion that reads the answer says what went wrong.
    fn held_loader(
        loads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> (
        impl FnOnce() -> ModelStatus + Send + 'static,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let loader = move || {
            loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = entered_tx.send(());
            match release_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(()) => ModelStatus::Loaded,
                Err(_) => ModelStatus::Failed("never released".to_string()),
            }
        };
        (loader, entered_rx, release_tx)
    }

    /// The 15 s around each of the four below is the guard against a hang, not
    /// the verdict: every one of them ends on a count or on `Pending`.
    const NO_HANG: std::time::Duration = std::time::Duration::from_secs(15);

    #[tokio::test]
    async fn two_waits_for_an_unloaded_model_share_one_load() {
        static CELL: OnceLock<ModelStatus> = OnceLock::new();
        tokio::time::timeout(NO_HANG, async {
            let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (first_loader, entered, release) = held_loader(loads.clone());
            let first = tokio::spawn(resolve_off_executor(&CELL, first_loader));
            entered.await.expect("the first loader never started");

            let second_loads = loads.clone();
            let mut second = Box::pin(resolve_off_executor(&CELL, move || {
                second_loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ModelStatus::Failed("second load".to_string())
            }));
            let polled = poll_once(&mut second).await;
            assert!(
                matches!(polled, std::task::Poll::Pending),
                "B answered while the load was in progress. A request that arrives while the \
                 model is loading has to wait for it: answering with the fallback is a hash \
                 vector written under a model that is about to be there"
            );

            release.send(()).expect("the first loader is still waiting");
            let from_first = first.await.expect("the wait of the first caller joined");
            let from_second = second.await;
            assert!(
                matches!(from_first, Some(ModelStatus::Loaded)),
                "the caller that started the load must get the model it loaded"
            );
            assert!(
                matches!(from_second, Some(ModelStatus::Loaded)),
                "the caller that arrived during the load must get the same answer, not its own"
            );
            assert_eq!(
                loads.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "two waits on an unresolved cell opened the model twice"
            );
        })
        .await
        .expect("two waits for one load did not finish in 15 s");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_load_never_runs_on_the_executor_thread() {
        static CELL: OnceLock<ModelStatus> = OnceLock::new();
        tokio::time::timeout(NO_HANG, async {
            let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (loader, _entered, release) = held_loader(loads);
            let mut waiting = Box::pin(resolve_off_executor(&CELL, loader));

            let polled = poll_once(&mut waiting).await;
            assert!(
                matches!(polled, std::task::Poll::Pending),
                "the load ran on the executor thread: the loader waits for a signal that only \
                 this thread sends, after the first poll, so a poll that came back Ready is a \
                 poll that sat inside the loader until it gave up. With 2 to 4 executor threads \
                 that is how a few first requests stop the runtime, the writer of stdout included"
            );

            release.send(()).expect("the loader is still waiting");
            let status = waiting.await;
            assert!(
                matches!(status, Some(ModelStatus::Loaded)),
                "once released the load has to finish and be the answer"
            );
        })
        .await
        .expect("the wait for the load did not finish in 15 s");
    }

    #[tokio::test]
    async fn a_wait_that_is_dropped_does_not_load_again() {
        static CELL: OnceLock<ModelStatus> = OnceLock::new();
        tokio::time::timeout(NO_HANG, async {
            let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (first_loader, entered, release) = held_loader(loads.clone());
            let first = tokio::spawn(resolve_off_executor(&CELL, first_loader));
            entered.await.expect("the first loader never started");

            first.abort();
            let _ = first.await;

            release
                .send(())
                .expect("the load must outlive the wait that was dropped");
            let second_loads = loads.clone();
            let status = resolve_off_executor(&CELL, move || {
                second_loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ModelStatus::Failed("second load".to_string())
            })
            .await;

            assert!(
                matches!(status, Some(ModelStatus::Loaded)),
                "the next caller has to get what the dropped wait started: a handler that times \
                 out must not leave the model to be opened a second time"
            );
            assert_eq!(
                loads.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "the load was repeated after the wait that started it went away. The second \
                 attempt finds the session already set and the model stays Failed for good"
            );
        })
        .await
        .expect("the wait after a dropped one did not finish in 15 s");
    }

    #[tokio::test]
    async fn a_load_that_panics_answers_no_model() {
        static CELL: OnceLock<ModelStatus> = OnceLock::new();
        tokio::time::timeout(NO_HANG, async {
            let broken = resolve_off_executor(&CELL, || -> ModelStatus { panic!("boom") }).await;
            assert!(
                broken.is_none(),
                "a load that panicked answers «no model» to whoever was waiting, without taking \
                 the waiter down with it and without calling the model loaded"
            );

            let again = resolve_off_executor(&CELL, || ModelStatus::Loaded).await;
            assert!(
                matches!(again, Some(ModelStatus::Loaded)),
                "the panic left the cell poisoned or resolved: a later call has to try the \
                 load again, as the OnceLock does after an initializer that did not finish"
            );
        })
        .await
        .expect("the panicking load did not finish in 15 s");
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

    /// CI-D5c (B.1). A stored vector says which model and which ONNX Runtime made it, because
    /// 1.26.0 and 1.31.1 do not compute the same numbers (measured on 11 Nov: up to 1.4e-3 in
    /// `1 - cos`) and two vectors under one label have to be interchangeable. The three strings
    /// are written by hand, not built the way the function under test builds them.
    #[test]
    fn the_vector_tag_names_the_model_and_the_runtime_that_made_it() {
        assert_eq!(
            vector_tag_of("multilingual-e5-small", Some("1.31.1")),
            "multilingual-e5-small@onnxruntime-1.31.1"
        );
        assert_eq!(
            vector_tag_of("e5-small", Some("1.26.0")),
            "e5-small@onnxruntime-1.26.0",
            "the runtime is part of the label whatever the model is called"
        );
        assert_eq!(
            vector_tag_of("multilingual-e5-small", None),
            "multilingual-e5-small",
            "with no runtime loaded there is nothing to name, and no vector is written either"
        );
    }

    /// The file name `locate_onnxruntime` searches for, written once for the
    /// fixtures instead of once per test.
    ///
    /// A copy that drifts from the production chain cannot make a test pass
    /// for the wrong reason: the fixture would be written under a name the
    /// search never asks for, and every assertion that expects to find it
    /// goes red.
    fn runtime_library_filename() -> &'static str {
        if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else {
            "libonnxruntime.so"
        }
    }

    /// What `locate_onnxruntime` does with the home, pinned before it moves to
    /// `envs::home()`.
    ///
    /// The home is not a result here, it is the first candidate of a chain:
    /// LD_LIBRARY_PATH and the system directories follow it. So «neither
    /// variable set» is not a failure and must not become one — a machine with
    /// the runtime in /usr/lib and no HOME still loads it, which is the row the
    /// last block fixes.
    #[tokio::test]
    async fn the_runtime_in_the_home_cache_leads_the_chain_and_its_absence_is_silent() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let lib = runtime_library_filename();
        let root = scratch_root("ort-home");
        let installed = root
            .join(".cache")
            .join("memory-industry")
            .join("onnxruntime");
        std::fs::create_dir_all(&installed).expect("the test owns this directory");
        let in_the_cache = installed.join(lib);
        std::fs::write(&in_the_cache, b"not a real library").expect("temp dir is writable");
        let never_created = root.join("userprofile-only");

        // A fresh guard per block: a call that finds something *writes*
        // ORT_DYLIB_PATH, so the next call would short-circuit on what this
        // test itself caused instead of resolving the home again.
        {
            let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "the runtime `models runtime` downloaded under HOME has to win over any system \
                 copy, or the daemon loads a different build of ONNX Runtime than the one whose \
                 provider libraries were installed beside it"
            );
        }

        // The other two states of `preferred.exists() || !legacy.exists()`.
        // With only the preferred directory there, as above, both sides of the
        // `||` are true, so the criterion answers the same however it is read
        // and the block above pins the path and not the rule. These two pin
        // the rule.
        //
        // A runtime in the legacy directory too, rather than an empty one: a
        // wrong pick then resolves to a concrete wrong file instead of falling
        // through to whatever this machine happens to keep in /usr/lib, which
        // would make the assertion depend on the host.
        let pre_rename = root.join(".cache").join("cuba-memorys").join("onnxruntime");
        std::fs::create_dir_all(&pre_rename).expect("the test owns this directory");
        let in_the_legacy_cache = pre_rename.join(lib);
        std::fs::write(&in_the_legacy_cache, b"not the library either")
            .expect("temp dir is writable");

        {
            let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "with a runtime under both names the documented one wins, because it is the \
                 one `models runtime` writes today and the one whose provider libraries were \
                 installed beside it. Loading the pre-rename copy here mixes a build with \
                 providers it was never shipped with"
            );
        }

        // A root of its own for «the old directory there and the new one not»:
        // the root above cannot be brought back to that state without undoing
        // what the two blocks before it just measured.
        let only_legacy_root = scratch_root("ort-home-pre-rename");
        let only_legacy = only_legacy_root
            .join(".cache")
            .join("cuba-memorys")
            .join("onnxruntime");
        std::fs::create_dir_all(&only_legacy).expect("the test owns this directory");
        let in_the_only_cache = only_legacy.join(lib);
        std::fs::write(&in_the_only_cache, b"not a real library").expect("temp dir is writable");
        let nothing_under_it = only_legacy_root.join("userprofile-only");

        {
            let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
            let _h = ScopedEnv::set("HOME", &only_legacy_root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &nothing_under_it.display().to_string());
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_only_cache),
                "an install that downloaded before the rename has its runtime under \
                 cuba-memorys and nothing under the new name. Looking only at the documented \
                 directory drops the candidate out of the chain, and the daemon loads a \
                 system copy — or reports no runtime on a machine that has one"
            );
        }

        {
            let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "on Windows that download went under USERPROFILE, the only one of the two \
                 names PowerShell defines"
            );
        }
        {
            let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            assert_ne!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "with neither name set the cache candidate drops out of the chain and the \
                 search goes on to LD_LIBRARY_PATH and the system directories. Whatever it \
                 finds there, it cannot be the directory it was just told nothing about"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&only_legacy_root);
    }

    /// `cache_roots` answers with an empty vector when there is no home, and
    /// every caller reads that as «no cache to look in».
    ///
    /// Pinned before the migration because an error here would stop
    /// `resolve_model_dir` cold on a machine that is merely unconfigured.
    #[tokio::test]
    async fn without_a_home_the_cache_roots_are_empty_rather_than_guessed() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let root = scratch_root("cache-roots");
        let cache = root.join(".cache");
        let preferred = cache.join("memory-industry");
        let legacy = cache.join("cuba-memorys");
        let never_created = root.join("userprofile-only");

        {
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                cache_roots(),
                vec![preferred.clone(), legacy.clone()],
                "nothing downloaded yet: the documented name is looked at first and the legacy \
                 one stays in the list, so a pre-rename install is still found"
            );

            std::fs::create_dir_all(&legacy).expect("the test owns this directory");
            assert_eq!(
                cache_roots(),
                vec![legacy.clone(), preferred.clone()],
                "a machine that downloaded before the rename keeps its models under \
                 cuba-memorys, and looking at the empty new directory first would report no \
                 model on a machine that has one"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
            assert_eq!(
                cache_roots(),
                vec![legacy.clone(), preferred.clone()],
                "the same answer from USERPROFILE, which is the only one of the two a Windows \
                 service ever has"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            assert_eq!(
                cache_roots(),
                Vec::<PathBuf>::new(),
                "no home means no cache to search, said as an empty list. `resolve_model_dir` \
                 walks it and falls through to the hash embedder, which is the supported \
                 Tier::Minimal machine — an error here would take that machine down instead"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A real directory named in LD_LIBRARY_PATH is a candidate of the search;
    /// the empty segments beside it are not.
    ///
    /// `.filter(|s| !s.is_empty())` reads like tidiness and is not. Delete the
    /// `!` and the chain keeps only the empty segments and throws the real
    /// directories away: the machine that keeps its ONNX Runtime in
    /// LD_LIBRARY_PATH stops finding it and drops to the hash embedder without
    /// saying anything. `cargo mutants` had that deletion alive on that line.
    ///
    /// The fixture directory is relative to the process directory, and that is
    /// not laziness either: the split is on `':'` on every platform, so a
    /// Windows absolute path — `std::env::temp_dir()` is a path under
    /// `C:\Users\...\AppData\Local\Temp` there — is cut at its drive letter
    /// into two segments that are neither empty nor real, and the fixture
    /// would fail as if the code were broken. A relative segment means the
    /// same thing on both platforms and is resolved against the same directory
    /// when this test writes the library and when the search looks for it.
    /// It goes under `target/` so an interrupted run leaves its scratch where
    /// git already ignores it.
    #[tokio::test]
    async fn a_real_directory_in_ld_library_path_is_searched_and_an_empty_segment_is_not() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let lib = runtime_library_filename();
        // `scratch_root` for its uuid and not for its path: two runs of this
        // test must not share a directory that each of them deletes.
        let unique = scratch_root("ort-ld");
        let in_the_path = PathBuf::from("target").join(
            unique
                .file_name()
                .expect("scratch_root always ends in a uuid-suffixed name"),
        );
        std::fs::create_dir_all(&in_the_path).expect("the process directory is writable");
        let installed = in_the_path.join(lib);
        std::fs::write(&installed, b"not a real library")
            .expect("the process directory is writable");

        // Positive control, before the assertion that matters: without it, a
        // process directory this test cannot write to — or one that moved
        // between the write and the search — would read as the search having
        // rejected a real candidate.
        assert!(
            installed.exists(),
            "the fixture is broken: the library was written but does not resolve back from the \
             relative path the search will use"
        );

        // A home with nothing under it, so the first candidate of the chain
        // drops out and what is measured is the LD_LIBRARY_PATH segment.
        let no_cache = scratch_root("ort-ld-home");
        // The empty segments the filter exists to throw away: a `:` too many
        // at the front and one at the end is what a shell leaves behind after
        // `LD_LIBRARY_PATH=$LD_LIBRARY_PATH:/opt/ort` on an unset variable.
        let with_empty_segments = format!(":{}:", in_the_path.display());

        let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
        let _h = ScopedEnv::set("HOME", &no_cache.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &no_cache.display().to_string());
        let _ld = ScopedEnv::set("LD_LIBRARY_PATH", &with_empty_segments);
        assert_eq!(
            locate_onnxruntime().as_ref(),
            Some(&installed),
            "the directory the operator put in LD_LIBRARY_PATH has to be the answer, named. \
             Asserting merely that something was found would pass on a host that keeps a \
             runtime in /usr/lib, and asserting nothing was found would pass on a host that \
             keeps none — both of which are the host answering, not this code"
        );

        let _ = std::fs::remove_dir_all(&in_the_path);
    }

    /// A search answers where the library is. It does not write the answer
    /// into the process environment on the way out.
    ///
    /// `std::env::set_var` is undefined behaviour while another thread reads
    /// the environment — the reason `ScopedEnv` exists — and the daemon calls
    /// this from a multi-threaded tokio runtime, on whichever worker first asks
    /// for an embedding or an NLI verdict. Nothing needs the write either:
    /// `ort` (rc.12 to rc.15) takes the library path by API, `ort::init_from`.
    #[tokio::test]
    async fn finding_the_runtime_does_not_write_ort_dylib_path_into_the_process() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let lib = runtime_library_filename();
        let root = scratch_root("ort-no-env-write");
        let installed = root
            .join(".cache")
            .join("memory-industry")
            .join("onnxruntime");
        std::fs::create_dir_all(&installed).expect("the test owns this directory");
        let in_the_cache = installed.join(lib);
        std::fs::write(&in_the_cache, b"not a real library").expect("temp dir is writable");

        let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
        let _h = ScopedEnv::set("HOME", &root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
        assert_eq!(
            std::env::var_os("ORT_DYLIB_PATH"),
            None,
            "the fixture is broken: the variable is set before the search runs, so what follows \
             would measure the operator's environment and not this function"
        );

        // Positive control: an assertion that the variable stays unset passes
        // just as well on a search that found nothing and so had nothing to
        // write.
        assert_eq!(
            locate_onnxruntime().as_ref(),
            Some(&in_the_cache),
            "the fixture is broken: the search did not find the runtime installed under HOME"
        );
        assert_eq!(
            std::env::var_os("ORT_DYLIB_PATH"),
            None,
            "locate_onnxruntime wrote ORT_DYLIB_PATH into the process. A search that can be \
             called from any tokio worker must not call std::env::set_var: another thread \
             reading the environment at the same moment is undefined behaviour. Hand the path \
             to `ort::init_from` instead"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// An `ORT_DYLIB_PATH` that is empty or blank is the same as none: the
    /// search goes on.
    ///
    /// `memory-industry setup service` renders `ORT_DYLIB_PATH=` into the
    /// env file as a placeholder for the operator, and systemd's
    /// `EnvironmentFile` exports it as an EMPTY variable, not an absent one.
    /// The daemon on Linux read that as «the operator named a file, and it is
    /// not there»: `locate_onnxruntime` answered `None` at once, never looked
    /// in the cache, `LD_LIBRARY_PATH` or the system directories, and the
    /// runtime was reported missing although it was installed.
    /// `ONNX_MODEL_PATH` has the same placeholder and `resolve_model_dir`
    /// already treats it as unset; this pins the same for the library.
    ///
    /// The control matters as much as the rest: a NON-empty path to a file
    /// that is not there stays `None`, because that one is the operator's
    /// word. A fix that merely ignored the variable would pass the first two
    /// assertions and fail the third.
    #[tokio::test]
    async fn an_empty_ort_dylib_path_is_the_same_as_none_and_the_search_goes_on() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let lib = runtime_library_filename();
        let root = scratch_root("ort-empty-explicit");
        let installed = root
            .join(".cache")
            .join("memory-industry")
            .join("onnxruntime");
        std::fs::create_dir_all(&installed).expect("the test owns this directory");
        let in_the_cache = installed.join(lib);
        std::fs::write(&in_the_cache, b"not a real library").expect("temp dir is writable");

        // The cache is the first candidate of the chain, so what answers is
        // this fixture and not a runtime the host keeps in /usr/lib or in its
        // own LD_LIBRARY_PATH.
        let _h = ScopedEnv::set("HOME", &root.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
        let _ld = ScopedEnv::cleared("LD_LIBRARY_PATH");

        {
            let _unset = ScopedEnv::cleared("ORT_DYLIB_PATH");
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "the fixture is broken: with the variable unset the search did not find the                  runtime installed under HOME, so the empty cases below measure nothing"
            );
        }
        {
            let _empty = ScopedEnv::set("ORT_DYLIB_PATH", "");
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "`ORT_DYLIB_PATH=` is what the generated env file leaves for the operator to                  fill in, and systemd exports it empty. An empty value names no file, so it                  has to mean «not set» and the search has to go on to the cache; answering                  None reports an installed runtime as missing"
            );
        }
        {
            let _blank = ScopedEnv::set("ORT_DYLIB_PATH", "   ");
            assert_eq!(
                locate_onnxruntime().as_ref(),
                Some(&in_the_cache),
                "a value of only spaces, left by an editor on that placeholder line, names no                  file either and reads as not set, the way an empty `ONNX_MODEL_PATH` does"
            );
        }
        {
            let missing = root.join("elsewhere").join(lib);
            let _explicit = ScopedEnv::set("ORT_DYLIB_PATH", &missing.display().to_string());
            assert_eq!(
                locate_onnxruntime(),
                None,
                "control: a non-empty path to a file that is not there is the operator's                  word and does not fall back to the search. If this found the cache, the                  variable would be ignored altogether and a typo in it would load a runtime                  nobody chose"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// An empty runtime path is refused, with an error that says it is empty,
    /// before anything reaches `ort` — and in bounded time.
    ///
    /// `ort` does not refuse it. `setup_api` (ort rc.12 to rc.15, `src/lib.rs`)
    /// reads an empty `ORT_DYLIB_PATH` as unset and loads the bare name
    /// `onnxruntime.dll`, which Windows resolves to the Windows ML build in
    /// System32 — the load described at `get_model_status` as a hang on the
    /// first embedding, and the 90 s TIMEOUT the mutant `Some(Default::default())`
    /// produced. And `ort::init_from` checks nothing once some library is
    /// loaded: it answers `Ok` for any path. So the check lives in this crate
    /// or nowhere, and it has to come before the call into `ort`.
    ///
    /// A plain thread and `recv_timeout`, not `tokio::time::timeout`: a
    /// `spawn_blocking` that times out keeps its thread, and dropping the
    /// runtime at the end of the test waits for it, so the test would hang all
    /// the same.
    #[test]
    fn an_empty_runtime_path_is_refused_by_name_and_never_handed_to_ort() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ =
                tx.send(load_onnxruntime(std::path::Path::new("")).map_err(|e| format!("{e:#}")));
        });

        let answer = rx.recv_timeout(std::time::Duration::from_secs(5)).expect(
            "loading the runtime from an empty path did not answer in 5 s. That is the hang \
                 the mutant `Some(Default::default())` hit: the path went through to `ort`, which \
                 falls back to whatever `onnxruntime.dll` the loader finds",
        );
        let reason = answer.expect_err(
            "an empty path loaded something. Whatever it loaded is not a library anyone named",
        );
        assert!(
            reason.contains("empty") || reason.contains("vacía") || reason.contains("vacia"),
            "the error has to say the path is empty, so an operator with `ORT_DYLIB_PATH=` in a \
             .env reads their own mistake and not a loader message: {reason}"
        );
    }

    /// A runtime file that is not a library is an error, in bounded time, and
    /// never reaches `ort`.
    ///
    /// Handed to `ort::init_from` in rc.12, it hung the thread forever: rc.12
    /// built the load error through `ort::api()`, which re-entered the `Once`
    /// the failing load still held. rc.13 fixed that (pykeio/ort 17ed727) and
    /// `init_from` now answers with a `LoadDynamicError`; the check stays for
    /// what `ort` does not give: an error that names the file and the way out
    /// (`models runtime`, `ORT_DYLIB_PATH`), and no library kept by `ort` that
    /// this crate did not open first. The ten seconds below are not a
    /// performance budget; they turn a hang, should one come back, into a
    /// failure with a name.
    ///
    /// `load_onnxruntime_once` and not `load_onnxruntime`: the second keeps the
    /// first answer in a cell that belongs to the process, and a broken
    /// library recorded there would refuse the real runtime to every later
    /// test in this binary. The cell adds no step between the check and
    /// `ort`, so this is the path the daemon takes.
    ///
    /// A plain thread and `recv_timeout`, for the reason given at the
    /// empty-path test above.
    #[test]
    fn a_runtime_that_is_not_a_library_is_refused_in_bounded_time_and_never_reaches_ort() {
        let dir = scratch_root("ort-not-a-library");
        std::fs::create_dir_all(&dir).expect("the test owns this directory");
        let runtime = dir.join(runtime_library_filename());
        std::fs::write(&runtime, b"these bytes are not a shared library\n")
            .expect("temp dir is writable");

        let (tx, rx) = std::sync::mpsc::channel();
        let handed = runtime.clone();
        std::thread::spawn(move || {
            let _ = tx.send(
                load_onnxruntime_once(&handed)
                    .map(drop)
                    .map_err(|e| format!("{e:#}")),
            );
        });

        let answer = rx.recv_timeout(std::time::Duration::from_secs(10)).expect(
            "loading a runtime that is not a library did not answer in 10 s. In ort rc.12 that \
             was a deadlock on its own `Once` (fixed in rc.13, pykeio/ort 17ed727): the file \
             went to `ort::init_from` without being opened and checked here first",
        );
        let reason = answer.expect_err(
            "a file of bytes that is not a library loaded as ONNX Runtime. If `ort` already held \
             a runtime from another test, `init_from` answers Ok for any path — which is why the \
             check cannot be left to it",
        );
        assert!(
            reason.contains("loading ONNX Runtime from"),
            "the error has to say it was the runtime that would not load: {reason}"
        );
        assert!(
            reason.contains(runtime_library_filename()),
            "and name the file, so `/health` has a path to replace with the variable that sets \
             it: {reason}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The version check reads the minor number the way `ort` does, because
    /// any runtime `ort` would refuse has to be refused here first.
    #[test]
    fn the_runtime_version_is_read_the_way_ort_reads_it() {
        for (version, minor) in [
            ("1.22.0", 22),
            ("1.24.1", 24),
            ("1.17.3", 17),
            ("1.x.0", 0),
            ("1", 0),
            ("", 0),
        ] {
            assert_eq!(minor_version(version), minor, "version string {version:?}");
        }
        assert!(
            minor_version("1.17.3") < ort::MINOR_VERSION,
            "the Windows ML ONNX Runtime in System32 is 1.17, and this build of `ort` refuses it \
             — in rc.12 through the error path that deadlocked for a file that is not a \
             library. If this fails, `ort` accepts 1.17 again and the check above is refusing a \
             runtime it would have loaded"
        );
    }

    /// `api-24` is a feature of `ort`, and it is what sets `ort::MINOR_VERSION`.
    /// `ort` is declared without its default features, so the literal pins the
    /// one that does not show up as a dependency.
    #[test]
    fn the_onnx_runtime_floor_is_1_24() {
        assert_eq!(
            ort::MINOR_VERSION,
            24,
            "this number comes from the `api-24` feature of `ort`; lowering it lets a runtime \
             through that lacks functions this build calls, and raising it refuses a runtime \
             the models already load on"
        );
    }

    /// With no runtime anywhere in the chain, loading the located runtime is
    /// an error that tells the operator how to get one — never `Ok`.
    ///
    /// An `Ok` here is the hang described at `get_model_status`: `ort` stays
    /// uninitialised, and the first session that opens loads whatever bare
    /// `onnxruntime.dll` the platform loader finds. `cargo mutants` had
    /// `load_located_onnxruntime -> Ok(())` with no test of its own.
    ///
    /// Every candidate of `locate_onnxruntime` is taken out: ORT_DYLIB_PATH
    /// cleared (the gate exports it at the real runtime), a home with no cache
    /// under it, LD_LIBRARY_PATH cleared. The system directories are fixed in
    /// production and cannot be moved from a test, so the positive control
    /// below fails by name on a host that keeps a runtime there, instead of
    /// letting this test hand a real library to `ort`.
    #[tokio::test]
    async fn with_no_runtime_anywhere_loading_the_located_one_says_how_to_get_it() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let empty_home = scratch_root("ort-none");
        let _explicit = ScopedEnv::cleared("ORT_DYLIB_PATH");
        let _h = ScopedEnv::set("HOME", &empty_home.display().to_string());
        let _u = ScopedEnv::set("USERPROFILE", &empty_home.display().to_string());
        let _ld = ScopedEnv::cleared("LD_LIBRARY_PATH");

        assert_eq!(
            locate_onnxruntime(),
            None,
            "the fixture is broken: with ORT_DYLIB_PATH and LD_LIBRARY_PATH cleared and an empty \
             home, the search still found a runtime, so this host keeps one in a system \
             directory (/usr/lib, /usr/local/lib, /usr/lib/x86_64-linux-gnu, /usr/lib64, \
             /opt/homebrew/lib). This test cannot measure the missing-runtime answer here"
        );

        let reason = load_located_onnxruntime()
            .map_err(|e| format!("{e:#}"))
            .expect_err(
                "no runtime anywhere in the chain and the load answered Ok. `ort` is left \
                 uninitialised and the first session loads whatever onnxruntime the platform \
                 loader finds — on Windows the Windows ML build in System32, which hangs in \
                 rc.12 and panics in rc.15",
            );
        assert!(
            reason.contains("no ONNX Runtime library found"),
            "the error has to say the runtime is missing: {reason}"
        );
        assert!(
            reason.contains("memory-industry models runtime") && reason.contains("ORT_DYLIB_PATH"),
            "and it has to name both ways out, the download and the variable: {reason}"
        );

        let _ = std::fs::remove_dir_all(&empty_home);
    }

    /// CI-D5b (D5b.8). From 1.31 the official builds of ONNX Runtime send
    /// telemetry to Microsoft unless `ORT_DISABLE_TELEMETRY` is set before the
    /// library starts, and the only place that runs ahead of it in the daemon is
    /// `main`, which calls this. An operator who set the variable keeps their
    /// value, whatever it is: it is theirs to decide, and `set_var` over it
    /// would be a second opinion nobody asked for. The environment is only
    /// written under the global guard: `set_var` is unsound next to a reader.
    #[tokio::test]
    async fn the_daemon_turns_onnx_runtime_telemetry_off_and_leaves_the_operators_word_alone() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        {
            let _unset = ScopedEnv::cleared("ORT_DISABLE_TELEMETRY");
            keep_onnx_runtime_telemetry_off();
            assert_eq!(
                std::env::var("ORT_DISABLE_TELEMETRY").ok().as_deref(),
                Some("1"),
                "nobody decided, so telemetry is off: the variable is the only thing that stops \
                 the first event and the persistent device identifier"
            );
        }
        {
            let _operator = ScopedEnv::set("ORT_DISABLE_TELEMETRY", "0");
            keep_onnx_runtime_telemetry_off();
            assert_eq!(
                std::env::var("ORT_DISABLE_TELEMETRY").ok().as_deref(),
                Some("0"),
                "an operator who set it to \"0\" asked for telemetry on, and the daemon respects it"
            );
        }
    }
}
