use anyhow::{Context, Result};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use std::path::PathBuf;
use std::sync::OnceLock;

static NLI_SESSION: OnceLock<std::sync::Mutex<Session>> = OnceLock::new();
static NLI_TOKENIZER: OnceLock<tokenizers::Tokenizer> = OnceLock::new();
static NLI_STATUS: OnceLock<NliStatus> = OnceLock::new();
static NLI_SEMAPHORE: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
static WANTS_TYPE_IDS: OnceLock<bool> = OnceLock::new();

/// Why NLI is or is not answering, in the three states that ask for three
/// different things from whoever reads them.
///
/// This was a `bool`, and a `bool` collapsed "there was no model to load" with
/// "there was a model and it did not open" — which is the one distinction that
/// matters, because the first is a machine to leave alone and the second is a
/// machine to go and fix. `/health` could only ever say `configured`.
enum NliStatus {
    Loaded,
    /// No model to load: nothing is wrong, `verify` falls back to the judge.
    Unavailable,
    /// A model is on disk and the session did not open. The reason is kept
    /// rather than logged and forgotten: the log line is on the machine, and
    /// whoever is reading `/health` is not.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entailment {
    Supports,
    Contradicts,
    Neutral,
}

impl Entailment {
    pub fn as_verdict(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Neutral => "unrelated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NliVerdict {
    pub label: Entailment,
    pub confidence: f64,
    pub decisive: bool,
    pub probs: [f64; 3],
}

fn intra_threads() -> usize {
    if let Some(n) = std::env::var("CUBA_NLI_INTRA_THREADS")
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

fn semaphore() -> &'static tokio::sync::Semaphore {
    NLI_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(1))
}

fn model_dir() -> Option<PathBuf> {
    if deferred_by_resource_plan() {
        return None;
    }
    if let Ok(p) = std::env::var("CUBA_NLI_PATH") {
        let p = PathBuf::from(p);
        if p.exists() && !p.to_string_lossy().contains("disabled-by-resource-plan") {
            return Some(p);
        }
    }
    cache_model_dir()
}

/// Cache install path, ignoring a resource-plan disable sentinel.
pub fn cache_model_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    let cache = PathBuf::from(home).join(".cache");
    let preferred = cache.join("memory-industry").join("models-nli");
    let legacy = cache.join("cuba-memorys").join("models-nli");
    if preferred.join("model.onnx").exists() || preferred.join("model_quantized.onnx").exists() {
        return Some(preferred);
    }
    if legacy.join("model.onnx").exists() || legacy.join("model_quantized.onnx").exists() {
        return Some(legacy);
    }
    None
}

pub fn available() -> bool {
    cache_model_dir().is_some()
}

pub fn deferred_by_resource_plan() -> bool {
    std::env::var("CUBA_NLI_PATH")
        .ok()
        .is_some_and(|p| p.contains("disabled-by-resource-plan"))
        && cache_model_dir().is_some()
}

pub fn enabled() -> bool {
    matches!(get_status(), NliStatus::Loaded)
}

/// Whether something already tried to open a session.
///
/// Reading the cell instead of forcing it is the whole point: `enabled()` is
/// also what loads ~1 GB, so a caller answering a health poll cannot use it.
pub fn status_resolved() -> bool {
    NLI_STATUS.get().is_some()
}

/// Why NLI is not loaded, when something already tried to load it.
///
/// `None` means it loaded, or nobody has asked yet.
///
/// The sentence is whatever `init` or ONNX Runtime said, model directory
/// included, so it is written for a caller standing on the machine. A caller
/// that answers over a socket puts it through
/// `http::reason_without_a_model_path` first, because that directory is
/// inventory and on Windows it has the operator's user name inside it.
pub fn failure_reason() -> Option<String> {
    reason_of(NLI_STATUS.get()?)
}

/// Only a session that had a model and could not open it has a reason worth
/// showing. "There is no model" is not a failure, and saying it here would put
/// it in `/health` as one.
fn reason_of(status: &NliStatus) -> Option<String> {
    match status {
        NliStatus::Loaded | NliStatus::Unavailable => None,
        NliStatus::Failed(reason) => Some(reason.clone()),
    }
}

fn get_status() -> &'static NliStatus {
    NLI_STATUS.get_or_init(|| match model_dir() {
        Some(dir) => match init(&dir) {
            Ok(()) => {
                tracing::info!(path = %dir.display(), "NLI (mDeBERTa-xnli) cargado — entailment local");
                NliStatus::Loaded
            }
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::warn!(error = %reason, "NLI no pudo cargarse — se usará el juez LLM");
                NliStatus::Failed(reason)
            }
        },
        None => NliStatus::Unavailable,
    })
}

fn init(dir: &std::path::Path) -> Result<()> {
    if crate::embeddings::onnx::locate_onnxruntime().is_none() {
        anyhow::bail!(
            "hay un modelo NLI en {dir:?} pero no encuentro libonnxruntime.so — \
             instalá onnxruntime o apuntá ORT_DYLIB_PATH a la librería"
        );
    }

    let full = dir.join("model.onnx");
    let quantized = dir.join("model_quantized.onnx");
    let model_file = if full.exists() {
        full
    } else if quantized.exists() {
        tracing::warn!(
            path = %quantized.display(),
            "usando el NLI CUANTIZADO: da entailments falsas con confianza \
             (mide 0.62 de 'supports' para una contradicción evidente). Descargá \
             model.onnx (fp32) — cuesta lo mismo por veredicto"
        );
        quantized
    } else {
        anyhow::bail!("no hay model.onnx ni model_quantized.onnx en {dir:?}");
    };

    // A factory, not a builder: if the GPU provider refuses to start we
    // need a second, clean builder for the CPU path.
    let make_builder = || {
        Session::builder()
            .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
            .with_intra_threads(intra_threads())
            .map_err(|e| anyhow::anyhow!("intra threads: {e}"))?
            .with_memory_pattern(false)
            .map_err(|e| anyhow::anyhow!("memory pattern: {e}"))?
            .with_intra_op_spinning(false)
            .map_err(|e| anyhow::anyhow!("intra-op spinning: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {e}"))
    };
    let session = crate::gpu::configure(make_builder, crate::gpu::Workload::Nli)?
        .commit_from_file(&model_file)
        .map_err(|e| anyhow::anyhow!("cargando {model_file:?}: {e}"))?;

    let wants = session
        .inputs()
        .iter()
        .any(|i| i.name() == "token_type_ids");
    let _ = WANTS_TYPE_IDS.set(wants);
    tracing::debug!(token_type_ids = wants, "NLI: inputs del grafo");

    NLI_SESSION
        .set(std::sync::Mutex::new(session))
        .map_err(|_| anyhow::anyhow!("sesión NLI ya inicializada"))?;

    let tok_path = dir.join("tokenizer.json");
    if !tok_path.exists() {
        anyhow::bail!("falta tokenizer.json en {dir:?}");
    }
    let mut tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
        .map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: 512,
            ..Default::default()
        }))
        .map_err(|e| anyhow::anyhow!("truncation: {e}"))?;
    NLI_TOKENIZER
        .set(tokenizer)
        .map_err(|_| anyhow::anyhow!("tokenizer NLI ya inicializado"))?;

    Ok(())
}

const SUPPORT_FLOOR: f64 = 0.80;

const CONTRA_FLOOR: f64 = 0.60;

const NEUTRAL_FLOOR: f64 = 0.50;

const MAX_TOKENS: usize = 512;

pub async fn entails(premise: &str, hypothesis: &str) -> Result<NliVerdict> {
    if !enabled() {
        anyhow::bail!("no hay modelo NLI cargado");
    }
    let premise = premise.trim();
    if premise.is_empty() {
        anyhow::bail!("la evidencia está vacía: no hay nada que pueda apoyar ni contradecir");
    }
    let (p, h) = (premise.to_string(), hypothesis.to_string());

    let _permit = semaphore()
        .acquire()
        .await
        .map_err(|_| anyhow::anyhow!("semáforo NLI cerrado"))?;

    tokio::task::spawn_blocking(move || {
        let (probs, truncated) = classify(&p, &h)?;
        if truncated {
            tracing::warn!(
                chars = p.chars().count(),
                "evidencia recortada a {MAX_TOKENS} tokens: una contradicción más allá del corte no se detectará"
            );
        }
        Ok(decide(probs))
    })
    .await
    .context("la tarea NLI hizo panic")?
}

fn decide(probs: [f64; 3]) -> NliVerdict {
    let (e, n, c) = (probs[0], probs[1], probs[2]);

    let (label, confidence, decisive) = if e >= SUPPORT_FLOOR && e > c {
        (Entailment::Supports, e, true)
    } else if c >= CONTRA_FLOOR && c > e {
        (Entailment::Contradicts, c, true)
    } else if n >= NEUTRAL_FLOOR && n > e && n > c {
        (Entailment::Neutral, n, true)
    } else {
        (Entailment::Neutral, n, false)
    };

    NliVerdict {
        label,
        confidence,
        decisive,
        probs,
    }
}

fn classify(premise: &str, hypothesis: &str) -> Result<([f64; 3], bool)> {
    let tokenizer = NLI_TOKENIZER
        .get()
        .context("tokenizer NLI no inicializado")?;
    let session_lock = NLI_SESSION.get().context("sesión NLI no inicializada")?;

    let encoding = tokenizer
        .encode((premise, hypothesis), true)
        .map_err(|e| anyhow::anyhow!("tokenizando el par: {e}"))?;

    let ids_raw = encoding.get_ids();
    let truncated = ids_raw.len() >= MAX_TOKENS;

    let ids: Vec<i64> = ids_raw.iter().map(|&i| i as i64).collect();
    let mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let types: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

    let shape = vec![1i64, ids.len() as i64];
    let ids_t = ort::value::Tensor::from_array((shape.clone(), ids)).context("tensor input_ids")?;
    let mask_t =
        ort::value::Tensor::from_array((shape.clone(), mask)).context("tensor attention_mask")?;

    let mut session = session_lock
        .lock()
        .map_err(|e| anyhow::anyhow!("lock NLI envenenado: {e}"))?;

    let wants_types = *WANTS_TYPE_IDS.get().unwrap_or(&false);
    let outputs = if wants_types {
        let types_t =
            ort::value::Tensor::from_array((shape, types)).context("tensor token_type_ids")?;
        session.run(ort::inputs! {
            "input_ids" => ids_t,
            "attention_mask" => mask_t,
            "token_type_ids" => types_t,
        })
    } else {
        session.run(ort::inputs! {
            "input_ids" => ids_t,
            "attention_mask" => mask_t,
        })
    }
    .map_err(|e| anyhow::anyhow!("inferencia NLI: {e}"))?;

    if outputs.len() == 0 {
        anyhow::bail!("el modelo NLI no devolvió salidas");
    }

    let logits: Vec<f32> = match outputs[0].try_extract_tensor::<f32>() {
        Ok((_, d)) => d.to_vec(),
        Err(_) => {
            let (_, d) = outputs[0]
                .try_extract_tensor::<half::f16>()
                .map_err(|e| anyhow::anyhow!("extrayendo logits (ni f32 ni f16): {e}"))?;
            d.iter().map(|h| h.to_f32()).collect()
        }
    };

    if logits.len() < 3 {
        anyhow::bail!(
            "esperaba 3 logits (entailment/neutral/contradiction), llegaron {}",
            logits.len()
        );
    }

    let max = logits[..3].iter().cloned().fold(f32::MIN, f32::max);
    let exps: [f64; 3] = [
        ((logits[0] - max) as f64).exp(),
        ((logits[1] - max) as f64).exp(),
        ((logits[2] - max) as f64).exp(),
    ];
    let sum: f64 = exps.iter().sum();

    Ok(([exps[0] / sum, exps[1] / sum, exps[2] / sum], truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envs::ScopedEnv;

    #[test]
    fn real_distributions_get_the_right_verdict() {
        let v = decide([0.000, 0.001, 0.999]);
        assert_eq!(v.label, Entailment::Contradicts);
        assert!(v.decisive);

        let v = decide([0.998, 0.001, 0.001]);
        assert_eq!(v.label, Entailment::Supports);
        assert!(v.decisive);

        let v = decide([0.016, 0.887, 0.097]);
        assert_eq!(v.label, Entailment::Neutral);
        assert!(
            v.decisive,
            "no es indecisión: la evidencia dice claramente que no habla de esto"
        );

        let v = decide([0.002, 0.267, 0.731]);
        assert_eq!(v.label, Entailment::Contradicts);
        assert!(v.decisive);
    }

    #[test]
    fn weak_entailment_never_confirms() {
        let spurious = decide([0.693, 0.100, 0.207]);
        assert!(
            !spurious.decisive,
            "0.69 de entailment no puede confirmar nada: {:?}",
            spurious
        );
        assert_eq!(spurious.label, Entailment::Neutral);

        let genuine = decide([0.952, 0.036, 0.011]);
        assert_eq!(genuine.label, Entailment::Supports);
        assert!(genuine.decisive);
    }

    #[test]
    fn only_a_model_that_was_there_and_did_not_open_has_a_reason() {
        assert_eq!(reason_of(&NliStatus::Loaded), None);
        assert_eq!(
            reason_of(&NliStatus::Unavailable),
            None,
            "no NLI model installed is not a failure. Reporting one here would put `/health` \
             into degraded on every machine that never downloaded the gigabyte, and a field \
             that is red forever stops being read"
        );

        let missing_runtime = "hay un modelo NLI en \"/models/nli\" pero no encuentro \
                               libonnxruntime.so";
        assert_eq!(
            reason_of(&NliStatus::Failed(missing_runtime.into())),
            Some(missing_runtime.to_string()),
            "the loader message has to come back word for word: it is the whole reason the cell \
             stopped being a bool, and `doctor` prints it verbatim"
        );
        assert_ne!(
            reason_of(&NliStatus::Failed(missing_runtime.into())),
            Some(String::new()),
            "an empty reason is worse than none: the reader gets a failure with nothing after \
             the colon"
        );
    }

    #[test]
    fn nothing_that_reads_the_status_can_load_the_model() {
        // `status_resolved` and `failure_reason` are the two accessors
        // `/health` calls on every poll, and `/health` may not force a load —
        // that is why it could only ever say `configured`. Nothing in this
        // binary calls `enabled()`, so the cell is still unresolved here and
        // both of these have to say so rather than resolving it themselves.
        let resolved_before = status_resolved();
        let _ = failure_reason();
        assert_eq!(
            status_resolved(),
            resolved_before,
            "reading why NLI failed resolved the cell that loads it. A `get_or_init` here is a \
             gigabyte read inside a monitor's poll, which is the defect this whole state exists \
             to remove"
        );
    }

    #[test]
    fn verdicts_speak_the_judge_vocabulary() {
        assert_eq!(Entailment::Supports.as_verdict(), "supports");
        assert_eq!(Entailment::Contradicts.as_verdict(), "contradicts");
        assert_eq!(
            Entailment::Neutral.as_verdict(),
            "unrelated",
            "neutral must map to `unrelated`: it counts for NEITHER side, which is the \
             whole repair — being on-topic is not support"
        );
    }

    /// What this answers about the home, pinned before it moves to
    /// `envs::home()`.
    ///
    /// Nothing in the repository fixed it until now: the only home resolution
    /// under test was `envs::home()` itself. `available()` and
    /// `deferred_by_resource_plan()` both derive from this, so a site that
    /// read a different variable — or the same two in the other order — would
    /// have `models nli` write into one directory and the loader look in
    /// another.
    #[tokio::test]
    async fn the_nli_cache_is_looked_for_under_the_home_and_nowhere_else() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let root =
            std::env::temp_dir().join(format!("memory-industry-nli-home-{}", uuid::Uuid::new_v4()));
        let installed = root
            .join(".cache")
            .join("memory-industry")
            .join("models-nli");
        std::fs::create_dir_all(&installed).expect("the test owns this directory");
        std::fs::write(installed.join("model.onnx"), b"not a real graph")
            .expect("temp dir is writable");
        // Never created: if USERPROFILE were read first, the block below would
        // find no model under it and say None.
        let never_created = root.join("userprofile-only");

        {
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                cache_model_dir().as_ref(),
                Some(&installed),
                "HOME is the one an operator sets on purpose, and the one `models nli` wrote \
                 under. Preferring USERPROFILE would install into one directory and load from \
                 another"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
            assert_eq!(
                cache_model_dir().as_ref(),
                Some(&installed),
                "PowerShell and cmd.exe define USERPROFILE and not HOME. Losing this fallback \
                 makes every Windows operator who did not start from Git Bash look like a \
                 machine with no NLI model installed"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            assert_eq!(
                cache_model_dir(),
                None,
                "with neither name set this says None and nothing else. `available()` reads \
                 that as «no model installed», which is a supported machine: making it an \
                 error would turn NLI's absence into a fault in `/health` on every box that \
                 never defined either name"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
