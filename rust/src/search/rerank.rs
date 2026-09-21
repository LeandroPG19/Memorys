use anyhow::{Context, Result};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use std::path::PathBuf;
use std::sync::OnceLock;

static RERANKER_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

static RERANKER_STATUS: OnceLock<RerankerStatus> = OnceLock::new();

static RERANKER_SESSION: OnceLock<std::sync::Mutex<Session>> = OnceLock::new();

static RERANKER_TOKENIZER: OnceLock<tokenizers::Tokenizer> = OnceLock::new();

static RERANKER_SEMAPHORE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();

enum RerankerStatus {
    Loaded,
    /// No model to load: nothing is wrong, the ranking just comes back in RRF
    /// order.
    Unavailable,
    /// A model is on disk and the session did not open. The reason is kept
    /// because `doctor` used to guess at it, and guessed wrong: it answered
    /// every load failure with "check ORT_DYLIB_PATH" while a real deployment
    /// was failing on a CUDA arena that was too small.
    Failed(String),
}

fn semaphore() -> &'static tokio::sync::Semaphore {
    RERANKER_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(rerank_concurrency()))
}

const RERANK_DEFAULT_CONCURRENCY: usize = 1;

pub fn rerank_concurrency() -> usize {
    std::env::var("CUBA_RERANK_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(RERANK_DEFAULT_CONCURRENCY)
}

fn intra_threads() -> usize {
    if let Some(n) = std::env::var("CUBA_RERANK_INTRA_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    if crate::gpu::wants_gpu(crate::gpu::Workload::Reranker) {
        return 2;
    }
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 8))
        .unwrap_or(2)
}

pub fn enabled() -> bool {
    matches!(get_status(), RerankerStatus::Loaded)
}

pub fn status_resolved() -> bool {
    RERANKER_STATUS.get().is_some()
}

/// Why the reranker is not loaded, when something already tried to load it.
///
/// `None` means it loaded, or nobody has asked yet. Reading the resolved cell
/// rather than forcing it keeps this callable from an async task.
pub fn failure_reason() -> Option<String> {
    reason_of(RERANKER_STATUS.get()?)
}

/// Only a session that had a model and could not open it has a reason worth
/// showing. "There is no model" is not a failure, and saying it here would put
/// it in `doctor` as one.
fn reason_of(status: &RerankerStatus) -> Option<String> {
    match status {
        RerankerStatus::Loaded | RerankerStatus::Unavailable => None,
        RerankerStatus::Failed(reason) => Some(reason.clone()),
    }
}

pub fn is_configured() -> bool {
    resolved_model_dir().is_some_and(|dir| model_file_in(&dir).is_some())
}

fn default_reranker_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let cache = PathBuf::from(home).join(".cache");
    let preferred = cache.join("memory-industry").join("reranker");
    let legacy = cache.join("cuba-memorys").join("reranker");
    Some(if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    })
}

const WARMUP_CANDIDATES: usize = 50;
/// Startup has time; a request does not.
const WARMUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
const WARMUP_PASSAGE_CHARS: usize = 240;

pub async fn warm_up() -> bool {
    if !tokio::task::spawn_blocking(enabled).await.unwrap_or(false) {
        return false;
    }
    let passage = "the retrieval pipeline fuses lexical and vector signals before the \
                   cross-encoder rescores the surviving candidates in a single batch "
        .repeat(WARMUP_PASSAGE_CHARS / 100 + 1);
    let passages: Vec<&str> = std::iter::repeat_n(passage.as_str(), WARMUP_CANDIDATES).collect();
    // Not the search budget: warming up exists precisely to pay the load, and
    // measuring it against a ceiling meant for a request would fail on any
    // machine where the model takes longer than one search may.
    let deadline = std::time::Instant::now() + WARMUP_BUDGET;
    rerank_within(
        "which passage answers the question best",
        &passages,
        deadline,
    )
    .await
    .is_ok()
}

/// The name the resource plan uses to switch a model off: a directory under
/// the temp dir that nothing ever creates. Matching on the name rather than on
/// existence is the whole point — the directory is *supposed* not to be there.
const DISABLED_MARKER: &str = "disabled-by-resource-plan";

/// The one rule that answers "which reranker directory, if any".
///
/// `is_configured()` and the loader both derive from this, so they cannot
/// disagree. They used to resolve the path differently: `is_configured` looked
/// at whatever `CUBA_RERANKER_PATH` named, while the loader filtered that path
/// through `.exists()` and fell through to the cache when it did not. Since the
/// plan disables the reranker with a directory it never creates, the filter
/// dropped the sentinel, the loader found the real model in the cache, and a
/// plan that said "no room for the reranker" still loaded it inside the first
/// search — while `doctor` reported it disabled.
///
/// A path that names a directory with no model in it now stays that path.
/// Quietly reranking with a different model than the one an operator pointed at
/// is the same class of bug.
pub fn resolved_model_dir() -> Option<PathBuf> {
    match std::env::var("CUBA_RERANKER_PATH") {
        Ok(raw) => {
            let dir = PathBuf::from(raw);
            if dir.to_string_lossy().contains(DISABLED_MARKER) {
                return None;
            }
            Some(dir)
        }
        Err(_) => default_reranker_dir(),
    }
}

/// Megabytes ONNX Runtime will have to place on the device, read from disk
/// without opening a session.
///
/// `model.onnx` is often only the graph: measured on a 2026-09 install it is
/// 614 KB against a sibling `model.onnx_data` of 4,23 GiB. Sizing a VRAM
/// budget from the `.onnx` alone is wrong by three orders of magnitude, so the
/// external-data file is part of the measurement. And the answer legitimately
/// differs per machine — an FP16 export of the same model is about 1,7 GiB —
/// which is exactly why this is measured instead of written down as a constant.
pub fn model_weights_mb() -> Option<u64> {
    let dir = resolved_model_dir()?;
    let model = model_file_in(&dir)?;
    let graph = std::fs::metadata(&model).ok()?.len();
    let name = model.file_name()?.to_string_lossy().into_owned();
    Some((graph + external_data_bytes(&dir, &name)) / (1024 * 1024))
}

/// Weights kept beside the graph rather than inside it. ORT accepts both
/// spellings and neither is required to exist.
fn external_data_bytes(dir: &std::path::Path, model_file_name: &str) -> u64 {
    [
        format!("{model_file_name}_data"),
        format!("{model_file_name}.data"),
    ]
    .iter()
    .filter_map(|name| std::fs::metadata(dir.join(name)).ok())
    .map(|meta| meta.len())
    .sum()
}

/// The file `init_session` would open, in the order it would try them.
fn model_file_in(dir: &std::path::Path) -> Option<PathBuf> {
    ["model_quantized.onnx", "model.onnx"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

fn get_status() -> &'static RerankerStatus {
    RERANKER_STATUS.get_or_init(|| {
        let path = RERANKER_PATH.get_or_init(resolved_model_dir);
        match path {
            Some(p) => match init_session(p) {
                Ok(()) => {
                    tracing::info!(path = %p.display(), "bge-reranker ONNX loaded");
                    RerankerStatus::Loaded
                }
                Err(e) => {
                    let reason = format!("{e:#}");
                    tracing::error!(error = %reason, "reranker init failed — identity fallback");
                    RerankerStatus::Failed(reason)
                }
            },
            None => RerankerStatus::Unavailable,
        }
    })
}

fn init_session(model_dir: &std::path::Path) -> Result<()> {
    let candidates = ["model_quantized.onnx", "model.onnx"];
    let model_file = candidates
        .iter()
        .map(|n| model_dir.join(n))
        .find(|p| p.exists())
        .ok_or_else(|| {
            anyhow::anyhow!("no model.onnx / model_quantized.onnx found in {model_dir:?}")
        })?;

    // A factory, not a builder: if the GPU provider refuses to start we
    // need a second, clean builder for the CPU path.
    let make_builder = || {
        Session::builder()
            .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
            .with_intra_threads(intra_threads())
            .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {e}"))
    };
    let session = crate::gpu::configure(make_builder, crate::gpu::Workload::Reranker)?
        .commit_from_file(&model_file)
        .map_err(|e| anyhow::anyhow!("load model: {e}"))?;
    RERANKER_SESSION
        .set(std::sync::Mutex::new(session))
        .map_err(|_| anyhow::anyhow!("session already initialized"))?;

    let tokenizer_path = model_dir.join("tokenizer.json");
    if !tokenizer_path.exists() {
        anyhow::bail!("tokenizer.json missing at {tokenizer_path:?}");
    }
    let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("tokenizer load: {e}"))?;
    let truncation = tokenizers::TruncationParams {
        max_length: 512,
        ..Default::default()
    };
    tokenizer
        .with_truncation(Some(truncation))
        .map_err(|e| anyhow::anyhow!("tokenizer truncation: {e}"))?;
    let padding = tokenizers::PaddingParams {
        strategy: tokenizers::PaddingStrategy::BatchLongest,
        ..Default::default()
    };
    tokenizer.with_padding(Some(padding));
    RERANKER_TOKENIZER
        .set(tokenizer)
        .map_err(|_| anyhow::anyhow!("tokenizer already initialized"))?;
    Ok(())
}

pub async fn rerank(query: &str, candidates: &[&str]) -> Result<Vec<(usize, f64)>> {
    rerank_within(query, candidates, std::time::Instant::now() + budget()).await
}

/// Wait for the one session permit, but not past the deadline.
///
/// `spawn_blocking` is not cancellable, so a batch whose caller already gave
/// up keeps the permit and the session mutex until it finishes on its own.
/// Without a bound here the next search waits on it forever.
async fn permit_before(
    deadline: std::time::Instant,
) -> Result<tokio::sync::SemaphorePermit<'static>> {
    let wait = deadline.saturating_duration_since(std::time::Instant::now());
    if wait.is_zero() {
        return Err(anyhow::anyhow!(
            "el presupuesto se consumió antes de llegar al reranker: en un proceso frío la carga del modelo se lo come, y subir CUBA_RERANK_TIMEOUT_SECS o precalentar con CUBA_WARM_RERANKER es lo que lo arregla"
        ));
    }
    match tokio::time::timeout(wait, semaphore().acquire()).await {
        Ok(permit) => permit.map_err(|_| anyhow::anyhow!("reranker semaphore closed")),
        Err(_) => Err(anyhow::anyhow!(
            "el reranker sigue ocupado con un lote anterior y este agotó su presupuesto"
        )),
    }
}

/// One rerank, finished by `deadline` or not at all.
///
/// The deadline covers resolving the model as well as the inference. It used
/// to start after the model had resolved, so a cold 1.1 GB read was unbounded
/// time the caller's 20 s never accounted for.
pub async fn rerank_within(
    query: &str,
    candidates: &[&str],
    deadline: std::time::Instant,
) -> Result<Vec<(usize, f64)>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Before anything, including the load. If the budget is already gone there
    // is nothing worth starting, and checking here means every path below is
    // reached with time left rather than discovering it halfway.
    if std::time::Instant::now() >= deadline {
        anyhow::bail!("el rerank no tenía presupuesto ya al empezar");
    }

    let n = candidates.len();

    // Resolve the model *before* taking the permit. Loading is already
    // serialised by the OnceLock, so holding the single session permit through
    // a multi-gigabyte read only makes every other caller wait for a load they
    // are not the one doing — and then time out on a permit, which reads like
    // a busy reranker rather than a cold one.
    let loaded = tokio::task::spawn_blocking(enabled)
        .await
        .context("reranker status task panicked")?;
    if !loaded {
        return Ok(identity_pairs(n));
    }
    let query_owned = query.to_string();
    let candidates_owned: Vec<String> = candidates.iter().map(|c| c.to_string()).collect();

    let _permit = permit_before(deadline).await?;

    Ok(ranked(
        score_off_runtime(query_owned, candidates_owned, deadline).await?,
    ))
}

/// The inference itself, on a blocking thread.
///
/// It holds the session mutex for as long as it runs, which is why it is the
/// one thing here carrying the deadline rather than trusting the caller to
/// cancel it: `spawn_blocking` cannot be cancelled from outside.
async fn score_off_runtime(
    query: String,
    candidates: Vec<String>,
    deadline: std::time::Instant,
) -> Result<Vec<f64>> {
    tokio::task::spawn_blocking(move || score_pairs(&query, &candidates, deadline))
        .await
        .context("reranker task panicked")?
}

/// Cross-encoder scores, best first. NaN sorts as equal rather than panicking:
/// a model that emitted one has already failed, and taking the process down
/// over the ordering of the evidence helps nobody.
fn ranked(scored: Vec<f64>) -> Vec<(usize, f64)> {
    let mut indexed: Vec<(usize, f64)> = scored.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    indexed
}

const RERANK_CHUNK: usize = 16;
const RERANK_MAX_TOKENS: usize = 512;

fn rerank_chunk() -> usize {
    std::env::var("CUBA_RERANK_CHUNK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(RERANK_CHUNK)
}

pub fn bucketed_len(longest: usize) -> usize {
    let bucket = rerank_bucket();
    if bucket == 0 {
        return longest.max(1);
    }
    let rounded = longest.div_ceil(bucket) * bucket;
    rounded.clamp(bucket, RERANK_MAX_TOKENS)
}

const RERANK_DEFAULT_BUCKET: usize = 512;

pub fn rerank_bucket() -> usize {
    std::env::var("CUBA_RERANK_BUCKET")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &usize| *v == 0 || (v.is_power_of_two() && *v <= RERANK_MAX_TOKENS))
        .unwrap_or(RERANK_DEFAULT_BUCKET)
}

pub fn fixed_shape() -> bool {
    match std::env::var("CUBA_RERANK_FIXED_SHAPE").as_deref() {
        Ok("0") | Ok("off") | Ok("false") => false,
        Ok(_) => true,
        Err(_) => crate::gpu::wants_gpu(crate::gpu::Workload::Reranker),
    }
}

fn length_bucketing() -> bool {
    match std::env::var("CUBA_RERANK_LENGTH_BUCKETING").as_deref() {
        Ok("0") | Ok("off") | Ok("false") => false,
        Ok(_) => true,
        Err(_) => !fixed_shape(),
    }
}

/// The budget one rerank gets, end to end.
pub fn budget() -> std::time::Duration {
    budget_from(std::env::var("CUBA_RERANK_TIMEOUT_SECS").ok().as_deref())
}

fn budget_from(raw: Option<&str>) -> std::time::Duration {
    let secs = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(20);
    std::time::Duration::from_secs(secs)
}

fn score_pairs(
    query: &str,
    candidates: &[String],
    deadline: std::time::Instant,
) -> Result<Vec<f64>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let session_lock = RERANKER_SESSION
        .get()
        .context("reranker session not initialized")?;
    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("session lock poisoned: {e}"))?;
    let tokenizer = RERANKER_TOKENIZER
        .get()
        .context("reranker tokenizer not initialized")?;

    let mut order: Vec<usize> = (0..candidates.len()).collect();
    if length_bucketing() {
        order.sort_by_key(|&i| std::cmp::Reverse(candidates[i].len()));
    }

    let chunk_size = rerank_chunk();
    let mut scores = vec![0.0_f64; candidates.len()];
    let total = order.len();
    let mut done = 0usize;
    for chunk in order.chunks(chunk_size) {
        // Cooperative, because spawn_blocking cannot be cancelled from outside.
        // Running the remaining chunks for a caller that already gave up keeps
        // the session mutex held, and the next search waits behind it.
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "el reranker agotó su presupuesto tras {done} de {total} candidatos; suelta la sesión en vez de terminar un lote que ya nadie espera"
            );
        }
        let texts: Vec<String> = chunk.iter().map(|&i| candidates[i].clone()).collect();

        let chunk_scores = score_one_chunk(&mut session, tokenizer, query, &texts, chunk_size)?;

        for (pos, &original) in chunk.iter().enumerate() {
            scores[original] = chunk_scores[pos];
        }
        done += chunk.len();
    }
    Ok(scores)
}

/// One batch, padded to the fixed shape when the session demands one.
///
/// Under `fixed_shape` every batch has to be the same size, so a short last
/// chunk is padded with empty strings and their scores dropped again. Doing it
/// here keeps the padding out of the loop that owns the deadline.
fn score_one_chunk(
    session: &mut Session,
    tokenizer: &tokenizers::Tokenizer,
    query: &str,
    texts: &[String],
    chunk_size: usize,
) -> Result<Vec<f64>> {
    if !fixed_shape() || texts.len() == chunk_size {
        return score_chunk(session, tokenizer, query, texts);
    }
    let mut padded = texts.to_vec();
    padded.resize(chunk_size, String::new());
    let mut scores = score_chunk(session, tokenizer, query, &padded)?;
    scores.truncate(texts.len());
    Ok(scores)
}

fn score_chunk(
    session: &mut Session,
    tokenizer: &tokenizers::Tokenizer,
    query: &str,
    candidates: &[String],
) -> Result<Vec<f64>> {
    let pairs: Vec<(&str, &str)> = candidates.iter().map(|c| (query, c.as_str())).collect();
    let encodings = tokenizer
        .encode_batch(pairs, true)
        .map_err(|e| anyhow::anyhow!("encode batch: {e}"))?;

    let batch = encodings.len();
    let longest = encodings
        .iter()
        .map(|e| e.get_ids().len())
        .max()
        .context("empty batch")?;
    let seq = bucketed_len(longest);

    let mut ids = Vec::with_capacity(batch * seq);
    let mut mask = Vec::with_capacity(batch * seq);
    let mut types = Vec::with_capacity(batch * seq);
    for e in &encodings {
        let (e_ids, e_mask, e_types) = (e.get_ids(), e.get_attention_mask(), e.get_type_ids());
        for i in 0..seq {
            ids.push(*e_ids.get(i).unwrap_or(&0) as i64);
            mask.push(*e_mask.get(i).unwrap_or(&0) as i64);
            types.push(*e_types.get(i).unwrap_or(&0) as i64);
        }
    }

    let shape = vec![batch as i64, seq as i64];
    let input_ids_t =
        ort::value::Tensor::from_array((shape.clone(), ids)).context("input_ids tensor")?;
    let attn_t =
        ort::value::Tensor::from_array((shape.clone(), mask)).context("attention_mask tensor")?;

    let wants_type_ids = session
        .inputs()
        .iter()
        .any(|i| i.name() == "token_type_ids");

    let outputs = if wants_type_ids {
        let type_t =
            ort::value::Tensor::from_array((shape, types)).context("token_type_ids tensor")?;
        session.run(ort::inputs! {
            "input_ids" => input_ids_t,
            "attention_mask" => attn_t,
            "token_type_ids" => type_t,
        })
    } else {
        session.run(ort::inputs! {
            "input_ids" => input_ids_t,
            "attention_mask" => attn_t,
        })
    }
    .map_err(|e| anyhow::anyhow!("inference: {e}"))?;

    if outputs.len() == 0 {
        anyhow::bail!("reranker returned no outputs");
    }

    let (out_shape, data): (Vec<i64>, Vec<f32>) = match outputs[0].try_extract_tensor::<f32>() {
        Ok((s, d)) => (s.to_vec(), d.to_vec()),
        Err(_) => {
            let (s, d) = outputs[0]
                .try_extract_tensor::<half::f16>()
                .map_err(|e| anyhow::anyhow!("extract logits (f32 and f16): {e}"))?;
            (s.to_vec(), d.iter().map(|h| h.to_f32()).collect())
        }
    };

    let num_labels = out_shape.last().copied().unwrap_or(1).max(1) as usize;
    if data.len() < batch * num_labels {
        anyhow::bail!(
            "reranker returned {} values, expected {}×{}",
            data.len(),
            batch,
            num_labels
        );
    }

    let mut scores = Vec::with_capacity(batch);
    for b in 0..batch {
        let row = &data[b * num_labels..(b + 1) * num_labels];
        let logit = match num_labels {
            1 => row[0],
            2 => row[1] - row[0],
            _ => row[0],
        };
        scores.push(1.0_f64 / (1.0 + (-logit as f64).exp()));
    }
    Ok(scores)
}

fn identity_pairs(n: usize) -> Vec<(usize, f64)> {
    (0..n).map(|i| (i, (n - i) as f64)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests move HOME and CUBA_RERANKER_PATH, which every other test in
    /// this process reads. Serialise them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A throwaway HOME whose cache holds a plausible reranker, so the tests
    /// below never depend on what this machine happens to have installed.
    struct FakeHome {
        root: PathBuf,
        home: Option<String>,
        userprofile: Option<String>,
    }

    impl FakeHome {
        fn with_a_model_in_the_cache(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "memory-industry-rerank-{}-{tag}",
                std::process::id()
            ));
            let cache = root.join(".cache").join("memory-industry").join("reranker");
            std::fs::create_dir_all(&cache).expect("temp dir is writable");
            std::fs::write(cache.join("model.onnx"), b"not a real graph").expect("writable");

            let me = Self {
                home: std::env::var("HOME").ok(),
                userprofile: std::env::var("USERPROFILE").ok(),
                root,
            };
            unsafe {
                std::env::set_var("HOME", &me.root);
                std::env::set_var("USERPROFILE", &me.root);
            }
            me
        }
    }

    impl Drop for FakeHome {
        fn drop(&mut self) {
            unsafe {
                match &self.home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                match &self.userprofile {
                    Some(v) => std::env::set_var("USERPROFILE", v),
                    None => std::env::remove_var("USERPROFILE"),
                }
                std::env::remove_var("CUBA_RERANKER_PATH");
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn the_path_the_plan_disabled_does_not_fall_through_to_the_cache() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = FakeHome::with_a_model_in_the_cache("disabled");

        // Positive control first: without the sentinel the fixture resolves, so
        // a None below means the sentinel did its job and not that the fake
        // cache was never found.
        unsafe { std::env::remove_var("CUBA_RERANKER_PATH") };
        assert!(
            resolved_model_dir().is_some(),
            "the fixture itself is broken: a cache with model.onnx must resolve"
        );

        unsafe {
            std::env::set_var(
                "CUBA_RERANKER_PATH",
                crate::resources::disabled_model_path(),
            )
        };
        assert_eq!(
            resolved_model_dir(),
            None,
            "the resource plan switches the reranker off by pointing CUBA_RERANKER_PATH at a              directory it deliberately never creates. Filtering that path on `.exists()` drops              the sentinel and falls through to the cache, so the plan says 'no room for the              reranker', doctor reports it disabled, and the first search still loads 1.1 GB"
        );
    }

    fn force_fallback_if_unresolved() {
        if !status_resolved() {
            unsafe { std::env::set_var("CUBA_RERANKER_PATH", std::env::temp_dir()) };
        }
    }

    #[tokio::test]
    async fn identity_when_disabled() {
        force_fallback_if_unresolved();
        assert!(
            !enabled(),
            "force_fallback_if_unresolved must guarantee the fallback path; if this \
             fires, something else in this process resolved the reranker status first \
             and the identity path went untested"
        );
        let pairs = rerank("anything", &["a", "b", "c"]).await.unwrap();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, 0);
        assert!(pairs[0].1 > pairs[1].1);
    }

    #[test]
    fn both_answers_about_the_reranker_come_from_the_same_rule() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = FakeHome::with_a_model_in_the_cache("same-rule");

        let empty = home.root.join("empty");
        std::fs::create_dir_all(&empty).expect("writable");
        let named = home.root.join("named");
        std::fs::create_dir_all(&named).expect("writable");
        std::fs::write(named.join("model_quantized.onnx"), b"graph").expect("writable");

        // (what CUBA_RERANKER_PATH says, does a directory resolve, is a model there)
        let cases: [(Option<PathBuf>, bool, bool); 4] = [
            (None, true, true),
            (
                Some(PathBuf::from(crate::resources::disabled_model_path())),
                false,
                false,
            ),
            (Some(empty), true, false),
            (Some(named), true, true),
        ];

        for (env, resolves, configured) in cases {
            unsafe {
                match &env {
                    Some(path) => std::env::set_var("CUBA_RERANKER_PATH", path),
                    None => std::env::remove_var("CUBA_RERANKER_PATH"),
                }
            }
            assert_eq!(
                resolved_model_dir().is_some(),
                resolves,
                "resolving with CUBA_RERANKER_PATH={env:?}"
            );
            assert_eq!(
                is_configured(),
                configured,
                "is_configured with CUBA_RERANKER_PATH={env:?} — this and the loader read the                  same rule now, so they cannot answer differently about the same machine"
            );
        }
    }

    #[test]
    fn the_plan_sizes_the_file_the_session_will_actually_open() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = FakeHome::with_a_model_in_the_cache("weights");

        let dir = home.root.join("weighed");
        std::fs::create_dir_all(&dir).expect("writable");
        // A decoy the loader would never open, deliberately the largest file.
        std::fs::write(dir.join("model.onnx"), vec![0u8; 5 * 1024 * 1024]).expect("writable");
        std::fs::write(dir.join("model_quantized.onnx"), vec![0u8; 1024 * 1024]).expect("writable");
        std::fs::write(
            dir.join("model_quantized.onnx_data"),
            vec![0u8; 2 * 1024 * 1024],
        )
        .expect("writable");

        unsafe { std::env::set_var("CUBA_RERANKER_PATH", &dir) };
        assert_eq!(
            model_weights_mb(),
            Some(3),
            "the weight of a reranker is the file init_session would open plus its external              data, never whatever .onnx happens to be biggest. A real install has 614 KB of              graph next to 4,23 GiB of model.onnx_data: reading only the graph under-counts by              three orders of magnitude, and picking the wrong candidate counts a model that              will never be loaded"
        );
    }

    #[test]
    fn identity_pairs_descending() {
        let pairs = identity_pairs(5);
        for win in pairs.windows(2) {
            assert!(win[0].1 > win[1].1);
        }
    }

    #[tokio::test]
    async fn empty_candidates_returns_empty_without_loading_the_model() {
        let resolved_before = status_resolved();
        let started = std::time::Instant::now();

        let pairs = rerank("q", &[]).await.unwrap();

        assert!(pairs.is_empty());
        assert_eq!(
            status_resolved(),
            resolved_before,
            "an empty rerank must not resolve — let alone load — the model"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "empty rerank took {:?}; it should not touch the model at all",
            started.elapsed()
        );
    }

    #[test]
    fn intra_threads_is_configurable_and_never_zero() {
        unsafe { std::env::set_var("CUBA_RERANK_INTRA_THREADS", "6") };
        assert_eq!(intra_threads(), 6);

        unsafe { std::env::set_var("CUBA_RERANK_INTRA_THREADS", "0") };
        assert!(intra_threads() >= 1, "0 must fall through to the default");

        unsafe { std::env::remove_var("CUBA_RERANK_INTRA_THREADS") };
        let auto = intra_threads();
        assert!((1..=8).contains(&auto), "auto value out of range: {auto}");
    }

    #[test]
    fn only_a_model_that_was_there_and_did_not_open_has_a_reason() {
        assert_eq!(reason_of(&RerankerStatus::Loaded), None);
        assert_eq!(
            reason_of(&RerankerStatus::Unavailable),
            None,
            "no model installed is not a failure. Reporting one here would put it in doctor as a fault to chase instead of a model to install."
        );

        let arena = "load model: BFCArena::AllocateRawInternal";
        assert_eq!(
            reason_of(&RerankerStatus::Failed(arena.into())),
            Some(arena.to_string()),
            "the loader message has to come back word for word: doctor prints it verbatim, and the whole point is that it stopped guessing"
        );
        assert_ne!(
            reason_of(&RerankerStatus::Failed(arena.into())),
            Some(String::new()),
            "an empty reason is worse than none: doctor would print a failure with nothing after the colon"
        );
    }

    #[test]
    fn the_budget_falls_back_rather_than_becoming_zero() {
        assert_eq!(budget_from(Some("5")), std::time::Duration::from_secs(5));
        assert_eq!(
            budget_from(Some(" 45 ")),
            std::time::Duration::from_secs(45)
        );
        assert_eq!(budget_from(None), std::time::Duration::from_secs(20));
        assert_eq!(
            budget_from(Some("0")),
            std::time::Duration::from_secs(20),
            "a zero budget would expire before the permit was even acquired, so every search would report a timeout and no rerank would ever run"
        );
        assert_eq!(
            budget_from(Some("no")),
            std::time::Duration::from_secs(20),
            "a typo must not silently disable reranking"
        );
    }

    #[test]
    fn the_batch_loop_asks_the_clock_before_each_chunk() {
        // spawn_blocking cannot be cancelled, so the only way a batch whose
        // caller gave up releases the session mutex is by checking a deadline
        // itself. Without this the next search waits behind a result nobody
        // is going to read.
        let source = include_str!("rerank.rs");
        let body = source
            .split_once("fn score_pairs(")
            .expect("score_pairs is in this file")
            .1;
        let body = body
            .split_once(
                "
}",
            )
            .expect("the function ends")
            .0;

        let loop_start = body
            .find("for chunk in order.chunks")
            .expect("the batch loop");
        let first_work = body.find("score_one_chunk(").expect("the inference call");
        // The check itself, not the word: `deadline` is also the parameter
        // name, which appears in the signature before the loop.
        let check = body
            .find("Instant::now() >= deadline")
            .expect("the deadline check");
        assert!(
            loop_start < check && check < first_work,
            "the deadline has to be consulted inside the loop and before the inference, or the check only runs once the work it was meant to stop is already done"
        );
    }
}
